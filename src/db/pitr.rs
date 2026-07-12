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
//! # Interner vocabulary guard (v1)
//!
//! The WAL stores node/edge **labels** and **property keys** as raw `u32`
//! interner ids (not strings); property *values* are self-contained. The base
//! backup carries the interner as of `source_lsn`, so a post-backup transaction
//! that introduces a **brand-new label or property key** has an id `>= K`, where
//! `K` is the restored interner's string count. Replaying such an id verbatim is
//! **silent data corruption**, not a mere failed lookup: the id first dangles,
//! then — because the restored interner's `next_id` equals `K` — the first
//! genuinely-new string a later write interns collides with the dangling id, so
//! a replayed node/edge is **mislabeled** (e.g. "Manager" becomes the user's new
//! "Department") or its property silently dropped.
//!
//! PITR therefore **refuses** such a window: before materializing anything, it
//! scans the included band prefix and constraint-declaration slice for any
//! interner id that references a string the base snapshot does not contain and,
//! if found, fails with
//! [`BackupError::WindowCrossesVocabularyChange`](crate::storage::backup::BackupError::WindowCrossesVocabularyChange)
//! (mapped to `FAILED_PRECONDITION` at the MCP boundary). This converts silent
//! mislabeling/dropping into a clean, honest error. Keep the label/key
//! vocabulary stable across the window, take a fresh base backup that includes
//! the new vocabulary, or target a coordinate before the change (a durable
//! interner archive is a follow-up).

use std::path::Path;

use serde::Serialize;

use crate::core::error::{Error, Result};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::storage::backup::{BackupError, check_target_empty, materialize_to_dir, read_artifact};
use crate::storage::wal::segment_reader::read_entries_from_dir_with_options;
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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

