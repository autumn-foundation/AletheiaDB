//! WAL Segment Reader.
//!
//! This module provides standalone functions for reading WAL segments from disk
//! for recovery purposes. It does not require any WAL writer state.
//!
//! # Memory Efficiency
//!
//! This module uses memory-mapped I/O (`memmap2`) to read WAL segments efficiently.
//! Instead of loading entire segment files (default 64MB) into memory, memory-mapped
//! files allow the OS to handle paging automatically. This provides several benefits:
//!
//! - **Lower memory usage**: OS pages in data as needed, not all at once
//! - **Better caching**: OS can cache frequently accessed pages
//! - **Automatic eviction**: OS evicts pages under memory pressure
//! - **Reduced recovery memory**: With 10 segments, peak memory drops from 640MB+ to O(working set)
//!
//! See issue #216 for details.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use crate::core::error::{Error, Result, StorageError};
use crate::core::hlc::HybridTimestamp;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::property::PropertyMap;

use super::serialization::{
    OP_CHECKPOINT, OP_CREATE_EDGE, OP_CREATE_NODE, OP_DELETE_EDGE, OP_DELETE_NODE, OP_UPDATE_EDGE,
    OP_UPDATE_NODE,
};
use super::{LSN, WalEntry, WalOperation};

/// Magic bytes identifying a AletheiaDB WAL segment file.
pub(crate) const WAL_MAGIC: [u8; 4] = *b"GWAL";

/// Current WAL format version (plaintext entries).
pub(crate) const WAL_VERSION: u8 = 1;

/// WAL format version for encrypted segments.
///
/// Version 2 segments use length-prefixed encrypted entries:
/// `[4-byte LE entry length][encrypted entry bytes]`
/// The header (magic + version) remains plaintext.
pub(crate) const WAL_VERSION_ENCRYPTED: u8 = 2;

/// Maximum supported WAL version (inclusive).
const WAL_VERSION_MAX: u8 = WAL_VERSION_ENCRYPTED;

/// Size of the WAL segment header (magic + version).
pub(crate) const WAL_HEADER_SIZE: usize = 5;

/// Maximum reasonable segment size (configurable, but 1GB is a safe upper bound)
/// Default segments are 64MB, so 1GB allows for 16x growth
pub(crate) const MAX_SEGMENT_SIZE: u64 = 1024 * 1024 * 1024; // 1GB

/// Read all WAL entries from a directory, starting from the specified LSN.
///
/// This function scans the directory for segment files (*.log), reads them in order,
/// and returns all entries with LSN >= start_lsn.
///
/// # Arguments
///
/// * `wal_dir` - Path to the WAL directory containing segment files
/// * `start_lsn` - Only entries with LSN >= this value are returned
///
/// # Returns
///
/// A vector of WAL entries sorted by LSN.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::wal::LSN;
/// use aletheiadb::storage::wal::segment_reader::read_entries_from_dir;
/// use std::fs::File;
/// use std::io::Write;
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let wal_dir = dir.path();
///
/// // Create a dummy segment file
/// let segment_path = wal_dir.join("0.log");
/// let mut file = File::create(&segment_path).unwrap();
///
/// // Write WAL header: Magic bytes *b"GWAL" and version 1
/// file.write_all(b"GWAL").unwrap();
/// file.write_all(&[1]).unwrap();
/// file.sync_all().unwrap();
///
/// // Read entries (returns an empty vector since we wrote no entries)
/// let entries = read_entries_from_dir(wal_dir, LSN(1)).unwrap();
/// assert!(entries.is_empty());
/// ```
pub fn read_entries_from_dir(wal_dir: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    read_entries_from_dir_with_cipher(wal_dir, start_lsn, None)
}

