//! A [`VersionSource`] over the live historical store (Issue #3351).
//!
//! The provenance-chain sealer and verifier both resolve authoritative version
//! content through this source, so a sealed leaf and a recomputed leaf hash
//! identical bytes unless the stored version was tampered with. Reading the
//! *stored* version (including superseded/tombstone versions) is exactly what
//! makes on-disk tamper detectable: a byte changed in a persisted version
//! changes the recomputed leaf and breaks the fold at that transaction.
//!
//! # Hashing the immutable logical version, not its storage encoding
//!
//! Several facets of a stored version are mutated by *later* writes and
//! therefore cannot be bound into an append-only leaf:
//! - its transaction-time END, its **valid-time END**, and its `is_current`
//!   flag are all closed/flipped when a newer version supersedes it (an update
//!   closes the prior version's open intervals at the successor's start — see
//!   `close_previous_version_intervals`), and
//! - its anchor/delta **encoding** changes: AletheiaDB re-encodes a prior
//!   anchor as a reverse delta when a new version arrives, so the raw
//!   `VersionData` for a fixed version id is not stable over time.
//!
//! So this source hashes the version's **reconstructed full property state**
//! (stable regardless of anchor/delta re-encoding) plus its immutable identity
//! and creation coordinates (`valid_from`/`transaction_from`, `prev_version`,
//! provenance), with the interval **ends** and `is_current` normalized out. The
//! same normalization runs on the seal and verify paths (both route through
//! this source), so their leaves match by construction — while a tampered
//! property value, valid-from/transaction-from, provenance, or identity still
//! diverges and is caught.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::core::id::VersionId;
use crate::core::interning::GLOBAL_INTERNER;
use crate::provenance_chain::{EntityKind, VersionHashInput, VersionSource};
use crate::storage::historical::HistoricalStorage;

/// A [`VersionSource`] backed by the database's historical storage.
pub(crate) struct DbVersionSource {
    historical: Arc<RwLock<HistoricalStorage>>,
}

impl DbVersionSource {
    /// Wrap a shared historical store as a version source.
    pub(crate) fn new(historical: Arc<RwLock<HistoricalStorage>>) -> Self {
        DbVersionSource { historical }
    }
}

impl VersionSource for DbVersionSource {
    fn fetch(&self, kind: EntityKind, _id: u64, version_id: u64) -> Option<VersionHashInput> {
        let historical = self.historical.read();
        fetch_locked(&historical, kind, version_id)
    }

    /// Hold the historical **read lock once** for the whole verification pass
    /// (Issue #3351 task #2). `verify_full`/`verify_entity` route every leaf
    /// re-fetch through the borrowed [`LockedDbVersionSource`], so the pass
    /// acquires the lock a single time instead of once per version — turning an
    /// O(versions) lock churn into O(1).
    fn scoped(&self, f: &mut dyn FnMut(&dyn VersionSource)) {
        let historical = self.historical.read();
        let locked = LockedDbVersionSource {
            historical: &historical,
        };
        f(&locked);
    }
}

/// A [`VersionSource`] view over an **already-locked** historical store,
/// borrowed for the duration of one `scoped` verification pass. Its `fetch`
/// never re-locks (Issue #3351 task #2).
struct LockedDbVersionSource<'a> {
    historical: &'a HistoricalStorage,
}

impl VersionSource for LockedDbVersionSource<'_> {
    fn fetch(&self, kind: EntityKind, _id: u64, version_id: u64) -> Option<VersionHashInput> {
        fetch_locked(self.historical, kind, version_id)
    }
    // `scoped` default (call `f(self)`): the read lock is already held.
}

