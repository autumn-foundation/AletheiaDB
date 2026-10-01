//! Point-in-time restore (PITR) to a transaction-time coordinate (Issue #3374).
//!
//! Backup/restore (#3217) recovers a database to the exact moment a `.albk`
//! backup was taken. PITR extends that to *any* transaction-time coordinate in
//! between: given a base `.albk` backup **plus an archived WAL chain**, it
//! materializes a fresh database whose recorded history ends exactly at an
//! operator-chosen target (a timestamp or an LSN).
//!
//! The mechanism is a bounded, deterministic replay over the same
//! crash-recovery machinery used at startup:
//!
//! 1. materialize the base snapshot (state at `source_lsn`);
//! 2. read the archived WAL from `source_lsn` (inclusive);
//! 3. keep only the prefix of **whole transaction bands** committed
//!    at-or-before the target (never a partial band — see [`filter_bands`]);
//! 4. replay that prefix into the restored storage, mirroring the startup
//!    differential-replay start-LSN and finalization semantics;
//! 5. persist the target state so a subsequent reopen loads it.
//!
//! Inputs (the `.albk` and the WAL archive) are **read-only**: PITR never
//! mutates them.
//!
//! # Tie-breaking
//!
//! A target falling between two transactions resolves **inclusively at-or-before**
//! the coordinate: every transaction committed at-or-before the target is
//! present, every transaction after it is absent.
//!
//! # Interner vocabulary guard (version-aware since WAL v13)
//!
//! Node/edge **labels** and **property keys** are interned; property *values*
//! are self-contained. The base backup carries the interner as of `source_lsn`.
//! How a WAL segment records those labels/keys — and thus whether the guard is
//! needed at all — depends on the segment's WAL version (Issue #3506, #3745):
//!
//! - **WAL v13+ (the common case):** segments store label / property-key
//!   **strings inline** and re-intern them to LIVE global-interner ids on read.
//!   Those ids are self-describing and always resolve, regardless of a foreign
//!   backup's file-space interner layout, so a post-backup transaction that
//!   introduces a **brand-new label or property key** restores **correctly**.
//!   The guard is therefore **skipped** for an all-v13 archive — range-checking
//!   a live re-interned id against the backup's file-space string count is an
//!   id-space mismatch that would false-reject a perfectly recoverable restore.
//!
//! - **Legacy WAL ≤v12:** segments store labels/keys as raw `u32` file-space
//!   interner ids (`InternedString::from_raw`, with NO re-intern). A post-backup
//!   id `>= K` (where `K` is the restored interner's string count) references a
//!   string the base snapshot does not carry. Replaying it verbatim is **silent
//!   data corruption**, not a mere failed lookup: the id first dangles, then —
//!   because the restored interner's `next_id` equals `K` — the first
//!   genuinely-new string a later write interns collides with the dangling id,
//!   so a replayed node/edge is **mislabeled** (e.g. "Manager" becomes the
//!   user's new "Department") or its property silently dropped.
//!
//! PITR is **fail-closed** for the legacy case: when any ≤v12 raw-label segment
//! is present in the archive, it scans the included band prefix and
//! constraint-declaration slice for any interner id that references a string the
//! base snapshot does not contain and, if found, refuses with
//! [`BackupError::WindowCrossesVocabularyChange`](crate::storage::backup::BackupError::WindowCrossesVocabularyChange)
//! (mapped to `FAILED_PRECONDITION` at the MCP boundary) **before materializing
//! anything** — converting silent mislabeling/dropping into a clean, honest
//! error. To recover a legacy window, keep the label/key vocabulary stable
//! across it, take a fresh base backup that includes the new vocabulary, or
//! target a coordinate before the change.
//!
//! The read-boundary signal that distinguishes the two eras is
//! [`read_entries_from_dir_with_options_reporting_raw_labels`](crate::storage::wal::segment_reader::read_entries_from_dir_with_options_reporting_raw_labels)
//! (Issue #3745); the inline rationale at the guard site in
//! [`AletheiaDB::restore_to_data_dir_at`] documents the gating in full.

use std::path::Path;

#[cfg(feature = "serde")]
use serde::Serialize;

use crate::core::error::{Error, Result};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::storage::backup::{BackupError, check_target_empty, materialize_to_dir, read_artifact};
use crate::storage::wal::segment_reader::{
    carries_string_labels, read_entries_from_dir_with_options,
    read_entries_from_dir_with_options_reporting_raw_labels,
};
use crate::storage::wal::{LSN, WalEntry, WalOperation};

/// A PITR stop target: an absolute transaction-time coordinate.
///
/// Valid time is deliberately **not** a PITR target — physical restore targets
/// transaction time only (valid time is a query dimension; see the issue's
/// out-of-scope note).
#[derive(Debug, Clone, Copy)]
pub enum PitrTarget {
    /// Stop at-or-before a transaction-time commit timestamp.
    AsOf(Timestamp),
    /// Stop at-or-before a WAL LSN.
    Lsn(u64),
}

/// A bi-coordinate (LSN + transaction time) describing a PITR stop point or a
/// window bound. Serde-serializable for `--dry-run` JSON output.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct PitrCoord {
    /// The WAL LSN of the coordinate.
    pub lsn: u64,
    /// The transaction-time coordinate, microseconds since the Unix epoch.
    pub timestamp_micros: i64,
    /// The transaction-time coordinate as an RFC 3339 string (UTC, `Z`).
    pub timestamp_rfc3339: String,
}

impl PitrCoord {
    fn new(lsn: u64, ts: Timestamp) -> Self {
        let micros = ts.wallclock();
        let rfc = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(micros)
            .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
            .unwrap_or_else(|| micros.to_string());
        Self {
            lsn,
            timestamp_micros: micros,
            timestamp_rfc3339: rfc,
        }
    }

    fn describe(&self) -> String {
        format!("lsn={} / {}", self.lsn, self.timestamp_rfc3339)
    }
}

/// The plan a `--dry-run` PITR inspection produces without materializing or
/// opening anything: the achievable window plus, for a given target, the
/// resolved stop coordinate and applied/discarded transaction counts.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct PitrPlan {
    /// The earliest reachable coordinate (the base backup). PITR cannot stop
    /// before this.
    pub earliest: PitrCoord,
    /// The latest reachable coordinate (the archived WAL tail).
    pub latest: PitrCoord,
    /// The coordinate replay would stop at for the given target, or `None`
    /// when no target was supplied or the target resolves to base-only.
    pub resolved_stop: Option<PitrCoord>,
    /// Number of post-backup transactions that would be applied.
    pub transactions_applied: u64,
    /// Number of post-backup transactions that would be discarded (the blast
    /// radius of the rollback).
    pub transactions_discarded: u64,
}

/// Result of [`filter_bands`]: the flattened band prefix plus stats.
struct FilteredBands {
    /// The LSN-ordered WAL entries of every included whole band (data ops with
    /// their `BeginTx`/`CommitTx` markers preserved, plus control ops).
    entries: Vec<WalEntry>,
    /// Number of included transaction bands.
    applied: u64,
    /// Number of complete transaction bands excluded (after the target).
    discarded: u64,
    /// The coordinate of the last included transaction band, if any.
    resolved_stop: Option<PitrCoord>,
}

/// Scan a WAL operation's **label / property-key** positions for the first
/// interner id that is `>= restored_count` — i.e. references a string the base
/// backup's interner (ids `0..restored_count`) does not contain. Property
/// *values* are self-contained (never interned) and are deliberately excluded.
///
/// A `Some` result means this operation would replay a dangling id and silently
/// mislabel/drop data (see the module-level vocabulary guard); `None` means every
/// id it references resolves against the base interner.
fn first_out_of_range_id(op: &WalOperation, restored_count: u32) -> Option<u32> {
    let check = |id: crate::core::interning::InternedString| -> Option<u32> {
        let raw = id.as_u32();
        (raw >= restored_count).then_some(raw)
    };
    let check_keys = |properties: &crate::core::property::PropertyMap| -> Option<u32> {
        properties
            .keys()
            .map(|k| k.as_u32())
            .find(|&raw| raw >= restored_count)
    };
    match op {
        // Create/update of nodes and edges all carry a `label` and a
        // `properties` map whose KEYS are interned (values are inline).
        WalOperation::CreateNode {
            label, properties, ..
        }
        | WalOperation::UpdateNode {
            label, properties, ..
        }
        | WalOperation::CreateEdge {
            label, properties, ..
        }
        | WalOperation::UpdateEdge {
            label, properties, ..
        } => check(*label).or_else(|| check_keys(properties)),
        // Constraint declarations reference an interned label + property key.
        WalOperation::DeclareUniqueConstraint { label, property }
        | WalOperation::DropUniqueConstraint { label, property } => {
            check(*label).or_else(|| check(*property))
        }
        // Deletes/retracts carry only ids/timestamps; control/framing markers
        // carry no vocabulary.
        _ => None,
    }
}

/// Return the first interner id, across all `entries`, that references a string
/// the base backup's interner does not define (`>= restored_count`), or `None`
/// if the whole stream resolves against the base vocabulary.
fn first_unresolved_interned_id<'a>(
    entries: impl IntoIterator<Item = &'a WalEntry>,
    restored_count: u32,
) -> Option<u32> {
    entries
        .into_iter()
        .find_map(|e| first_out_of_range_id(&e.operation, restored_count))
}

