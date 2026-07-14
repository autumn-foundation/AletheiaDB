//! Temporal index persistence.
//!
//! This module handles the serialization and deserialization of temporal index data,
//! which includes the version history of all nodes and edges.
//!
//! # Persistence Format
//!
//! The temporal index is stored as a `TemporalIndexData` struct, serialized using `bitcode`
//! and protected by a CRC32 checksum.
//!
//! File structure:
//! ```text
//! [bitcode_data][crc32_checksum_4_bytes]
//! ```
//!
//! The `TemporalIndexData` contains:
//! - `node_versions`: List of all node versions (both Anchors and Deltas).
//! - `edge_versions`: List of all edge versions (both Anchors and Deltas).
//!
//! # Version Entries
//!
//! Versions are persisted as `NodeVersionEntry` and `EdgeVersionEntry` structs, which flatten
//! the complex `VersionData` enum into a format suitable for serialization.
//!
//! - **Anchors**: Store full property maps.
//! - **Deltas**: Store only changed properties and a list of removed keys.
//!
//! # Vector Deltas
//!
//! Special handling is required for `VectorDelta`s:
//! - `VectorDelta::Full` is persisted as a regular property value.
//! - `VectorDelta::Sparse` **CANNOT** be persisted directly to prevent data loss. It must be
//!   materialized into a full vector before persistence using `PropertyDelta::materialize_vector_deltas()`.

use std::path::Path;
use std::sync::Arc;

use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::InternedString;
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::provenance::Provenance;
use crate::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange};
use crate::core::version::{EdgeVersion, NodeVersion, PropertyDelta, VersionData};

use super::error::{IndexPersistenceError, Result};
use super::formats::{
    EdgeVersionEntry, NodeVersionEntry, PersistedProvenance, PersistedVersionType,
    TemporalIndexData, legacy_v1::TemporalIndexDataV1,
};
use super::graph::{persist_property_map, restore_property_map};
use super::{MANIFEST_VERSION, TEMPORAL_MAGIC};
use crate::encryption::cipher::Cipher;

/// Convert an in-memory [`Provenance`] into its persisted representation.
pub(crate) fn persist_provenance(provenance: Option<&Provenance>) -> Option<PersistedProvenance> {
    provenance.map(|p| PersistedProvenance {
        source: p.source().map(String::from),
        confidence: p.confidence(),
        note: p.note().map(String::from),
        correlation_id: p.correlation_id().map(String::from),
        principal: p.principal().map(String::from),
    })
}

/// Restore a persisted provenance bundle back into an in-memory [`Provenance`].
///
/// Returns an error if the persisted `confidence` is out of `[0.0, 1.0]`,
/// which would indicate on-disk corruption (the writer always validates
/// before persisting).
pub(crate) fn restore_provenance(
    persisted: Option<PersistedProvenance>,
) -> Result<Option<Arc<Provenance>>> {
    let Some(p) = persisted else {
        return Ok(None);
    };
    let provenance = Provenance::from_parts(
        p.source,
        p.confidence,
        p.note,
        p.correlation_id,
        p.principal,
    )
    .map_err(|e| {
        IndexPersistenceError::Serialization(format!("Invalid persisted provenance: {}", e))
    })?;
    Ok(Some(Arc::new(provenance)))
}

/// True if this version data carries a `VectorDelta::Sparse`, which cannot be
/// persisted without first materializing it against its base vector
/// (Issue #3387 availability fix: callers materialize instead of failing the
/// whole checkpoint/save).
pub fn needs_sparse_vector_materialization(data: &VersionData) -> bool {
    match data {
        VersionData::Delta { delta } => delta
            .vector_deltas
            .values()
            .any(|d| matches!(d, crate::core::version::VectorDelta::Sparse { .. })),
        VersionData::Anchor { .. } => false,
    }
}

/// Return a copy of `data` with all vector deltas materialized against
/// `base` (the fully reconstructed property state of the *previous* version),
/// ready for persistence. The live in-memory version is never mutated --
/// only the persisted copy carries the materialized (Full) vectors.
///
/// # Errors
///
/// Returns an error if a sparse delta cannot be resolved against `base`
/// (missing or non-vector base property) -- persisting it anyway would be
/// silent data loss.
pub fn materialize_version_data_for_persistence(
    data: &VersionData,
    base: &PropertyMap,
) -> Result<VersionData> {
    match data {
        VersionData::Delta { delta } => {
            let mut materialized = delta.clone();
            materialized
                .materialize_vector_deltas(base)
                .map_err(IndexPersistenceError::Serialization)?;
            Ok(VersionData::Delta {
                delta: materialized,
            })
        }
        anchor @ VersionData::Anchor { .. } => Ok(anchor.clone()),
    }
}

/// Convert NodeVersion to NodeVersionEntry for persistence.
///
/// This function flattens the `NodeVersion` structure into a `NodeVersionEntry` suitable for disk storage.
/// It handles both `Anchor` and `Delta` versions.
///
/// # Vector Delta Handling
///
/// If the version contains `VectorDelta::Sparse`, this function will return an error.
/// Sparse deltas rely on the base version to be fully reconstructed, which is complex during persistence.
/// Therefore, sparse deltas must be materialized (converted to full vectors) before persistence.
///
/// # Errors
///
/// Returns an error if:
/// - Property conversion fails (e.g., unsupported Array type).
/// - A `VectorDelta::Sparse` is encountered (preventing data loss).
pub fn convert_node_version(version: &NodeVersion) -> Result<NodeVersionEntry> {
    let valid_time = version.temporal.valid_time();
    let tx_time = version.temporal.transaction_time();

    // Extract properties based on version type
    let (version_type, properties, vector_snapshot_id) = match &version.data {
        VersionData::Anchor {
            properties,
            vector_snapshot_id,
        } => (
            PersistedVersionType::Anchor,
            persist_property_map(properties)?,
            vector_snapshot_id.map(|id| id as u64),
        ),
        VersionData::Delta { delta } => {
            // For deltas, we persist changed properties AND removed keys
            let mut builder = crate::core::property::PropertyMapBuilder::new();

            // Add regular changed properties
            for (key, value) in &delta.changed {
                builder = builder.insert_by_key(*key, value.clone());
            }

            // Convert vector deltas to full vectors for persistence
            // VectorDelta::Sparse instances MUST be materialized before persistence
            // to prevent data loss. Use PropertyDelta::materialize_vector_deltas() first.
            for (key, vec_delta) in &delta.vector_deltas {
                match vec_delta {
                    crate::core::version::VectorDelta::Full(vec) => {
                        builder = builder.insert_by_key(*key, PropertyValue::Vector(vec.clone()));
                    }
                    crate::core::version::VectorDelta::Sparse { .. } => {
                        // CRITICAL: Cannot persist sparse deltas without base vector
                        // This would cause silent data loss - return error instead
                        return Err(IndexPersistenceError::Serialization(format!(
                            "Cannot persist NodeVersion {}: VectorDelta::Sparse found for property key {:?}. \
                             Call PropertyDelta::materialize_vector_deltas() before persistence to prevent data loss.",
                            version.id.as_u64(),
                            key
                        )));
                    }
                }
            }

            // Collect removed property keys (as interned string indices)
            let removed_keys: Vec<u32> = delta
                .removed
                .iter()
                .map(|k: &crate::core::interning::InternedString| k.as_u32())
                .collect();

            let props = builder.build();
            (
                PersistedVersionType::Delta {
                    // Phase 2: Extract wallclock for persistence format (i64)
                    base_anchor_tx: tx_time.start().wallclock(),
                    base_anchor_tx_logical: tx_time.start().logical(),
                    removed_keys,
                },
                persist_property_map(&props)?,
                None,
            )
        }
    };

    // Phase 2: Extract wallclock components for persistence format
    Ok(NodeVersionEntry {
        version_id: version.id.as_u64(),
        node_id: version.node_id.as_u64(),
        label_idx: version.label.as_u32(),
        valid_from: valid_time.start().wallclock(),
        valid_from_logical: valid_time.start().logical(),
        valid_to: if valid_time.is_current() {
            None
        } else {
            Some(valid_time.end().wallclock())
        },
        valid_to_logical: if valid_time.is_current() {
            None
        } else {
            Some(valid_time.end().logical())
        },
        tx_time: tx_time.start().wallclock(),
        tx_time_logical: tx_time.start().logical(),
        version_type,
        properties,
        vector_snapshot_id,
        provenance: persist_provenance(version.provenance.as_deref()),
        // Issue #3387: round-trip the tx-time closure and chain links so a
        // restore-only recovery serves correct AS OF SYSTEM_TIME reads.
        tx_end: if tx_time.is_current() {
            None
        } else {
            Some(tx_time.end().wallclock())
        },
        tx_end_logical: if tx_time.is_current() {
            None
        } else {
            Some(tx_time.end().logical())
        },
        prev_version: version.prev_version.map(|v| v.as_u64()),
        next_version: version.next_version.map(|v| v.as_u64()),
    })
}