/// Read all WAL entries from a directory with optional decryption.
///
/// This function scans the directory for segment files (*.log), reads them in order,
/// and returns all entries with LSN >= start_lsn. If a cipher is provided, version 2
/// (encrypted) segments are decrypted transparently. Version 1 (plaintext) segments
/// are always read without decryption regardless of the cipher parameter.
///
/// # Arguments
///
/// * `wal_dir` - Path to the WAL directory containing segment files
/// * `start_lsn` - Only entries with LSN >= this value are returned
/// * `cipher` - Optional cipher for decrypting version 2 segments
///
/// # Returns
///
/// A vector of WAL entries sorted by LSN.
pub fn read_entries_from_dir_with_cipher(
    wal_dir: &Path,
    start_lsn: LSN,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
) -> Result<Vec<WalEntry>> {
    let mut entries = Vec::new();

    // Find all WAL segments
    let mut segments = Vec::new();
    if let Ok(dir_entries) = std::fs::read_dir(wal_dir) {
        for entry in dir_entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.ends_with(".log")
                && let Some(seg_id) = name
                    .strip_suffix(".log")
                    .and_then(|s| s.parse::<u64>().ok())
            {
                segments.push((seg_id, entry.path()));
            }
        }
    }

    // Sort segments by ID
    segments.sort_by_key(|(id, _)| *id);

    // Read entries from each segment
    for (_, path) in segments {
        let segment_entries = read_segment_with_cipher(&path, start_lsn, cipher)?;
        entries.extend(segment_entries);
    }

    // Sort entries by LSN to ensure correct ordering across segments.
    // In a striped WAL architecture, entries can be flushed to different segments
    // in an order that differs from their LSN assignment order.
    entries.sort_by_key(|entry| entry.lsn);

    Ok(entries)
}

/// Read WAL entries from a single segment file.
///
/// This function uses memory-mapped I/O for efficient reading without loading
/// the entire file into memory. The OS handles paging automatically, which is
/// especially important for large segment files (default 64MB).
///
/// # Arguments
///
/// * `path` - Path to the segment file
/// * `start_lsn` - Only entries with LSN >= this value are returned
///
/// # Returns
///
/// A vector of WAL entries from this segment.
///
/// # Memory Efficiency
///
/// Uses `memmap2` for memory-mapped I/O. Peak memory usage is O(working set)
/// rather than O(file size). See issue #216.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::wal::LSN;
/// use aletheiadb::storage::wal::segment_reader::read_segment;
/// use std::fs::File;
/// use std::io::Write;
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let segment_path = dir.path().join("segment.log");
///
/// // Create a segment file with a valid header: Magic bytes *b"GWAL" and version 1
/// let mut file = File::create(&segment_path).unwrap();
/// file.write_all(b"GWAL").unwrap();
/// file.write_all(&[1]).unwrap();
/// file.sync_all().unwrap();
///
/// // Read the empty segment
/// let entries = read_segment(&segment_path, LSN(1)).unwrap();
/// assert!(entries.is_empty());
/// ```
pub fn read_segment(path: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    read_segment_with_cipher(path, start_lsn, None)
}

