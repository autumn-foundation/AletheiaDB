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
        let vid = VersionId::new(version_id).ok()?;
        let historical = self.historical.read();
        let mut input = match kind {
            EntityKind::Node => {
                let v = historical.get_node_version(vid)?;
                let mut input = VersionHashInput::from(v);
                let full = historical.reconstruct_node_properties(vid).ok()?;
                input.properties = resolve_properties(&full);
                input
            }
            EntityKind::Edge => {
                let v = historical.get_edge_version(vid)?;
                let mut input = VersionHashInput::from(v);
                let full = historical.reconstruct_edge_properties(vid).ok()?;
                input.properties = resolve_properties(&full);
                input
            }
        };
        normalize_immutable(&mut input);
        Some(input)
    }
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
/// so a version's leaf is stable after sealing. The interval **ends** are
/// closed and `is_current` flips when a successor version arrives, so all three
/// are excluded from the bound content; the interval **starts**
/// (`valid_from`/`transaction_from`), which are fixed at creation, remain bound.
fn normalize_immutable(input: &mut VersionHashInput) {
    input.valid_to = None;
    input.transaction_to = None;
    input.is_current = true;
}
