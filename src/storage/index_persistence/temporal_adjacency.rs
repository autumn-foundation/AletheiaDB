//! Temporal adjacency index persistence.

use std::path::Path;
use std::sync::Arc;

use crate::core::hlc::HybridTimestamp;
use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::InternedString;
use crate::index::temporal_adjacency::{
    TemporalAdjacencyConfig, TemporalAdjacencyEntry, TemporalAdjacencyIndex,
};

use super::error::{IndexPersistenceError, Result};
use super::formats::{NodeAdjacencyEntry, PersistedTemporalAdjacencyEntry, TemporalAdjacencyData};
use super::{MANIFEST_VERSION, TEMPORAL_ADJACENCY_MAGIC};

/// Save temporal adjacency index to disk.
///
/// Creates `temporal_adjacency/adjacency.idx` with all index entries.
pub fn save_temporal_adjacency_index(
    index: &TemporalAdjacencyIndex,
    data_dir: &Path,
) -> Result<()> {
    // Create temporal_adjacency directory
    let adjacency_dir = data_dir.join("temporal_adjacency");
    std::fs::create_dir_all(&adjacency_dir)?;

    // Extract only outgoing edges (incoming will be rebuilt during load)
    let outgoing = extract_outgoing_data(index);

    // Create persisted format
    let data = TemporalAdjacencyData {
        magic: TEMPORAL_ADJACENCY_MAGIC,
        version: MANIFEST_VERSION,
        outgoing,
    };

    // Serialize with bitcode
    let bytes = bitcode::encode(&data);

    // Write atomically
    let adjacency_file = adjacency_dir.join("adjacency.idx");
    super::atomic_write(&adjacency_file, &bytes)?;

    Ok(())
}

/// Maximum file size for temporal adjacency index (100 MB)
const MAX_ADJACENCY_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// Load temporal adjacency index from disk.
///
/// Reads `temporal_adjacency/adjacency.idx` and reconstructs the index.
/// The incoming index is automatically rebuilt from outgoing edges during reconstruction.
pub fn load_temporal_adjacency_index(data_dir: &Path) -> Result<Arc<TemporalAdjacencyIndex>> {
    let adjacency_file = data_dir.join("temporal_adjacency").join("adjacency.idx");

    // Check file size to prevent DoS
    let metadata = std::fs::metadata(&adjacency_file)?;
    if metadata.len() > MAX_ADJACENCY_FILE_SIZE {
        return Err(IndexPersistenceError::Serialization(format!(
            "Temporal adjacency file too large: {} bytes (max: {})",
            metadata.len(),
            MAX_ADJACENCY_FILE_SIZE
        )));
    }

    // Read file
    let bytes = std::fs::read(&adjacency_file)?;

    // Deserialize
    let data: TemporalAdjacencyData = bitcode::decode(&bytes).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Failed to deserialize temporal adjacency index: {}",
            e
        ))
    })?;

    // Validate version
    if data.version != MANIFEST_VERSION {
        return Err(IndexPersistenceError::Serialization(format!(
            "Unsupported temporal adjacency format version: {} (expected: {})",
            data.version, MANIFEST_VERSION
        )));
    }

    // Verify magic bytes
    if data.magic != TEMPORAL_ADJACENCY_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: adjacency_file,
            expected: TEMPORAL_ADJACENCY_MAGIC,
            got: data.magic,
        });
    }

    // Reconstruct index
    let index = reconstruct_index(data)?;

    Ok(Arc::new(index))
}

/// Extract outgoing edges for persistence.
///
/// Only outgoing edges are persisted to disk. The incoming index is automatically
/// rebuilt during load by calling insert_edge(), which populates both directions.
fn extract_outgoing_data(index: &TemporalAdjacencyIndex) -> Vec<NodeAdjacencyEntry> {
    let mut outgoing_entries = Vec::new();

    // Extract outgoing edges
    for item in index.outgoing.iter() {
        let node_id = item.key().as_u64();
        let entries: Vec<PersistedTemporalAdjacencyEntry> =
            item.value().iter().map(convert_to_persisted).collect();

        outgoing_entries.push(NodeAdjacencyEntry { node_id, entries });
    }

    outgoing_entries
}

/// Convert runtime entry to persisted format.
fn convert_to_persisted(entry: &TemporalAdjacencyEntry) -> PersistedTemporalAdjacencyEntry {
    PersistedTemporalAdjacencyEntry {
        edge_id: entry.edge_id.as_u64(),
        neighbor: entry.neighbor.as_u64(),
        label: entry.label.as_u32(),
        valid_from_wallclock: entry.valid_from.wallclock(),
        valid_from_logical: entry.valid_from.logical(),
        valid_to_wallclock: entry.valid_to.wallclock(),
        valid_to_logical: entry.valid_to.logical(),
        tx_from_wallclock: entry.tx_from.wallclock(),
        tx_from_logical: entry.tx_from.logical(),
        tx_to_wallclock: entry.tx_to.wallclock(),
        tx_to_logical: entry.tx_to.logical(),
    }
}

/// Reconstruct index from persisted data.
///
/// Only outgoing edges are loaded from disk. The incoming index is automatically
/// rebuilt by calling `insert_edge()`, which populates both directions.
fn reconstruct_index(data: TemporalAdjacencyData) -> Result<TemporalAdjacencyIndex> {
    let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());

    // Reconstruct outgoing edges
    for node_entry in data.outgoing {
        let node_id = NodeId::new(node_entry.node_id)
            .map_err(|e| IndexPersistenceError::Serialization(format!("Invalid node ID: {}", e)))?;

        for entry in node_entry.entries {
            let persisted_entry = convert_from_persisted(entry)?;
            // Insert into index
            index
                .insert_edge(
                    persisted_entry.edge_id,
                    node_id,
                    persisted_entry.neighbor,
                    persisted_entry.label,
                    persisted_entry.valid_from,
                    persisted_entry.valid_to,
                    persisted_entry.tx_from,
                    persisted_entry.tx_to,
                )
                .map_err(|e| {
                    IndexPersistenceError::Serialization(format!(
                        "Failed to insert edge into index: {}",
                        e
                    ))
                })?;
        }
    }

    Ok(index)
}

/// Convert persisted entry back to runtime format.
fn convert_from_persisted(
    entry: PersistedTemporalAdjacencyEntry,
) -> Result<TemporalAdjacencyEntry> {
    let edge_id = EdgeId::new(entry.edge_id)
        .map_err(|e| IndexPersistenceError::Serialization(format!("Invalid edge ID: {}", e)))?;

    let neighbor = NodeId::new(entry.neighbor)
        .map_err(|e| IndexPersistenceError::Serialization(format!("Invalid neighbor ID: {}", e)))?;

    let label = InternedString::from_raw(entry.label);

    // SAFETY: Timestamps were validated when originally created by HybridTimestamp::new()
    // before being persisted. We trust the persisted data to contain valid timestamp values.
    // This avoids redundant validation on every load while maintaining correctness.
    let valid_from =
        HybridTimestamp::new_unchecked(entry.valid_from_wallclock, entry.valid_from_logical);
    let valid_to = HybridTimestamp::new_unchecked(entry.valid_to_wallclock, entry.valid_to_logical);
    let tx_from = HybridTimestamp::new_unchecked(entry.tx_from_wallclock, entry.tx_from_logical);
    let tx_to = HybridTimestamp::new_unchecked(entry.tx_to_wallclock, entry.tx_to_logical);

    Ok(TemporalAdjacencyEntry {
        edge_id,
        neighbor,
        label,
        valid_from,
        valid_to,
        tx_from,
        tx_to,
    })
}