/// Read WAL entries from a single segment file with optional decryption.
///
/// This function uses memory-mapped I/O for efficient reading. It transparently
/// handles both version 1 (plaintext) and version 2 (encrypted) segments:
///
/// - **Version 1**: Entries are parsed directly (no cipher needed).
/// - **Version 2**: Each entry is length-prefixed (`[4-byte LE len][encrypted data]`).
///   A cipher must be provided to decrypt version 2 segments.
///
/// # Arguments
///
/// * `path` - Path to the segment file
/// * `start_lsn` - Only entries with LSN >= this value are returned
/// * `cipher` - Optional cipher for decrypting version 2 segments
///
/// # Returns
///
/// A vector of WAL entries from this segment.
///
/// # Errors
///
/// Returns an error if:
/// - The segment is version 2 but no cipher is provided
/// - Decryption fails (wrong key, corrupted data, tampered header)
/// - The segment format is invalid
pub fn read_segment_with_cipher(
    path: &Path,
    start_lsn: LSN,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
) -> Result<Vec<WalEntry>> {
    // Open file, only treating NotFound as "empty" - all other errors are propagated
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(StorageError::IoError(format!(
                "Failed to open WAL segment {:?}: {}",
                path, e
            ))
            .into());
        }
    };

    // Validate file size before mapping to prevent DoS attacks with huge files
    let metadata = file
        .metadata()
        .map_err(|e| StorageError::IoError(format!("Failed to get file metadata: {}", e)))?;

    if metadata.len() > MAX_SEGMENT_SIZE {
        return Err(StorageError::CorruptedData(format!(
            "WAL segment too large: {} bytes (max: {} bytes)",
            metadata.len(),
            MAX_SEGMENT_SIZE
        ))
        .into());
    }

    // Handle empty files explicitly to avoid mmap failure on some platforms (e.g. macOS/Windows)
    // A zero-byte file can occur if a crash happens immediately after segment creation.
    if metadata.len() == 0 {
        return Ok(Vec::new());
    }

    // Memory-map the file for efficient reading without loading entire file into memory.
    // SAFETY: We only read from the memory map, never write. The file is opened read-only.
    // The mapping is valid for the lifetime of this function and is automatically unmapped
    // when dropped. We have verified the file size above to prevent out-of-bounds reads.
    let mmap = unsafe {
        memmap2::Mmap::map(&file).map_err(|e| {
            StorageError::IoError(format!("Failed to memory-map WAL segment: {}", e))
        })?
    };

    // Use the memory-mapped region as a byte slice
    let buffer = &mmap[..];

    let mut entries = Vec::new();

    // Detect WAL format version
    let (version, mut offset) = if buffer.len() >= WAL_HEADER_SIZE && buffer[0..4] == WAL_MAGIC {
        // Version 1+ format: has magic header
        let ver = buffer[4];
        if ver > WAL_VERSION_MAX {
            return Err(StorageError::CorruptedData(format!(
                "Unsupported WAL version: {} (max supported: {})",
                ver, WAL_VERSION_MAX
            ))
            .into());
        }
        (ver, WAL_HEADER_SIZE)
    } else if !buffer.is_empty() {
        // Invalid format: no magic header
        return Err(StorageError::CorruptedData(
            "Invalid WAL segment: missing GWAL magic header".to_string(),
        )
        .into());
    } else {
        return Ok(Vec::new()); // Empty segment
    };

    // Version 2 (encrypted) segments require a cipher for decryption.
    if version == WAL_VERSION_ENCRYPTED && cipher.is_none() {
        return Err(StorageError::Encryption(
            "Cannot read encrypted WAL segment (version 2) without a cipher".to_string(),
        )
        .into());
    }

    // Dispatch to the appropriate parsing loop based on version.
    if version == WAL_VERSION_ENCRYPTED {
        // Version 2: length-prefixed encrypted entries.
        let cipher = cipher.expect("cipher presence checked above");
        parse_encrypted_entries(buffer, &mut offset, start_lsn, cipher, path, &mut entries)?;
    } else {
        // Version 1: plaintext entries (original format).
        parse_plaintext_entries(buffer, &mut offset, version, start_lsn, path, &mut entries)?;
    }

    Ok(entries)
}

