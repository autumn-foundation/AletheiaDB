//! Graph index persistence.

#[cfg(not(target_arch = "wasm32"))]
use std::fs;
use std::path::Path;
use std::sync::Arc;

use crc32fast::Hasher;

use crate::core::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};

#[cfg(not(target_arch = "wasm32"))]
use crate::storage::compression::decompress_with_limit;

#[cfg(not(target_arch = "wasm32"))]
use super::DELTA_MAGIC;
use super::error::{IndexPersistenceError, Result};
use super::formats::{GraphIndexData, PersistedPropertyMap, PersistedPropertyValue};
#[cfg(not(target_arch = "wasm32"))]
use super::formats::{GraphIndexDelta, PersistedEdge, PersistedNode};
use super::{GRAPH_MAGIC, MANIFEST_VERSION};
use crate::encryption::cipher::Cipher;

/// Write a fully-built plaintext graph-file buffer to disk, encrypting the
/// whole buffer with the encrypted-index header when a cipher is supplied
/// (Issue #481). The plaintext buffer (`[bitcode|zstd][crc32]`) is identical
/// whether or not it is subsequently encrypted, so the existing zstd-detect +
/// CRC + decode read logic runs unchanged after decryption.
fn write_graph_buffer_maybe_encrypted(
    path: &Path,
    plaintext: &[u8],
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<()> {
    match cipher {
        Some(cipher) => {
            let encrypted = super::common::encrypt_index_bytes(plaintext, cipher)?;
            super::atomic_write(path, &encrypted)
        }
        None => super::atomic_write(path, plaintext),
    }
}

/// Write a graph-file buffer, encrypting with the current generation of an
/// [`IndexKeyring`](super::common::IndexKeyring) and stamping its `key_version`
/// (Issue #488 key rotation). A `None` keyring writes plaintext.
#[cfg(not(target_arch = "wasm32"))]
fn write_graph_buffer_with_keyring(
    path: &Path,
    plaintext: &[u8],
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<()> {
    match keyring.and_then(super::common::IndexKeyring::current) {
        Some((cipher, key_version)) => {
            let encrypted =
                super::common::encrypt_index_bytes_versioned(plaintext, &cipher, key_version)?;
            super::atomic_write(path, &encrypted)
        }
        None => super::atomic_write(path, plaintext),
    }
}

/// Map decompression errors to `IndexPersistenceError`, preserving the
/// specific `SizeLimitExceeded` variant for capacity violations.
#[cfg(not(target_arch = "wasm32"))]
fn map_decompress_error(e: crate::core::error::Error) -> IndexPersistenceError {
    match e {
        crate::core::error::Error::Storage(
            crate::core::error::StorageError::CapacityExceeded { current, limit, .. },
        ) => IndexPersistenceError::SizeLimitExceeded {
            message: format!(
                "Decompressed size {} exceeds limit {} (possible zip bomb)",
                current, limit
            ),
        },
        _ => IndexPersistenceError::Serialization(format!("zstd decompression failed: {}", e)),
    }
}

/// Convert PropertyValue to PersistedPropertyValue.
///
/// # Errors
///
/// Returns an error if:
/// - The property contains an Array (not yet supported)
/// - String interning fails (interner out of capacity)
pub fn persist_property_value(value: &PropertyValue) -> Result<PersistedPropertyValue> {
    Ok(match value {
        PropertyValue::Null => PersistedPropertyValue::Null,
        PropertyValue::Bool(b) => PersistedPropertyValue::Bool(*b),
        PropertyValue::Int(i) => PersistedPropertyValue::Int(*i),
        PropertyValue::Float(f) => PersistedPropertyValue::Float(*f),
        PropertyValue::String(s) => {
            let interned = GLOBAL_INTERNER.intern(s.as_ref()).map_err(|e| {
                // Preserve a STRUCTURED interner-capacity signal (configurable
                // interner cap) so the background-persist worker suspends the
                // affected index deterministically instead of scraping Display
                // text. A capacity exhaustion here is exactly the "egregore hang"
                // trigger: high-cardinality property VALUES interned lazily at
                // persist time. Any other intern error keeps the generic wrap.
                match e {
                    crate::core::error::Error::Storage(
                        crate::core::error::StorageError::CapacityExceeded {
                            current, limit, ..
                        },
                    ) => IndexPersistenceError::InternerCapacityExceeded { current, limit },
                    other => IndexPersistenceError::Serialization(format!(
                        "Failed to intern string: {}",
                        other
                    )),
                }
            })?;
            PersistedPropertyValue::String(interned.as_u32())
        }
        PropertyValue::Bytes(b) => PersistedPropertyValue::Bytes(b.to_vec()),
        PropertyValue::Vector(v) => PersistedPropertyValue::Vector(v.to_vec()),
        // Array variant exists but is not yet supported in serialization
        PropertyValue::Array(_) => {
            return Err(IndexPersistenceError::Serialization(
                "Array properties are not yet supported for persistence. \
                 This prevents silent data loss. Support will be added in a future update."
                    .to_string(),
            ));
        }
        // SparseVector variant exists but is not yet supported in index persistence
        PropertyValue::SparseVector(_) => {
            return Err(IndexPersistenceError::Serialization(
                "SparseVector properties are not yet supported for index persistence. \
                 This prevents silent data loss. Support will be added in a future update."
                    .to_string(),
            ));
        }
    })
}

/// Convert PersistedPropertyValue back to PropertyValue.
///
/// # Errors
///
/// Returns an error if:
/// - An interned string ID cannot be resolved (data corruption)
/// - Vector dimensions exceed MAX_VECTOR_DIMENSIONS (DoS protection)
pub fn restore_property_value(persisted: &PersistedPropertyValue) -> Result<PropertyValue> {
    Ok(match persisted {
        PersistedPropertyValue::Null => PropertyValue::Null,
        PersistedPropertyValue::Bool(b) => PropertyValue::Bool(*b),
        PersistedPropertyValue::Int(i) => PropertyValue::Int(*i),
        PersistedPropertyValue::Float(f) => PropertyValue::Float(*f),
        PersistedPropertyValue::String(idx) => {
            // Note: Use resolve() instead of resolve_with() here because PropertyValue::String
            // requires an owned Arc<str>. Cloning the existing Arc is more efficient
            // than allocating a new one from a &str.
            #[allow(deprecated)]
            let s = GLOBAL_INTERNER
                .resolve(crate::core::InternedString::from_raw(*idx))
                .ok_or_else(|| {
                    IndexPersistenceError::Serialization(format!(
                        "Failed to resolve interned string with ID: {}. \
                         This likely indicates data corruption.",
                        idx
                    ))
                })?;
            PropertyValue::String(s)
        }
        PersistedPropertyValue::Bytes(b) => PropertyValue::Bytes(Arc::from(b.as_slice())),
        PersistedPropertyValue::Vector(v) => {
            // Validate vector size to prevent DoS via maliciously large vectors
            if v.len() > super::MAX_VECTOR_DIMENSIONS {
                return Err(IndexPersistenceError::SizeLimitExceeded {
                    message: format!(
                        "Vector dimension {} exceeds maximum allowed dimension {}",
                        v.len(),
                        super::MAX_VECTOR_DIMENSIONS
                    ),
                });
            }
            PropertyValue::Vector(Arc::from(v.as_slice()))
        }
    })
}

/// Convert PropertyMap to PersistedPropertyMap.
///
/// # Errors
///
/// Returns an error if any property value fails to persist.
pub fn persist_property_map(props: &PropertyMap) -> Result<PersistedPropertyMap> {
    let mut entries = Vec::with_capacity(props.len());
    for (k, v) in props.iter() {
        // k is already an InternedString (PropertyKey)
        entries.push((k.as_u32(), persist_property_value(v)?));
    }
    Ok(PersistedPropertyMap { entries })
}

/// Convert PersistedPropertyMap back to PropertyMap.
///
/// # Errors
///
/// Returns an error if:
/// - Any property key cannot be resolved (data corruption)
/// - Any property value fails to restore
pub fn restore_property_map(persisted: &PersistedPropertyMap) -> Result<PropertyMap> {
    let mut builder = PropertyMapBuilder::new();
    for (key_idx, value) in &persisted.entries {
        let key_id = crate::core::InternedString::from_raw(*key_idx);
        let val = restore_property_value(value)?;
        // Resolve the key to an owned Arc<str> BEFORE inserting it into the
        // builder. `resolve_with` runs its closure while holding a DashMap shard
        // read-guard on the interner; calling `builder.insert` (which re-interns
        // the key) inside that closure re-enters the interner on the same shard
        // and can self-deadlock under concurrent writers (shard reader/writer
        // starvation — a restore racing another thread's fresh intern). Cloning
        // the Arc via `resolve` releases the guard before the re-intern.
        #[allow(deprecated)]
        let key_str = GLOBAL_INTERNER.resolve(key_id).ok_or_else(|| {
            IndexPersistenceError::Serialization(format!(
                "Failed to resolve interned property key with ID: {}. \
                 This likely indicates data corruption.",
                key_idx
            ))
        })?;
        builder = builder.insert(key_str.as_ref(), val);
    }
    Ok(builder.build())
}

/// Extract `GraphIndexData` from a `CurrentStorageSnapshot`.
///
/// Shared by the checkpoint and backup code paths so the conversion logic
/// lives in one place.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn extract_graph_data_from_snapshot(
    snapshot: &crate::storage::snapshot::CurrentStorageSnapshot,
) -> Result<GraphIndexData> {
    let mut nodes = Vec::with_capacity(snapshot.node_count());
    let mut edges = Vec::with_capacity(snapshot.edge_count());

    for node in snapshot.iter_nodes() {
        nodes.push(PersistedNode {
            id: node.id.as_u64(),
            label_idx: node.label.as_u32(),
            version_id: node.current_version.as_u64(),
            properties: persist_property_map(&node.properties)?,
        });
    }

    for edge in snapshot.iter_edges() {
        edges.push(PersistedEdge {
            id: edge.id.as_u64(),
            source_id: edge.source.as_u64(),
            target_id: edge.target.as_u64(),
            label_idx: edge.label.as_u32(),
            version_id: edge.current_version.as_u64(),
            properties: persist_property_map(&edge.properties)?,
        });
    }

    Ok(GraphIndexData {
        magic: GRAPH_MAGIC,
        version: MANIFEST_VERSION,
        node_count: nodes.len() as u64,
        edge_count: edges.len() as u64,
        nodes,
        edges,
        outgoing_node_ids: Vec::new(),
        outgoing_offsets: Vec::new(),
        outgoing_neighbors: Vec::new(),
        incoming_node_ids: Vec::new(),
        incoming_offsets: Vec::new(),
        incoming_neighbors: Vec::new(),
    })
}

/// Save graph index data to disk with CRC32 checksum using atomic write.
///
/// Format: `[bitcode_data][crc32_checksum_4_bytes]`
///
/// Uses write-temp-then-rename to prevent corruption on crash.
pub fn save_graph_index(data: &GraphIndexData, path: &Path) -> Result<()> {
    save_graph_index_with_cipher(data, path, None)
}

/// Build the uncompressed plaintext graph-file buffer: `[bitcode][crc32]`.
fn build_graph_plaintext(data: &GraphIndexData) -> Vec<u8> {
    let encoded = bitcode::encode(data);

    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    let mut buffer = encoded;
    buffer.extend_from_slice(&checksum.to_le_bytes());
    buffer
}

/// Save graph index data (uncompressed), optionally encrypting it at rest
/// (Issue #481). With `cipher == None` this is identical to
/// [`save_graph_index`].
///
/// # Errors
///
/// Returns an error if serialization, encryption, or file I/O fails.
pub fn save_graph_index_with_cipher(
    data: &GraphIndexData,
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<()> {
    let plaintext = build_graph_plaintext(data);
    write_graph_buffer_maybe_encrypted(path, &plaintext, cipher)
}

/// Save graph index data (uncompressed), encrypting with an
/// [`IndexKeyring`](super::common::IndexKeyring) (Issue #488 key rotation).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_graph_index_with_keyring(
    data: &GraphIndexData,
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<()> {
    let plaintext = build_graph_plaintext(data);
    write_graph_buffer_with_keyring(path, &plaintext, keyring)
}

/// Load graph index data from disk and validate CRC32 checksum.
///
/// Automatically detects zstd compression by checking for magic bytes.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_graph_index(path: &Path) -> Result<GraphIndexData> {
    load_graph_index_with_cipher(path, None)
}

/// Decode a plaintext graph-file buffer (`[bitcode|zstd][crc32]`): auto-detect
/// zstd compression, verify the CRC32, decode, and validate magic/version.
///
/// Shared by the buffered and memory-mapped read paths so the exact
/// zstd-detect + CRC + decode logic is applied identically to a plaintext file
/// and to the decrypted contents of an encrypted one.
#[cfg(not(target_arch = "wasm32"))]
fn decode_graph_bytes(buffer: &[u8], path: &Path) -> Result<GraphIndexData> {
    // Check minimum size (must have at least 4 bytes for CRC)
    if buffer.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "File too small to contain CRC32 checksum".into(),
        });
    }

    // Split data and checksum
    let (data_slice, checksum_bytes) = buffer.split_at(buffer.len() - 4);
    let stored_checksum = u32::from_le_bytes(checksum_bytes.try_into().map_err(|_| {
        IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "Invalid CRC32 checksum format".into(),
        }
    })?);

    // Check for zstd compression (magic bytes: 0x28B52FFD in big-endian)
    const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
    let decompressed_data;
    let data_to_verify = if data_slice.len() >= 4 && data_slice[..4] == ZSTD_MAGIC {
        decompressed_data = decompress_with_limit(data_slice, super::MAX_GRAPH_DECOMPRESSED_SIZE)
            .map_err(map_decompress_error)?;
        &decompressed_data[..]
    } else {
        data_slice
    };

    // Verify checksum (against decompressed data if compressed)
    let mut hasher = Hasher::new();
    hasher.update(data_to_verify);
    let computed_checksum = hasher.finalize();

    if computed_checksum != stored_checksum {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: format!(
                "CRC32 checksum mismatch: expected {}, got {}",
                stored_checksum, computed_checksum
            )
            .into(),
        });
    }

    // Decode and validate
    let data: GraphIndexData = bitcode::decode(data_to_verify)?;

    if data.magic != GRAPH_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: GRAPH_MAGIC,
            got: data.magic,
        });
    }

    if data.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: data.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(data)
}