/// Whether a replay-window entry must be range-checked by the vocabulary-drift
/// guard (Issue #3745).
///
/// An entry is checked **unless it is KNOWN to carry inline string labels** — a
/// v13+ (`carries_string_labels`) segment whose label / property-key ids were
/// re-interned to LIVE global-interner ids on read and are therefore always
/// resolvable regardless of the foreign backup's file-space layout. Only such an
/// entry is skipped.
///
/// This is deliberately **fail-closed on uncertainty**: an entry whose
/// `segment_version` is `None`/unknown is treated as raw-labeled and IS
/// range-checked. Every entry decoded from disk is stamped `Some(payload_version)`
/// at the single decode site (Issue #3746), so no `None`-bearing entry reaches
/// this guard today — but for a data-corruption guard the correct polarity on the
/// impossible case is to check (refuse dangling ids), never to skip: a future
/// code path that ever surfaces an unstamped raw entry must not be able to
/// silently bypass the range check. `Some(v)` with `v < 13` is likewise checked.
pub(crate) fn entry_needs_vocab_check(e: &WalEntry) -> bool {
    !e.segment_version.is_some_and(carries_string_labels)
}

/// Is this a data-bearing operation (as opposed to a control/framing marker)?
fn is_data_op(op: &WalOperation) -> bool {
    matches!(
        op,
        WalOperation::CreateNode { .. }
            | WalOperation::CreateEdge { .. }
            | WalOperation::UpdateNode { .. }
            | WalOperation::UpdateEdge { .. }
            | WalOperation::DeleteNode { .. }
            | WalOperation::DeleteEdge { .. }
            | WalOperation::RetractNode { .. }
            | WalOperation::RetractEdge { .. }
    )
}

/// A parsed transaction band: the atomic unit PITR includes or excludes.
struct Band {
    entries: Vec<WalEntry>,
    /// The band's stop coordinate (its `CommitTx`, or a legacy/raw op's own).
    stop_lsn: LSN,
    stop_ts: Timestamp,
    /// Whether the band represents a data transaction (counted in stats) as
    /// opposed to a self-committing control op (constraint/checkpoint).
    is_transaction: bool,
    /// Whether the band is a complete, durable transaction. Incomplete
    /// (torn-tail) bands are excluded from replay and never counted.
    complete: bool,
}

impl Band {
    /// Whether this band is a complete, post-backup transaction counted in the
    /// applied/discarded stats: a durable data transaction whose commit LSN is
    /// at or above the base backup's replay floor (bands fully below
    /// `source_lsn` already live in the base snapshot).
    fn is_post_backup_transaction(&self, source_lsn: u64) -> bool {
        self.complete && self.is_transaction && self.stop_lsn.0 >= source_lsn
    }
}

/// Group an LSN-sorted WAL entry stream into whole transaction bands.
///
/// A framed `[BeginTx .. CommitTx]` frame is one band stamped with the
/// `CommitTx` coordinate; a legacy (`!framed`) entry or a framed raw/control op
/// is a singleton band stamped with its own coordinate. A `BeginTx` with no
/// matching `CommitTx` (crash tail) yields a single `complete = false` band.
fn parse_bands(entries: Vec<WalEntry>) -> Vec<Band> {
    /// Flush a still-open frame as an INCOMPLETE (torn) band, draining `open`.
    fn flush_incomplete(open: &mut Vec<WalEntry>, bands: &mut Vec<Band>) {
        if let Some(last) = open.last() {
            let (stop_lsn, stop_ts) = (last.lsn, last.timestamp);
            bands.push(Band {
                stop_lsn,
                stop_ts,
                is_transaction: true,
                complete: false,
                entries: std::mem::take(open),
            });
        }
    }

    let mut bands: Vec<Band> = Vec::new();
    let mut open: Vec<WalEntry> = Vec::new();
    let mut open_active = false;

    for entry in entries {
        if !entry.framed {
            // Legacy / non-transactional entry: apply as a singleton band.
            if open_active {
                flush_incomplete(&mut open, &mut bands);
                open_active = false;
            }
            let is_tx = is_data_op(&entry.operation);
            bands.push(Band {
                stop_lsn: entry.lsn,
                stop_ts: entry.timestamp,
                is_transaction: is_tx,
                complete: true,
                entries: vec![entry],
            });
            continue;
        }

        match &entry.operation {
            WalOperation::BeginTx { .. } => {
                if open_active {
                    flush_incomplete(&mut open, &mut bands);
                }
                open_active = true;
                open.push(entry);
            }
            WalOperation::CommitTx {
                commit_timestamp, ..
            } => {
                if open_active {
                    let stop_lsn = entry.lsn;
                    let stop_ts = *commit_timestamp;
                    open.push(entry);
                    bands.push(Band {
                        stop_lsn,
                        stop_ts,
                        is_transaction: true,
                        complete: true,
                        entries: std::mem::take(&mut open),
                    });
                    open_active = false;
                }
                // A stray CommitTx with no open frame is a benign skip (its
                // BeginTx lies below the read floor) — drop it.
            }
            WalOperation::Checkpoint { .. }
            | WalOperation::DeclareUniqueConstraint { .. }
            | WalOperation::DropUniqueConstraint { .. } => {
                if open_active {
                    flush_incomplete(&mut open, &mut bands);
                    open_active = false;
                }
                bands.push(Band {
                    stop_lsn: entry.lsn,
                    stop_ts: entry.timestamp,
                    is_transaction: false,
                    complete: true,
                    entries: vec![entry],
                });
            }
            _ => {
                // A framed data op.
                if open_active {
                    open.push(entry);
                } else {
                    // Raw framed append with no open frame (test/tooling path):
                    // treat as a singleton transaction band.
                    bands.push(Band {
                        stop_lsn: entry.lsn,
                        stop_ts: entry.timestamp,
                        is_transaction: true,
                        complete: true,
                        entries: vec![entry],
                    });
                }
            }
        }
    }

    // Any still-open frame at end-of-stream is an uncommitted crash tail.
    if open_active {
        flush_incomplete(&mut open, &mut bands);
    }

    bands
}

/// Whether a band's stop coordinate is at-or-before the target.
fn band_within(band: &Band, target: &PitrTarget) -> bool {
    match target {
        PitrTarget::AsOf(t) => band.stop_ts <= *t,
        PitrTarget::Lsn(n) => band.stop_lsn.0 <= *n,
    }
}

/// Filter an LSN-sorted **full** WAL entry stream (from `LSN::initial()`, not a
/// floor-cut read) to the prefix of whole transaction bands committed
/// at-or-before `target` (inclusive tie-break), restricted to bands at or above
/// the base backup's replay floor `source_lsn`.
///
/// Parsing whole bands from the full stream, rather than from a read that starts
/// at `LSN(source_lsn)`, structurally eliminates **band-straddle** (Issue #3374,
/// F5): if `source_lsn` ever lands INSIDE a `[BeginTx .. CommitTx]` frame, that
/// frame is still parsed as one whole band — stamped with its authoritative
/// `CommitTx` coordinate — and is either wholly included or wholly excluded,
/// never split into mis-timed singleton data ops. A band whose commit LSN is
/// below `source_lsn` is already captured by the base snapshot and is skipped
/// (differential replay). The straddling band's sub-`source_lsn` data ops are
/// re-touched on replay, which is safe under the #3419 idempotent
/// re-application guards.
///
/// Never includes a partial band; drops an incomplete trailing band (torn
/// tail); returns the flattened prefix (markers preserved for replay), the
/// applied/discarded transaction counts (relative to post-`source_lsn`
/// transactions), and the resolved stop coordinate.
fn filter_bands(entries: Vec<WalEntry>, target: &PitrTarget, source_lsn: u64) -> FilteredBands {
    let bands = parse_bands(entries);

    // Post-backup transaction bands are those whose commit LSN is at or above
    // the replay floor; bands fully below `source_lsn` live in the base snapshot
    // and are neither replayed nor counted.
    let total_transactions = bands
        .iter()
        .filter(|b| b.is_post_backup_transaction(source_lsn))
        .count() as u64;

    let mut out: Vec<WalEntry> = Vec::new();
    let mut applied: u64 = 0;
    let mut resolved_stop: Option<PitrCoord> = None;
    let mut stopped = false;

    for band in bands {
        if !band.complete {
            continue; // torn tail: excluded, never counted
        }
        if band.stop_lsn.0 < source_lsn {
            continue; // already in the base snapshot (differential replay floor)
        }
        if stopped {
            continue; // prefix already closed; remaining bands are discarded
        }
        if band_within(&band, target) {
            let coord = PitrCoord::new(band.stop_lsn.0, band.stop_ts);
            if band.is_transaction {
                applied += 1;
                resolved_stop = Some(coord);
            }
            out.extend(band.entries);
        } else {
            // Monotonicity assumption (F6): the stream is LSN-sorted, and the
            // WAL's HLC <-> LSN contract makes commit timestamps monotonic with
            // commit LSN — a later-committing transaction has BOTH a strictly
            // greater `CommitTx` LSN and a strictly greater commit timestamp.
            // So once a band's stop coordinate exceeds the target, every later
            // band's does too: closing the prefix at the first out-of-range band
            // is correct for an `AsOf` (timestamp) target exactly as it is for an
            // `Lsn` target. If that contract were ever violated, a later band
            // committing at a smaller timestamp could be wrongly included; the
            // WAL write path upholds it.
            stopped = true;
        }
    }

    FilteredBands {
        entries: out,
        applied,
        discarded: total_transactions.saturating_sub(applied),
        resolved_stop,
    }
}