/// Convert EdgeVersion to EdgeVersionEntry for persistence.
///
/// Similar to `convert_node_version`, this flattens `EdgeVersion` for storage.
///
/// # Errors
///
/// Returns an error if property conversion fails or `VectorDelta::Sparse` is encountered.
pub fn convert_edge_version(version: &EdgeVersion) -> Result<EdgeVersionEntry> {
    let valid_time = version.temporal.valid_time();
    let tx_time = version.temporal.transaction_time();

    // Extract properties based on version type
    let (version_type, properties) = match &version.data {
        VersionData::Anchor { properties, .. } => (
            PersistedVersionType::Anchor,
            persist_property_map(properties)?,
        ),
        VersionData::Delta { delta } => {
            // For deltas, we persist changed properties AND removed keys
            let mut builder = crate::core::property::PropertyMapBuilder::new();

            // Add regular changed properties
            for (key, value) in &delta.changed {
                builder = builder.insert_by_key(*key, value.clone());
            }

            // Convert vector deltas to full vectors for persistence
            // VectorDelta::Sparse instances MUST be materialized before persistence
            for (key, vec_delta) in &delta.vector_deltas {
                match vec_delta {
                    crate::core::version::VectorDelta::Full(vec) => {
                        builder = builder.insert_by_key(*key, PropertyValue::Vector(vec.clone()));
                    }
                    crate::core::version::VectorDelta::Sparse { .. } => {
                        // CRITICAL: Cannot persist sparse deltas without base vector
                        return Err(IndexPersistenceError::Serialization(format!(
                            "Cannot persist EdgeVersion {}: VectorDelta::Sparse found for property key {:?}. \
                             Call PropertyDelta::materialize_vector_deltas() before persistence to prevent data loss.",
                            version.id.as_u64(),
                            key
                        )));
                    }
                }
            }

            // Collect removed property keys (as interned string indices)
            let removed_keys: Vec<u32> = delta.removed.iter().map(|k| k.as_u32()).collect();

            let props = builder.build();
            (
                PersistedVersionType::Delta {
                    // Phase 2: Extract wallclock for persistence format (i64)
                    base_anchor_tx: tx_time.start().wallclock(),
                    base_anchor_tx_logical: tx_time.start().logical(),
                    removed_keys,
                },
                persist_property_map(&props)?,
            )
        }
    };

    // Phase 2: Extract wallclock components for persistence format
    Ok(EdgeVersionEntry {
        version_id: version.id.as_u64(),
        edge_id: version.edge_id.as_u64(),
        source_id: version.source.as_u64(),
        target_id: version.target.as_u64(),
        label_idx: version.label.as_u32(),
        valid_from: valid_time.start().wallclock(),
        valid_from_logical: valid_time.start().logical(),
        valid_to: if valid_time.is_current() {
            None
        } else {
            Some(valid_time.end().wallclock())
        },
        valid_to_logical: if valid_time.is_current() {
            None
        } else {
            Some(valid_time.end().logical())
        },
        tx_time: tx_time.start().wallclock(),
        tx_time_logical: tx_time.start().logical(),
        version_type,
        properties,
        provenance: persist_provenance(version.provenance.as_deref()),
        // Issue #3387: round-trip the tx-time closure and chain links so a
        // restore-only recovery serves correct AS OF SYSTEM_TIME reads.
        tx_end: if tx_time.is_current() {
            None
        } else {
            Some(tx_time.end().wallclock())
        },
        tx_end_logical: if tx_time.is_current() {
            None
        } else {
            Some(tx_time.end().logical())
        },
        prev_version: version.prev_version.map(|v| v.as_u64()),
        next_version: version.next_version.map(|v| v.as_u64()),
    })
}

/// Restore NodeVersionEntry back to NodeVersion.
///
/// This reconstructs the in-memory `NodeVersion` from the persisted entry.
/// It resolves interned strings and rebuilds `VersionData` (Anchor or Delta).
///
/// # Arguments
///
/// * `entry` - The persisted node version entry
///
/// Restore a persisted NodeVersionEntry back into a NodeVersion.
///
/// Rebuilds the complex `VersionData` structure (either Anchor or Delta) from the
/// flattened `NodeVersionEntry`. This maps raw integer labels back to `InternedString`s
/// and reconstructs `HybridTimestamp`s.
///
/// # Errors
///
/// Returns an error if property restoration fails (e.g., corrupted interned strings).
pub fn restore_node_version(entry: &NodeVersionEntry) -> Result<NodeVersion> {
    let label = resolve_label_or_error(entry.label_idx, "node")?;
    let node_id = NodeId::new(entry.node_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!("Invalid node ID {}: {}", entry.node_id, e))
    })?;

    // Restore temporal interval
    // Phase 2: Convert i64 from persistence format to HybridTimestamp
    use crate::core::hlc::HybridTimestamp;
    let valid_start = HybridTimestamp::new_unchecked(entry.valid_from, entry.valid_from_logical);
    let valid_end = entry
        .valid_to
        .map(|t| HybridTimestamp::new_unchecked(t, entry.valid_to_logical.unwrap_or(0)))
        .unwrap_or(TIMESTAMP_MAX);

    let valid_time = TimeRange::new(valid_start, valid_end).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid valid time range [{}, {:?}]: {}",
            entry.valid_from, entry.valid_to, e
        ))
    })?;

    let tx_start = HybridTimestamp::new_unchecked(entry.tx_time, entry.tx_time_logical);
    // Issue #3387: restore the persisted transaction-time closure
    // (None = still current knowledge, open-ended interval).
    let tx_time = match entry.tx_end {
        Some(end) => {
            // The writer always persists the pair together; a lone tx_end is
            // on-disk corruption, not a value to default.
            let end_logical = entry.tx_end_logical.ok_or_else(|| {
                IndexPersistenceError::Serialization(format!(
                    "Corrupt version entry {}: tx_end is set but tx_end_logical is missing",
                    entry.version_id
                ))
            })?;
            TimeRange::new(tx_start, HybridTimestamp::new_unchecked(end, end_logical)).map_err(
                |e| {
                    IndexPersistenceError::Serialization(format!(
                        "Invalid transaction time range [{}, {}]: {}",
                        entry.tx_time, end, e
                    ))
                },
            )?
        }
        None => TimeRange::from(tx_start),
    };
    let temporal = BiTemporalInterval::new(valid_time, tx_time);

    // Use the preserved version ID from the persisted entry
    let version_id = crate::core::id::VersionId::new(entry.version_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid version ID {}: {}",
            entry.version_id, e
        ))
    })?;

    // Restore version data based on type
    let data = match &entry.version_type {
        PersistedVersionType::Anchor => {
            let properties = restore_property_map(&entry.properties)?;
            let mut version_data = VersionData::anchor(properties);
            if let Some(snapshot_id) = entry.vector_snapshot_id {
                version_data.set_vector_snapshot_id(snapshot_id as usize);
            }
            version_data
        }
        PersistedVersionType::Delta { removed_keys, .. } => {
            // Restore delta - convert properties back to PropertyDelta
            let properties = restore_property_map(&entry.properties)?;
            let mut delta = PropertyDelta::new();

            // Restore changed properties
            for (key, value) in properties.iter() {
                delta.changed.insert(*key, value.clone());
            }

            // Restore removed property keys
            for key_idx in removed_keys {
                delta
                    .removed
                    .insert(crate::core::InternedString::from_raw(*key_idx));
            }

            VersionData::Delta { delta }
        }
    };

    Ok(NodeVersion {
        id: version_id,
        node_id,
        commit_timestamp: temporal.transaction_time().start(),
        temporal,
        label,
        data,
        // Issue #3387: restore the persisted chain links (legacy formats
        // carry None here; `rebuild_version_chains` re-derives them).
        next_version: restore_version_link(entry.next_version, "next_version")?,
        prev_version: restore_version_link(entry.prev_version, "prev_version")?,
        provenance: restore_provenance(entry.provenance.clone())?,
    })
}

/// Validate that a persisted label id resolves against the live interner,
/// returning the [`InternedString`] on success.
///
/// Issue #3490: persisted labels are file-space interner ids translated to
/// live ids by the load-time [`InternerRemap`](super::strings::InternerRemap).
/// An unmappable id (the remap's `UNMAPPABLE_FILE_ID` sentinel, or genuine
/// on-disk corruption) must fail LOUDLY here rather than be stored as a garbage
/// [`InternedString`] via `from_raw`, mirroring the graph restore path and
/// `restore_property_map`'s property-key check.
fn resolve_label_or_error(label_idx: u32, kind: &str) -> Result<InternedString> {
    let label = InternedString::from_raw(label_idx);
    if crate::core::GLOBAL_INTERNER
        .resolve_with(label, |_| ())
        .is_none()
    {
        return Err(IndexPersistenceError::Serialization(format!(
            "Failed to resolve interned {kind} label with ID: {label_idx}. \
             This likely indicates data corruption."
        )));
    }
    Ok(label)
}

/// Restore an optional persisted version-chain link, validating the raw ID.
fn restore_version_link(
    raw: Option<u64>,
    field: &str,
) -> Result<Option<crate::core::id::VersionId>> {
    raw.map(crate::core::id::VersionId::new)
        .transpose()
        .map_err(|e| {
            IndexPersistenceError::Serialization(format!("Invalid {} ID {:?}: {}", field, raw, e))
        })
}