/// Load graph index data, transparently decrypting it if written encrypted
/// (Issue #481). A legacy plaintext graph index is loaded even when a cipher
/// is supplied (header sniffing); zstd auto-detection then runs on the
/// decrypted plaintext.
///
/// # Errors
///
/// Same as [`load_graph_index`], plus a structured error if the file is
/// encrypted but no cipher is supplied or decryption fails.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_graph_index_with_cipher(
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<GraphIndexData> {
    let buffer = super::common::read_index_file(
        path,
        super::MAX_GRAPH_INDEX_FILE_SIZE,
        "Graph index",
        cipher,
    )?;
    decode_graph_bytes(&buffer, path)
}

/// Load graph index data, decrypting via an
/// [`IndexKeyring`](super::common::IndexKeyring) that dispatches on the header
/// `key_version` (Issue #488 key rotation).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_graph_index_with_keyring(
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<GraphIndexData> {
    let buffer = super::common::read_index_file_with_keyring(
        path,
        super::MAX_GRAPH_INDEX_FILE_SIZE,
        "Graph index",
        keyring,
    )?;
    decode_graph_bytes(&buffer, path)
}

/// Create a new empty GraphIndexData.
pub fn new_graph_index_data() -> GraphIndexData {
    GraphIndexData {
        magic: GRAPH_MAGIC,
        version: MANIFEST_VERSION,
        node_count: 0,
        edge_count: 0,
        nodes: Vec::new(),
        edges: Vec::new(),
        outgoing_node_ids: Vec::new(),
        outgoing_offsets: Vec::new(),
        outgoing_neighbors: Vec::new(),
        incoming_node_ids: Vec::new(),
        incoming_offsets: Vec::new(),
        incoming_neighbors: Vec::new(),
    }
}