/// The achievable PITR window derived from the base backup + archived WAL.
struct PitrWindow {
    /// The base backup coordinate (the earliest reachable LSN).
    source_lsn: u64,
    /// Max commit timestamp of any transaction already in the base
    /// (`lsn < source_lsn`); `None` if the base is empty.
    base_ts: Option<Timestamp>,
    /// The latest reachable transaction's LSN.
    latest_lsn: u64,
    /// The latest reachable transaction's commit timestamp.
    latest_ts: Timestamp,
}

impl PitrWindow {
    fn earliest_coord(&self) -> PitrCoord {
        PitrCoord::new(
            self.source_lsn,
            self.base_ts.unwrap_or_else(|| Timestamp::from(0)),
        )
    }

    fn latest_coord(&self) -> PitrCoord {
        PitrCoord::new(self.latest_lsn, self.latest_ts)
    }

    /// Reject a target that lies **outside** the achievable window — below the
    /// base backup (unreachable from base + forward replay) or above the archived
    /// WAL tail (Issue #3374, F2). An explicit target above the tail is an error,
    /// not a silent full replay; a caller who wants "restore to the tail" passes
    /// no target (`--latest`), which resolves to the latest coordinate — see
    /// [`AletheiaDB::restore_to_data_dir_at`].
    fn validate(&self, target: &PitrTarget) -> Result<()> {
        let below = match target {
            PitrTarget::Lsn(n) => *n < self.source_lsn,
            PitrTarget::AsOf(t) => self.base_ts.is_some_and(|bt| *t < bt),
        };
        let above = match target {
            PitrTarget::Lsn(n) => *n > self.latest_lsn,
            PitrTarget::AsOf(t) => *t > self.latest_ts,
        };
        if below || above {
            let requested = match target {
                PitrTarget::Lsn(n) => format!("lsn={n}"),
                PitrTarget::AsOf(t) => PitrCoord::new(0, *t).timestamp_rfc3339,
            };
            return Err(Error::Backup(BackupError::TargetOutsideWindow {
                requested,
                earliest: self.earliest_coord().describe(),
                latest: self.latest_coord().describe(),
            }));
        }
        Ok(())
    }
}

/// Compute the achievable window by scanning the full archive once.
fn compute_window(all: &[WalEntry], source_lsn: u64) -> PitrWindow {
    let mut base_ts: Option<Timestamp> = None;
    let mut latest_lsn = source_lsn;
    let mut latest_ts = Timestamp::from(0);
    let mut have_latest = false;

    for e in all {
        // A transaction's commit coordinate: the framed `CommitTx`, or a legacy
        // (unframed) data op's own coordinate. Control ops and framed data ops
        // (accounted by their `CommitTx`) are skipped to avoid double counting.
        let coord = match &e.operation {
            WalOperation::CommitTx {
                commit_timestamp, ..
            } => Some((e.lsn.0, *commit_timestamp)),
            op if is_data_op(op) && !e.framed => Some((e.lsn.0, e.timestamp)),
            _ => None,
        };
        if let Some((lsn, ts)) = coord {
            if lsn < source_lsn {
                base_ts = Some(base_ts.map_or(ts, |b| b.max(ts)));
            }
            if !have_latest || lsn > latest_lsn {
                latest_lsn = lsn;
                latest_ts = ts;
                have_latest = true;
            }
        }
    }

    if !have_latest {
        latest_lsn = source_lsn;
        latest_ts = base_ts.unwrap_or_else(|| Timestamp::from(0));
    }

    PitrWindow {
        source_lsn,
        base_ts,
        latest_lsn,
        latest_ts,
    }
}

/// Count complete post-backup transaction bands in a full stream (for the
/// no-target dry-run / to-latest full replay). Bands whose commit LSN is below
/// `source_lsn` live in the base snapshot and are not counted.
fn count_transactions(entries: &[WalEntry], source_lsn: u64) -> u64 {
    parse_bands(entries.to_vec())
        .iter()
        .filter(|b| b.is_post_backup_transaction(source_lsn))
        .count() as u64
}

/// Whether a raw entry's own coordinate is at-or-before the target (used to
/// scope constraint declarations, which are self-committing control ops).
fn entry_at_or_before(entry: &WalEntry, target: &PitrTarget) -> bool {
    match target {
        PitrTarget::AsOf(t) => entry.timestamp <= *t,
        PitrTarget::Lsn(n) => entry.lsn.0 <= *n,
    }
}