/// Filter an LSN-sorted WAL entry stream to the prefix of whole transaction
/// bands committed at-or-before `target` (inclusive tie-break).
///
/// Never includes a partial band; drops an incomplete trailing band (torn
/// tail); returns the flattened prefix (markers preserved for replay), the
/// applied/discarded transaction counts, and the resolved stop coordinate.
fn filter_bands(entries: Vec<WalEntry>, target: &PitrTarget) -> FilteredBands {
    let bands = parse_bands(entries);

    let total_transactions = bands
        .iter()
        .filter(|b| b.complete && b.is_transaction)
        .count() as u64;

    let mut out: Vec<WalEntry> = Vec::new();
    let mut applied: u64 = 0;
    let mut resolved_stop: Option<PitrCoord> = None;
    let mut stopped = false;

    for band in bands {
        if !band.complete {
            continue; // torn tail: excluded, never counted
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

    /// Reject a target that lies below the achievable window (before the base
    /// backup). Targets above the latest reachable coordinate are permitted:
    /// they resolve to a full replay ("everything at-or-before the target").
    fn validate(&self, target: &PitrTarget) -> Result<()> {
        let outside = match target {
            PitrTarget::Lsn(n) => *n < self.source_lsn,
            PitrTarget::AsOf(t) => self.base_ts.is_some_and(|bt| *t < bt),
        };
        if outside {
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

/// Count complete transaction bands in a stream (for the no-target dry-run).
fn count_transactions(entries: &[WalEntry]) -> u64 {
    parse_bands(entries.to_vec())
        .iter()
        .filter(|b| b.complete && b.is_transaction)
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

/// Recompute the HLC seed from restored state (mirrors the startup bootstrap),
/// so writes to the returned database stay monotonic above every replayed
/// transaction time.
fn reseed_current_timestamp(db: &AletheiaDB) -> Result<()> {
    let mut max_ts = crate::core::temporal::time::now();

    for node in db.current.all_nodes() {
        if let Some(ts) = node.metadata.commit_timestamp
            && ts > max_ts
        {
            max_ts = ts;
        }
    }
    for edge in db.current.all_edges() {
        if let Some(ts) = edge.metadata.commit_timestamp
            && ts > max_ts
        {
            max_ts = ts;
        }
    }
    {
        let hist = db.historical.read();
        for v in hist.get_node_versions().values() {
            let tt = v.temporal.transaction_time();
            if tt.start() > max_ts {
                max_ts = tt.start();
            }
            if !tt.is_current() && tt.end() > max_ts {
                max_ts = tt.end();
            }
        }
        for v in hist.get_edge_versions().values() {
            let tt = v.temporal.transaction_time();
            if tt.start() > max_ts {
                max_ts = tt.start();
            }
            if !tt.is_current() && tt.end() > max_ts {
                max_ts = tt.end();
            }
        }
    }

    let mut ct = db.current_timestamp.lock().map_err(|_| {
        Error::Storage(crate::core::error::StorageError::LockPoisoned {
            resource: "current_timestamp".to_string(),
        })
    })?;
    if max_ts > *ct {
        *ct = max_ts;
    }
    Ok(())
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
    /// - [`BackupError::TargetOutsideWindow`] — `target` is below the base
    ///   backup (unreachable); the error names the achievable window.
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
        // out-of-window target fails without touching the target directory.
        let payload = read_artifact(albk).map_err(Error::Backup)?;
        let source_lsn = payload.source_lsn;

        let all = read_entries_from_dir_with_options(wal_archive, LSN::initial(), None, true)?;
        let window = compute_window(&all, source_lsn);
        window.validate(&target)?;

        // Post-backup entries drive the bounded data replay; the base snapshot
        // already covers everything below `source_lsn`.
        let post = read_entries_from_dir_with_options(wal_archive, LSN(source_lsn), None, true)?;
        let filtered = filter_bands(post, &target);

        // Materialize the base snapshot and open the base-state database. The
        // open path fully finalizes the base (id generators, temporal index,
        // extent, HLC seed) from the snapshot at `source_lsn`.
        materialize_to_dir(&payload, &index_root).map_err(Error::Backup)?;
        drop(payload);
        let config = crate::config::durable_config_for_data_dir(data_dir.to_path_buf());
        let db = AletheiaDB::with_unified_config(config)?;

        // Net constraint state at the target from the FULL archive (including
        // declarations that predate the backup, which the base snapshot does
        // not carry — the differential replay would otherwise miss them).
        let constraint_slice: Vec<WalEntry> = all
            .into_iter()
            .filter(|e| entry_at_or_before(e, &target))
            .collect();
        crate::storage::recovery::apply_constraint_declarations(
            &constraint_slice,
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
        db.historical.write().rebuild_temporal_index_from_versions();
        reseed_current_timestamp(&db)?;

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
    /// - [`BackupError::TargetOutsideWindow`] — `target` is below the window.
    /// - Artifact/WAL read failures propagate as [`Error`].
    pub fn inspect_pitr(
        albk: &Path,
        wal_archive: &Path,
        target: Option<PitrTarget>,
    ) -> Result<PitrPlan> {
        let source_lsn = read_artifact(albk).map_err(Error::Backup)?.source_lsn;
        let all = read_entries_from_dir_with_options(wal_archive, LSN::initial(), None, true)?;
        let window = compute_window(&all, source_lsn);
        let post = read_entries_from_dir_with_options(wal_archive, LSN(source_lsn), None, true)?;

        let (resolved_stop, applied, discarded) = match &target {
            Some(t) => {
                window.validate(t)?;
                let f = filter_bands(post, t);
                (f.resolved_stop, f.applied, f.discarded)
            }
            None => {
                // No target: report the full window; a full replay applies
                // every reachable post-backup transaction, discarding none.
                let total = count_transactions(&post);
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
        }
    }

    fn legacy(lsn: u64, op: WalOperation, timestamp: Timestamp) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp,
            operation: op,
            checksum: 0,
            framed: false,
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

    #[test]
    fn asof_exact_boundary_is_inclusive() {
        // Two transactions committing at t=100 and t=200; target exactly 100
        // keeps the first, drops the second.
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(100)));
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
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(150)));
        assert_eq!(f.applied, 1);
        assert_eq!(f.discarded, 1);
        assert_eq!(f.resolved_stop.unwrap().timestamp_micros, 100);
    }

    #[test]
    fn target_before_all_yields_empty_prefix() {
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(50)));
        assert_eq!(f.applied, 0);
        assert_eq!(f.discarded, 2);
        assert!(f.resolved_stop.is_none());
        assert!(f.entries.is_empty());
    }

    #[test]
    fn target_after_all_yields_full_prefix() {
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(9999)));
        assert_eq!(f.applied, 2);
        assert_eq!(f.discarded, 0);
        assert_eq!(f.resolved_stop.unwrap().timestamp_micros, 200);
        assert_eq!(f.entries.len(), 6);
    }

    #[test]
    fn lsn_target_stops_at_commit_lsn() {
        // First band commits at LSN 3, second at LSN 6.
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::Lsn(3));
        assert_eq!(f.applied, 1);
        assert_eq!(f.discarded, 1);
        assert_eq!(f.resolved_stop.unwrap().lsn, 3);

        // A target between the two commit LSNs (4 or 5) still keeps only the
        // first whole band — never a partial second band.
        let entries = all_of(vec![tx(1, 1, 1, ts(100)), tx(4, 2, 1, ts(200))]);
        let f = filter_bands(entries, &PitrTarget::Lsn(5));
        assert_eq!(f.applied, 1);
        assert_eq!(f.entries.len(), 3);
    }

    #[test]
    fn torn_trailing_band_is_excluded() {
        // A complete band, then a BeginTx with no CommitTx (crash tail).
        let mut entries = tx(1, 1, 1, ts(100));
        entries.push(framed(4, WalOperation::BeginTx { tx_id: 2 }, ts(0)));
        entries.push(framed(5, create_node_op(5), ts(0)));
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(9999)));
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
        let f = filter_bands(entries.clone(), &PitrTarget::AsOf(ts(100)));
        assert_eq!(f.applied, 1, "only the legacy op is at-or-before 100");
        assert_eq!(f.discarded, 1);
        assert_eq!(f.resolved_stop.unwrap().timestamp_micros, 50);

        let f = filter_bands(entries, &PitrTarget::AsOf(ts(200)));
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
        let f = filter_bands(entries, &PitrTarget::AsOf(ts(150)));
        assert_eq!(f.applied, 1, "one transaction band before 150");
        assert_eq!(f.discarded, 1);
        // Begin+data+Commit (3) + the declaration (1) = 4 entries carried.
        assert_eq!(f.entries.len(), 4);
    }

    #[test]
    fn window_lower_bound_rejects_target_below_base() {
        // Base at source_lsn=10 with a pre-backup commit at t=500 (lsn 3).
        let all = all_of(vec![tx(1, 1, 1, ts(500)), tx(10, 2, 1, ts(800))]);
        let window = compute_window(&all, 10);
        // A timestamp before the base's latest commit is unreachable.
        assert!(window.validate(&PitrTarget::AsOf(ts(400))).is_err());
        // At-or-after the base is fine.
        assert!(window.validate(&PitrTarget::AsOf(ts(500))).is_ok());
        // An LSN below source_lsn is unreachable.
        assert!(window.validate(&PitrTarget::Lsn(3)).is_err());
        assert!(window.validate(&PitrTarget::Lsn(10)).is_ok());
        // The window bounds are reported.
        assert_eq!(window.earliest_coord().lsn, 10);
        assert_eq!(window.latest_coord().timestamp_micros, 800);
    }
}