/// Save graph index data with zstd compression.
///
/// # Arguments
///
/// * `data` - The graph index data to save
/// * `path` - The file path to write to
/// * `compression_level` - zstd compression level (0-22, default 3)
///
/// # Errors
///
/// Returns an error if serialization, compression, or file write fails.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_graph_index_compressed(
    data: &GraphIndexData,
    path: &Path,
    compression_level: i32,
) -> Result<()> {
    save_graph_index_compressed_with_cipher(data, path, compression_level, None)
}

/// Build the compressed plaintext graph-file buffer: `[zstd][crc32]` where the
/// CRC32 is computed over the *uncompressed* bitcode (matching the read path).
#[cfg(not(target_arch = "wasm32"))]
fn build_graph_plaintext_compressed(
    data: &GraphIndexData,
    compression_level: i32,
) -> Result<Vec<u8>> {
    let encoded = bitcode::encode(data);

    // Calculate CRC32 of uncompressed data
    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    // Compress the encoded data
    let compressed = zstd::encode_all(&encoded[..], compression_level).map_err(|e| {
        IndexPersistenceError::Serialization(format!("zstd compression failed: {}", e))
    })?;

    // buffer: compressed_data + checksum (of uncompressed)
    let mut buffer = compressed;
    buffer.extend_from_slice(&checksum.to_le_bytes());
    Ok(buffer)
}

