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
use crate::encryption::cipher::Cipher;

/// Save temporal adjacency index to disk.
///
/// Creates `temporal_adjacency/adjacency.idx` with all index entries.
/// Only the outgoing edges are extracted and serialized to disk to save space,
/// since the incoming index can be efficiently reconstructed from the outgoing data.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or if writing the file fails.
pub fn save_temporal_adjacency_index(
    index: &TemporalAdjacencyIndex,
    data_dir: &Path,
) -> Result<()> {
    save_temporal_adjacency_index_with_cipher(index, data_dir, None)
}

/// Save the temporal adjacency index, optionally encrypting it at rest
/// (Issue #481). With `cipher == None` this is identical to
/// [`save_temporal_adjacency_index`].
///
/// # Errors
///
/// Returns an error if the directory cannot be created, serialization or
/// encryption fails, or writing the file fails.
pub fn save_temporal_adjacency_index_with_cipher(
    index: &TemporalAdjacencyIndex,
    data_dir: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
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

    // Serialize with bitcode (this format carries no trailing CRC; the AEAD
    // tag provides integrity when encrypted, and the magic/version guard the
    // plaintext path as before).
    let bytes = bitcode::encode(&data);

    // Write atomically, encrypting the whole buffer when a cipher is present.
    let adjacency_file = adjacency_dir.join("adjacency.idx");
    match cipher {
        Some(cipher) => {
            let encrypted = super::common::encrypt_index_bytes(&bytes, cipher)?;
            super::atomic_write(&adjacency_file, &encrypted)?;
        }
        None => super::atomic_write(&adjacency_file, &bytes)?,
    }

    Ok(())
}

/// Maximum file size for temporal adjacency index (10 MB)
///
/// With 64 bytes per entry, this allows ~163K entries total.
/// At 1M entries/node limit and assuming moderate node connectivity,
/// this provides protection against DoS while allowing typical workloads.
const MAX_ADJACENCY_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Load temporal adjacency index from disk.
///
/// Reads `temporal_adjacency/adjacency.idx` and reconstructs the index.
/// The incoming index is automatically rebuilt from outgoing edges during reconstruction
/// by inserting edges symmetrically.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read or is missing.
/// - The file exceeds `MAX_ADJACENCY_FILE_SIZE` (DoS protection).
/// - The data cannot be deserialized.
/// - The manifest version or magic bytes do not match.
pub fn load_temporal_adjacency_index(data_dir: &Path) -> Result<Arc<TemporalAdjacencyIndex>> {
    load_temporal_adjacency_index_with_cipher(data_dir, None)
}

/// Load the temporal adjacency index, transparently decrypting it if written
/// encrypted (Issue #481). A legacy plaintext file is loaded even when a
/// cipher is supplied (header sniffing).
///
/// # Errors
///
/// Same as [`load_temporal_adjacency_index`], plus a structured error if the
/// file is encrypted but no cipher is supplied or decryption fails.
pub fn load_temporal_adjacency_index_with_cipher(
    data_dir: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<Arc<TemporalAdjacencyIndex>> {
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

    // Read file, decrypting transparently if it carries the encrypted header.
    let raw = std::fs::read(&adjacency_file)?;
    let bytes = if super::common::is_encrypted_index(&raw) {
        super::common::decrypt_index_bytes(&raw, &adjacency_file, cipher)?
    } else {
        raw
    };

    // Deserialize
    let data: TemporalAdjacencyData = bitcode::decode(&bytes).map_err(|e| {
        IndexPersistenceError::Serialization(format!(
            "Failed to deserialize temporal adjacency index: {}",
            e
        ))
    })?;

    // Validate version. Like the other index formats (graph, manifest,
    // strings, vector), this only rejects strictly-newer versions: the
    // on-disk shape of `TemporalAdjacencyData` has not changed since it was
    // introduced, so any older file sharing the crate-wide `MANIFEST_VERSION`
    // counter (e.g. bumped by an unrelated format's schema change, such as
    // Issue #3224's provenance addition to the temporal-index format) is
    // still byte-compatible and must keep loading correctly.
    if data.version > MANIFEST_VERSION {
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
    let mut outgoing_entries = Vec::with_capacity(index.outgoing.len());

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test: `TemporalAdjacencyData`'s on-disk shape has not
    /// changed since it was introduced, but it shares the crate-wide
    /// `MANIFEST_VERSION` counter with unrelated formats (e.g. the
    /// temporal-index format bumped by Issue #3224's provenance addition).
    /// A file written when `MANIFEST_VERSION` was 1 must still load
    /// correctly after an unrelated format bump takes the shared constant
    /// to 2 or higher, since this format's bytes are unaffected.
    #[test]
    fn load_accepts_older_manifest_version_with_unchanged_shape() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        let adjacency_dir = data_dir.join("temporal_adjacency");
        std::fs::create_dir_all(&adjacency_dir).unwrap();

        // Simulate a file written by a build where MANIFEST_VERSION was
        // still 1 (before some unrelated format's schema bump). The shape
        // of `TemporalAdjacencyData` itself is unchanged, so this must
        // still decode successfully.
        let data = TemporalAdjacencyData {
            magic: TEMPORAL_ADJACENCY_MAGIC,
            version: 1,
            outgoing: vec![],
        };
        let bytes = bitcode::encode(&data);
        std::fs::write(adjacency_dir.join("adjacency.idx"), &bytes).unwrap();

        let loaded = load_temporal_adjacency_index(&data_dir).unwrap();
        let t = crate::core::temporal::time::now();
        assert_eq!(
            loaded
                .get_outgoing_at_time(NodeId::new(1).unwrap(), t, t)
                .len(),
            0
        );
    }

    #[test]
    fn load_rejects_strictly_newer_manifest_version() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        let adjacency_dir = data_dir.join("temporal_adjacency");
        std::fs::create_dir_all(&adjacency_dir).unwrap();

        let data = TemporalAdjacencyData {
            magic: TEMPORAL_ADJACENCY_MAGIC,
            version: MANIFEST_VERSION + 1,
            outgoing: vec![],
        };
        let bytes = bitcode::encode(&data);
        std::fs::write(adjacency_dir.join("adjacency.idx"), &bytes).unwrap();

        match load_temporal_adjacency_index(&data_dir) {
            Err(IndexPersistenceError::Serialization(_)) => {}
            Err(other) => panic!("expected Serialization error, got {other:?}"),
            Ok(_) => panic!("expected an error, but load succeeded"),
        }
    }
}
