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
use super::strings::InternerRemap;
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

/// Save the temporal adjacency index, encrypting with the current generation of
/// an [`IndexKeyring`](super::common::IndexKeyring) (Issue #488 key rotation).
pub(crate) fn save_temporal_adjacency_index_with_keyring(
    index: &TemporalAdjacencyIndex,
    data_dir: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<()> {
    let adjacency_dir = data_dir.join("temporal_adjacency");
    std::fs::create_dir_all(&adjacency_dir)?;

    let outgoing = extract_outgoing_data(index);
    let data = TemporalAdjacencyData {
        magic: TEMPORAL_ADJACENCY_MAGIC,
        version: MANIFEST_VERSION,
        outgoing,
    };
    let bytes = bitcode::encode(&data);

    let adjacency_file = adjacency_dir.join("adjacency.idx");
    match keyring.and_then(super::common::IndexKeyring::current) {
        Some((cipher, key_version)) => {
            let encrypted =
                super::common::encrypt_index_bytes_versioned(&bytes, &cipher, key_version)?;
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
    load_temporal_adjacency_index_with_cipher_and_remap(data_dir, None, &InternerRemap::identity())
}

/// Load the temporal adjacency index, transparently decrypting it if written
/// encrypted (Issue #481). A legacy plaintext file is loaded even when a
/// cipher is supplied (header sniffing). Uses an identity interner remap.
///
/// # Errors
///
/// Same as [`load_temporal_adjacency_index`], plus a structured error if the
/// file is encrypted but no cipher is supplied or decryption fails.
pub fn load_temporal_adjacency_index_with_cipher(
    data_dir: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<Arc<TemporalAdjacencyIndex>> {
    load_temporal_adjacency_index_with_cipher_and_remap(
        data_dir,
        cipher,
        &InternerRemap::identity(),
    )
}

/// Load the temporal adjacency index, translating each persisted edge-type
/// label from **file-space** interner ids to **live** ids via `remap`
/// (Issue #3490) before reconstruction. Uses no cipher (plaintext).
///
/// This is the startup-path entry point: `load_indexes_startup` restores the
/// interner (obtaining the remap) before the persisted adjacency labels are
/// resolved, and those labels — like graph/temporal labels — are file-space ids
/// that must be routed through the remap or they silently resolve to the wrong
/// edge type (corrupting #3225 AS-OF edge-type filtering). An
/// [`InternerRemap::identity`] remap leaves the labels unchanged, reproducing
/// the plain [`load_temporal_adjacency_index`] behavior.
pub fn load_temporal_adjacency_index_with_remap(
    data_dir: &Path,
    remap: &InternerRemap,
) -> Result<Arc<TemporalAdjacencyIndex>> {
    load_temporal_adjacency_index_with_cipher_and_remap(data_dir, None, remap)
}

/// Load the temporal adjacency index, transparently decrypting it with `cipher`
/// if written encrypted (Issue #481) AND translating each persisted edge-type
/// label from file-space interner ids to live ids via `remap` (Issue #3490)
/// before reconstruction.
///
/// This is the combined startup entry point that reconciles encryption-at-rest
/// with the interner remap; the other `load_temporal_adjacency_index*` variants
/// delegate here with `None` / [`InternerRemap::identity`] defaults.
///
/// # Errors
///
/// Same as [`load_temporal_adjacency_index`], plus a structured error if the
/// file is encrypted but no cipher is supplied or decryption fails.
pub fn load_temporal_adjacency_index_with_cipher_and_remap(
    data_dir: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
    remap: &InternerRemap,
) -> Result<Arc<TemporalAdjacencyIndex>> {
    let ring = cipher.map(|c| super::common::IndexKeyring::single(c.clone()));
    load_temporal_adjacency_index_with_keyring_and_remap(data_dir, ring.as_ref(), remap)
}

/// Load the temporal adjacency index, decrypting via an
/// [`IndexKeyring`](super::common::IndexKeyring) that dispatches on the header
/// `key_version` (Issue #488 key rotation) and applying the interner `remap`.
pub(crate) fn load_temporal_adjacency_index_with_keyring_and_remap(
    data_dir: &Path,
    keyring: Option<&super::common::IndexKeyring>,
    remap: &InternerRemap,
) -> Result<Arc<TemporalAdjacencyIndex>> {
    let mut data = load_temporal_adjacency_data_with_keyring(data_dir, keyring)?;
    remap.remap_temporal_adjacency_data(&mut data);
    Ok(Arc::new(reconstruct_index(data)?))
}

/// Read, size-check, decrypt (Issue #481), deserialize, and validate the
/// on-disk temporal adjacency blob into [`TemporalAdjacencyData`] (no
/// interner-id translation). Passing `cipher == None` reads a plaintext file;
/// a legacy plaintext file is read even when a cipher is supplied (header
/// sniffing).
fn load_temporal_adjacency_data_with_keyring(
    data_dir: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<TemporalAdjacencyData> {
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
        super::common::decrypt_index_bytes_with_keyring(&raw, &adjacency_file, keyring)?
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

    Ok(data)
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

    // Count entries whose edge-type label does not resolve to a live interned
    // string, so an unmappable label (the Issue #3490 UNMAPPABLE_FILE_ID
    // sentinel written by the remap, or any other on-disk corruption) is
    // skipped LOUDLY rather than stored as a garbage edge type that would
    // silently break #3225 AS-OF edge-type filtering. Mirrors the graph
    // restore path's label validation in `load_indexes_startup`.
    let mut skipped_unresolvable_labels = 0usize;

    // Reconstruct outgoing edges
    for node_entry in data.outgoing {
        let node_id = NodeId::new(node_entry.node_id)
            .map_err(|e| IndexPersistenceError::Serialization(format!("Invalid node ID: {}", e)))?;

        for entry in node_entry.entries {
            let persisted_entry = convert_from_persisted(entry)?;

            // Validate the edge-type label resolves against the live interner.
            // `from_raw` never fails, so a bad/unmappable id would otherwise be
            // stored verbatim and later resolve to the wrong (or no) string.
            if crate::core::GLOBAL_INTERNER
                .resolve_with(persisted_entry.label, |_| ())
                .is_none()
            {
                skipped_unresolvable_labels += 1;
                continue;
            }

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

    if skipped_unresolvable_labels > 0 {
        eprintln!(
            "Warning: Skipped {} temporal adjacency entr{} whose edge-type label \
             could not be resolved in the string interner (unmappable/corrupt id); \
             these edges are omitted from the temporal adjacency index",
            skipped_unresolvable_labels,
            if skipped_unresolvable_labels == 1 {
                "y"
            } else {
                "ies"
            }
        );
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

    // ── Encryption-at-rest tests (Issue #481) ────────────────────────

    fn enc_test_cipher(seed: u8) -> Arc<dyn Cipher> {
        use crate::encryption::Aes256GcmCipher;
        use zeroize::Zeroizing;
        let mut key = Zeroizing::new([0u8; 32]);
        key[0] = seed;
        key[9] = seed.wrapping_add(3);
        Arc::new(Aes256GcmCipher::new(&key))
    }

    /// Build a single-edge index whose edge-type label is interned so it
    /// survives the load-time label-resolution guard.
    fn index_with_one_edge(src: NodeId) -> TemporalAdjacencyIndex {
        use crate::core::temporal::time;
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let label = crate::core::GLOBAL_INTERNER.intern("KNOWS").unwrap();
        index
            .insert_edge(
                EdgeId::new(100).unwrap(),
                src,
                NodeId::new(2).unwrap(),
                label,
                time::from_secs(10),
                time::from_secs(20),
                time::from_secs(10),
                crate::core::TIMESTAMP_MAX,
            )
            .unwrap();
        index
    }

    #[test]
    fn encrypted_round_trip_preserves_edges() {
        use crate::core::temporal::time;
        let temp_dir = tempfile::TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        let cipher = enc_test_cipher(0x11);
        let src = NodeId::new(1).unwrap();

        let index = index_with_one_edge(src);
        save_temporal_adjacency_index_with_cipher(&index, &data_dir, Some(&cipher)).unwrap();

        // The persisted file carries the AEIX encrypted-index header on disk.
        let raw = std::fs::read(data_dir.join("temporal_adjacency").join("adjacency.idx")).unwrap();
        assert!(
            super::super::common::is_encrypted_index(&raw),
            "expected AEIX header on disk"
        );

        // Decrypts with the right key and the edge is recovered intact.
        let loaded = load_temporal_adjacency_index_with_cipher(&data_dir, Some(&cipher)).unwrap();
        let out = loaded.get_outgoing_at_time(src, time::from_secs(15), time::now());
        assert_eq!(out, vec![EdgeId::new(100).unwrap()]);

        // Empty valid-time window before the edge existed => nothing.
        assert!(
            loaded
                .get_outgoing_at_time(src, time::from_secs(5), time::now())
                .is_empty()
        );
    }

    #[test]
    fn encrypted_file_fails_closed_without_cipher() {
        // Header says encrypted but no key is configured => structured error,
        // never a fallback that reads ciphertext as a bitcode blob.
        let temp_dir = tempfile::TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        let cipher = enc_test_cipher(0x22);
        let src = NodeId::new(1).unwrap();

        let index = index_with_one_edge(src);
        save_temporal_adjacency_index_with_cipher(&index, &data_dir, Some(&cipher)).unwrap();

        match load_temporal_adjacency_index_with_cipher(&data_dir, None) {
            Err(IndexPersistenceError::Corrupted { .. }) => {}
            Err(other) => panic!("expected Corrupted fail-closed error, got {other:?}"),
            Ok(_) => panic!("expected fail-closed error, but load succeeded"),
        }
    }

    #[test]
    fn encrypted_wrong_key_fails_closed() {
        // A different key must never decrypt to the original data.
        let temp_dir = tempfile::TempDir::new().unwrap();
        let data_dir = temp_dir.path().to_path_buf();
        let cipher = enc_test_cipher(0x33);
        let wrong = enc_test_cipher(0x44);
        let src = NodeId::new(1).unwrap();

        let index = index_with_one_edge(src);
        save_temporal_adjacency_index_with_cipher(&index, &data_dir, Some(&cipher)).unwrap();

        match load_temporal_adjacency_index_with_cipher(&data_dir, Some(&wrong)) {
            Err(IndexPersistenceError::Corrupted { .. }) => {}
            Err(other) => panic!("expected Corrupted wrong-key error, got {other:?}"),
            Ok(_) => panic!("expected wrong-key error, but load succeeded"),
        }
    }
}