/// Save graph index data with zstd compression, optionally encrypting the
/// whole compressed buffer at rest (Issue #481). Encryption is applied
/// **after** compression, so the read path decrypts first and then runs its
/// existing zstd auto-detection. With `cipher == None` this is identical to
/// [`save_graph_index_compressed`].
///
/// # Errors
///
/// Returns an error if serialization, compression, encryption, or file write
/// fails.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_graph_index_compressed_with_cipher(
    data: &GraphIndexData,
    path: &Path,
    compression_level: i32,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<()> {
    let plaintext = build_graph_plaintext_compressed(data, compression_level)?;
    write_graph_buffer_maybe_encrypted(path, &plaintext, cipher)
}

/// Save graph index data (zstd-compressed), encrypting with an
/// [`IndexKeyring`](super::common::IndexKeyring) (Issue #488 key rotation).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_graph_index_compressed_with_keyring(
    data: &GraphIndexData,
    path: &Path,
    compression_level: i32,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<()> {
    let plaintext = build_graph_plaintext_compressed(data, compression_level)?;
    write_graph_buffer_with_keyring(path, &plaintext, keyring)
}

/// Load graph index data using memory-mapped file for efficient large file handling.
///
/// This function uses memory-mapping to avoid loading the entire file into memory,
/// which can be more efficient for large index files and enables working with
/// indexes larger than available RAM.
///
/// # Arguments
///
/// * `path` - The file path to read from
///
/// # Errors
///
/// Returns an error if:
/// - File cannot be opened or memory-mapped
/// - File is corrupted (CRC mismatch)
/// - Decompression fails
/// - Deserialization fails
/// - Magic bytes or version are invalid
///
/// # Examples
///
/// ```ignore
/// use aletheiadb::storage::index_persistence::graph::load_graph_index_mmap;
///
/// let data = load_graph_index_mmap(&path)?;
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub fn load_graph_index_mmap(path: &Path) -> Result<GraphIndexData> {
    load_graph_index_mmap_with_cipher(path, None)
}

/// Memory-mapped graph index load, transparently handling encryption
/// (Issue #481).
///
/// An encrypted file (identified by its plaintext header) **cannot** be
/// memory-mapped as a live struct — the on-disk bytes are ciphertext — so this
/// falls back to a buffered read + decrypt + decode via
/// [`load_graph_index_with_cipher`]. A legacy plaintext file takes the true
/// mmap path unchanged.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_graph_index_mmap_with_cipher(
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<GraphIndexData> {
    use std::io::Read;

    // Peek the first bytes to detect the encrypted-index header.
    let mut header = [0u8; super::common::ENC_HEADER_LEN];
    let read_len = {
        let mut file = std::fs::File::open(path)?;
        // A short read (small file) simply won't match the magic; that's fine.
        let mut filled = 0usize;
        loop {
            match file.read(&mut header[filled..]) {
                Ok(0) => break,
                Ok(n) => {
                    filled += n;
                    if filled == header.len() {
                        break;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        filled
    };

    if super::common::is_encrypted_index(&header[..read_len]) {
        // Encrypted: buffered decrypt (mmap of ciphertext would be meaningless).
        return load_graph_index_with_cipher(path, cipher);
    }

    load_graph_index_mmap_plaintext(path)
}

/// The original plaintext memory-mapped load path.
#[cfg(not(target_arch = "wasm32"))]
fn load_graph_index_mmap_plaintext(path: &Path) -> Result<GraphIndexData> {
    use memmap2::Mmap;
    use std::fs::File;

    // Open file and create memory map
    let file = File::open(path)?;

    // Sanity check for extremely large files
    let metadata = file.metadata()?;
    if metadata.len() > super::MAX_MMAP_FILE_SIZE {
        return Err(IndexPersistenceError::SizeLimitExceeded {
            message: format!(
                "Graph index file size {} exceeds sanity limit {}",
                metadata.len(),
                super::MAX_MMAP_FILE_SIZE
            ),
        });
    }

    let mmap = unsafe { Mmap::map(&file)? };

    // Check minimum size (must have at least 4 bytes for CRC)
    if mmap.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "File too small to contain CRC32 checksum".into(),
        });
    }

    // Split data and checksum
    let (data_slice, checksum_bytes) = mmap.split_at(mmap.len() - 4);
    let stored_checksum = u32::from_le_bytes(checksum_bytes.try_into().map_err(|_| {
        IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "Invalid CRC32 checksum format".into(),
        }
    })?);

    // Check for zstd compression (magic bytes: 0x28B52FFD in big-endian)
    const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
    let decompressed_data;
    let data_to_verify = if data_slice.len() >= 4 && data_slice[..4] == ZSTD_MAGIC {
        decompressed_data = decompress_with_limit(data_slice, super::MAX_GRAPH_DECOMPRESSED_SIZE)
            .map_err(map_decompress_error)?;
        &decompressed_data[..]
    } else {
        data_slice
    };

    // Verify checksum (against decompressed data if compressed)
    let mut hasher = Hasher::new();
    hasher.update(data_to_verify);
    let computed_checksum = hasher.finalize();

    if computed_checksum != stored_checksum {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: format!(
                "CRC32 checksum mismatch: expected {}, got {}",
                stored_checksum, computed_checksum
            )
            .into(),
        });
    }

    // Decode and validate
    let data: GraphIndexData = bitcode::decode(data_to_verify)?;

    if data.magic != GRAPH_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: GRAPH_MAGIC,
            got: data.magic,
        });
    }

    if data.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: data.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(data)
}