impl AletheiaDB {
    /// Restore a base `.albk` backup plus its archived WAL chain to an exact
    /// transaction-time coordinate, producing a fresh durable database at
    /// `data_dir` whose recorded history ends at `target` (Issue #3374).
    ///
    /// Every transaction committed at-or-before `target` is present with full
    /// bi-temporal fidelity; every transaction after it is absent. Constraints
    /// (#3218) and provenance (#3224) are re-established at the target.
    ///
    /// Inputs are **read-only**: neither `albk` nor `wal_archive` is mutated.
    /// The target directory must be empty.
    ///
    /// # Errors
    ///
    /// - [`BackupError::TargetNotEmpty`] — `data_dir` already holds data.
    /// - [`BackupError::TargetOutsideWindow`] — `target` is outside the
    ///   achievable window: below the base backup (unreachable) **or** above the
    ///   archived WAL tail (Issue #3374, F2 — an above-tail target is an error,
    ///   not a silent full replay). The error names the achievable window. For a
    ///   deliberate "restore to the tail" pass the latest coordinate (the CLI's
    ///   no-target / `--latest` path resolves this from
    ///   [`AletheiaDB::inspect_pitr`]).
    /// - [`BackupError::WindowCrossesVocabularyChange`] — a transaction
    ///   at-or-before `target` introduced a label/property key absent from the
    ///   base backup's interner; replaying it would silently mislabel/drop data,
    ///   so PITR refuses (Issue #3374, F1).
    /// - Artifact/WAL read or replay failures propagate as [`Error`].
    pub fn restore_to_data_dir_at(
        albk: &Path,
        wal_archive: &Path,
        target: PitrTarget,
        data_dir: &Path,
    ) -> Result<AletheiaDB> {
        // Canonical index-persistence root (mirrors `restore_to_data_dir`).
        let index_root = data_dir.join("indexes");
        check_target_empty(&index_root).map_err(Error::Backup)?;

        // Read the artifact header and the archive BEFORE materializing, so an
        // out-of-window target (or a vocabulary-change crossing) fails without
        // touching the target directory.
        let payload = read_artifact(albk).map_err(Error::Backup)?;
        let source_lsn = payload.source_lsn;
        // The base interner defines ids `0..restored_interner_count`; any WAL id
        // at or above this references a string introduced after the backup.
        let restored_interner_count = payload.interner.strings.len() as u32;

        // Read the whole archive, and learn whether ANY segment used the pre-v13
        // raw-label encoding (Issue #3506). v13+ segments store label /
        // property-key STRINGS inline and re-intern them to LIVE global-interner
        // ids on read, so their ids are self-describing and always resolvable;
        // only pre-v13 (`from_raw`) segments carry raw file-space ids that can
        // dangle past a foreign backup's archived interner. This flag gates the
        // vocabulary-drift guard below.
        let (all, archive_has_raw_labels) =
            read_entries_from_dir_with_options_reporting_raw_labels(
                wal_archive,
                LSN::initial(),
                None,
                true,
            )?;
        let window = compute_window(&all, source_lsn);
        window.validate(&target)?;

        // Net constraint state at the target from the FULL archive (including
        // declarations that predate the backup, which the base snapshot does not
        // carry — the differential replay would otherwise miss them). Only the
        // constraint ops matter to `apply_constraint_declarations`; filtering to
        // them keeps this slice small even for large archives.
        let constraint_slice: Vec<WalEntry> = all
            .iter()
            .filter(|e| {
                matches!(
                    e.operation,
                    WalOperation::DeclareUniqueConstraint { .. }
                        | WalOperation::DropUniqueConstraint { .. }
                ) && entry_at_or_before(e, &target)
            })
            .cloned()
            .collect();

        // The band prefix (whole bands, differential floor at `source_lsn`).
        // Parsing from the FULL stream eliminates band-straddle (F5).
        let filtered = filter_bands(all, &target, source_lsn);

        // Vocabulary-change guard (F1) — version-aware since WAL v13 (Issue
        // #3506, made this guard obsolete for v13; kept for ≤v12).
        //
        // The guard compares each included op's label / property-key interner ids
        // against the base backup's *file-space* interner count. That comparison
        // is only meaningful for RAW ids: a pre-v13 (`!carries_string_labels`)
        // segment stores raw file-space ids via `InternedString::from_raw` with
        // NO re-intern, so an id `>= restored_interner_count` references a string
        // the base backup does not carry and would silently mislabel/drop data on
        // replay — we MUST still refuse (fail-closed).
        //
        // v13+ segments instead store the label / property-key STRINGS inline and
        // re-intern them into the process-global interner on read
        // (`segment_reader::read_label`, `PropertyMap` key codec). By the time the
        // entries reach this guard those strings have ALREADY interned
        // successfully (else the archive read above would have errored), so their
        // (live) ids are always resolvable regardless of the foreign backup's
        // file-space layout. Range-checking those live ids against the backup's
        // file-space count is an id-space mismatch that false-rejects perfectly
        // recoverable foreign restores — so when the archive is all-v13 we skip
        // the check entirely.
        //
        // Gating is now PER-ENTRY (Issue #3745 follow-up): every WAL entry
        // recovered from disk carries the payload format version it was decoded
        // from (`WalEntry::segment_version`, Issue #3746, stamped at the single
        // decode site and preserved intact through band filtering and the
        // constraint slice, which move/clone whole entries). An entry is
        // raw-label iff `entry_needs_vocab_check` returns true. We range-check
        // ONLY the raw-label entries in the replay window (band prefix +
        // constraint slice), so a fully-v13 window is never checked even when the
        // archive ALSO contains out-of-band ≤v12 segments.
        //
        // The predicate is fail-CLOSED on uncertainty: an entry is skipped ONLY
        // when it is KNOWN to carry inline string labels (`Some(v>=13)`); a
        // `None`/unknown version — or any `Some(v<13)` — is checked. Every
        // disk-decoded entry is stamped `Some(payload_version)` at the single
        // decode site (Issue #3746), so no `None` entry reaches here today; the
        // fail-closed polarity guarantees that if one ever did, a dangling raw id
        // would still be refused rather than silently replayed.
        //
        // This lifts the former over-rejection (a database created under a ≤v12
        // binary and later upgraded to v13 produces a genuinely MIXED archive:
        // old ≤v12 raw-label segments + new v13 inline-string appends). Such an
        // archive whose replay window is fully v13 now RESTORES: its v13 entries'
        // live re-interned ids resolve fine and are no longer range-checked
        // against the foreign backup's file-space count. The whole-archive
        // `archive_has_raw_labels` flag is retained only as a cheap fast-path —
        // when no segment in the archive is raw-label there is nothing to check.
        //
        // The fail-closed guarantee is UNCHANGED for genuine ≤v12 raw entries in
        // the window: a raw entry whose label / property-key id is out of range
        // (`>= restored_interner_count`) references a string the base backup does
        // not carry and would silently mislabel/drop data on replay, so it is
        // still refused with `WindowCrossesVocabularyChange` before any write.
        if archive_has_raw_labels
            && let Some(first_unresolved_id) = first_unresolved_interned_id(
                filtered
                    .entries
                    .iter()
                    .chain(constraint_slice.iter())
                    .filter(|e| entry_needs_vocab_check(e)),
                restored_interner_count,
            )
        {
            return Err(Error::Backup(BackupError::WindowCrossesVocabularyChange {
                first_unresolved_id,
                restored_interner_count,
            }));
        }

        // Materialize the base snapshot and open the base-state database. The
        // open path fully finalizes the base (id generators, temporal index,
        // extent, HLC seed) from the snapshot at `source_lsn`.
        materialize_to_dir(&payload, &index_root).map_err(Error::Backup)?;
        drop(payload);

        // Everything past this point mutates the (now committed) base manifest.
        // If any step fails, the target `indexes/` would otherwise be left
        // looking like a complete base-only restore (no in-progress sentinel),
        // blocking a clean retry. Wrap the replay + persistence so any error
        // removes the target `indexes/` before returning, matching #3217's
        // "a failed restore leaves a clean target" posture (Issue #3374, F4).
        let outcome = Self::finish_pitr_replay(data_dir, filtered, &constraint_slice);
        match outcome {
            Ok(db) => Ok(db),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&index_root);
                Err(e)
            }
        }
    }

    /// Post-materialize half of [`AletheiaDB::restore_to_data_dir_at`]: open the
    /// base-state DB, apply the net constraints, replay the band prefix, and
    /// persist the target state. Extracted so the caller can clean up the target
    /// directory if any step here fails (Issue #3374, F4).
    fn finish_pitr_replay(
        data_dir: &Path,
        filtered: FilteredBands,
        constraint_slice: &[WalEntry],
    ) -> Result<AletheiaDB> {
        let config = crate::config::durable_config_for_data_dir(data_dir.to_path_buf());
        let db = AletheiaDB::with_unified_config(config)?;

        crate::storage::recovery::apply_constraint_declarations(
            constraint_slice,
            &db.constraint_registry,
        );

        // Bounded differential replay of the band prefix into current +
        // historical storage.
        {
            let initial_version_id = db.version_id_gen.current();
            let mut hist = db.historical.write();
            let (_final_lsn, max_node_id, max_edge_id, next_version_id) =
                crate::storage::recovery::replay_entries_into_storage_with_constraints(
                    &db.wal,
                    filtered.entries,
                    &db.current,
                    &mut hist,
                    initial_version_id,
                    Some(&db.constraint_registry),
                )?;
            drop(hist);

            if let Some(m) = max_node_id {
                db.node_id_gen.ensure_at_least(m + 1);
            }
            if let Some(m) = max_edge_id {
                db.edge_id_gen.ensure_at_least(m + 1);
            }
            db.version_id_gen.ensure_at_least(next_version_id);
        }

        // Rebuild the reservation index for declared constraints from the
        // restored current-state nodes.
        for (label_str, property_str) in db.constraint_registry.list() {
            if let (Some(label_id), Some(property_id)) = (
                GLOBAL_INTERNER.get_id(&label_str),
                GLOBAL_INTERNER.get_id(&property_str),
            ) {
                let nodes = db.current.get_nodes_by_label(&label_str);
                db.constraint_registry
                    .rebuild_from_nodes(&nodes, label_id, property_id);
            }
        }

        // Wire the replayed versions into the temporal index and reseed the HLC.
        // Reuse the startup seed helper (Issue #3387 closed-tx-end folding lives
        // there): on a freshly-opened restored DB the recomputed bootstrap value
        // is always >= the value `open()` already seeded (same `now()` floor over
        // a superset of versions), so the helper's unconditional set is identical
        // to the previous `if max_ts > *ct` guard.
        db.historical.write().rebuild_temporal_index_from_versions();
        crate::db::config::seed_startup_current_timestamp(&db)?;

        // Record the net constraint declarations in the target WAL so a later
        // reopen recovers them (index snapshots do not carry constraints), then
        // persist the target state so a reopen loads it.
        for (label_str, property_str) in db.constraint_registry.list() {
            if let (Some(label_id), Some(property_id)) = (
                GLOBAL_INTERNER.get_id(&label_str),
                GLOBAL_INTERNER.get_id(&property_str),
            ) {
                db.wal.append(WalOperation::DeclareUniqueConstraint {
                    label: label_id,
                    property: property_id,
                })?;
            }
        }
        db.wal.flush()?;

        if let (Some(manager), Some(tracker)) = (&db.persistence_manager, &db.persistence_tracker) {
            crate::storage::index_persistence::operations::persist_all_indexes(
                &db.current,
                &db.historical,
                &db.temporal_indexes,
                &db.wal,
                manager,
                tracker,
            )?;
        }

        Ok(db)
    }

    /// Inspect the achievable PITR window and, for a given `target`, the
    /// resolved stop and the count of transactions that would be discarded —
    /// without materializing or opening anything (the `--dry-run` path).
    ///
    /// # Errors
    ///
    /// - [`BackupError::TargetOutsideWindow`] — `target` is outside the window
    ///   (below the base backup or above the archived WAL tail).
    /// - Artifact/WAL read failures propagate as [`Error`].
    pub fn inspect_pitr(
        albk: &Path,
        wal_archive: &Path,
        target: Option<PitrTarget>,
    ) -> Result<PitrPlan> {
        let source_lsn = read_artifact(albk).map_err(Error::Backup)?.source_lsn;
        let all = read_entries_from_dir_with_options(wal_archive, LSN::initial(), None, true)?;
        let window = compute_window(&all, source_lsn);

        let (resolved_stop, applied, discarded) = match &target {
            Some(t) => {
                window.validate(t)?;
                // Band selection is over the full stream with a `source_lsn`
                // differential floor (F5); parsing whole bands is straddle-safe.
                let f = filter_bands(all, t, source_lsn);
                (f.resolved_stop, f.applied, f.discarded)
            }
            None => {
                // No target: report the full window; a full replay to the tail
                // applies every reachable post-backup transaction, discarding
                // none.
                let total = count_transactions(&all, source_lsn);
                (Some(window.latest_coord()), total, 0)
            }
        };

        Ok(PitrPlan {
            earliest: window.earliest_coord(),
            latest: window.latest_coord(),
            resolved_stop,
            transactions_applied: applied,
            transactions_discarded: discarded,
        })
    }
}

#[cfg(test)]
mod band_filter_tests {
    use super::*;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMap;

    fn ts(v: i64) -> Timestamp {
        HybridTimestamp::new_unchecked(v, 0)
    }

