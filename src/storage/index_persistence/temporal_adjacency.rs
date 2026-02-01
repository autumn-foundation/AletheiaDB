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

    // Extract data from index
    let (outgoing, incoming) = extract_index_data(index);

    // Create persisted format
    let data = TemporalAdjacencyData {
        magic: TEMPORAL_ADJACENCY_MAGIC,
        version: MANIFEST_VERSION,
        outgoing,
        incoming,
    };

    // Serialize with bitcode
    let bytes = bitcode::encode(&data);

    // Write atomically
    let adjacency_file = adjacency_dir.join("adjacency.idx");
    super::atomic_write(&adjacency_file, &bytes)?;

    Ok(())
}

/// Load temporal adjacency index from disk.
///
/// Reads `temporal_adjacency/adjacency.idx` and reconstructs the index.
pub fn load_temporal_adjacency_index(data_dir: &Path) -> Result<Arc<TemporalAdjacencyIndex>> {
    let adjacency_file = data_dir.join("temporal_adjacency").join("adjacency.idx");

    // Read file
    let bytes = std::fs::read(&adjacency_file)?;

    // Deserialize
    let data: TemporalAdjacencyData = bitcode::decode(&bytes).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Failed to deserialize temporal adjacency index: {}",
            e
        ))
    })?;

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

/// Extract index data for persistence.
fn extract_index_data(
    index: &TemporalAdjacencyIndex,
) -> (Vec<NodeAdjacencyEntry>, Vec<NodeAdjacencyEntry>) {
    let mut outgoing_entries = Vec::new();
    let mut incoming_entries = Vec::new();

    // Extract outgoing edges
    for item in index.outgoing.iter() {
        let node_id = item.key().as_u64();
        let entries: Vec<PersistedTemporalAdjacencyEntry> =
            item.value().iter().map(convert_to_persisted).collect();

        outgoing_entries.push(NodeAdjacencyEntry { node_id, entries });
    }

    // Extract incoming edges
    for item in index.incoming.iter() {
        let node_id = item.key().as_u64();
        let entries: Vec<PersistedTemporalAdjacencyEntry> =
            item.value().iter().map(convert_to_persisted).collect();

        incoming_entries.push(NodeAdjacencyEntry { node_id, entries });
    }

    (outgoing_entries, incoming_entries)
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