/// Save graph index delta with zstd compression.
///
/// This function saves only the changes between the base and modified graph data,
/// significantly reducing the size of incremental saves. Tracks additions, modifications,
/// and deletions for complete change tracking.
///
/// # Arguments
///
/// * `base` - The base graph index data (previous snapshot)
/// * `modified` - The modified graph index data (current state)
/// * `path` - The file path to write the delta to
/// * `compression_level` - zstd compression level (0-22, default 3)
///
/// # Errors
///
/// Returns an error if serialization, compression, or file write fails.
///
/// # Examples
///
/// ```ignore
/// use aletheiadb::storage::index_persistence::graph::{
///     save_graph_index_delta, load_graph_index, save_graph_index_compressed
/// };
///
/// // Save base snapshot
/// save_graph_index_compressed(&base_data, &base_path, 3)?;
///
/// // ... make changes to create modified_data ...
///
/// // Save only the delta (additions, modifications, deletions)
/// save_graph_index_delta(&base_data, &modified_data, &delta_path, 3)?;
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub fn save_graph_index_delta(
    base: &GraphIndexData,
    modified: &GraphIndexData,
    path: &Path,
    compression_level: i32,
) -> Result<()> {
    // Build lookup maps for efficient comparison
    let base_nodes: std::collections::HashMap<u64, &super::formats::PersistedNode> =
        base.nodes.iter().map(|n| (n.id, n)).collect();
    let modified_nodes: std::collections::HashMap<u64, &super::formats::PersistedNode> =
        modified.nodes.iter().map(|n| (n.id, n)).collect();

    let base_edges: std::collections::HashMap<u64, &super::formats::PersistedEdge> =
        base.edges.iter().map(|e| (e.id, e)).collect();
    let modified_edges: std::collections::HashMap<u64, &super::formats::PersistedEdge> =
        modified.edges.iter().map(|e| (e.id, e)).collect();

    // Detect added nodes (exist in modified but not in base)
    let added_nodes: Vec<_> = modified
        .nodes
        .iter()
        .filter(|node| !base_nodes.contains_key(&node.id))
        .cloned()
        .collect();

    // Detect modified nodes (exist in both but with different content)
    let modified_nodes_vec: Vec<_> = modified
        .nodes
        .iter()
        .filter(|node| {
            base_nodes
                .get(&node.id)
                .is_some_and(|base_node| *base_node != *node)
        })
        .cloned()
        .collect();

    // Detect deleted nodes (exist in base but not in modified)
    let deleted_node_ids: Vec<_> = base
        .nodes
        .iter()
        .filter(|node| !modified_nodes.contains_key(&node.id))
        .map(|node| node.id)
        .collect();

    // Detect added edges (exist in modified but not in base)
    let added_edges: Vec<_> = modified
        .edges
        .iter()
        .filter(|edge| !base_edges.contains_key(&edge.id))
        .cloned()
        .collect();

    // Detect modified edges (exist in both but with different content)
    let modified_edges_vec: Vec<_> = modified
        .edges
        .iter()
        .filter(|edge| {
            base_edges
                .get(&edge.id)
                .is_some_and(|base_edge| *base_edge != *edge)
        })
        .cloned()
        .collect();

    // Detect deleted edges (exist in base but not in modified)
    let deleted_edge_ids: Vec<_> = base
        .edges
        .iter()
        .filter(|edge| !modified_edges.contains_key(&edge.id))
        .map(|edge| edge.id)
        .collect();

    // Create delta structure
    let delta = GraphIndexDelta {
        magic: DELTA_MAGIC,
        version: MANIFEST_VERSION,
        added_nodes,
        modified_nodes: modified_nodes_vec,
        deleted_node_ids,
        added_edges,
        modified_edges: modified_edges_vec,
        deleted_edge_ids,
        new_node_count: modified.node_count,
        new_edge_count: modified.edge_count,
    };

    // Encode delta
    let encoded = bitcode::encode(&delta);

    // Calculate CRC32 of uncompressed data
    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    // Compress the encoded data
    let compressed = zstd::encode_all(&encoded[..], compression_level).map_err(|e| {
        IndexPersistenceError::Serialization(format!("zstd compression failed: {}", e))
    })?;

    // Write: compressed_data + checksum (of uncompressed)
    let mut data_with_checksum = compressed;
    data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

    super::atomic_write(path, &data_with_checksum)?;
    Ok(())
}