/// Parse plaintext (version 1) entries from a WAL segment buffer.
fn parse_plaintext_entries(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    start_lsn: LSN,
    path: &Path,
    entries: &mut Vec<WalEntry>,
) -> Result<()> {
    while *offset < buffer.len() {
        match parse_entry_at(buffer, *offset, version) {
            Ok((entry, bytes_consumed)) => {
                if entry.lsn >= start_lsn {
                    entries.push(entry);
                }
                *offset += bytes_consumed;
            }
            Err(e) => {
                // Distinguish between expected EOF truncation vs. unexpected corruption
                if *offset + 24 > buffer.len() {
                    #[cfg(feature = "observability")]
                    tracing::debug!(
                        "Partial entry at end of WAL segment {:?} (offset {}/{}), stopping read",
                        path,
                        offset,
                        buffer.len()
                    );
                    break;
                } else {
                    let header_slice = &buffer[*offset..*offset + 24];
                    if header_slice.iter().all(|&b| b == 0) {
                        #[cfg(feature = "observability")]
                        tracing::debug!(
                            "Zeroed region at end of WAL segment {:?} (offset {}/{}), stopping read",
                            path,
                            offset,
                            buffer.len()
                        );
                        break;
                    }

                    #[cfg(feature = "observability")]
                    tracing::error!(
                        "Failed to parse WAL entry in segment {:?} at offset {}: {}",
                        path,
                        offset,
                        e
                    );
                    #[cfg(not(feature = "observability"))]
                    {
                        eprintln!(
                            "CRITICAL: Failed to parse WAL entry in segment {:?} at offset {}: {}",
                            path, offset, e
                        );
                        eprintln!("Header slice: {:?}", header_slice);
                    }
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

/// Parse encrypted (version 2) entries from a WAL segment buffer.
///
/// Each entry is stored as `[4-byte LE length][encrypted entry bytes]`.
/// The encrypted entry bytes are decrypted using the provided cipher,
/// then parsed as a normal WAL entry (version 1 format).
fn parse_encrypted_entries(
    buffer: &[u8],
    offset: &mut usize,
    start_lsn: LSN,
    cipher: &Arc<dyn crate::encryption::cipher::Cipher>,
    path: &Path,
    entries: &mut Vec<WalEntry>,
) -> Result<()> {
    while *offset < buffer.len() {
        // Need at least 4 bytes for the length prefix
        if *offset + 4 > buffer.len() {
            // Partial length prefix at EOF -- truncated write
            #[cfg(feature = "observability")]
            tracing::debug!(
                "Partial length prefix at end of encrypted WAL segment {:?} (offset {}/{}), stopping read",
                path,
                offset,
                buffer.len()
            );
            break;
        }

        // Check for zeroed length prefix (indicates end of data in pre-allocated files)
        let len_bytes: [u8; 4] = buffer[*offset..*offset + 4]
            .try_into()
            .expect("slice length verified above");
        let entry_len = u32::from_le_bytes(len_bytes) as usize;

        if entry_len == 0 {
            // Zero-length entry marks end of valid data
            break;
        }

        *offset += 4;

        // Validate entry length
        if *offset + entry_len > buffer.len() {
            // Truncated encrypted entry at EOF
            #[cfg(feature = "observability")]
            tracing::debug!(
                "Truncated encrypted entry at end of WAL segment {:?} (offset {}, entry_len {}, buf_len {}), stopping read",
                path,
                offset,
                entry_len,
                buffer.len()
            );
            break;
        }

        let encrypted_entry = &buffer[*offset..*offset + entry_len];
        *offset += entry_len;

        // Decrypt the entry
        let decrypted =
            crate::encryption::wal_encryption::decrypt_wal_payload(encrypted_entry, cipher)
                .map_err(|e| {
                    Error::Storage(StorageError::Encryption(format!(
                        "Failed to decrypt WAL entry in segment {:?}: {}",
                        path, e
                    )))
                })?;

        // Parse the decrypted bytes as a normal (version 1) entry
        match parse_entry_at(&decrypted, 0, WAL_VERSION) {
            Ok((entry, _bytes_consumed)) => {
                if entry.lsn >= start_lsn {
                    entries.push(entry);
                }
            }
            Err(e) => {
                #[cfg(feature = "observability")]
                tracing::error!(
                    "Failed to parse decrypted WAL entry in segment {:?}: {}",
                    path,
                    e
                );
                #[cfg(not(feature = "observability"))]
                eprintln!(
                    "CRITICAL: Failed to parse decrypted WAL entry in segment {:?}: {}",
                    path, e
                );
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Parse a single WAL entry from a buffer at the specified offset.
///
/// This function extracts the parsing logic that was previously duplicated
/// in multiple places (issue #218). It handles all WAL operation types and
/// returns both the parsed entry and the number of bytes consumed.
///
/// # Arguments
///
/// * `buffer` - The buffer containing serialized WAL data
/// * `offset` - The offset in the buffer to start parsing from
/// * `version` - The WAL format version
///
/// # Returns
///
/// A tuple of (WalEntry, bytes_consumed) on success, or an error if:
/// - The buffer is too small to contain a valid entry
/// - The operation type is unknown
/// - The data is corrupted or truncated
///
/// # Example
///
/// ```ignore
/// let (entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION)?;
/// let next_offset = offset + bytes_consumed;
/// ```
pub(crate) fn parse_entry_at(
    buffer: &[u8],
    offset: usize,
    version: u8,
) -> Result<(WalEntry, usize)> {
    let start_offset = offset;
    let mut current_offset = offset;

    // Helper macro for checked addition to prevent overflow panics
    macro_rules! add_offset {
        ($n:expr) => {
            current_offset = current_offset.checked_add($n).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })?;
        };
    }

    // Phase 2: Need at least 24 bytes for LSN (8) + HybridTimestamp (12) + checksum (4)
    // Use checked arithmetic for bounds check
    if current_offset.checked_add(24).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(
            "WAL offset overflow".to_string(),
        ))
    })? > buffer.len()
    {
        return Err(StorageError::CorruptedData(
            "Insufficient buffer size for WAL entry header".to_string(),
        )
        .into());
    }

    // Read LSN (8 bytes)
    let lsn = LSN(u64::from_le_bytes(
        buffer[current_offset..current_offset + 8]
            .try_into()
            .unwrap(), // Safe due to buffer length check above
    ));
    add_offset!(8);

    // Read timestamp (12 bytes: Phase 2 HybridTimestamp)
    let (timestamp, _) = HybridTimestamp::deserialize(&buffer[current_offset..]).map_err(|e| {
        StorageError::CorruptedData(format!("Failed to deserialize timestamp: {}", e))
    })?;
    add_offset!(12);

    // Read checksum (4 bytes)
    let checksum = u32::from_le_bytes(
        buffer[current_offset..current_offset + 4]
            .try_into()
            .unwrap(), // Safe due to buffer length check above
    );
    add_offset!(4);

    // Read operation type
    if current_offset >= buffer.len() {
        return Err(StorageError::CorruptedData(
            "Insufficient buffer size for operation type".to_string(),
        )
        .into());
    }
    let op_type = buffer[current_offset];
    add_offset!(1);

    // Parse operation data based on type and version
    let operation = match op_type {
        OP_CREATE_NODE => {
            if current_offset.checked_add(12).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })? > buffer.len()
            {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for CreateNode".to_string(),
                )
                .into());
            }
            let node_id = deserialize_node_id(buffer, current_offset, "CreateNode")?;
            add_offset!(8);

            // Read 4-byte InternedString ID
            if current_offset.checked_add(4).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })? > buffer.len()
            {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for CreateNode label".to_string(),
                )
                .into());
            }
            let label_id = u32::from_le_bytes(
                buffer[current_offset..current_offset + 4]
                    .try_into()
                    .unwrap(), // Safe due to buffer length check above
            );
            add_offset!(4);

            // Reconstruct InternedString from ID
            // During recovery, the string should already be in the interner
            // (either from checkpoint or previous WAL entries)
            let label = crate::core::interning::InternedString::from_raw(label_id);

            // V1+: deserialize properties and temporal
            let (properties, valid_from) = if version >= WAL_VERSION {
                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                add_offset!(props_len);
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                add_offset!(ts_len);
                (props, valid_from_ts)
            } else {
                (PropertyMap::new(), timestamp)
            };

            WalOperation::CreateNode {
                node_id,
                label,
                properties,
                valid_from,
            }
        }
        OP_CREATE_EDGE => {
            if current_offset.checked_add(28).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })? > buffer.len()
            {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for CreateEdge".to_string(),
                )
                .into());
            }
            let edge_id = deserialize_edge_id(buffer, current_offset, "CreateEdge")?;
            add_offset!(8);

            let source = deserialize_node_id(buffer, current_offset, "CreateEdge source")?;
            add_offset!(8);

            let target = deserialize_node_id(buffer, current_offset, "CreateEdge target")?;
            add_offset!(8);

            // Read 4-byte InternedString ID
            if current_offset.checked_add(4).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })? > buffer.len()
            {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for CreateEdge label".to_string(),
                )
                .into());
            }
            let label_id = u32::from_le_bytes(
                buffer[current_offset..current_offset + 4]
                    .try_into()
                    .unwrap(), // Safe due to buffer length check above
            );
            add_offset!(4);

            // Reconstruct InternedString from ID
            let label = crate::core::interning::InternedString::from_raw(label_id);

            let (properties, valid_from) = if version >= WAL_VERSION {
                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                add_offset!(props_len);
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                add_offset!(ts_len);
                (props, valid_from_ts)
            } else {
                (PropertyMap::new(), timestamp)
            };

            WalOperation::CreateEdge {
                edge_id,
                source,
                target,
                label,
                properties,
                valid_from,
            }
        }
        OP_UPDATE_NODE => {
            if current_offset.checked_add(16).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })? > buffer.len()
            {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for UpdateNode".to_string(),
                )
                .into());
            }
            let node_id = deserialize_node_id(buffer, current_offset, "UpdateNode")?;
            add_offset!(8);

            let version_id = deserialize_version_id(buffer, current_offset, "UpdateNode")?;
            add_offset!(8);

            let (label, properties, valid_from) = if version >= WAL_VERSION {
                // Read 4-byte InternedString ID
                if current_offset.checked_add(4).ok_or_else(|| {
                    Error::Storage(StorageError::CorruptedData(
                        "WAL offset overflow".to_string(),
                    ))
                })? > buffer.len()
                {
                    return Err(StorageError::CorruptedData(
                        "Insufficient buffer size for UpdateNode label".to_string(),
                    )
                    .into());
                }
                let label_id = u32::from_le_bytes([
                    buffer[current_offset],
                    buffer[current_offset + 1],
                    buffer[current_offset + 2],
                    buffer[current_offset + 3],
                ]);
                add_offset!(4);

                // Reconstruct InternedString from ID
                let lbl = crate::core::interning::InternedString::from_raw(label_id);

                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                add_offset!(props_len);
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                add_offset!(ts_len);
                (lbl, props, valid_from_ts)
            } else {
                (
                    // For old WAL format, create a dummy InternedString (this shouldn't happen in practice)
                    crate::core::interning::InternedString::from_raw(0),
                    PropertyMap::new(),
                    timestamp,
                )
            };

            WalOperation::UpdateNode {
                node_id,
                version_id,
                label,
                properties,
                valid_from,
            }
        }
        OP_UPDATE_EDGE => {
            // V0: 16 bytes (EdgeId + VersionId)
            // V1+: 20 bytes (EdgeId + VersionId + LabelId)
            let required = if version >= WAL_VERSION { 20 } else { 16 };
            if current_offset.checked_add(required).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })? > buffer.len()
            {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for UpdateEdge".to_string(),
                )
                .into());
            }
            let edge_id = deserialize_edge_id(buffer, current_offset, "UpdateEdge")?;
            add_offset!(8);

            let version_id = deserialize_version_id(buffer, current_offset, "UpdateEdge")?;
            add_offset!(8);

            let (label, properties, valid_from) = if version >= WAL_VERSION {
                // Read 4-byte InternedString ID
                if current_offset.checked_add(4).ok_or_else(|| {
                    Error::Storage(StorageError::CorruptedData(
                        "WAL offset overflow".to_string(),
                    ))
                })? > buffer.len()
                {
                    return Err(StorageError::CorruptedData(
                        "Insufficient buffer size for UpdateEdge label".to_string(),
                    )
                    .into());
                }
                let label_id = u32::from_le_bytes([
                    buffer[current_offset],
                    buffer[current_offset + 1],
                    buffer[current_offset + 2],
                    buffer[current_offset + 3],
                ]);
                add_offset!(4);

                // Reconstruct InternedString from ID
                let lbl = crate::core::interning::InternedString::from_raw(label_id);

                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                add_offset!(props_len);
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                add_offset!(ts_len);
                (lbl, props, valid_from_ts)
            } else {
                (
                    // For old WAL format, create a dummy InternedString (this shouldn't happen in practice)
                    crate::core::interning::InternedString::from_raw(0),
                    PropertyMap::new(),
                    timestamp,
                )
            };

            WalOperation::UpdateEdge {
                edge_id,
                version_id,
                label,
                properties,
                valid_from,
            }
        }
        OP_CHECKPOINT => {
            // LSN (8 bytes) + HybridTimestamp (12 bytes) = 20 bytes
            if current_offset.checked_add(20).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })? > buffer.len()
            {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for Checkpoint".to_string(),
                )
                .into());
            }
            let cp_lsn = LSN(u64::from_le_bytes(
                buffer[current_offset..current_offset + 8]
                    .try_into()
                    .unwrap(), // Safe due to buffer length check above
            ));
            add_offset!(8);

            // Phase 2: Deserialize HybridTimestamp (12 bytes: 8 wallclock + 4 logical)
            let (cp_timestamp, consumed) = HybridTimestamp::deserialize(&buffer[current_offset..])?;
            add_offset!(consumed);

            WalOperation::Checkpoint {
                lsn: cp_lsn,
                timestamp: cp_timestamp,
            }
        }
        OP_DELETE_NODE => {
            if current_offset.checked_add(8).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })? > buffer.len()
            {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for DeleteNode".to_string(),
                )
                .into());
            }
            let node_id = deserialize_node_id(buffer, current_offset, "DeleteNode")?;
            add_offset!(8);

            let valid_from = if version >= WAL_VERSION {
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                add_offset!(ts_len);
                valid_from_ts
            } else {
                timestamp
            };

            WalOperation::DeleteNode {
                node_id,
                valid_from,
            }
        }
        OP_DELETE_EDGE => {
            if current_offset.checked_add(8).ok_or_else(|| {
                Error::Storage(StorageError::CorruptedData(
                    "WAL offset overflow".to_string(),
                ))
            })? > buffer.len()
            {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for DeleteEdge".to_string(),
                )
                .into());
            }
            let edge_id = deserialize_edge_id(buffer, current_offset, "DeleteEdge")?;
            add_offset!(8);

            let valid_from = if version >= WAL_VERSION {
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                add_offset!(ts_len);
                valid_from_ts
            } else {
                timestamp
            };

            WalOperation::DeleteEdge {
                edge_id,
                valid_from,
            }
        }
        _ => {
            // Unknown operation type
            return Err(StorageError::CorruptedData(format!(
                "Unknown WAL operation type: {}",
                op_type
            ))
            .into());
        }
    };

    // Verify checksum to ensure data integrity (critical for WAL correctness)
    let mut hasher = crc32fast::Hasher::new();
    // Hash LSN (8 bytes) + timestamp (12 bytes) = bytes 0..20
    hasher.update(&buffer[start_offset..start_offset + 20]);
    // Hash operation data (from after checksum field to end of entry)
    hasher.update(&buffer[start_offset + 24..current_offset]);
    let computed_checksum = hasher.finalize();

    if checksum != computed_checksum {
        return Err(StorageError::CorruptedData(format!(
            "WAL entry checksum mismatch for LSN {}: expected {:#x}, got {:#x}. Entry is corrupted.",
            lsn.0, checksum, computed_checksum
        ))
        .into());
    }

    let entry = WalEntry {
        lsn,
        timestamp,
        operation,
        checksum,
    };

    let bytes_consumed = current_offset - start_offset;
    Ok((entry, bytes_consumed))
}