    fn create_node_op(id: u64) -> WalOperation {
        WalOperation::CreateNode {
            node_id: crate::core::NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern("PitrBand").unwrap(),
            properties: PropertyMap::new(),
            valid_from: ts(1),
            provenance: None,
        }
    }

    fn framed(lsn: u64, op: WalOperation, timestamp: Timestamp) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp,
            operation: op,
            checksum: 0,
            framed: true,
            // In-memory test fixture; segment version is irrelevant to the PITR
            // framing logic under test (Issue #3746).
            segment_version: None,
        }
    }

    fn legacy(lsn: u64, op: WalOperation, timestamp: Timestamp) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp,
            operation: op,
            checksum: 0,
            framed: false,
            // In-memory test fixture; segment version is irrelevant to the PITR
            // framing logic under test (Issue #3746).
            segment_version: None,
        }
    }

    /// Build a framed transaction band `[BeginTx, data.., CommitTx]` occupying
    /// contiguous LSNs starting at `begin_lsn`, committing at `commit_ts`.
    fn tx(begin_lsn: u64, tx_id: u64, data: usize, commit_ts: Timestamp) -> Vec<WalEntry> {
        let mut v = vec![framed(begin_lsn, WalOperation::BeginTx { tx_id }, ts(0))];
        for i in 0..data {
            let lsn = begin_lsn + 1 + i as u64;
            v.push(framed(lsn, create_node_op(lsn), ts(0)));
        }
        let commit_lsn = begin_lsn + 1 + data as u64;
        v.push(framed(
            commit_lsn,
            WalOperation::CommitTx {
                tx_id,
                entry_count: data as u32,
                commit_timestamp: commit_ts,
            },
            ts(0),
        ));
        v
    }

    fn all_of(bands: Vec<Vec<WalEntry>>) -> Vec<WalEntry> {
        bands.into_iter().flatten().collect()
    }

    /// Build an entry carrying an explicit `segment_version` (the only field the
    /// vocabulary-guard predicate inspects).
    fn entry_with_version(segment_version: Option<u8>) -> WalEntry {
        WalEntry {
            lsn: LSN(1),
            timestamp: ts(1),
            operation: create_node_op(1),
            checksum: 0,
            framed: true,
            segment_version,
        }
    }

    /// Locks the fail-closed-on-unknown polarity of the #3745 vocabulary guard.
    /// A future `None`-bearing raw entry must be CHECKED (true), never silently
    /// skipped, and only a segment KNOWN to carry inline string labels
    /// (`Some(v>=13)`) is skipped (false).
    #[test]
    fn entry_needs_vocab_check_is_fail_closed_on_unknown() {
        // Known raw (pre-v13) segment: MUST be range-checked.
        assert!(
            entry_needs_vocab_check(&entry_with_version(Some(11))),
            "a pre-v13 (raw-label) entry must be checked"
        );
        // Unknown/unstamped version: fail CLOSED — MUST be range-checked, not
        // skipped (this is the case the old `is_some_and(!string)` form failed
        // OPEN on).
        assert!(
            entry_needs_vocab_check(&entry_with_version(None)),
            "an entry with unknown segment version must fail closed and be checked"
        );
        // Known string-label (v13+) segment: safe to skip.
        assert!(
            !entry_needs_vocab_check(&entry_with_version(Some(13))),
            "a v13+ (inline-string) entry needs no range check"
        );
    }

    #[test]
    fn asof_exact_boundary_is_inclusive() {
        // Two transactions committing at t=100 and t=200; target exactly 100
        // keeps the first, drops the second.
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(100)), 0);
        assert_eq!(f.applied, 1);
        assert_eq!(f.discarded, 1);
        assert_eq!(f.resolved_stop.as_ref().unwrap().timestamp_micros, 100);
        // Prefix is exactly the first band (Begin + 1 data + Commit = 3).
        assert_eq!(f.entries.len(), 3);
    }

    #[test]
    fn asof_between_transactions_takes_at_or_before() {
        // Commits at 100 and 200; target 150 keeps only the first.
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(150)), 0);
        assert_eq!(f.applied, 1);
        assert_eq!(f.discarded, 1);
        assert_eq!(f.resolved_stop.unwrap().timestamp_micros, 100);
    }

    #[test]
    fn target_before_all_yields_empty_prefix() {
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(50)), 0);
        assert_eq!(f.applied, 0);
        assert_eq!(f.discarded, 2);
        assert!(f.resolved_stop.is_none());
        assert!(f.entries.is_empty());
    }

    #[test]
    fn target_after_all_yields_full_prefix() {
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(9999)), 0);
        assert_eq!(f.applied, 2);
        assert_eq!(f.discarded, 0);
        assert_eq!(f.resolved_stop.unwrap().timestamp_micros, 200);
        assert_eq!(f.entries.len(), 6);
    }

    #[test]
    fn lsn_target_stops_at_commit_lsn() {
        // First band commits at LSN 3, second at LSN 6.
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::Lsn(3), 0);
        assert_eq!(f.applied, 1);
        assert_eq!(f.discarded, 1);
        assert_eq!(f.resolved_stop.unwrap().lsn, 3);

        // A target between the two commit LSNs (4 or 5) still keeps only the
        // first whole band — never a partial second band.
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::Lsn(5), 0);
        assert_eq!(f.applied, 1);
        assert_eq!(f.entries.len(), 3);
    }

    #[test]
    fn torn_trailing_band_is_excluded() {
        // A complete band, then a BeginTx with no CommitTx (crash tail).
        let mut entries = tx(1, 1, 1, ts(100));
        entries.push(framed(4, WalOperation::BeginTx { tx_id: 2 }, ts(0)));
        entries.push(framed(5, create_node_op(5), ts(0)));
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(9999)), 0);
        assert_eq!(f.applied, 1, "only the complete band is applied");
        assert_eq!(
            f.discarded, 0,
            "the torn tail is not a committed transaction"
        );
        assert_eq!(f.entries.len(), 3);
    }

    #[test]
    fn mixed_framed_and_legacy_entries() {
        // A legacy (pre-v7) data op at t=50, then a framed tx at t=150.
        let mut entries = vec![legacy(1, create_node_op(1), ts(50))];
        entries.extend(tx(2, 1, 1, ts(150)));
        let f = filter_bands(entries.clone(), &PitrTarget::AsOf(ts(100)), 0);
        assert_eq!(f.applied, 1, "only the legacy op is at-or-before 100");
        assert_eq!(f.discarded, 1);
        assert_eq!(f.resolved_stop.unwrap().timestamp_micros, 50);

        let f = filter_bands(entries, &PitrTarget::AsOf(ts(200)), 0);
        assert_eq!(f.applied, 2);
        assert_eq!(f.discarded, 0);
    }

    #[test]
    fn control_ops_are_included_but_not_counted() {
        // A constraint declaration between two transactions is carried in the
        // prefix (so replay re-establishes it) but is not a counted transaction.
        let decl = framed(
            4,
            WalOperation::DeclareUniqueConstraint {
                label: GLOBAL_INTERNER.intern("PitrBand").unwrap(),
                property: GLOBAL_INTERNER.intern("k").unwrap(),
            },
            ts(120),
        );
        let mut entries = tx(1, 1, 1, ts(100));
        entries.push(decl);
        entries.extend(tx(5, 2, 1, ts(200)));
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(150)), 0);
        assert_eq!(f.applied, 1, "one transaction band before 150");
        assert_eq!(f.discarded, 1);
        // Begin+data+Commit (3) + the declaration (1) = 4 entries carried.
        assert_eq!(f.entries.len(), 4);
    }

    #[test]
    fn window_rejects_targets_outside_bounds() {
        // Base at source_lsn=10 with a pre-backup commit at t=500 (lsn 3) and a
        // post-backup commit at t=800 (begin 10, data 11, commit lsn 12).
        let all = all_of(vec![tx(1, 1, 1, ts(500)), tx(10, 2, 1, ts(800))]);
        let window = compute_window(&all, 10);
        // A timestamp before the base's latest commit is unreachable (below).
        assert!(window.validate(&PitrTarget::AsOf(ts(400))).is_err());
        // At-or-after the base is fine.
        assert!(window.validate(&PitrTarget::AsOf(ts(500))).is_ok());
        // An LSN below source_lsn is unreachable (below).
        assert!(window.validate(&PitrTarget::Lsn(3)).is_err());
        assert!(window.validate(&PitrTarget::Lsn(10)).is_ok());
        // The latest coordinate itself is in-window (a to-tail full replay).
        assert!(window.validate(&PitrTarget::AsOf(ts(800))).is_ok());
        assert!(window.validate(&PitrTarget::Lsn(12)).is_ok());
        // F2: an explicit target ABOVE the archived tail is an error, not a
        // silent full replay.
        assert!(window.validate(&PitrTarget::AsOf(ts(801))).is_err());
        assert!(window.validate(&PitrTarget::Lsn(13)).is_err());
        // The window bounds are reported.
        assert_eq!(window.earliest_coord().lsn, 10);
        assert_eq!(window.latest_coord().lsn, 12);
        assert_eq!(window.latest_coord().timestamp_micros, 800);
    }

    #[test]
    fn source_lsn_inside_a_band_selects_whole_band() {
        // F5 regression: `source_lsn` lands INSIDE the second band
        // (begin lsn 4 < source_lsn 5 <= commit lsn 6). Parsing whole bands from
        // the full stream must select that band ENTIRE — with its authoritative
        // CommitTx timestamp — never split it into mis-timed singletons, and must
        // skip the first band, which lives fully below the floor (in the base).
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(9999)), 5);

        assert_eq!(f.applied, 1, "only the straddling band is post-backup");
        assert_eq!(f.discarded, 0);
        // Begin(lsn4) + data(lsn5) + Commit(lsn6) = 3 whole-band entries.
        assert_eq!(f.entries.len(), 3, "whole band, not split singletons");
        assert!(
            matches!(f.entries[0].operation, WalOperation::BeginTx { .. }),
            "band prefix begins with its BeginTx marker"
        );
        let stop = f.resolved_stop.expect("a band was applied");
        assert_eq!(
            stop.timestamp_micros, 200,
            "band carries its CommitTx timestamp, not a data op's"
        );
        assert_eq!(stop.lsn, 6, "stop is the CommitTx LSN");
    }

    /// Fail-closed contract (Issue #3745): the vocabulary-drift predicate must
    /// still flag an out-of-range RAW label id — the shape a legacy ≤v12
    /// (`InternedString::from_raw`, un-re-interned) segment carries. The
    /// version-aware gating in `restore_to_data_dir_at` only RUNS this predicate
    /// when a pre-v13 raw-label segment is present in the archive (see the
    /// `read_entries_from_dir_with_options_reporting_raw_labels` seam and its
    /// `reports_raw_labels_for_pre_v13_segment_only` unit test), so a raw id past
    /// the archived interner is never silently replayed. This test pins the
    /// predicate itself: in-range ids resolve (None), an out-of-range id is
    /// detected (Some), across both a single op and a stream.
    #[test]
    fn vocabulary_guard_predicate_still_flags_out_of_range_raw_label() {
        use crate::core::interning::InternedString;

        // Base interner defines file-space ids 0..5.
        let restored_count = 5u32;

        let make_op = |label_raw: u32| WalOperation::CreateNode {
            node_id: crate::core::NodeId::new(1).unwrap(),
            label: InternedString::from_raw(label_raw),
            properties: PropertyMap::new(),
            valid_from: ts(1),
            provenance: None,
        };

        // In-range label id (< restored_count): fully resolvable, no drift.
        assert_eq!(first_out_of_range_id(&make_op(3), restored_count), None);

        // Out-of-range label id (>= restored_count): references a string the base
        // backup does not carry — MUST be flagged (fail-closed).
        assert_eq!(first_out_of_range_id(&make_op(7), restored_count), Some(7));

        // And the stream aggregator surfaces the first offending id.
        let entries = vec![framed(1, make_op(2), ts(1)), framed(2, make_op(9), ts(1))];
        assert_eq!(
            first_unresolved_interned_id(&entries, restored_count),
            Some(9)
        );
    }
}