/// Load graph index data by loading base and applying delta.
///
/// This function loads a base graph index and applies a delta to reconstruct
/// the modified state. Applies all change types: deletions, modifications, and additions.
/// This is more efficient than loading the full modified state when only a small
/// portion of the graph has changed.
///
/// # Delta Application Order
///
/// 1. **Deletions**: Remove nodes/edges that were deleted
/// 2. **Modifications**: Update properties/labels of existing nodes/edges
/// 3. **Additions**: Add new nodes/edges
/// 4. **Counts**: Update node_count and edge_count
///
/// # Arguments
///
/// * `base_path` - Path to the base graph index file
/// * `delta_path` - Path to the delta file
///
/// # Errors
///
/// Returns an error if:
/// - Base or delta file cannot be loaded
/// - Files are corrupted (CRC mismatch)
/// - Decompression fails
/// - Deserialization fails
/// - Magic bytes or version are invalid
///
/// # Examples
///
/// ```ignore
/// use aletheiadb::storage::index_persistence::graph::load_graph_index_with_delta;
///
/// let reconstructed_data = load_graph_index_with_delta(&base_path, &delta_path, None)?;
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub fn load_graph_index_with_delta(
    base_path: &Path,
    delta_path: &Path,
    limit: Option<usize>,
) -> Result<GraphIndexData> {
    // Load base index (this performs size check internally)
    let mut base = load_graph_index(base_path)?;

    // Check delta file size
    let metadata = fs::metadata(delta_path)?;
    if metadata.len() > super::MAX_GRAPH_INDEX_FILE_SIZE {
        return Err(IndexPersistenceError::SizeLimitExceeded {
            message: format!(
                "Graph index delta file size {} exceeds limit {}",
                metadata.len(),
                super::MAX_GRAPH_INDEX_FILE_SIZE
            ),
        });
    }

    // Load and decompress delta
    let bytes = fs::read(delta_path)?;

    // Check minimum size (must have at least 4 bytes for CRC)
    if bytes.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: delta_path.to_path_buf(),
            source: "File too small to contain CRC32 checksum".into(),
        });
    }

    // Split data and checksum
    let (data_slice, checksum_bytes) = bytes.split_at(bytes.len() - 4);
    let stored_checksum = u32::from_le_bytes(checksum_bytes.try_into().map_err(|_| {
        IndexPersistenceError::Corrupted {
            path: delta_path.to_path_buf(),
            source: "Invalid CRC32 checksum format".into(),
        }
    })?);

    // Check for zstd compression (magic bytes: 0x28B52FFD in big-endian)
    const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
    let decompressed_data;
    let data_to_verify = if data_slice.len() >= 4 && data_slice[..4] == ZSTD_MAGIC {
        decompressed_data = decompress_with_limit(data_slice, super::MAX_GRAPH_DECOMPRESSED_SIZE)
            .map_err(map_decompress_error)?;
        &decompressed_data[..]
    } else {
        data_slice
    };

    // Verify checksum (against decompressed data if compressed)
    let mut hasher = Hasher::new();
    hasher.update(data_to_verify);
    let computed_checksum = hasher.finalize();

    if computed_checksum != stored_checksum {
        return Err(IndexPersistenceError::Corrupted {
            path: delta_path.to_path_buf(),
            source: format!(
                "CRC32 checksum mismatch: expected {}, got {}",
                stored_checksum, computed_checksum
            )
            .into(),
        });
    }

    // Decode delta
    let delta: GraphIndexDelta = bitcode::decode(data_to_verify)?;

    // Validate magic and version
    if delta.magic != DELTA_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: delta_path.to_path_buf(),
            expected: DELTA_MAGIC,
            got: delta.magic,
        });
    }

    if delta.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: delta.version,
            supported: MANIFEST_VERSION,
        });
    }

    // Apply delta changes

    // 1. Handle deletions first (remove from base)
    // Use HashSets to turn O(M*N) Vec::contains() into O(N+M) HashSet::contains()
    // This significantly improves performance when thousands of nodes are deleted.
    let deleted_node_set: std::collections::HashSet<_> =
        delta.deleted_node_ids.into_iter().collect();
    let deleted_edge_set: std::collections::HashSet<_> =
        delta.deleted_edge_ids.into_iter().collect();

    base.nodes
        .retain(|node| !deleted_node_set.contains(&node.id));
    base.edges
        .retain(|edge| !deleted_edge_set.contains(&edge.id));

    // 2. Handle modifications (update existing entries)
    // Convert modifications to HashMaps to reduce O(M*N) lookups to O(N+M)
    let mut node_mods = std::collections::HashMap::new();
    for n in delta.modified_nodes {
        node_mods.insert(n.id, n);
    }

    let mut edge_mods = std::collections::HashMap::new();
    for e in delta.modified_edges {
        edge_mods.insert(e.id, e);
    }

    for existing in base.nodes.iter_mut() {
        if let Some(modified_node) = node_mods.remove(&existing.id) {
            *existing = modified_node;
        }
    }

    for existing in base.edges.iter_mut() {
        if let Some(modified_edge) = edge_mods.remove(&existing.id) {
            *existing = modified_edge;
        }
    }

    // 3. Handle additions (append to base)
    base.nodes.extend(delta.added_nodes);
    base.edges.extend(delta.added_edges);

    // Apply optional limit to prevent unbounded memory allocation
    if let Some(l) = limit {
        base.nodes.truncate(l);
        base.edges.truncate(l);
    }

    // 4. Update counts
    base.node_count = base.nodes.len() as u64;
    base.edge_count = base.edges.len() as u64;

    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index_persistence::formats::PersistedNode;
    use tempfile::tempdir;

    #[test]
    fn test_property_value_round_trip() {
        // Test various property types
        let values = vec![
            PropertyValue::Null,
            PropertyValue::Bool(true),
            PropertyValue::Int(42),
            PropertyValue::Float(2.71), // e approximation, not PI
            PropertyValue::String(Arc::from("test")),
            PropertyValue::Bytes(Arc::from(vec![1u8, 2, 3].as_slice())),
            PropertyValue::Vector(Arc::from(vec![1.0f32, 2.0, 3.0].as_slice())),
        ];

        for value in values {
            let persisted = persist_property_value(&value).unwrap();
            let restored = restore_property_value(&persisted).unwrap();

            // Compare string representation for simplicity
            assert_eq!(format!("{:?}", value), format!("{:?}", restored));
        }
    }

    #[test]
    fn test_graph_index_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");

        let mut data = new_graph_index_data();
        data.node_count = 2;
        data.nodes.push(PersistedNode {
            id: 1,
            label_idx: GLOBAL_INTERNER.intern("Person").unwrap().as_u32(),
            version_id: 1,
            properties: PersistedPropertyMap { entries: vec![] },
        });
        data.nodes.push(PersistedNode {
            id: 2,
            label_idx: GLOBAL_INTERNER.intern("Document").unwrap().as_u32(),
            version_id: 2,
            properties: PersistedPropertyMap { entries: vec![] },
        });

        save_graph_index(&data, &path).unwrap();
        let loaded = load_graph_index(&path).unwrap();

        assert_eq!(loaded.node_count, 2);
        assert_eq!(loaded.nodes.len(), 2);
    }

    // ── Encryption-at-rest tests (Issue #481) ────────────────────────

    fn enc_test_cipher() -> Arc<dyn Cipher> {
        use crate::encryption::Aes256GcmCipher;
        use zeroize::Zeroizing;
        let mut key = Zeroizing::new([0u8; 32]);
        key[0] = 0x5A;
        key[7] = 0xE1;
        Arc::new(Aes256GcmCipher::new(&key))
    }

    fn sample_graph() -> GraphIndexData {
        let mut data = new_graph_index_data();
        data.node_count = 1;
        data.nodes.push(PersistedNode {
            id: 7,
            label_idx: GLOBAL_INTERNER.intern("Person").unwrap().as_u32(),
            version_id: 3,
            properties: PersistedPropertyMap { entries: vec![] },
        });
        data
    }

    #[test]
    fn test_graph_index_encrypted_round_trip_plain() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");
        let cipher = enc_test_cipher();
        let data = sample_graph();

        save_graph_index_with_cipher(&data, &path, Some(&cipher)).unwrap();

        // On-disk file carries the encrypted-index header.
        let raw = fs::read(&path).unwrap();
        assert!(super::super::common::is_encrypted_index(&raw));

        let loaded = load_graph_index_with_cipher(&path, Some(&cipher)).unwrap();
        assert_eq!(loaded.nodes.len(), 1);
        assert_eq!(loaded.nodes[0].id, 7);

        // Without a cipher, loading fails closed (never reads ciphertext as data).
        assert!(load_graph_index_with_cipher(&path, None).is_err());
    }

    #[test]
    fn test_graph_index_encrypted_round_trip_zstd() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");
        let cipher = enc_test_cipher();
        let data = sample_graph();

        save_graph_index_compressed_with_cipher(&data, &path, 3, Some(&cipher)).unwrap();
        assert!(super::super::common::is_encrypted_index(
            &fs::read(&path).unwrap()
        ));

        let loaded = load_graph_index_with_cipher(&path, Some(&cipher)).unwrap();
        assert_eq!(loaded.nodes.len(), 1);
        assert_eq!(loaded.nodes[0].id, 7);
    }

    #[test]
    fn test_graph_index_mmap_falls_back_to_buffered_decrypt() {
        // use_mmap + encryption: the mmap loader must NOT map ciphertext; it
        // falls back to buffered decrypt and returns correct data.
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");
        let cipher = enc_test_cipher();
        let data = sample_graph();

        save_graph_index_with_cipher(&data, &path, Some(&cipher)).unwrap();

        let loaded = load_graph_index_mmap_with_cipher(&path, Some(&cipher)).unwrap();
        assert_eq!(loaded.nodes.len(), 1);
        assert_eq!(loaded.nodes[0].id, 7);
    }

    #[test]
    fn test_graph_index_mmap_plaintext_still_works_with_cipher_present() {
        // A legacy plaintext file loads through the mmap+cipher path (upgrade).
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");
        let cipher = enc_test_cipher();
        let data = sample_graph();

        save_graph_index(&data, &path).unwrap(); // plaintext
        let loaded = load_graph_index_mmap_with_cipher(&path, Some(&cipher)).unwrap();
        assert_eq!(loaded.nodes[0].id, 7);
    }

    #[test]
    fn test_graph_index_legacy_plaintext_loads_with_cipher() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");
        let cipher = enc_test_cipher();
        let data = sample_graph();

        save_graph_index(&data, &path).unwrap(); // plaintext, no header
        let loaded = load_graph_index_with_cipher(&path, Some(&cipher)).unwrap();
        assert_eq!(loaded.nodes[0].id, 7);
    }

    #[test]
    fn test_graph_index_encrypted_wrong_key_fails() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");
        let cipher = enc_test_cipher();
        let data = sample_graph();

        save_graph_index_with_cipher(&data, &path, Some(&cipher)).unwrap();

        let wrong = {
            use crate::encryption::Aes256GcmCipher;
            use zeroize::Zeroizing;
            let mut key = Zeroizing::new([0u8; 32]);
            key[0] = 0x11;
            Arc::new(Aes256GcmCipher::new(&key)) as Arc<dyn Cipher>
        };
        assert!(load_graph_index_with_cipher(&path, Some(&wrong)).is_err());
    }

    #[test]
    fn test_graph_index_mmap_encrypted_fails_closed_without_cipher() {
        // The mmap load path must also fail closed for an encrypted file when
        // no cipher is supplied: it detects the header, routes to the buffered
        // decrypt, and errors rather than mapping ciphertext as a live struct.
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");
        let cipher = enc_test_cipher();
        let data = sample_graph();

        save_graph_index_with_cipher(&data, &path, Some(&cipher)).unwrap();

        assert!(load_graph_index_mmap_with_cipher(&path, None).is_err());
    }

    #[test]
    fn test_graph_index_zstd_encrypted_tampered_fails() {
        // A tampered byte inside an encrypted+compressed graph file must fail
        // authentication on decrypt (Corrupted), never surface a half-decoded
        // or wrong graph.
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");
        let cipher = enc_test_cipher();
        let data = sample_graph();

        save_graph_index_compressed_with_cipher(&data, &path, 3, Some(&cipher)).unwrap();

        let mut raw = fs::read(&path).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 0xFF;
        fs::write(&path, &raw).unwrap();

        match load_graph_index_with_cipher(&path, Some(&cipher)) {
            Err(IndexPersistenceError::Corrupted { .. }) => {}
            other => {
                panic!("expected Corrupted error for tampered encrypted zstd graph, got {other:?}")
            }
        }
    }

    #[test]
    fn test_array_property_errors() {
        // Test that Array properties properly error instead of silently losing data
        let array_value = PropertyValue::Array(Arc::from(vec![
            PropertyValue::Int(1),
            PropertyValue::Int(2),
            PropertyValue::Int(3),
        ]));

        let result = persist_property_value(&array_value);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Array properties are not yet supported")
        );
    }

    #[test]
    fn test_missing_string_interned_id_errors() {
        // Test that missing interned string IDs properly error
        let persisted = PersistedPropertyValue::String(999999); // Non-existent ID

        let result = restore_property_value(&persisted);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Failed to resolve interned string")
        );
    }

    #[test]
    fn test_vector_size_limit_dos_protection() {
        // Test that vectors exceeding MAX_VECTOR_DIMENSIONS are rejected
        let oversized_vector = vec![0.0f32; super::super::MAX_VECTOR_DIMENSIONS + 1];
        let persisted = PersistedPropertyValue::Vector(oversized_vector);

        let result = restore_property_value(&persisted);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Size limit exceeded"));
        assert!(err.to_string().contains("Vector dimension"));
    }

    #[test]
    fn test_vector_at_size_limit_allowed() {
        // Test that vectors exactly at MAX_VECTOR_DIMENSIONS are allowed
        let max_vector = vec![1.0f32; super::super::MAX_VECTOR_DIMENSIONS];
        let persisted = PersistedPropertyValue::Vector(max_vector);

        let result = restore_property_value(&persisted);
        assert!(result.is_ok());
        if let PropertyValue::Vector(ref v) = result.unwrap() {
            assert_eq!(v.len(), super::super::MAX_VECTOR_DIMENSIONS);
        } else {
            panic!("Expected vector property");
        }
    }
}