/// Restore EdgeVersionEntry back to EdgeVersion.
///
/// Reconstructs the in-memory `EdgeVersion` from the persisted entry.
///
/// # Arguments
///
/// * `entry` - The persisted edge version entry
///
/// Restore a persisted EdgeVersionEntry back into an EdgeVersion.
///
/// Like `restore_node_version`, this rebuilds the runtime representation of an edge
/// from its serialized format, resolving interned strings and timestamps.
///
/// # Errors
///
/// Returns an error if property restoration fails (e.g., corrupted interned strings).
pub fn restore_edge_version(entry: &EdgeVersionEntry) -> Result<EdgeVersion> {
    let label = resolve_label_or_error(entry.label_idx, "edge")?;
    let edge_id = EdgeId::new(entry.edge_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!("Invalid edge ID {}: {}", entry.edge_id, e))
    })?;

    let source = NodeId::new(entry.source_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid source node ID {}: {}",
            entry.source_id, e
        ))
    })?;

    let target = NodeId::new(entry.target_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid target node ID {}: {}",
            entry.target_id, e
        ))
    })?;

    // Restore temporal interval
    // Phase 2: Convert i64 from persistence format to HybridTimestamp
    use crate::core::hlc::HybridTimestamp;
    let valid_start = HybridTimestamp::new_unchecked(entry.valid_from, entry.valid_from_logical);
    let valid_end = entry
        .valid_to
        .map(|t| HybridTimestamp::new_unchecked(t, entry.valid_to_logical.unwrap_or(0)))
        .unwrap_or(TIMESTAMP_MAX);

    let valid_time = TimeRange::new(valid_start, valid_end).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid valid time range [{}, {:?}]: {}",
            entry.valid_from, entry.valid_to, e
        ))
    })?;

    let tx_start = HybridTimestamp::new_unchecked(entry.tx_time, entry.tx_time_logical);
    // Issue #3387: restore the persisted transaction-time closure
    // (None = still current knowledge, open-ended interval).
    let tx_time = match entry.tx_end {
        Some(end) => {
            // The writer always persists the pair together; a lone tx_end is
            // on-disk corruption, not a value to default.
            let end_logical = entry.tx_end_logical.ok_or_else(|| {
                IndexPersistenceError::Serialization(format!(
                    "Corrupt version entry {}: tx_end is set but tx_end_logical is missing",
                    entry.version_id
                ))
            })?;
            TimeRange::new(tx_start, HybridTimestamp::new_unchecked(end, end_logical)).map_err(
                |e| {
                    IndexPersistenceError::Serialization(format!(
                        "Invalid transaction time range [{}, {}]: {}",
                        entry.tx_time, end, e
                    ))
                },
            )?
        }
        None => TimeRange::from(tx_start),
    };
    let temporal = BiTemporalInterval::new(valid_time, tx_time);

    // Use the preserved version ID from the persisted entry
    let version_id = crate::core::id::VersionId::new(entry.version_id).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Invalid version ID {}: {}",
            entry.version_id, e
        ))
    })?;

    // Restore version data based on type
    let data = match &entry.version_type {
        PersistedVersionType::Anchor => {
            let properties = restore_property_map(&entry.properties)?;
            VersionData::anchor(properties)
        }
        PersistedVersionType::Delta { removed_keys, .. } => {
            // Restore delta - convert properties back to PropertyDelta
            let properties = restore_property_map(&entry.properties)?;
            let mut delta = PropertyDelta::new();

            // Restore changed properties
            for (key, value) in properties.iter() {
                delta.changed.insert(*key, value.clone());
            }

            // Restore removed property keys
            for key_idx in removed_keys {
                delta
                    .removed
                    .insert(crate::core::InternedString::from_raw(*key_idx));
            }

            VersionData::Delta { delta }
        }
    };

    Ok(EdgeVersion {
        id: version_id,
        edge_id,
        commit_timestamp: temporal.transaction_time().start(),
        temporal,
        label,
        source,
        target,
        data,
        // Issue #3387: restore the persisted chain links (legacy formats
        // carry None here; `rebuild_version_chains` re-derives them).
        next_version: restore_version_link(entry.next_version, "next_version")?,
        prev_version: restore_version_link(entry.prev_version, "prev_version")?,
        provenance: restore_provenance(entry.provenance.clone())?,
    })
}

/// Restore temporal index data into HistoricalStorage.
///
/// This function populates the `HistoricalStorage` with versions loaded from disk.
/// It also triggers `rebuild_version_chains()` to reconstruct the linked lists
/// of versions (prev/next pointers).
///
/// # Arguments
///
/// * `data` - The temporal index data loaded from disk
/// * `historical` - The HistoricalStorage to populate
///
/// Restore all persisted versions into `HistoricalStorage`.
///
/// Processes the deserialized `TemporalIndexData`, converting each entry back into its
/// runtime `NodeVersion` or `EdgeVersion` format, and inserts them sequentially into
/// the provided `HistoricalStorage` instance.
///
/// # Errors
///
/// Returns an error if version restoration or insertion fails.
pub fn restore_into_historical_storage(
    data: &TemporalIndexData,
    historical: &mut crate::storage::historical::HistoricalStorage,
) -> Result<()> {
    // Pre-allocate capacity for better performance during bulk restoration
    historical.reserve_restoration_capacity(data.node_versions.len(), data.edge_versions.len());

    // Restore node versions
    for entry in &data.node_versions {
        let version = restore_node_version(entry)?;
        historical
            .insert_restored_node_version(version)
            .map_err(|e| {
                IndexPersistenceError::Serialization(format!(
                    "Failed to insert node version: {}",
                    e
                ))
            })?;
    }

    // Restore edge versions
    for entry in &data.edge_versions {
        let version = restore_edge_version(entry)?;
        historical
            .insert_restored_edge_version(version)
            .map_err(|e| {
                IndexPersistenceError::Serialization(format!(
                    "Failed to insert edge version: {}",
                    e
                ))
            })?;
    }

    // Rebuild version chains now that all versions are loaded.
    // This reconstructs prev_version/next_version links and ensures
    // version heads point to the correct (latest tx_time) version.
    historical.rebuild_version_chains();

    Ok(())
}

/// Save temporal index data to disk with CRC32 checksum using atomic write.
///
/// Ensures the integrity of the stored temporal data by computing and appending a CRC32
/// checksum. This allows AletheiaDB to verify data hasn't been corrupted at rest.
///
/// # Errors
///
/// Returns an error if serialization or disk I/O fails.
pub fn save_temporal_index(data: &TemporalIndexData, path: &Path) -> Result<()> {
    save_temporal_index_with_cipher(data, path, None)
}