/// Reconstruct the authoritative, normalized [`VersionHashInput`] for a version
/// from an already-locked historical store. Shared by the per-call
/// [`DbVersionSource::fetch`] and the single-lock
/// [`LockedDbVersionSource::fetch`] so both produce byte-identical leaves.
///
/// # GDPR crypto-shred invariant — DO NOT reroute through the unsealing boundary
///
/// Properties MUST be reconstructed via the **raw** `reconstruct_node_properties`
/// / `reconstruct_edge_properties` storage path, which is crypto-shred-**unaware**
/// and returns the bytes exactly as stored — for a designated property that is the
/// sealed `PropertyValue::Bytes(SUBJ…)` envelope (ciphertext), never the plaintext.
/// This is what makes the provenance-chain leaf **erasure-stable** (AC4, Issue
/// #3359): the leaf binds the ciphertext, and `erase_subject` destroys only the
/// subject key — never the stored envelope — so the recomputed leaf is unchanged
/// and `verify_chain` still passes after a crypto-shred, while a tamper of the
/// envelope bytes is still caught.
///
/// This function MUST NOT route through the db-API unsealing hooks
/// (`materialize_shred` / `unseal_node_view` / `unseal_edge_view`,
/// `src/db/crypto_shred/api.rs`). Doing so would make the leaf bind **plaintext**,
/// which (1) breaks AC4 — a leaf over destroyed plaintext can no longer be
/// recomputed after erasure, so `verify_chain` would spuriously fail — and (2)
/// would defeat on-disk tamper detection for sealed values. Non-crypto-shred data
/// is unaffected: with no designation, historical holds plaintext and the leaf
/// binds it exactly as before.
fn fetch_locked(
    historical: &HistoricalStorage,
    kind: EntityKind,
    version_id: u64,
) -> Option<VersionHashInput> {
    let vid = VersionId::new(version_id).ok()?;
    // Cold-tier aware (Issue #3351 finding 5): read the version metadata via
    // the tiered getter so verify does not FALSE-FAIL ("version not found")
    // once a sealed version has migrated to the Redb cold tier. Property
    // reconstruction (`reconstruct_*_properties`) is already tier-aware.
    let mut input = match kind {
        EntityKind::Node => {
            let v = historical.get_node_version_tiered(vid).ok().flatten()?;
            let mut input = VersionHashInput::from(v.as_ref());
            let full = historical.reconstruct_node_properties(vid).ok()?;
            input.properties = resolve_properties(&full);
            input
        }
        EntityKind::Edge => {
            let v = historical.get_edge_version_tiered(vid).ok().flatten()?;
            let mut input = VersionHashInput::from(v.as_ref());
            let full = historical.reconstruct_edge_properties(vid).ok()?;
            input.properties = resolve_properties(&full);
            input
        }
    };
    normalize_immutable(&mut input);
    Some(input)
}

/// Resolve an interned-keyed property map into `(String, value)` pairs. Key
/// ordering is irrelevant — the canonical encoder sorts by key.
fn resolve_properties(
    map: &crate::core::property::PropertyMap,
) -> Vec<(String, crate::core::property::PropertyValue)> {
    map.iter()
        .filter_map(|(key, value)| {
            GLOBAL_INTERNER
                .resolve_with(*key, |s| s.to_string())
                .map(|name| (name, value.clone()))
        })
        .collect()
}

/// Normalize the fields a later supersession mutates to their as-written state,
/// so a version's leaf is stable after sealing.
///
/// `transaction_to` and `is_current` are ALWAYS normalized out: a successor
/// version closes the prior version's open transaction interval and flips
/// `is_current`, so neither can be bound into an append-only leaf.
///
/// The valid-time **end** (`valid_to`) is handled per finding 2c:
/// - For a *live* version it starts open and is closed by a later supersession
///   (`close_previous_version_intervals` only closes **open** valid intervals),
///   so it is mutable and MUST be normalized out — otherwise a legitimately
///   superseded version would fail verification.
/// - For a **born-closed terminal** version (a delete tombstone, or a
///   retraction) the valid end is immutable: it is closed at creation and never
///   re-closed (an already-closed valid interval fails the
///   `is_currently_valid()` guard in supersession). Binding it directly makes a
///   re-validation tamper (extending a retraction's `valid_to`, or re-opening a
///   tombstone) change the leaf and break verification.
///
/// We detect "born-closed / historical closure" as `valid_to <= transaction_from`
/// (the fact stopped being true at or before it was recorded — a tombstone has
/// `valid_to == valid_from <= tx_from`, a default retraction has
/// `valid_to == now == tx_from`) plus the explicit tombstone flag. This is a
/// stable seal==verify predicate for the common case.
///
/// # Known limitation (documented v1)
///
/// A *backdated* supersession — two heavily backdated writes to the same entity
/// where the successor's `valid_start` lands at or before the prior version's
/// own `transaction_from` — closes the prior (live) version's `valid_to` to a
/// point `<= tx_from`, which this predicate would then treat as born-closed and
/// bind, while it was open at seal time. That is a rare false-positive class, not
/// a missed tamper; the timeline-consistency check does not add false negatives.
fn normalize_immutable(input: &mut VersionHashInput) {
    let born_closed = input.is_tombstone
        || matches!(
            (input.valid_to, input.transaction_from),
            (Some(vt), Some(tf)) if vt <= tf
        );
    if !born_closed {
        input.valid_to = None;
    }
    input.transaction_to = None;
    input.is_current = true;
}