/// End-to-end wiring coverage for the version-aware vocabulary guard
/// (Issue #3745). The two unit conjuncts in `band_filter_tests`
/// (`reports_raw_labels_for_pre_v13_segment_only`, living in
/// `segment_reader.rs`, proves the raw-label FLAG; and
/// `vocabulary_guard_predicate_still_flags_out_of_range_raw_label` proves the
/// PREDICATE) verify the two halves in isolation — but neither drives a real
/// ≤v12 raw-label archive through [`AletheiaDB::restore_to_data_dir_at`], so
/// deleting the guard's `if` block would leave both green. This test closes
/// that gap: it constructs a genuine on-disk v11 raw-label WAL archive plus a
/// real base backup and asserts the public restore API actually REFUSES
/// fail-closed, materializing nothing.
///
/// It lives here (a crate-internal `#[cfg(test)]` module) rather than in
/// `tests/pitr.rs` because building the raw-label segment requires the
/// `pub(crate)` WAL wire constants (`WAL_MAGIC`, `WAL_VERSION_DESTRUCTIVE_PROVENANCE`,
/// `OP_CREATE_NODE`) that the external integration crate cannot name; it reuses
/// the exact segment-writer technique proven by
/// `reports_raw_labels_for_pre_v13_segment_only`.
#[cfg(test)]
mod vocab_guard_e2e_tests {
    use super::*;
    use crate::core::NodeId;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::property::PropertyMap;
    use crate::core::temporal::time;
    use crate::storage::wal::segment_reader::{
        WAL_MAGIC, WAL_VERSION_DESTRUCTIVE_PROVENANCE, WAL_VERSION_STRING_LABELS,
    };
    use crate::storage::wal::serialization::{
        OP_BEGIN_TX, OP_COMMIT_TX, OP_CREATE_NODE, OP_DECLARE_UNIQUE_CONSTRAINT,
    };
    use std::io::Write;
    use std::path::Path;
    use tempfile::TempDir;