/// Save temporal index data, optionally encrypting it at rest (Issue #481).
/// With `cipher == None` this is identical to [`save_temporal_index`].
///
/// # Errors
///
/// Returns an error if serialization, encryption, or disk I/O fails.
pub fn save_temporal_index_with_cipher(
    data: &TemporalIndexData,
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<()> {
    super::common::save_encoded_maybe_encrypted(data, path, cipher)
}

/// Save temporal index data, encrypting with an
/// [`IndexKeyring`](super::common::IndexKeyring) (Issue #488 key rotation).
pub(crate) fn save_temporal_index_with_keyring(
    data: &TemporalIndexData,
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<()> {
    super::common::save_encoded_maybe_encrypted_with_keyring(data, path, keyring)
}

/// Load temporal index data from disk and validate CRC32 checksum.
///
/// # Validation
///
/// This function performs strict validation:
/// - File size check (against `MAX_TEMPORAL_INDEX_FILE_SIZE`)
/// - CRC32 checksum verification
/// - Magic bytes check (`TEMPORAL_MAGIC`)
/// - Version check (`MANIFEST_VERSION`)
///
/// Load temporal index data from disk.
///
/// Reads the encoded temporal data and verifies its CRC32 checksum before decoding
/// to ensure we don't load corrupted historical states into memory.
///
/// # Errors
///
/// Returns an error if the file is missing, corrupted, or incompatible.
pub fn load_temporal_index(path: &Path) -> Result<TemporalIndexData> {
    decode_temporal_blob(path, None)
}

/// Load temporal index data, transparently decrypting it if written encrypted
/// (Issue #481). A legacy plaintext temporal index is loaded even when a
/// cipher is supplied (header sniffing), and all four version fallbacks run on
/// the decrypted bytes.
///
/// # Errors
///
/// Same as [`load_temporal_index`], plus a structured error if the file is
/// encrypted but no cipher is supplied or decryption fails.
pub fn load_temporal_index_with_cipher(
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<TemporalIndexData> {
    let ring = cipher.map(|c| super::common::IndexKeyring::single(c.clone()));
    decode_temporal_blob(path, ring.as_ref())
}

/// Load temporal index data, decrypting via an
/// [`IndexKeyring`](super::common::IndexKeyring) that dispatches on the header
/// `key_version` (Issue #488 key rotation).
pub(crate) fn load_temporal_index_with_keyring(
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<TemporalIndexData> {
    decode_temporal_blob(path, keyring)
}

/// Decode temporal index bytes, transparently upgrading pre-fidelity
/// (Issue #3387, `version == 3`), pre-principal (Issue #3350,
/// `version == 2`), and pre-provenance (Issue #3224, `version == 1`) files.
///
/// `bitcode` is positional and non-self-describing, so an older-version file
/// cannot decode directly as the current [`TemporalIndexData`]: version 1
/// predates the `provenance` field on `NodeVersionEntry`/`EdgeVersionEntry`,
/// version 2 predates the `principal` field inside [`PersistedProvenance`],
/// and version 3 predates the Issue #3387 tx-end/chain-link fields. We try
/// decoding candidate shapes newest-first; a decode only "wins" when it
/// also produces a plausible magic + the version that shape was written at
/// (guarding against a decode "succeeding" on bytes it wasn't meant for) --
/// this is the same magic+version cross-check used throughout this module,
/// just applied across four candidate shapes instead of one.
///
/// The file is read from disk and CRC32-verified exactly once via
/// [`super::common::read_and_verify_crc`]; all candidate decodes are
/// attempted against that single in-memory buffer, avoiding extra disk
/// reads and checksum passes on the (common, cheap) legacy-fallback paths.
fn decode_temporal_blob(
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<TemporalIndexData> {
    use crate::storage::index_persistence::formats::legacy_v2::TemporalIndexDataV2;
    use crate::storage::index_persistence::formats::legacy_v3::TemporalIndexDataV3;

    let bytes = super::common::read_and_verify_crc_maybe_encrypted_with_keyring(
        path,
        super::MAX_TEMPORAL_INDEX_FILE_SIZE,
        "Temporal index",
        keyring,
    )?;

    if let Ok(data) = bitcode::decode::<TemporalIndexData>(&bytes)
        && data.magic == TEMPORAL_MAGIC
        && data.version == MANIFEST_VERSION
    {
        return Ok(data);
    }

    // Issue #3350 shape (principal-carrying provenance, no tx-end /
    // chain-link fields), written at MANIFEST_VERSION == 3.
    if let Ok(v3) = bitcode::decode::<TemporalIndexDataV3>(&bytes)
        && v3.magic == TEMPORAL_MAGIC
        && v3.version == 3
    {
        return Ok(v3.into());
    }

    // Issue #3224 shape (provenance without principal), written at
    // MANIFEST_VERSION == 2.
    if let Ok(v2) = bitcode::decode::<TemporalIndexDataV2>(&bytes)
        && v2.magic == TEMPORAL_MAGIC
        && v2.version == 2
    {
        return Ok(v2.into());
    }

    let legacy: TemporalIndexDataV1 =
        bitcode::decode(&bytes).map_err(|e| IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: format!("Failed to decode temporal index: {e}").into(),
        })?;

    if legacy.magic != TEMPORAL_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: TEMPORAL_MAGIC,
            got: legacy.magic,
        });
    }
    // The V1 shape is the last candidate: only an exact version-1 stamp may
    // decode through it. Anything else (0, or a 2/3/4 whose proper-shape
    // decode failed above, or a future version) would be a misdecode, not a
    // legacy file.
    if legacy.version != 1 {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: legacy.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(legacy.into())
}

/// Create a new empty TemporalIndexData.
///
/// Initializes a new temporal index container with the correct magic bytes
/// (`TEMPORAL_MAGIC`) and the current manifest version.
pub fn new_temporal_index_data() -> TemporalIndexData {
    TemporalIndexData {
        magic: TEMPORAL_MAGIC,
        version: MANIFEST_VERSION,
        node_versions: Vec::new(),
        node_anchors: Vec::new(),
        edge_versions: Vec::new(),
        edge_anchors: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::{NodeId, VersionId};
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::{BiTemporalInterval, TimeRange};
    use crate::core::version::{NodeVersion, VersionData};
    use crate::storage::index_persistence::formats::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn test_temporal_index_round_trip() {
        use crate::core::GLOBAL_INTERNER;
        let dir = tempdir().unwrap();
        let path = dir.path().join("temporal.idx");

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let mut data = new_temporal_index_data();
        data.node_versions.push(NodeVersionEntry {
            version_id: 100,
            node_id: 1,
            label_idx: label.as_u32(),
            valid_from: 1000,
            valid_from_logical: 0,
            valid_to: Some(2000),
            valid_to_logical: Some(0),
            tx_time: 1000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Anchor,
            properties: PersistedPropertyMap { entries: vec![] },
            vector_snapshot_id: Some(42),
            provenance: None,
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        });
        data.node_anchors.push(NodeAnchorEntry {
            node_id: 1,
            anchor_tx_time: 1000,
            full_state: PersistedPropertyMap { entries: vec![] },
            vector_snapshot_id: Some(42),
        });

        save_temporal_index(&data, &path).unwrap();
        let loaded = load_temporal_index(&path).unwrap();

        assert_eq!(loaded.node_versions.len(), 1);
        assert_eq!(loaded.node_anchors.len(), 1);
        assert_eq!(loaded.node_versions[0].vector_snapshot_id, Some(42));
    }

    #[test]
    fn test_temporal_index_encrypted_round_trip() {
        use crate::core::GLOBAL_INTERNER;
        use crate::encryption::Aes256GcmCipher;
        use zeroize::Zeroizing;

        let dir = tempdir().unwrap();
        let path = dir.path().join("temporal.idx");
        let cipher: Arc<dyn Cipher> = {
            let mut key = Zeroizing::new([0u8; 32]);
            key[2] = 0x7F;
            Arc::new(Aes256GcmCipher::new(&key))
        };

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let mut data = new_temporal_index_data();
        data.node_versions.push(NodeVersionEntry {
            version_id: 100,
            node_id: 1,
            label_idx: label.as_u32(),
            valid_from: 1000,
            valid_from_logical: 0,
            valid_to: Some(2000),
            valid_to_logical: Some(0),
            tx_time: 1000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Anchor,
            properties: PersistedPropertyMap { entries: vec![] },
            vector_snapshot_id: Some(42),
            provenance: None,
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        });

        save_temporal_index_with_cipher(&data, &path, Some(&cipher)).unwrap();
        assert!(super::super::common::is_encrypted_index(
            &std::fs::read(&path).unwrap()
        ));

        let loaded = load_temporal_index_with_cipher(&path, Some(&cipher)).unwrap();
        assert_eq!(loaded.node_versions.len(), 1);
        assert_eq!(loaded.node_versions[0].vector_snapshot_id, Some(42));

        // Fails closed with no cipher; legacy plaintext still loads with one.
        assert!(load_temporal_index_with_cipher(&path, None).is_err());
        save_temporal_index(&data, &path).unwrap();
        assert_eq!(
            load_temporal_index_with_cipher(&path, Some(&cipher))
                .unwrap()
                .node_versions
                .len(),
            1
        );
    }

    #[test]
    fn test_temporal_index_roundtrip_with_provenance() {
        use crate::core::GLOBAL_INTERNER;
        let dir = tempdir().unwrap();
        let path = dir.path().join("temporal_provenance.idx");

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let mut data = new_temporal_index_data();
        data.node_versions.push(NodeVersionEntry {
            version_id: 100,
            node_id: 1,
            label_idx: label.as_u32(),
            valid_from: 1000,
            valid_from_logical: 0,
            valid_to: None,
            valid_to_logical: None,
            tx_time: 1000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Anchor,
            properties: PersistedPropertyMap { entries: vec![] },
            vector_snapshot_id: None,
            provenance: Some(PersistedProvenance {
                source: Some("hr-system".to_string()),
                confidence: Some(0.95),
                note: None,
                correlation_id: Some("batch-42".to_string()),
                principal: Some("ingest-writer".to_string()),
            }),
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        });

        save_temporal_index(&data, &path).unwrap();
        let loaded = load_temporal_index(&path).unwrap();

        let provenance = loaded.node_versions[0].provenance.as_ref().unwrap();
        assert_eq!(provenance.source.as_deref(), Some("hr-system"));
        assert_eq!(provenance.confidence, Some(0.95));
        assert_eq!(provenance.note, None);
        assert_eq!(provenance.correlation_id.as_deref(), Some("batch-42"));
        assert_eq!(provenance.principal.as_deref(), Some("ingest-writer"));

        // And the version without provenance round-trips as None, not a
        // fabricated default.
        let version = restore_node_version(&loaded.node_versions[0]).unwrap();
        assert!(version.provenance.is_some());
        assert_eq!(version.provenance.unwrap().source(), Some("hr-system"));
    }

    #[test]
    fn test_load_v1_temporal_index_file_defaults_provenance_none() {
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::index_persistence::formats::legacy_v1::{
            EdgeVersionEntryV1, NodeVersionEntryV1, TemporalIndexDataV1,
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy_temporal.idx");

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let legacy = TemporalIndexDataV1 {
            magic: TEMPORAL_MAGIC,
            version: 1,
            node_versions: vec![NodeVersionEntryV1 {
                version_id: 100,
                node_id: 1,
                label_idx: label.as_u32(),
                valid_from: 1000,
                valid_from_logical: 0,
                valid_to: None,
                valid_to_logical: None,
                tx_time: 1000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Anchor,
                properties: PersistedPropertyMap { entries: vec![] },
                vector_snapshot_id: Some(7),
            }],
            node_anchors: vec![],
            edge_versions: vec![EdgeVersionEntryV1 {
                version_id: 101,
                edge_id: 10,
                source_id: 1,
                target_id: 2,
                label_idx: label.as_u32(),
                valid_from: 1000,
                valid_from_logical: 0,
                valid_to: None,
                valid_to_logical: None,
                tx_time: 1000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Anchor,
                properties: PersistedPropertyMap { entries: vec![] },
            }],
            edge_anchors: vec![],
        };

        // Write bytes exactly as a pre-#3224 binary would have: no `provenance`
        // field exists in this shape at all, so `super::common::save_encoded_with_crc`
        // (which just bitcode-encodes whatever type it's given) reproduces that
        // on-disk layout precisely.
        crate::storage::index_persistence::common::save_encoded_with_crc(&legacy, &path).unwrap();

        let loaded = load_temporal_index(&path).unwrap();

        assert_eq!(loaded.node_versions.len(), 1);
        assert_eq!(loaded.node_versions[0].vector_snapshot_id, Some(7));
        assert!(loaded.node_versions[0].provenance.is_none());

        let version = restore_node_version(&loaded.node_versions[0]).unwrap();
        assert!(version.provenance.is_none());

        // The v1 EDGE entry upgrades the same way: provenance and the
        // Issue #3387 fidelity fields default to None.
        assert_eq!(loaded.edge_versions.len(), 1);
        let edge_entry = &loaded.edge_versions[0];
        assert!(edge_entry.provenance.is_none());
        assert_eq!(edge_entry.tx_end, None);
        assert_eq!(edge_entry.prev_version, None);
        let edge = restore_edge_version(edge_entry).unwrap();
        assert!(edge.temporal.transaction_time().is_current());
        assert!(edge.prev_version.is_none());
    }

    #[test]
    fn test_load_v2_temporal_index_file_defaults_principal_none() {
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::index_persistence::formats::legacy_v2::{
            NodeVersionEntryV2, PersistedProvenanceV2, TemporalIndexDataV2,
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy_v2_temporal.idx");

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let legacy = TemporalIndexDataV2 {
            magic: TEMPORAL_MAGIC,
            version: 2,
            node_versions: vec![NodeVersionEntryV2 {
                version_id: 100,
                node_id: 1,
                label_idx: label.as_u32(),
                valid_from: 1000,
                valid_from_logical: 0,
                valid_to: None,
                valid_to_logical: None,
                tx_time: 1000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Anchor,
                properties: PersistedPropertyMap { entries: vec![] },
                vector_snapshot_id: Some(7),
                provenance: Some(PersistedProvenanceV2 {
                    source: Some("hr-system".to_string()),
                    confidence: Some(0.5),
                    note: None,
                    correlation_id: None,
                }),
            }],
            node_anchors: vec![],
            edge_versions: vec![],
            edge_anchors: vec![],
        };

        // Write bytes exactly as an Issue-#3224-era binary (pre-#3350) would
        // have: provenance exists but has no `principal` field.
        crate::storage::index_persistence::common::save_encoded_with_crc(&legacy, &path).unwrap();

        let loaded = load_temporal_index(&path).unwrap();

        assert_eq!(loaded.node_versions.len(), 1);
        let provenance = loaded.node_versions[0].provenance.as_ref().unwrap();
        assert_eq!(provenance.source.as_deref(), Some("hr-system"));
        assert_eq!(provenance.confidence, Some(0.5));
        assert!(provenance.principal.is_none());

        let version = restore_node_version(&loaded.node_versions[0]).unwrap();
        let restored = version.provenance.unwrap();
        assert_eq!(restored.source(), Some("hr-system"));
        assert_eq!(restored.principal(), None);
    }

    #[test]
    fn test_convert_node_version_anchor() {
        // RED: This test will fail because convert_node_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;

        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        let temporal_636 = BiTemporalInterval::new(
            TimeRange::new(1000.into(), 2000.into()).unwrap(),
            TimeRange::new(1000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
        );
        let version = NodeVersion {
            id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            commit_timestamp: temporal_636.transaction_time().start(),
            temporal: temporal_636,
            label,
            data: VersionData::Anchor {
                properties: props.clone(),
                vector_snapshot_id: Some(42),
            },
            next_version: None,
            prev_version: None,
            provenance: None,
        };

        let entry = convert_node_version(&version).unwrap();

        assert_eq!(entry.node_id, 1);
        assert_eq!(entry.valid_from, 1000);
        assert_eq!(entry.valid_to, Some(2000));
        assert_eq!(entry.tx_time, 1000);
        assert!(matches!(entry.version_type, PersistedVersionType::Anchor));
        assert_eq!(entry.vector_snapshot_id, Some(42));
        assert_eq!(entry.properties.entries.len(), 2);
    }

    #[test]
    fn test_convert_node_version_delta() {
        // RED: This test will fail because convert_node_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;
        use crate::core::version::PropertyDelta;

        let mut delta = PropertyDelta::new();
        delta.changed.insert(
            GLOBAL_INTERNER.intern("age").unwrap(),
            crate::core::property::PropertyValue::Int(31),
        );

        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        let temporal_677 = BiTemporalInterval::new(
            TimeRange::new(2000.into(), 3000.into()).unwrap(),
            TimeRange::new(2000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
        );
        let version = NodeVersion {
            id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            commit_timestamp: temporal_677.transaction_time().start(),
            temporal: temporal_677,
            label,
            data: VersionData::Delta { delta },
            next_version: None,
            prev_version: Some(VersionId::new(1).unwrap()),
            provenance: None,
        };

        let entry = convert_node_version(&version).unwrap();

        assert_eq!(entry.node_id, 1);
        assert_eq!(entry.valid_from, 2000);
        assert_eq!(entry.valid_to, Some(3000));
        assert_eq!(entry.tx_time, 2000);
        assert_eq!(entry.vector_snapshot_id, None); // Deltas don't have snapshots
        // Delta should have changed properties
        assert!(!entry.properties.entries.is_empty());
    }

    #[test]
    fn test_convert_edge_version_anchor() {
        // RED: This test will fail because convert_edge_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;
        use crate::core::id::EdgeId;
        use crate::core::version::EdgeVersion;

        let props = PropertyMapBuilder::new()
            .insert("weight", 1.5f64)
            .insert("label", "KNOWS")
            .build();

        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let temporal_715 = BiTemporalInterval::new(
            TimeRange::new(1000.into(), 2000.into()).unwrap(),
            TimeRange::new(1000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
        );
        let version = EdgeVersion {
            id: VersionId::new(100).unwrap(),
            edge_id: EdgeId::new(10).unwrap(),
            commit_timestamp: temporal_715.transaction_time().start(),
            temporal: temporal_715,
            label,
            source: NodeId::new(1).unwrap(),
            target: NodeId::new(2).unwrap(),
            data: VersionData::Anchor {
                properties: props.clone(),
                vector_snapshot_id: None,
            },
            next_version: None,
            prev_version: None,
            provenance: None,
        };

        let entry = convert_edge_version(&version).unwrap();

        assert_eq!(entry.edge_id, 10);
        assert_eq!(entry.source_id, 1);
        assert_eq!(entry.target_id, 2);
        assert_eq!(entry.valid_from, 1000);
        assert_eq!(entry.valid_to, Some(2000));
        assert_eq!(entry.tx_time, 1000);
        assert!(matches!(entry.version_type, PersistedVersionType::Anchor));
        assert_eq!(entry.properties.entries.len(), 2);
    }

    #[test]
    fn test_restore_node_version_anchor() {
        // RED: This test will fail because restore_node_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;

        // Create a persisted node version entry
        let age_key = GLOBAL_INTERNER.intern("age").unwrap();
        let name_key = GLOBAL_INTERNER.intern("name").unwrap();

        let mut properties = PersistedPropertyMap { entries: vec![] };
        properties.entries.push((
            name_key.as_u32(),
            PersistedPropertyValue::String(GLOBAL_INTERNER.intern("Alice").unwrap().as_u32()),
        ));
        properties
            .entries
            .push((age_key.as_u32(), PersistedPropertyValue::Int(30)));

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let entry = NodeVersionEntry {
            version_id: 100,
            node_id: 1,
            label_idx: label.as_u32(),
            valid_from: 1000,
            valid_from_logical: 0,
            valid_to: Some(2000),
            valid_to_logical: Some(0),
            tx_time: 1000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Anchor,
            properties,
            vector_snapshot_id: Some(42),
            provenance: None,
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        };

        let version = restore_node_version(&entry).unwrap();

        assert_eq!(version.id.as_u64(), 100);
        assert_eq!(version.node_id.as_u64(), 1);
        assert_eq!(version.temporal.valid_time().start().wallclock(), 1000);
        assert_eq!(version.temporal.valid_time().end().wallclock(), 2000);
        assert_eq!(
            version.temporal.transaction_time().start().wallclock(),
            1000
        );
        assert!(version.data.is_anchor());
        assert_eq!(version.data.get_vector_snapshot_id(), Some(42));

        // Check properties were restored
        if let VersionData::Anchor { properties, .. } = &version.data {
            assert_eq!(properties.len(), 2);
            assert_eq!(
                properties.get("name").unwrap(),
                &crate::core::property::PropertyValue::String(Arc::from("Alice"))
            );
        } else {
            panic!("Expected anchor version");
        }
    }

    #[test]
    fn test_restore_node_version_delta() {
        // RED: This test will fail because restore_node_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;

        let age_key = GLOBAL_INTERNER.intern("age").unwrap();
        let mut properties = PersistedPropertyMap { entries: vec![] };
        properties
            .entries
            .push((age_key.as_u32(), PersistedPropertyValue::Int(31)));

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let entry = NodeVersionEntry {
            version_id: 101,
            node_id: 1,
            label_idx: label.as_u32(),
            valid_from: 2000,
            valid_from_logical: 0,
            valid_to: Some(3000),
            valid_to_logical: Some(0),
            tx_time: 2000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Delta {
                base_anchor_tx: 1000,
                base_anchor_tx_logical: 0,
                removed_keys: vec![],
            },
            properties,
            vector_snapshot_id: None,
            provenance: None,
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        };

        let version = restore_node_version(&entry).unwrap();

        assert_eq!(version.node_id.as_u64(), 1);
        assert!(version.data.is_delta());
        assert_eq!(version.data.get_vector_snapshot_id(), None);

        // Check delta was restored
        if let VersionData::Delta { delta } = &version.data {
            assert_eq!(delta.changed.len(), 1);
            assert!(delta.removed.is_empty());
        } else {
            panic!("Expected delta version");
        }
    }

    #[test]
    fn test_restore_edge_version_anchor() {
        // RED: This test will fail because restore_edge_version doesn't exist yet
        use crate::core::GLOBAL_INTERNER;

        let weight_key = GLOBAL_INTERNER.intern("weight").unwrap();
        let mut properties = PersistedPropertyMap { entries: vec![] };
        properties
            .entries
            .push((weight_key.as_u32(), PersistedPropertyValue::Float(1.5)));

        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let entry = EdgeVersionEntry {
            version_id: 200,
            edge_id: 10,
            source_id: 1,
            target_id: 2,
            label_idx: label.as_u32(),
            valid_from: 1000,
            valid_from_logical: 0,
            valid_to: Some(2000),
            valid_to_logical: Some(0),
            tx_time: 1000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Anchor,
            properties,
            provenance: None,
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        };

        let version = restore_edge_version(&entry).unwrap();

        assert_eq!(version.id.as_u64(), 200);
        assert_eq!(version.edge_id.as_u64(), 10);
        assert_eq!(version.source.as_u64(), 1);
        assert_eq!(version.target.as_u64(), 2);
        assert_eq!(version.temporal.valid_time().start().wallclock(), 1000);
        assert_eq!(version.temporal.valid_time().end().wallclock(), 2000);
        assert!(version.data.is_anchor());

        // Check properties were restored
        if let VersionData::Anchor { properties, .. } = &version.data {
            assert_eq!(properties.len(), 1);
        } else {
            panic!("Expected anchor version");
        }
    }

    #[test]
    fn test_restore_versions_into_historical_storage() {
        // RED: This test will fail because restore_into_historical_storage doesn't exist yet
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::historical::HistoricalStorage;

        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        GLOBAL_INTERNER.intern("name").unwrap();
        GLOBAL_INTERNER.intern("age").unwrap();

        // Create a persisted node version entry
        let age_key = GLOBAL_INTERNER.intern("age").unwrap();
        let name_key = GLOBAL_INTERNER.intern("name").unwrap();

        let mut properties = PersistedPropertyMap { entries: vec![] };
        properties.entries.push((
            name_key.as_u32(),
            PersistedPropertyValue::String(GLOBAL_INTERNER.intern("Alice").unwrap().as_u32()),
        ));
        properties
            .entries
            .push((age_key.as_u32(), PersistedPropertyValue::Int(30)));

        let entry = NodeVersionEntry {
            version_id: 100,
            node_id: 1,
            label_idx: person_label.as_u32(),
            valid_from: 1000,
            valid_from_logical: 0,
            valid_to: Some(2000),
            valid_to_logical: Some(0),
            tx_time: 1000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Anchor,
            properties,
            vector_snapshot_id: Some(42),
            provenance: None,
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        };

        // Create temporal data (labels are now stored in entries)
        let mut temporal_data = new_temporal_index_data();
        temporal_data.node_versions.push(entry);

        // Restore into HistoricalStorage
        let mut historical = HistoricalStorage::new();
        restore_into_historical_storage(&temporal_data, &mut historical).unwrap();

        // Verify the version was restored
        let versions = historical.get_node_versions();
        assert_eq!(versions.len(), 1, "Should have 1 node version");

        let version = versions.values().next().unwrap();
        assert_eq!(version.node_id.as_u64(), 1);
        assert_eq!(version.temporal.valid_time().start().wallclock(), 1000);
        assert_eq!(version.temporal.valid_time().end().wallclock(), 2000);
        assert!(version.data.is_anchor());
    }

    // ========================================================================
    // Vector Delta Persistence Tests (Issue #215, Code Review C1/C2)
    // ========================================================================

    #[test]
    fn test_persist_delta_with_full_vector_delta() {
        // Test that VectorDelta::Full can be persisted successfully
        use crate::core::GLOBAL_INTERNER;
        use crate::core::version::{PropertyDelta, VectorDelta};

        let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();

        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Full(Arc::from(embedding.as_slice())),
        );

        let label = GLOBAL_INTERNER.intern("Document").unwrap();
        let temporal_973 = BiTemporalInterval::new(
            TimeRange::new(2000.into(), 3000.into()).unwrap(),
            TimeRange::new(2000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
        );
        let version = NodeVersion {
            id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            commit_timestamp: temporal_973.transaction_time().start(),
            temporal: temporal_973,
            label,
            data: VersionData::Delta { delta },
            next_version: None,
            prev_version: Some(VersionId::new(1).unwrap()),
            provenance: None,
        };

        // Should succeed - Full deltas can be persisted
        let entry = convert_node_version(&version).unwrap();
        assert_eq!(entry.version_id, 2);
        // Vector should be in properties as a full vector
        assert!(!entry.properties.entries.is_empty());
    }

    #[test]
    fn test_persist_delta_with_sparse_vector_delta_fails() {
        // Test that VectorDelta::Sparse causes persistence to fail (prevents data loss)
        use crate::core::GLOBAL_INTERNER;
        use crate::core::version::{PropertyDelta, VectorDelta};

        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();

        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Sparse {
                dimension: 384,
                changes: Arc::new(vec![(0, 0.5f32), (100, 0.6f32)]),
            },
        );

        let label = GLOBAL_INTERNER.intern("Document").unwrap();
        let temporal_1011 = BiTemporalInterval::new(
            TimeRange::new(2000.into(), 3000.into()).unwrap(),
            TimeRange::new(2000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
        );
        let version = NodeVersion {
            id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            commit_timestamp: temporal_1011.transaction_time().start(),
            temporal: temporal_1011,
            label,
            data: VersionData::Delta { delta },
            next_version: None,
            prev_version: Some(VersionId::new(1).unwrap()),
            provenance: None,
        };

        // Should FAIL - Sparse deltas cannot be persisted without materialization
        let result = convert_node_version(&version);
        assert!(result.is_err(), "Should fail to persist Sparse delta");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("VectorDelta::Sparse"),
            "Error should mention Sparse delta: {}",
            err_msg
        );
        assert!(
            err_msg.contains("materialize_vector_deltas"),
            "Error should mention materialization: {}",
            err_msg
        );
    }

    #[test]
    fn test_materialize_vector_deltas() {
        // Test that PropertyDelta::materialize_vector_deltas() correctly converts Sparse to Full
        use crate::core::GLOBAL_INTERNER;
        use crate::core::version::{PropertyDelta, VectorDelta};

        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();

        // Create base properties with a vector
        let base_embedding = vec![0.1f32; 384];
        let base_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&base_embedding))
            .build();

        // Create a delta with a sparse change
        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Sparse {
                dimension: 384,
                changes: Arc::new(vec![(0, 0.5f32), (100, 0.6f32)]),
            },
        );

        // Materialize the delta
        delta.materialize_vector_deltas(&base_props).unwrap();

        // After materialization:
        // 1. vector_deltas should be empty
        // 2. changed should contain the full vector
        assert_eq!(
            delta.vector_deltas.len(),
            0,
            "vector_deltas should be empty after materialization"
        );
        assert_eq!(
            delta.changed.len(),
            1,
            "changed should contain the materialized vector"
        );

        let materialized = delta.changed.get(&embedding_key).unwrap();
        let materialized_vec = materialized.as_vector().unwrap();
        assert_eq!(materialized_vec.len(), 384);
        assert_eq!(
            materialized_vec[0], 0.5f32,
            "First element should be updated"
        );
        assert_eq!(
            materialized_vec[100], 0.6f32,
            "Element 100 should be updated"
        );
        assert_eq!(
            materialized_vec[50], 0.1f32,
            "Unchanged element should remain"
        );
    }

    #[test]
    fn test_materialize_vector_deltas_missing_base() {
        // Test that materialization fails gracefully when base property is missing
        use crate::core::GLOBAL_INTERNER;
        use crate::core::version::{PropertyDelta, VectorDelta};

        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();
        let base_props = PropertyMapBuilder::new().build(); // Empty base

        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Sparse {
                dimension: 384,
                changes: Arc::new(vec![(0, 0.5f32)]),
            },
        );

        // Should fail - base property not found
        let result = delta.materialize_vector_deltas(&base_props);
        assert!(result.is_err(), "Should fail when base property missing");
    }

    #[test]
    fn test_persist_materialized_delta_succeeds() {
        // Round-trip test: create sparse delta, materialize it, then persist
        use crate::core::GLOBAL_INTERNER;
        use crate::core::version::{PropertyDelta, VectorDelta};

        let embedding_key = GLOBAL_INTERNER.intern("embedding").unwrap();

        // Create base properties
        let base_embedding = vec![0.1f32; 384];
        let base_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&base_embedding))
            .build();

        // Create delta with sparse change
        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            embedding_key,
            VectorDelta::Sparse {
                dimension: 384,
                changes: Arc::new(vec![(0, 0.5f32), (100, 0.6f32)]),
            },
        );

        // Materialize BEFORE persistence
        delta.materialize_vector_deltas(&base_props).unwrap();

        // Create version with materialized delta
        let label = GLOBAL_INTERNER.intern("Document").unwrap();
        let temporal_1150 = BiTemporalInterval::new(
            TimeRange::new(2000.into(), 3000.into()).unwrap(),
            TimeRange::new(2000.into(), crate::core::temporal::TIMESTAMP_MAX).unwrap(),
        );
        let version = NodeVersion {
            id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            commit_timestamp: temporal_1150.transaction_time().start(),
            temporal: temporal_1150,
            label,
            data: VersionData::Delta { delta },
            next_version: None,
            prev_version: Some(VersionId::new(1).unwrap()),
            provenance: None,
        };

        // Should succeed - delta is now materialized
        let entry = convert_node_version(&version).unwrap();
        assert_eq!(entry.version_id, 2);
        assert!(
            !entry.properties.entries.is_empty(),
            "Should have materialized vector property"
        );
    }

    #[test]
    fn test_persist_delta_preserves_logical_timestamp() {
        // Test that Delta version persistence preserves logical timestamps
        // even though they are currently unused in restoration (for future proofing)
        use crate::core::GLOBAL_INTERNER;
        use crate::core::hlc::HybridTimestamp;
        use crate::core::version::PropertyDelta;

        let wallclock = 2_000_000_000;
        let logical = 99;

        let start_time = HybridTimestamp::new(wallclock, logical).unwrap();
        let temporal = BiTemporalInterval::current(start_time);

        let label = GLOBAL_INTERNER.intern("Person").unwrap();

        let mut delta = PropertyDelta::new();
        delta.changed.insert(
            GLOBAL_INTERNER.intern("age").unwrap(),
            crate::core::property::PropertyValue::Int(31),
        );

        let version = NodeVersion {
            id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            commit_timestamp: temporal.transaction_time().start(),
            temporal,
            label,
            data: VersionData::Delta { delta },
            next_version: None,
            prev_version: Some(VersionId::new(1).unwrap()),
            provenance: None,
        };

        // Convert to persisted entry
        let entry = convert_node_version(&version).unwrap();

        // Verify logical timestamp fields in entry
        assert_eq!(entry.tx_time, wallclock);
        assert_eq!(entry.tx_time_logical, logical);

        // Verify delta-specific fields
        if let PersistedVersionType::Delta {
            base_anchor_tx,
            base_anchor_tx_logical,
            ..
        } = entry.version_type
        {
            assert_eq!(base_anchor_tx, wallclock);
            assert_eq!(base_anchor_tx_logical, logical);
        } else {
            panic!("Expected Delta version type");
        }

        // Restore and verify (logical timestamp should be preserved in temporal)
        let restored = restore_node_version(&entry).unwrap();
        assert_eq!(
            restored.temporal.transaction_time().start().logical(),
            logical
        );
    }

    #[test]
    fn test_hlc_logical_component_persistence_loss() {
        // Regression test for HLC logical counter loss during index persistence
        use crate::core::GLOBAL_INTERNER;
        use crate::core::hlc::HybridTimestamp;

        let wallclock = 1_000_000_000;
        let logical: u32 = 42; // Non-zero logical counter

        // Create a HybridTimestamp with non-zero logical counter
        let ts_start = HybridTimestamp::new(wallclock, logical).unwrap();
        let ts_end = HybridTimestamp::new(wallclock + 1000, 0).unwrap();

        let time_range = TimeRange::new(ts_start, ts_end).unwrap();
        let tx_range = TimeRange::new(ts_start, TIMESTAMP_MAX).unwrap();
        let temporal = BiTemporalInterval::new(time_range, tx_range);

        let props = PropertyMapBuilder::new().build();
        let label = GLOBAL_INTERNER.intern("Test").unwrap();

        let version = NodeVersion {
            id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            commit_timestamp: temporal.transaction_time().start(),
            temporal,
            label,
            data: VersionData::Anchor {
                properties: props,
                vector_snapshot_id: None,
            },
            next_version: None,
            prev_version: None,
            provenance: None,
        };

        // Persist
        let entry = convert_node_version(&version).unwrap();

        // Restore
        let restored = restore_node_version(&entry).unwrap();

        // Check if logical counter was preserved
        let restored_start = restored.temporal.valid_time().start();
        assert_eq!(
            restored_start.wallclock(),
            wallclock,
            "Wallclock should be preserved"
        );
        assert_eq!(
            restored_start.logical(),
            logical,
            "Logical counter should be preserved"
        );
    }

    /// Issue #3387: tx-time closures and version chain links must round-trip
    /// through convert -> save -> load -> restore for node versions.
    #[test]
    fn test_node_tx_closure_and_chain_links_round_trip() {
        use crate::core::GLOBAL_INTERNER;
        use crate::core::hlc::HybridTimestamp;

        let dir = tempdir().unwrap();
        let path = dir.path().join("fidelity_node.idx");

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let tx_start = HybridTimestamp::new(1000, 3).unwrap();
        let tx_end = HybridTimestamp::new(2000, 7).unwrap();
        let temporal = BiTemporalInterval::new(
            TimeRange::new(500.into(), TIMESTAMP_MAX).unwrap(),
            TimeRange::new(tx_start, tx_end).unwrap(),
        );
        let version = NodeVersion {
            id: VersionId::new(10).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            commit_timestamp: tx_start,
            temporal,
            label,
            data: VersionData::Anchor {
                properties: PropertyMapBuilder::new().insert("name", "Alice").build(),
                vector_snapshot_id: None,
            },
            next_version: Some(VersionId::new(11).unwrap()),
            prev_version: Some(VersionId::new(9).unwrap()),
            provenance: None,
        };

        let entry = convert_node_version(&version).unwrap();
        assert_eq!(entry.tx_end, Some(2000));
        assert_eq!(entry.tx_end_logical, Some(7));
        assert_eq!(entry.prev_version, Some(9));
        assert_eq!(entry.next_version, Some(11));

        let mut data = new_temporal_index_data();
        data.node_versions.push(entry);
        save_temporal_index(&data, &path).unwrap();
        let loaded = load_temporal_index(&path).unwrap();

        let restored = restore_node_version(&loaded.node_versions[0]).unwrap();
        let restored_tx = restored.temporal.transaction_time();
        assert!(!restored_tx.is_current(), "tx-time closure must round-trip");
        assert_eq!(restored_tx.start(), tx_start);
        assert_eq!(restored_tx.end(), tx_end, "closed tx end (incl. logical)");
        assert_eq!(restored.prev_version, Some(VersionId::new(9).unwrap()));
        assert_eq!(restored.next_version, Some(VersionId::new(11).unwrap()));
    }

    /// Issue #3387: same round-trip contract for edge versions.
    #[test]
    fn test_edge_tx_closure_and_chain_links_round_trip() {
        use crate::core::GLOBAL_INTERNER;
        use crate::core::hlc::HybridTimestamp;

        let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let tx_start = HybridTimestamp::new(1000, 0).unwrap();
        let tx_end = HybridTimestamp::new(1500, 2).unwrap();
        let temporal = BiTemporalInterval::new(
            TimeRange::new(500.into(), TIMESTAMP_MAX).unwrap(),
            TimeRange::new(tx_start, tx_end).unwrap(),
        );
        let version = EdgeVersion {
            id: VersionId::new(20).unwrap(),
            edge_id: EdgeId::new(5).unwrap(),
            source: NodeId::new(1).unwrap(),
            target: NodeId::new(2).unwrap(),
            commit_timestamp: tx_start,
            temporal,
            label,
            data: VersionData::Anchor {
                properties: PropertyMapBuilder::new().insert("weight", 1i64).build(),
                vector_snapshot_id: None,
            },
            next_version: Some(VersionId::new(21).unwrap()),
            prev_version: None,
            provenance: None,
        };

        let entry = convert_edge_version(&version).unwrap();
        assert_eq!(entry.tx_end, Some(1500));
        assert_eq!(entry.tx_end_logical, Some(2));
        assert_eq!(entry.prev_version, None);
        assert_eq!(entry.next_version, Some(21));

        let restored = restore_edge_version(&entry).unwrap();
        let restored_tx = restored.temporal.transaction_time();
        assert!(!restored_tx.is_current(), "tx-time closure must round-trip");
        assert_eq!(restored_tx.end(), tx_end);
        assert_eq!(restored.prev_version, None);
        assert_eq!(restored.next_version, Some(VersionId::new(21).unwrap()));
    }

    /// Issue #3387: an open (current) tx interval persists as None and
    /// restores open -- the closure fields never fabricate an end.
    #[test]
    fn test_open_tx_interval_round_trips_open() {
        use crate::core::GLOBAL_INTERNER;

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let temporal = BiTemporalInterval::new(
            TimeRange::new(500.into(), TIMESTAMP_MAX).unwrap(),
            TimeRange::new(1000.into(), TIMESTAMP_MAX).unwrap(),
        );
        let version = NodeVersion {
            id: VersionId::new(10).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            commit_timestamp: temporal.transaction_time().start(),
            temporal,
            label,
            data: VersionData::Anchor {
                properties: PropertyMapBuilder::new().build(),
                vector_snapshot_id: None,
            },
            next_version: None,
            prev_version: None,
            provenance: None,
        };

        let entry = convert_node_version(&version).unwrap();
        assert_eq!(entry.tx_end, None);
        assert_eq!(entry.tx_end_logical, None);

        let restored = restore_node_version(&entry).unwrap();
        assert!(restored.temporal.transaction_time().is_current());
        assert_eq!(restored.prev_version, None);
        assert_eq!(restored.next_version, None);
    }

    /// Issue #3387: a `version == 2` file (pre-fidelity layout, written by a
    /// pre-#3387 binary) still loads, upgrading in memory with the new
    /// fields `None` and provenance preserved.
    #[test]
    fn test_load_v2_temporal_index_file_defaults_fidelity_fields_none() {
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::index_persistence::formats::legacy_v2::{
            EdgeVersionEntryV2, NodeVersionEntryV2, PersistedProvenanceV2, TemporalIndexDataV2,
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy_v2_temporal.idx");

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let legacy = TemporalIndexDataV2 {
            magic: TEMPORAL_MAGIC,
            version: 2,
            node_versions: vec![NodeVersionEntryV2 {
                version_id: 100,
                node_id: 1,
                label_idx: label.as_u32(),
                valid_from: 1000,
                valid_from_logical: 0,
                valid_to: None,
                valid_to_logical: None,
                tx_time: 1000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Anchor,
                properties: PersistedPropertyMap { entries: vec![] },
                vector_snapshot_id: Some(7),
                provenance: Some(PersistedProvenanceV2 {
                    source: Some("hr-system".to_string()),
                    confidence: Some(0.95),
                    note: None,
                    correlation_id: None,
                }),
            }],
            node_anchors: vec![],
            edge_versions: vec![EdgeVersionEntryV2 {
                version_id: 101,
                edge_id: 10,
                source_id: 1,
                target_id: 2,
                label_idx: label.as_u32(),
                valid_from: 1000,
                valid_from_logical: 0,
                valid_to: Some(2000),
                valid_to_logical: Some(0),
                tx_time: 1000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Anchor,
                properties: PersistedPropertyMap { entries: vec![] },
                provenance: Some(PersistedProvenanceV2 {
                    source: Some("edge-system".to_string()),
                    confidence: None,
                    note: None,
                    correlation_id: None,
                }),
            }],
            edge_anchors: vec![],
        };

        // Write bytes exactly as a pre-#3387 binary would have: no tx-end /
        // chain-link fields exist in this shape at all.
        crate::storage::index_persistence::common::save_encoded_with_crc(&legacy, &path).unwrap();

        let loaded = load_temporal_index(&path).unwrap();

        assert_eq!(loaded.node_versions.len(), 1);
        let entry = &loaded.node_versions[0];
        assert_eq!(entry.vector_snapshot_id, Some(7));
        assert!(
            entry.provenance.is_some(),
            "v2 provenance must be preserved"
        );
        assert_eq!(entry.tx_end, None);
        assert_eq!(entry.tx_end_logical, None);
        assert_eq!(entry.prev_version, None);
        assert_eq!(entry.next_version, None);

        // Restored version: open tx interval, no links (the historical
        // storage rebuild heuristic then reconstructs chains as before).
        let version = restore_node_version(entry).unwrap();
        assert!(version.temporal.transaction_time().is_current());
        assert!(version.prev_version.is_none());
        assert!(version.next_version.is_none());

        // The v2 EDGE entry upgrades identically: provenance preserved,
        // fidelity fields None, closed valid interval kept.
        assert_eq!(loaded.edge_versions.len(), 1);
        let edge_entry = &loaded.edge_versions[0];
        assert!(edge_entry.provenance.is_some());
        assert_eq!(edge_entry.valid_to, Some(2000));
        assert_eq!(edge_entry.tx_end, None);
        assert_eq!(edge_entry.next_version, None);
        let edge = restore_edge_version(edge_entry).unwrap();
        assert!(edge.temporal.transaction_time().is_current());
        assert_eq!(edge.temporal.valid_time().end().wallclock(), 2000);
    }

    /// Issue #3387 x #3350 reconciliation: a `version == 3` file (the #3350
    /// principal-era layout, written by a pre-#3387 binary) still loads,
    /// upgrading in memory with the principal preserved and the fidelity
    /// fields `None`.
    #[test]
    fn test_load_v3_temporal_index_file_defaults_fidelity_fields_none() {
        use crate::core::GLOBAL_INTERNER;
        use crate::storage::index_persistence::formats::legacy_v3::{
            EdgeVersionEntryV3, NodeVersionEntryV3, TemporalIndexDataV3,
        };

        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy_v3_temporal.idx");

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let legacy = TemporalIndexDataV3 {
            magic: TEMPORAL_MAGIC,
            version: 3,
            node_versions: vec![NodeVersionEntryV3 {
                version_id: 100,
                node_id: 1,
                label_idx: label.as_u32(),
                valid_from: 1000,
                valid_from_logical: 0,
                valid_to: None,
                valid_to_logical: None,
                tx_time: 1000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Anchor,
                properties: PersistedPropertyMap { entries: vec![] },
                vector_snapshot_id: Some(7),
                provenance: Some(PersistedProvenance {
                    source: Some("hr-system".to_string()),
                    confidence: Some(0.95),
                    note: None,
                    correlation_id: None,
                    principal: Some("alice@example.com".to_string()),
                }),
            }],
            node_anchors: vec![],
            edge_versions: vec![EdgeVersionEntryV3 {
                version_id: 101,
                edge_id: 10,
                source_id: 1,
                target_id: 2,
                label_idx: label.as_u32(),
                valid_from: 1000,
                valid_from_logical: 0,
                valid_to: Some(2000),
                valid_to_logical: Some(0),
                tx_time: 1000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Anchor,
                properties: PersistedPropertyMap { entries: vec![] },
                provenance: Some(PersistedProvenance {
                    source: Some("edge-system".to_string()),
                    confidence: None,
                    note: None,
                    correlation_id: None,
                    principal: None,
                }),
            }],
            edge_anchors: vec![],
        };

        // Write bytes exactly as a #3350-era (pre-#3387) binary would have:
        // principal-carrying provenance, no tx-end / chain-link fields.
        crate::storage::index_persistence::common::save_encoded_with_crc(&legacy, &path).unwrap();

        let loaded = load_temporal_index(&path).unwrap();

        assert_eq!(loaded.node_versions.len(), 1);
        let entry = &loaded.node_versions[0];
        assert_eq!(entry.vector_snapshot_id, Some(7));
        let provenance = entry.provenance.as_ref().unwrap();
        assert_eq!(
            provenance.principal.as_deref(),
            Some("alice@example.com"),
            "v3 principal must be preserved"
        );
        assert_eq!(entry.tx_end, None);
        assert_eq!(entry.tx_end_logical, None);
        assert_eq!(entry.prev_version, None);
        assert_eq!(entry.next_version, None);

        // Restored version: open tx interval, no links (the historical
        // storage rebuild heuristic then reconstructs chains as before).
        let version = restore_node_version(entry).unwrap();
        assert!(version.temporal.transaction_time().is_current());
        assert!(version.prev_version.is_none());
        assert!(version.next_version.is_none());

        // The v3 EDGE entry upgrades identically.
        assert_eq!(loaded.edge_versions.len(), 1);
        let edge_entry = &loaded.edge_versions[0];
        assert!(edge_entry.provenance.is_some());
        assert_eq!(edge_entry.valid_to, Some(2000));
        assert_eq!(edge_entry.tx_end, None);
        assert_eq!(edge_entry.next_version, None);
        let edge = restore_edge_version(edge_entry).unwrap();
        assert!(edge.temporal.transaction_time().is_current());
        assert_eq!(edge.temporal.valid_time().end().wallclock(), 2000);
    }

    // ------------------------------------------------------------------
    // Issue #3387 hardening: corrupt-entry and helper arms
    // ------------------------------------------------------------------

    fn minimal_node_entry() -> NodeVersionEntry {
        use crate::core::GLOBAL_INTERNER;
        NodeVersionEntry {
            version_id: 100,
            node_id: 1,
            label_idx: GLOBAL_INTERNER.intern("CorruptNode").unwrap().as_u32(),
            valid_from: 1000,
            valid_from_logical: 0,
            valid_to: None,
            valid_to_logical: None,
            tx_time: 1000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Anchor,
            properties: PersistedPropertyMap { entries: vec![] },
            vector_snapshot_id: None,
            provenance: None,
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        }
    }

    fn minimal_edge_entry() -> EdgeVersionEntry {
        use crate::core::GLOBAL_INTERNER;
        EdgeVersionEntry {
            version_id: 200,
            edge_id: 1,
            source_id: 1,
            target_id: 2,
            label_idx: GLOBAL_INTERNER.intern("CORRUPT_EDGE").unwrap().as_u32(),
            valid_from: 1000,
            valid_from_logical: 0,
            valid_to: None,
            valid_to_logical: None,
            tx_time: 1000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Anchor,
            properties: PersistedPropertyMap { entries: vec![] },
            provenance: None,
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        }
    }

    /// A persisted tx_end without its logical component is corruption, not
    /// a value to default (node variant).
    #[test]
    fn test_restore_node_version_rejects_tx_end_without_logical() {
        let mut entry = minimal_node_entry();
        entry.tx_end = Some(2000);
        entry.tx_end_logical = None;

        let err = restore_node_version(&entry).unwrap_err();
        assert!(
            err.to_string().contains("tx_end_logical is missing"),
            "unexpected error: {err}"
        );
    }

    /// Edge mirror of the lone-tx_end corruption check.
    #[test]
    fn test_restore_edge_version_rejects_tx_end_without_logical() {
        let mut entry = minimal_edge_entry();
        entry.tx_end = Some(2000);
        entry.tx_end_logical = None;

        let err = restore_edge_version(&entry).unwrap_err();
        assert!(
            err.to_string().contains("tx_end_logical is missing"),
            "unexpected error: {err}"
        );
    }

    /// A persisted tx interval that closes at/before its start is invalid.
    #[test]
    fn test_restore_node_version_rejects_inverted_tx_interval() {
        let mut entry = minimal_node_entry();
        entry.tx_end = Some(500); // < tx_time (1000)
        entry.tx_end_logical = Some(0);

        let err = restore_node_version(&entry).unwrap_err();
        assert!(
            err.to_string().contains("Invalid transaction time range"),
            "unexpected error: {err}"
        );
    }

    /// Edge mirror of the inverted-interval check.
    #[test]
    fn test_restore_edge_version_rejects_inverted_tx_interval() {
        let mut entry = minimal_edge_entry();
        entry.tx_end = Some(500);
        entry.tx_end_logical = Some(0);

        let err = restore_edge_version(&entry).unwrap_err();
        assert!(
            err.to_string().contains("Invalid transaction time range"),
            "unexpected error: {err}"
        );
    }

    /// A persisted chain link exceeding MAX_VALID_ID is rejected, per the
    /// crate-wide ID validation contract (node prev + edge next variants).
    #[test]
    fn test_restore_version_rejects_invalid_chain_link_ids() {
        let mut entry = minimal_node_entry();
        entry.prev_version = Some(u64::MAX);
        let err = restore_node_version(&entry).unwrap_err();
        assert!(
            err.to_string().contains("Invalid prev_version ID"),
            "unexpected error: {err}"
        );

        let mut entry = minimal_edge_entry();
        entry.next_version = Some(u64::MAX);
        let err = restore_edge_version(&entry).unwrap_err();
        assert!(
            err.to_string().contains("Invalid next_version ID"),
            "unexpected error: {err}"
        );
    }

    /// The materialization helpers' non-delta arms: anchors need no
    /// materialization and pass through unchanged.
    #[test]
    fn test_materialization_helpers_anchor_and_full_arms() {
        use crate::core::GLOBAL_INTERNER;
        use crate::core::version::VectorDelta;

        let anchor = VersionData::anchor(PropertyMapBuilder::new().insert("k", "v").build());
        assert!(!needs_sparse_vector_materialization(&anchor));

        let base = PropertyMapBuilder::new().build();
        let out = materialize_version_data_for_persistence(&anchor, &base).unwrap();
        assert!(matches!(out, VersionData::Anchor { .. }));

        // A delta carrying only FULL vector deltas needs no materialization.
        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            GLOBAL_INTERNER.intern("embedding").unwrap(),
            VectorDelta::Full(Arc::from(vec![1.0f32, 2.0].as_slice())),
        );
        let full_only = VersionData::Delta { delta };
        assert!(!needs_sparse_vector_materialization(&full_only));

        // A delta with a SPARSE vector delta does.
        let mut delta = PropertyDelta::new();
        delta.vector_deltas.insert(
            GLOBAL_INTERNER.intern("embedding").unwrap(),
            VectorDelta::Sparse {
                dimension: 4,
                changes: Arc::new(vec![(1, 9.0)]),
            },
        );
        let sparse = VersionData::Delta { delta };
        assert!(needs_sparse_vector_materialization(&sparse));

        // Materializing the sparse delta against a matching base succeeds
        // and folds the full vector into `changed`.
        let base = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 0.0, 0.0, 0.0])
            .build();
        let out = materialize_version_data_for_persistence(&sparse, &base).unwrap();
        let VersionData::Delta { delta } = out else {
            panic!("materialized delta must stay a delta");
        };
        assert!(delta.vector_deltas.is_empty());

        // Materializing against a base MISSING the vector fails loudly.
        let empty_base = PropertyMapBuilder::new().build();
        let err = materialize_version_data_for_persistence(&sparse, &empty_base).unwrap_err();
        assert!(
            err.to_string().contains("base property not found"),
            "unexpected error: {err}"
        );
    }
}