/// Regression tests for zstd decompression bomb protection.
#[cfg(test)]
mod zstd_bomb_tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;
    use zstd::stream::write::Encoder;

    /// Create a compressed zstd bomb: `uncompressed_mb` MB of zeros,
    /// wrapped with a fake CRC32 trailer to look like our file format.
    fn create_zstd_bomb(uncompressed_mb: usize) -> Vec<u8> {
        let mut compressed = Vec::new();
        {
            let mut encoder = Encoder::new(&mut compressed, 1).unwrap();
            let chunk = vec![0u8; 1024 * 1024]; // 1MB chunk
            for _ in 0..uncompressed_mb {
                encoder.write_all(&chunk).unwrap();
            }
            encoder.finish().unwrap();
        }
        // Append fake CRC32 (4 bytes) — the loader splits on trailing 4 bytes
        compressed.extend_from_slice(&[0, 0, 0, 0]);
        compressed
    }

    #[test]
    fn test_zstd_bomb_blocked_by_load_graph_index() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bomb.idx");

        // 200MB bomb, test limit is 100MB
        let bomb = create_zstd_bomb(200);
        // Bomb should be small on disk (zeros compress extremely well)
        assert!(bomb.len() < 500_000, "Bomb compressed size: {}", bomb.len());

        std::fs::write(&path, &bomb).unwrap();

        let result = load_graph_index(&path);
        assert!(
            matches!(result, Err(IndexPersistenceError::SizeLimitExceeded { .. })),
            "Expected SizeLimitExceeded, got: {:?}",
            result,
        );
    }

    #[test]
    fn test_zstd_bomb_blocked_by_load_graph_index_mmap() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bomb_mmap.idx");

        let bomb = create_zstd_bomb(200);
        std::fs::write(&path, &bomb).unwrap();

        let result = load_graph_index_mmap(&path);
        assert!(
            matches!(result, Err(IndexPersistenceError::SizeLimitExceeded { .. })),
            "Expected SizeLimitExceeded, got: {:?}",
            result,
        );
    }

    #[test]
    fn test_zstd_bomb_blocked_by_load_graph_index_with_delta() {
        let dir = tempdir().unwrap();

        // Create a valid base index
        let base_path = dir.path().join("base.idx");
        let base_data = new_graph_index_data();
        save_graph_index(&base_data, &base_path).unwrap();

        // Create a bomb as the delta
        let delta_path = dir.path().join("bomb_delta.idx");
        let bomb = create_zstd_bomb(200);
        std::fs::write(&delta_path, &bomb).unwrap();

        let result = load_graph_index_with_delta(&base_path, &delta_path, None);
        assert!(
            matches!(result, Err(IndexPersistenceError::SizeLimitExceeded { .. })),
            "Expected SizeLimitExceeded, got: {:?}",
            result,
        );
    }

    #[test]
    fn test_legitimate_compressed_index_loads_fine() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("compressed.idx");

        // Create and save a legitimate compressed index
        let data = new_graph_index_data();
        save_graph_index_compressed(&data, &path, 3).unwrap();

        // Should load without error
        let loaded = load_graph_index(&path).unwrap();
        assert_eq!(loaded.node_count, 0);
    }
}

#[cfg(test)]
#[path = "graph_delta_tests.rs"]
mod delta_tests;