    /// Frame one WAL entry (LSN + 12-byte HLC timestamp + 4-byte CRC slot + op
    /// payload), with the CRC taken over the header and the payload (the 4-byte
    /// checksum slot excluded), exactly as the writer's `serialize_operation_into`.
    fn build_framed_entry(lsn: u64, ts: Timestamp, write_op: &dyn Fn(&mut Vec<u8>)) -> Vec<u8> {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&LSN(lsn).0.to_le_bytes());
        ts.serialize_into(&mut buffer);
        let cs = buffer.len();
        buffer.extend_from_slice(&[0u8; 4]); // checksum placeholder
        write_op(&mut buffer);
        let mut h = crc32fast::Hasher::new();
        h.update(&buffer[0..cs]);
        h.update(&buffer[cs + 4..]);
        buffer[cs..cs + 4].copy_from_slice(&h.finalize().to_le_bytes());
        buffer
    }

    /// v11 (≤v12) `CreateNode` label codec: a RAW u32 file-space interner id, no
    /// inline string and no re-intern — the un-resolvable ≤v12 shape the guard
    /// must range-check.
    fn write_create_node_raw(
        buf: &mut Vec<u8>,
        node_id: NodeId,
        raw_label: u32,
        valid_from: Timestamp,
    ) {
        buf.push(OP_CREATE_NODE);
        buf.extend_from_slice(&node_id.as_u64().to_le_bytes());
        buf.extend_from_slice(&raw_label.to_le_bytes());
        PropertyMap::new().serialize_into(buf).unwrap();
        valid_from.serialize_into(buf); // op-level valid_from
        buf.push(0u8); // provenance: absent
    }

    /// v11 (≤v12) `DeclareUniqueConstraint` codec: two RAW u32 file-space
    /// interner ids (label + property key), no inline strings and no re-intern —
    /// the un-resolvable ≤v12 shape the vocabulary guard must range-check when the
    /// declaration reaches the constraint slice. An out-of-range `raw_label` is
    /// the dangling id the guard must refuse.
    fn write_declare_constraint_raw(buf: &mut Vec<u8>, raw_label: u32, raw_property: u32) {
        buf.push(OP_DECLARE_UNIQUE_CONSTRAINT);
        buf.extend_from_slice(&raw_label.to_le_bytes());
        buf.extend_from_slice(&raw_property.to_le_bytes());
    }

    /// Write a single framed control/data op as its own segment under WAL format
    /// `version`, at `lsn`/`ts`. A framed control op (e.g. a constraint
    /// declaration) parses as a self-committing singleton band, so no
    /// `BeginTx`/`CommitTx` framing is needed (Issue #3745 constraint-slice
    /// coverage).
    fn write_single_op_segment(
        path: &Path,
        version: u8,
        lsn: u64,
        ts: Timestamp,
        write_op: &dyn Fn(&mut Vec<u8>),
    ) {
        let entry = build_framed_entry(lsn, ts, write_op);
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&WAL_MAGIC).unwrap();
        f.write_all(&[version]).unwrap();
        f.write_all(&entry).unwrap();
        f.sync_all().unwrap();
    }

    /// v13+ `CreateNode` label codec: the label STRING inline (length-prefixed
    /// UTF-8), re-interned to a live global-interner id on read.
    fn write_create_node_string(
        buf: &mut Vec<u8>,
        node_id: NodeId,
        label: &str,
        valid_from: Timestamp,
    ) {
        buf.push(OP_CREATE_NODE);
        buf.extend_from_slice(&node_id.as_u64().to_le_bytes());
        let bytes = label.as_bytes();
        buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(bytes);
        PropertyMap::new().serialize_into(buf).unwrap();
        valid_from.serialize_into(buf); // op-level valid_from
        buf.push(0u8); // provenance: absent
    }

    /// Write a single-`CreateNode` framed transaction band
    /// `[BeginTx, CreateNode, CommitTx]` occupying `begin_lsn..=begin_lsn+2` to
    /// `path` under WAL format `version`, committing at `commit_ts`.
    /// `write_create` supplies the version-specific `CreateNode` label codec.
    fn write_band_segment(
        path: &Path,
        version: u8,
        begin_lsn: u64,
        tx_id: u64,
        commit_ts: Timestamp,
        write_create: &dyn Fn(&mut Vec<u8>),
    ) {
        let begin = build_framed_entry(begin_lsn, commit_ts, &|b| {
            b.push(OP_BEGIN_TX);
            b.extend_from_slice(&tx_id.to_le_bytes());
        });
        let create = build_framed_entry(begin_lsn + 1, commit_ts, write_create);
        let commit = build_framed_entry(begin_lsn + 2, commit_ts, &|b| {
            b.push(OP_COMMIT_TX);
            b.extend_from_slice(&tx_id.to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes()); // entry_count
            commit_ts.serialize_into(b);
        });
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&WAL_MAGIC).unwrap();
        f.write_all(&[version]).unwrap();
        f.write_all(&begin).unwrap();
        f.write_all(&create).unwrap();
        f.write_all(&commit).unwrap();
        f.sync_all().unwrap();
    }

    /// Create a real base backup with a small archived vocabulary; return the
    /// artifact path, its `source_lsn`, and a wallclock floor (`>=` the base's
    /// commit time) that hand-built post-backup bands must sit above so the PITR
    /// window validates.
    fn make_base_backup(dir: &Path) -> (std::path::PathBuf, u64, i64) {
        let db = AletheiaDB::new().unwrap();
        db.create_node("PitrBase3745", PropertyMap::new()).unwrap();
        let albk = dir.join("base.albk");
        let source_lsn = db.backup(&albk).unwrap().source_lsn;
        let base_wall = time::now().wallclock();
        drop(db);
        (albk, source_lsn, base_wall)
    }

    /// Fail-closed, end-to-end (Issue #3745): a REAL on-disk ≤v12 (v11)
    /// raw-label WAL archive whose post-backup band carries a label id OUT OF
    /// RANGE of the base backup's interner must drive
    /// [`AletheiaDB::restore_to_data_dir_at`] to an actual
    /// [`BackupError::WindowCrossesVocabularyChange`] refusal — and, because the
    /// guard fires BEFORE `materialize_to_dir`, must leave the target directory
    /// **unmaterialized** (no partial restore). Deleting the guard's `if` block
    /// in `restore_to_data_dir_at` would replay the dangling raw id and make this
    /// test fail (it would `Ok`), which the two isolated unit conjuncts cannot
    /// catch.
    #[test]
    #[serial_test::serial]
    fn pre_v13_raw_label_out_of_range_archive_refuses_and_leaves_target_unmaterialized() {
        let tmp = TempDir::new().unwrap();

        // --- 1. A real base backup with a small base vocabulary. Its archived
        //        interner defines only ids `0..restored_interner_count`. ---
        let db = AletheiaDB::new().unwrap();
        db.create_node("PitrBase3745", PropertyMap::new()).unwrap();
        let albk = tmp.path().join("base.albk");
        let source_lsn = db.backup(&albk).unwrap().source_lsn;
        drop(db);

        // --- 2. A hand-built v11 (≤v12) raw-label WAL archive: one FRAMED
        //        transaction band `[BeginTx, CreateNode, CommitTx]` (a v11
        //        segment is a framed WAL version, so a bare data op is not a
        //        window-forming transaction; the `CommitTx` supplies the band's
        //        stop coordinate). The `CreateNode`'s RAW file-space label id is
        //        astronomically beyond any interner count, so it is guaranteed
        //        `>=` the base backup's `restored_interner_count` regardless of
        //        live interner state. LSNs sit above `source_lsn` so the band is
        //        post-backup and included in the replay window; the commit
        //        timestamp is the PITR target. The per-entry framing (LSN +
        //        timestamp + CRC over header + payload) mirrors the writer's
        //        `serialize_operation_into` and the segment-writer technique
        //        proven by `reports_raw_labels_for_pre_v13_segment_only`. ---
        const OUT_OF_RANGE_RAW_LABEL: u32 = 2_000_000_000;
        let tx_id = 3745u64;
        let node_id = NodeId::new(3745).unwrap();
        let commit_ts = time::now();

        // Frame one WAL entry: LSN + 12-byte HLC timestamp + 4-byte CRC slot +
        // op payload, with the CRC taken over the header and the payload (the
        // 4-byte checksum slot excluded), exactly as `serialize_operation_into`.
        let build_entry = |lsn: u64, ts: Timestamp, write_op: &dyn Fn(&mut Vec<u8>)| -> Vec<u8> {
            let mut buffer = Vec::new();
            buffer.extend_from_slice(&LSN(lsn).0.to_le_bytes());
            ts.serialize_into(&mut buffer);
            let cs = buffer.len();
            buffer.extend_from_slice(&[0u8; 4]); // checksum placeholder
            write_op(&mut buffer);
            let mut h = crc32fast::Hasher::new();
            h.update(&buffer[0..cs]);
            h.update(&buffer[cs + 4..]);
            buffer[cs..cs + 4].copy_from_slice(&h.finalize().to_le_bytes());
            buffer
        };

        let begin_lsn = source_lsn + 1;
        let begin = build_entry(begin_lsn, commit_ts, &|buf| {
            buf.push(OP_BEGIN_TX);
            buf.extend_from_slice(&tx_id.to_le_bytes());
        });
        let create = build_entry(begin_lsn + 1, commit_ts, &|buf| {
            buf.push(OP_CREATE_NODE);
            buf.extend_from_slice(&node_id.as_u64().to_le_bytes());
            // v11 label codec: a raw u32 file-space id (no inline string, no
            // re-intern) — the un-resolvable ≤v12 shape the guard must catch.
            buf.extend_from_slice(&OUT_OF_RANGE_RAW_LABEL.to_le_bytes());
            PropertyMap::new().serialize_into(buf).unwrap();
            commit_ts.serialize_into(buf); // op-level valid_from
            buf.push(0u8); // provenance: absent
        });
        let commit = build_entry(begin_lsn + 2, commit_ts, &|buf| {
            buf.push(OP_COMMIT_TX);
            buf.extend_from_slice(&tx_id.to_le_bytes());
            buf.extend_from_slice(&1u32.to_le_bytes()); // entry_count
            commit_ts.serialize_into(buf);
        });

        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        {
            let mut f = std::fs::File::create(archive.join("0.log")).unwrap();
            f.write_all(&WAL_MAGIC).unwrap();
            f.write_all(&[WAL_VERSION_DESTRUCTIVE_PROVENANCE]).unwrap();
            f.write_all(&begin).unwrap();
            f.write_all(&create).unwrap();
            f.write_all(&commit).unwrap();
            f.sync_all().unwrap();
        }

        // --- 3. Drive it through the PUBLIC restore API. The guard must REFUSE. ---
        let dst = TempDir::new().unwrap();
        let data_dir = dst.path().join("db");
        let err = AletheiaDB::restore_to_data_dir_at(
            &albk,
            &archive,
            PitrTarget::AsOf(commit_ts),
            &data_dir,
        )
        .expect_err(
            "a ≤v12 raw-label archive with an out-of-range label id must be REFUSED \
             fail-closed, not silently replayed (Issue #3745)",
        );
        assert!(
            matches!(
                err,
                Error::Backup(BackupError::WindowCrossesVocabularyChange { .. })
            ),
            "expected WindowCrossesVocabularyChange, got {err:?}"
        );

        // --- 4. Fail-closed: the guard fires BEFORE materialize, so the target
        //        `indexes/` must NOT exist (no partial restore left behind). ---
        assert!(
            !data_dir.join("indexes").exists(),
            "a refused restore must leave the target directory unmaterialized"
        );
    }

    /// T2a — the headline fix (Issue #3745 follow-up). A MIXED archive whose
    /// replay window is fully v13 must RESTORE even though the archive also
    /// contains an out-of-band ≤v12 raw-label segment (here, one committing
    /// AFTER the target, so no raw entry falls in the window). The OLD
    /// whole-archive gating armed the guard over the entire replay window the
    /// moment ANY pre-v13 segment existed, range-checking the v13 window entries'
    /// live re-interned ids against the foreign backup's file-space count and
    /// FALSE-REJECTING them. Per-entry tagging arms only over entries decoded
    /// from a raw-label segment, so a fully-v13 window is never checked.
    #[test]
    #[serial_test::serial]
    fn mixed_archive_with_v13_replay_window_restores() {
        let tmp = TempDir::new().unwrap();
        let (albk, source_lsn, base_wall) = make_base_backup(tmp.path());

        // The v13 band commits BEFORE the target; the ≤v12 raw band AFTER it.
        let t_v13 = HybridTimestamp::new_unchecked(base_wall + 1_000, 0);
        let t_v11 = HybridTimestamp::new_unchecked(base_wall + 2_000, 0);

        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        // 0.log: v13 band IN the replay window. Its fresh inline-string label
        // re-interns to a live global-interner id far above the tiny base
        // backup's `restored_interner_count` — the id the OLD guard wrongly
        // range-checked and rejected.
        write_band_segment(
            &archive.join("0.log"),
            WAL_VERSION_STRING_LABELS,
            source_lsn + 1,
            3745,
            t_v13,
            &|b| {
                write_create_node_string(
                    b,
                    NodeId::new(3745).unwrap(),
                    "MixedT2aWindowLabel3745",
                    t_v13,
                )
            },
        );
        // 1.log: ≤v12 raw-label band committing AFTER the target — out of the
        // replay window, but present in the archive (arms the whole-archive
        // fast-path). Its raw id is astronomically out of range.
        write_band_segment(
            &archive.join("1.log"),
            WAL_VERSION_DESTRUCTIVE_PROVENANCE,
            source_lsn + 4,
            3746,
            t_v11,
            &|b| write_create_node_raw(b, NodeId::new(3746).unwrap(), 2_000_000_000, t_v11),
        );

        let dst = TempDir::new().unwrap();
        let data_dir = dst.path().join("db");
        let db =
            AletheiaDB::restore_to_data_dir_at(&albk, &archive, PitrTarget::AsOf(t_v13), &data_dir)
                .expect(
                    "a MIXED archive with a fully-v13 replay window must RESTORE: the only \
             raw-label segment commits after the target, so no raw entry is in the \
             window (Issue #3745 follow-up)",
                );

        // The v13 window transaction actually replayed.
        assert!(
            db.get_node(NodeId::new(3745).unwrap()).is_ok(),
            "the v13 replay-window node must be materialized by the restore"
        );
        drop(db);
    }

    /// T2c — per-entry precision + fail-closed preserved. A MIXED archive where a
    /// ≤v12 raw entry falls INSIDE the replay window with an out-of-range id,
    /// alongside an in-window v13 band, must still REFUSE: the per-entry guard
    /// must catch the real raw entry (not merely skip the check because v13
    /// entries are also present) and leave the target unmaterialized.
    #[test]
    #[serial_test::serial]
    fn mixed_archive_raw_entry_inside_window_out_of_range_refuses() {
        let tmp = TempDir::new().unwrap();
        let (albk, source_lsn, base_wall) = make_base_backup(tmp.path());

        // Both bands commit at-or-before the target, so BOTH are in the replay
        // window (the target is the later band's commit — the achievable tail).
        let t_v13 = HybridTimestamp::new_unchecked(base_wall + 1_000, 0);
        let t_v11 = HybridTimestamp::new_unchecked(base_wall + 2_000, 0);
        let target = t_v11;

        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        write_band_segment(
            &archive.join("0.log"),
            WAL_VERSION_STRING_LABELS,
            source_lsn + 1,
            3745,
            t_v13,
            &|b| {
                write_create_node_string(
                    b,
                    NodeId::new(3745).unwrap(),
                    "MixedT2cWindowLabel3745",
                    t_v13,
                )
            },
        );
        // ≤v12 raw band IN the window with an out-of-range raw id.
        write_band_segment(
            &archive.join("1.log"),
            WAL_VERSION_DESTRUCTIVE_PROVENANCE,
            source_lsn + 4,
            3746,
            t_v11,
            &|b| write_create_node_raw(b, NodeId::new(3746).unwrap(), 2_000_000_000, t_v11),
        );

        let dst = TempDir::new().unwrap();
        let data_dir = dst.path().join("db");
        let err = AletheiaDB::restore_to_data_dir_at(
            &albk,
            &archive,
            PitrTarget::AsOf(target),
            &data_dir,
        )
        .expect_err(
            "a raw ≤v12 entry with an out-of-range id INSIDE the replay window must be \
             REFUSED fail-closed, even in a mixed archive with in-window v13 entries",
        );
        assert!(
            matches!(
                err,
                Error::Backup(BackupError::WindowCrossesVocabularyChange { .. })
            ),
            "expected WindowCrossesVocabularyChange, got {err:?}"
        );
        assert!(
            !data_dir.join("indexes").exists(),
            "a refused restore must leave the target directory unmaterialized"
        );
    }

    /// T2d — regression guard. An all-v13 archive arms no raw-label entry, so the
    /// guard is never run and the window restores (unchanged behavior).
    #[test]
    #[serial_test::serial]
    fn all_v13_archive_restores() {
        let tmp = TempDir::new().unwrap();
        let (albk, source_lsn, base_wall) = make_base_backup(tmp.path());

        let t_v13 = HybridTimestamp::new_unchecked(base_wall + 1_000, 0);

        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        write_band_segment(
            &archive.join("0.log"),
            WAL_VERSION_STRING_LABELS,
            source_lsn + 1,
            3745,
            t_v13,
            &|b| {
                write_create_node_string(
                    b,
                    NodeId::new(3745).unwrap(),
                    "AllV13WindowLabel3745",
                    t_v13,
                )
            },
        );

        let dst = TempDir::new().unwrap();
        let data_dir = dst.path().join("db");
        let db =
            AletheiaDB::restore_to_data_dir_at(&albk, &archive, PitrTarget::AsOf(t_v13), &data_dir)
                .expect("an all-v13 archive must restore (guard never armed)");
        assert!(
            db.get_node(NodeId::new(3745).unwrap()).is_ok(),
            "the v13 node must be materialized"
        );
        drop(db);
    }

    /// T2f — the guard checks RANGE, not mere presence. A ≤v12 raw entry INSIDE
    /// the replay window whose raw id is IN range (`< restored_interner_count`)
    /// resolves against the base vocabulary and must be ALLOWED to restore.
    #[test]
    #[serial_test::serial]
    fn raw_entry_in_window_in_range_is_allowed() {
        let tmp = TempDir::new().unwrap();
        let (albk, source_lsn, base_wall) = make_base_backup(tmp.path());

        let t_v11 = HybridTimestamp::new_unchecked(base_wall + 1_000, 0);

        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        // A ≤v12 raw band IN the window whose raw label id is 0 — always within
        // any non-empty base interner (`0 < restored_interner_count`), so it
        // resolves and the guard permits the restore.
        write_band_segment(
            &archive.join("0.log"),
            WAL_VERSION_DESTRUCTIVE_PROVENANCE,
            source_lsn + 1,
            3745,
            t_v11,
            &|b| write_create_node_raw(b, NodeId::new(3745).unwrap(), 0, t_v11),
        );

        let dst = TempDir::new().unwrap();
        let data_dir = dst.path().join("db");
        let db =
            AletheiaDB::restore_to_data_dir_at(&albk, &archive, PitrTarget::AsOf(t_v11), &data_dir)
                .expect(
                    "a raw ≤v12 entry whose id is IN range must be allowed (the guard checks \
             range, not the mere presence of a raw-label segment)",
                );
        assert!(
            data_dir.join("indexes").exists(),
            "an allowed restore must materialize the target"
        );
        drop(db);
    }

    /// Constraint-slice fail-closed (Issue #3745). The guard filters
    /// `filtered.entries.chain(constraint_slice.iter())`, so a raw ≤v12
    /// `DeclareUniqueConstraint` op carrying an OUT-OF-RANGE interned label id
    /// must be range-checked and REFUSED just like a raw data op — the constraint
    /// arm of the chain, not previously exercised end-to-end. Here the raw
    /// constraint band commits IN the replay window (at-or-before the target), so
    /// it lands in the constraint slice with `segment_version = Some(v11)`
    /// (`entry_needs_vocab_check == true`); its dangling label id must drive an
    /// actual [`BackupError::WindowCrossesVocabularyChange`] refusal BEFORE
    /// materialize, leaving the target unmaterialized.
    #[test]
    #[serial_test::serial]
    fn raw_constraint_declaration_in_window_out_of_range_refuses() {
        let tmp = TempDir::new().unwrap();
        let (albk, source_lsn, base_wall) = make_base_backup(tmp.path());

        // A v13 data band supplies the window's reachable tail (its `CommitTx`
        // fixes `latest_ts`); the target is its commit so the window validates.
        let t_v13 = HybridTimestamp::new_unchecked(base_wall + 1_000, 0);
        // The raw constraint declaration commits at-or-before the target, so it
        // is collected into the constraint slice.
        let t_constraint = HybridTimestamp::new_unchecked(base_wall + 500, 0);
        let target = t_v13;

        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        // 0.log: v13 data band IN the window — its live re-interned id resolves
        // and is (correctly) never range-checked.
        write_band_segment(
            &archive.join("0.log"),
            WAL_VERSION_STRING_LABELS,
            source_lsn + 1,
            3745,
            t_v13,
            &|b| {
                write_create_node_string(
                    b,
                    NodeId::new(3745).unwrap(),
                    "ConstraintSliceWindowLabel3745",
                    t_v13,
                )
            },
        );
        // 1.log: a single ≤v12 (v11) raw `DeclareUniqueConstraint` op IN the
        // window whose raw label id is astronomically out of range of the base
        // backup's interner. It reaches the guard via the constraint slice.
        write_single_op_segment(
            &archive.join("1.log"),
            WAL_VERSION_DESTRUCTIVE_PROVENANCE,
            source_lsn + 4,
            t_constraint,
            &|b| write_declare_constraint_raw(b, 2_000_000_000, 1),
        );

        let dst = TempDir::new().unwrap();
        let data_dir = dst.path().join("db");
        let err = AletheiaDB::restore_to_data_dir_at(
            &albk,
            &archive,
            PitrTarget::AsOf(target),
            &data_dir,
        )
        .expect_err(
            "a raw ≤v12 constraint declaration with an out-of-range label id in the \
             replay window must be REFUSED fail-closed via the constraint slice \
             (Issue #3745)",
        );
        assert!(
            matches!(
                err,
                Error::Backup(BackupError::WindowCrossesVocabularyChange { .. })
            ),
            "expected WindowCrossesVocabularyChange, got {err:?}"
        );
        assert!(
            !data_dir.join("indexes").exists(),
            "a refused restore must leave the target directory unmaterialized"
        );
    }
}