/// Helper to deserialize and validate a NodeId from WAL buffer
#[inline]
fn deserialize_node_id(buffer: &[u8], offset: usize, context: &str) -> Result<NodeId> {
    let bytes = buffer.get(offset..offset + 8).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Insufficient buffer size for NodeId in {}",
            context
        )))
    })?;
    let raw_id = u64::from_le_bytes(bytes.try_into().unwrap());
    NodeId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid node ID in WAL {}: {}",
            context, e
        )))
    })
}

/// Helper to deserialize and validate an EdgeId from WAL buffer
#[inline]
fn deserialize_edge_id(buffer: &[u8], offset: usize, context: &str) -> Result<EdgeId> {
    let bytes = buffer.get(offset..offset + 8).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Insufficient buffer size for EdgeId in {}",
            context
        )))
    })?;
    let raw_id = u64::from_le_bytes(bytes.try_into().unwrap());
    EdgeId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid edge ID in WAL {}: {}",
            context, e
        )))
    })
}

/// Helper to deserialize and validate a VersionId from WAL buffer
#[inline]
fn deserialize_version_id(buffer: &[u8], offset: usize, context: &str) -> Result<VersionId> {
    let bytes = buffer.get(offset..offset + 8).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Insufficient buffer size for VersionId in {}",
            context
        )))
    })?;
    let raw_id = u64::from_le_bytes(bytes.try_into().unwrap());
    VersionId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid version ID in WAL {}: {}",
            context, e
        )))
    })
}

#[cfg(test)]
#[cfg(test)]
mod tests;
