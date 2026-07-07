//! WAL Segment Reader.
//!
//! This module provides standalone functions for reading WAL segments from disk
//! for recovery purposes. It does not require any WAL writer state.
//!
//! # Segment Format
//!
//! Each segment file on disk has the following binary layout:
//!
//! ```text
//!  Offset  Size  Field
//!  ──────  ────  ──────────────────────────────────────────────────
//!       0     4  Magic bytes: b"GWAL"  (WAL_MAGIC)
//!       4     1  Format version: 1 (plaintext) or 2 (encrypted)
//!       5     …  Entry frames (repeated until end of file)
//! ```
//!
//! **Entry frame (version 1 – plaintext)**:
//! ```text
//!  [bincode-serialised WalEntry bytes]
//! ```
//! Entries are length-prefixed by `bincode` (8-byte little-endian usize).
//!
//! **Entry frame (version 2 – encrypted)**:
//! ```text
//!  [4-byte LE u32 entry length][encrypted entry bytes]
//! ```
//! The header (magic + version) is always plaintext; only the entry frames
//! are encrypted in version 2.
//!
//! See the `WAL_MAGIC`, `WAL_VERSION`, `WAL_VERSION_ENCRYPTED`, and
//! `WAL_HEADER_SIZE` constants defined below for the associated values.
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
use crate::core::provenance::Provenance;

use super::serialization::{
    OP_CHECKPOINT, OP_CREATE_EDGE, OP_CREATE_NODE, OP_DECLARE_UNIQUE_CONSTRAINT, OP_DELETE_EDGE,
    OP_DELETE_NODE, OP_DROP_UNIQUE_CONSTRAINT, OP_UPDATE_EDGE, OP_UPDATE_NODE,
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

/// WAL format version for plaintext segments whose `CreateNode`/`CreateEdge`/
/// `UpdateNode`/`UpdateEdge` payloads carry an optional [`Provenance`] bundle
/// after `valid_from` (Issue #3224).
///
/// Segments below this version simply lack the provenance bytes; parsing
/// falls back to `provenance: None` for them (see `read_provenance`).
pub(crate) const WAL_VERSION_PROVENANCE: u8 = 3;

/// WAL format version for encrypted segments whose decrypted payload uses
/// the provenance-carrying entry format (i.e. `WAL_VERSION_PROVENANCE`).
///
/// Mirrors the `WAL_VERSION` / `WAL_VERSION_ENCRYPTED` relationship: this is
/// `WAL_VERSION_ENCRYPTED`'s counterpart once provenance was added.
pub(crate) const WAL_VERSION_ENCRYPTED_PROVENANCE: u8 = 4;

/// Maximum supported WAL version (inclusive).
const WAL_VERSION_MAX: u8 = WAL_VERSION_ENCRYPTED_PROVENANCE;

/// Returns `true` if `version` denotes an encrypted segment (either the
/// original encrypted format or its provenance-carrying successor).
#[inline]
fn is_encrypted_version(version: u8) -> bool {
    version == WAL_VERSION_ENCRYPTED || version == WAL_VERSION_ENCRYPTED_PROVENANCE
}

/// Map a segment/container format version to the logical *payload* version
/// used to gate field parsing (e.g. `version >= WAL_VERSION_PROVENANCE`).
///
/// Encrypted versions describe the on-disk *container* (length-prefixed +
/// encrypted), not the entry payload layout once decrypted; this maps them
/// back to the plaintext version whose entry layout they share.
#[inline]
fn payload_version(version: u8) -> u8 {
    match version {
        WAL_VERSION_ENCRYPTED => WAL_VERSION,
        WAL_VERSION_ENCRYPTED_PROVENANCE => WAL_VERSION_PROVENANCE,
        v => v,
    }
}

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
    let mut segments = Vec::with_capacity(16); // ⚡ Bolt Optimization: Pre-allocate space for WAL segment paths to prevent small heap reallocations when reading directories.
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

    // ⚡ Bolt Optimization: Pre-allocate vector based on buffer size.
    // Assuming an average WAL entry is ~128 bytes, this prevents numerous
    // heap reallocations when reading large segments.
    let capacity_hint = (buffer.len() / 128).max(1);
    let mut entries = Vec::with_capacity(capacity_hint);

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

    // Encrypted segments (version 2 or 4) require a cipher for decryption.
    if is_encrypted_version(version) && cipher.is_none() {
        return Err(StorageError::Encryption(format!(
            "Cannot read encrypted WAL segment (version {}) without a cipher",
            version
        ))
        .into());
    }

    // Dispatch to the appropriate parsing loop based on version.
    if is_encrypted_version(version) {
        // Version 2/4: length-prefixed encrypted entries.
        let cipher = cipher.expect("cipher presence checked above");
        parse_encrypted_entries(
            buffer,
            &mut offset,
            start_lsn,
            cipher,
            path,
            &mut entries,
            version,
        )?;
    } else {
        // Version 1/3: plaintext entries.
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

/// Parse encrypted (version 2 or 4) entries from a WAL segment buffer.
///
/// Each entry is stored as `[4-byte LE length][encrypted entry bytes]`.
/// The encrypted entry bytes are decrypted using the provided cipher,
/// then parsed as a normal WAL entry using the payload version implied by
/// `container_version` (see [`payload_version`]).
fn parse_encrypted_entries(
    buffer: &[u8],
    offset: &mut usize,
    start_lsn: LSN,
    cipher: &Arc<dyn crate::encryption::cipher::Cipher>,
    path: &Path,
    entries: &mut Vec<WalEntry>,
    container_version: u8,
) -> Result<()> {
    let entry_version = payload_version(container_version);
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

        // Parse the decrypted bytes as a normal entry, using the payload
        // version implied by the container version (plaintext-equivalent).
        match parse_entry_at(&decrypted, 0, entry_version) {
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

/// Advance `offset` by `n` bytes with overflow protection.
#[inline]
fn advance(offset: &mut usize, n: usize) -> Result<()> {
    *offset = offset.checked_add(n).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(
            "WAL offset overflow".to_string(),
        ))
    })?;
    Ok(())
}

/// Verify at least `n` bytes are available from `offset` in `buffer`.
///
/// Returns an overflow error if `offset + n` would overflow, or a `CorruptedData`
/// error with `context` in the message if the buffer is too short.
#[inline]
fn require_bytes(buffer: &[u8], offset: usize, n: usize, context: &str) -> Result<()> {
    let end = offset.checked_add(n).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(
            "WAL offset overflow".to_string(),
        ))
    })?;
    if end > buffer.len() {
        return Err(StorageError::CorruptedData(format!(
            "Insufficient buffer size for {}",
            context
        ))
        .into());
    }
    Ok(())
}

/// Read an optional `String` field: `[1-byte presence][4-byte LE len][UTF-8 bytes]`.
///
/// The length and bytes are only present when the presence byte is nonzero.
fn read_opt_string(buffer: &[u8], offset: &mut usize, context: &str) -> Result<Option<String>> {
    require_bytes(buffer, *offset, 1, context)?;
    let present = buffer[*offset];
    advance(offset, 1)?;
    if present == 0 {
        return Ok(None);
    }
    require_bytes(buffer, *offset, 4, context)?;
    let len = u32::from_le_bytes(buffer[*offset..*offset + 4].try_into().unwrap()) as usize;
    advance(offset, 4)?;
    require_bytes(buffer, *offset, len, context)?;
    let s = std::str::from_utf8(&buffer[*offset..*offset + len])
        .map_err(|e| StorageError::CorruptedData(format!("Invalid UTF-8 in {}: {}", context, e)))?
        .to_string();
    advance(offset, len)?;
    Ok(Some(s))
}

/// Read an optional `f64` field: `[1-byte presence][8-byte LE value]`.
fn read_opt_f64(buffer: &[u8], offset: &mut usize, context: &str) -> Result<Option<f64>> {
    require_bytes(buffer, *offset, 1, context)?;
    let present = buffer[*offset];
    advance(offset, 1)?;
    if present == 0 {
        return Ok(None);
    }
    require_bytes(buffer, *offset, 8, context)?;
    let v = f64::from_le_bytes(buffer[*offset..*offset + 8].try_into().unwrap());
    advance(offset, 8)?;
    Ok(Some(v))
}

/// Read an optional [`Provenance`] bundle written by `serialize_provenance_into`.
///
/// Segments below [`WAL_VERSION_PROVENANCE`] never contain these bytes at
/// all; for those, this returns `Ok(None)` without touching `offset`.
fn read_provenance(buffer: &[u8], offset: &mut usize, version: u8) -> Result<Option<Provenance>> {
    if version < WAL_VERSION_PROVENANCE {
        return Ok(None);
    }
    require_bytes(buffer, *offset, 1, "provenance presence")?;
    let present = buffer[*offset];
    advance(offset, 1)?;
    if present == 0 {
        return Ok(None);
    }

    let source = read_opt_string(buffer, offset, "provenance.source")?;
    let confidence = read_opt_f64(buffer, offset, "provenance.confidence")?;
    let note = read_opt_string(buffer, offset, "provenance.note")?;
    let correlation_id = read_opt_string(buffer, offset, "provenance.correlation_id")?;

    let mut builder = Provenance::builder();
    if let Some(source) = source {
        builder = builder.source(source);
    }
    if let Some(confidence) = confidence {
        builder = builder.confidence(confidence);
    }
    if let Some(note) = note {
        builder = builder.note(note);
    }
    if let Some(correlation_id) = correlation_id {
        builder = builder.correlation_id(correlation_id);
    }
    let provenance = builder.build().map_err(|e| {
        StorageError::CorruptedData(format!("Invalid provenance in WAL entry: {}", e))
    })?;
    Ok(Some(provenance))
}

/// Read a 4-byte InternedString label ID from `buffer` at `offset`, advancing `offset` by 4.
#[inline]
fn read_label(
    buffer: &[u8],
    offset: &mut usize,
    context: &str,
) -> Result<crate::core::interning::InternedString> {
    require_bytes(buffer, *offset, 4, context)?;
    let label_id = u32::from_le_bytes(buffer[*offset..*offset + 4].try_into().unwrap());
    advance(offset, 4)?;
    Ok(crate::core::interning::InternedString::from_raw(label_id))
}

/// Read a PropertyMap and valid_from HybridTimestamp for version 1+ entries.
///
/// For version 0 (legacy format), returns an empty property map and the entry's
/// transaction timestamp as the valid_from time.
fn read_props_and_valid_from(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<(PropertyMap, HybridTimestamp)> {
    if version >= WAL_VERSION {
        let (props, props_len) = PropertyMap::deserialize(&buffer[*offset..])?;
        advance(offset, props_len)?;
        let (valid_from, ts_len) = HybridTimestamp::deserialize(&buffer[*offset..])?;
        advance(offset, ts_len)?;
        Ok((props, valid_from))
    } else {
        Ok((PropertyMap::new(), tx_timestamp))
    }
}

fn parse_create_node_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let node_id = deserialize_node_id(buffer, *offset, "CreateNode")?;
    advance(offset, 8)?;
    let label = read_label(buffer, offset, "CreateNode label")?;
    let (properties, valid_from) =
        read_props_and_valid_from(buffer, offset, version, tx_timestamp)?;
    let provenance = read_provenance(buffer, offset, version)?;
    Ok(WalOperation::CreateNode {
        node_id,
        label,
        properties,
        valid_from,
        provenance,
    })
}

fn parse_create_edge_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let edge_id = deserialize_edge_id(buffer, *offset, "CreateEdge")?;
    advance(offset, 8)?;
    let source = deserialize_node_id(buffer, *offset, "CreateEdge source")?;
    advance(offset, 8)?;
    let target = deserialize_node_id(buffer, *offset, "CreateEdge target")?;
    advance(offset, 8)?;
    let label = read_label(buffer, offset, "CreateEdge label")?;
    let (properties, valid_from) =
        read_props_and_valid_from(buffer, offset, version, tx_timestamp)?;
    let provenance = read_provenance(buffer, offset, version)?;
    Ok(WalOperation::CreateEdge {
        edge_id,
        source,
        target,
        label,
        properties,
        valid_from,
        provenance,
    })
}

fn parse_update_node_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let node_id = deserialize_node_id(buffer, *offset, "UpdateNode")?;
    advance(offset, 8)?;
    let version_id = deserialize_version_id(buffer, *offset, "UpdateNode")?;
    advance(offset, 8)?;
    let (label, properties, valid_from) = if version >= WAL_VERSION {
        let label = read_label(buffer, offset, "UpdateNode label")?;
        let (props, valid_from) = read_props_and_valid_from(buffer, offset, version, tx_timestamp)?;
        (label, props, valid_from)
    } else {
        (
            crate::core::interning::InternedString::from_raw(0),
            PropertyMap::new(),
            tx_timestamp,
        )
    };
    let provenance = read_provenance(buffer, offset, version)?;
    Ok(WalOperation::UpdateNode {
        node_id,
        version_id,
        label,
        properties,
        valid_from,
        provenance,
    })
}

fn parse_update_edge_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    // Upfront check is required: for V1 it pre-validates EdgeId+VersionId+LabelId (20 bytes)
    // as a unit, producing the "UpdateEdge" error message that tests assert on.
    // Removing it would shift the failure to read_label with a different message.
    let required = if version >= WAL_VERSION { 20 } else { 16 };
    require_bytes(buffer, *offset, required, "UpdateEdge")?;
    let edge_id = deserialize_edge_id(buffer, *offset, "UpdateEdge")?;
    advance(offset, 8)?;
    let version_id = deserialize_version_id(buffer, *offset, "UpdateEdge")?;
    advance(offset, 8)?;
    let (label, properties, valid_from) = if version >= WAL_VERSION {
        let label = read_label(buffer, offset, "UpdateEdge label")?;
        let (props, valid_from) = read_props_and_valid_from(buffer, offset, version, tx_timestamp)?;
        (label, props, valid_from)
    } else {
        (
            crate::core::interning::InternedString::from_raw(0),
            PropertyMap::new(),
            tx_timestamp,
        )
    };
    let provenance = read_provenance(buffer, offset, version)?;
    Ok(WalOperation::UpdateEdge {
        edge_id,
        version_id,
        label,
        properties,
        valid_from,
        provenance,
    })
}

fn parse_delete_node_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let node_id = deserialize_node_id(buffer, *offset, "DeleteNode")?;
    advance(offset, 8)?;
    let valid_from = if version >= WAL_VERSION {
        let (ts, ts_len) = HybridTimestamp::deserialize(&buffer[*offset..])?;
        advance(offset, ts_len)?;
        ts
    } else {
        tx_timestamp
    };
    Ok(WalOperation::DeleteNode {
        node_id,
        valid_from,
    })
}

fn parse_delete_edge_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let edge_id = deserialize_edge_id(buffer, *offset, "DeleteEdge")?;
    advance(offset, 8)?;
    let valid_from = if version >= WAL_VERSION {
        let (ts, ts_len) = HybridTimestamp::deserialize(&buffer[*offset..])?;
        advance(offset, ts_len)?;
        ts
    } else {
        tx_timestamp
    };
    Ok(WalOperation::DeleteEdge {
        edge_id,
        valid_from,
    })
}

fn parse_checkpoint_op(buffer: &[u8], offset: &mut usize) -> Result<WalOperation> {
    // LSN (8 bytes) + HybridTimestamp (12 bytes) = 20 bytes
    require_bytes(buffer, *offset, 20, "Checkpoint")?;
    let cp_lsn = LSN(u64::from_le_bytes(
        buffer[*offset..*offset + 8].try_into().unwrap(),
    ));
    advance(offset, 8)?;
    let (cp_timestamp, consumed) = HybridTimestamp::deserialize(&buffer[*offset..])?;
    advance(offset, consumed)?;
    Ok(WalOperation::Checkpoint {
        lsn: cp_lsn,
        timestamp: cp_timestamp,
    })
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
    let mut cur = offset;

    // Need at least 24 bytes for LSN (8) + HybridTimestamp (12) + checksum (4)
    require_bytes(buffer, cur, 24, "WAL entry header")?;

    let lsn = LSN(u64::from_le_bytes(
        buffer[cur..cur + 8].try_into().unwrap(), // Safe: require_bytes verified 24 bytes
    ));
    advance(&mut cur, 8)?;

    let (timestamp, _) = HybridTimestamp::deserialize(&buffer[cur..]).map_err(|e| {
        StorageError::CorruptedData(format!("Failed to deserialize timestamp: {}", e))
    })?;
    advance(&mut cur, 12)?;

    let checksum = u32::from_le_bytes(
        buffer[cur..cur + 4].try_into().unwrap(), // Safe: require_bytes verified 24 bytes
    );
    advance(&mut cur, 4)?;

    if cur >= buffer.len() {
        return Err(StorageError::CorruptedData(
            "Insufficient buffer size for operation type".to_string(),
        )
        .into());
    }
    let op_type = buffer[cur];
    advance(&mut cur, 1)?;

    let operation = match op_type {
        OP_CREATE_NODE => parse_create_node_op(buffer, &mut cur, version, timestamp)?,
        OP_CREATE_EDGE => parse_create_edge_op(buffer, &mut cur, version, timestamp)?,
        OP_UPDATE_NODE => parse_update_node_op(buffer, &mut cur, version, timestamp)?,
        OP_UPDATE_EDGE => parse_update_edge_op(buffer, &mut cur, version, timestamp)?,
        OP_DELETE_NODE => parse_delete_node_op(buffer, &mut cur, version, timestamp)?,
        OP_DELETE_EDGE => parse_delete_edge_op(buffer, &mut cur, version, timestamp)?,
        OP_CHECKPOINT => parse_checkpoint_op(buffer, &mut cur)?,
        OP_DECLARE_UNIQUE_CONSTRAINT => {
            let label = read_label(buffer, &mut cur, "DeclareUniqueConstraint.label")?;
            let property = read_label(buffer, &mut cur, "DeclareUniqueConstraint.property")?;
            WalOperation::DeclareUniqueConstraint { label, property }
        }
        OP_DROP_UNIQUE_CONSTRAINT => {
            let label = read_label(buffer, &mut cur, "DropUniqueConstraint.label")?;
            let property = read_label(buffer, &mut cur, "DropUniqueConstraint.property")?;
            WalOperation::DropUniqueConstraint { label, property }
        }
        _ => {
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
    hasher.update(&buffer[start_offset + 24..cur]);
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
    let bytes_consumed = cur - start_offset;
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
mod tests;

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn test_repro_fuzz_update_edge_panic() {
        // Failing input from fuzzer:
        // [71, 87, 65, 76, 1, 190, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 40, 1, 1, 1, 1, 1, 71, 87, 65, 76, 0, 4, 0, 0, 0, 1, 40, 1, 1, 1, 1, 1, 71, 87, 65, 76, 76, 0, 0, 0]
        let data = vec![
            71, 87, 65, 76, 1, // Header: GWAL, Ver 1
            190, 0, 0, 0, 0, 0, 0, 0, // LSN: 190
            0, 1, 1, 1, 1, 40, 1, 1, 1, 1, 1, 71, // Timestamp (12 bytes)
            87, 65, 76, 0, // Checksum (4 bytes)
            4, // OpType: 4 (UpdateEdge)
            0, 0, 0, 1, 40, 1, 1, 1, // EdgeId (8 bytes)
            1, 1, 71, 87, 65, 76, 76,
            0, // VersionId (8 bytes)
               // Total length: 48 bytes
               // Missing LabelId (4 bytes) required for Ver 1
        ];

        // Offset 5 to skip header
        let result = parse_entry_at(&data, 5, 1);

        // Before fix: Panics with index out of bounds
        // After fix: Returns Error
        assert!(
            result.is_err(),
            "Should return error for truncated buffer, got {:?}",
            result
        );
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        // Fuzz parse_entry_at with arbitrary bytes
        #[test]
        fn fuzz_parse_entry_at(
            bytes in prop::collection::vec(any::<u8>(), 0..2048),
            offset in 0..100usize,
            version in 0..2u8
        ) {
            // Should not panic
            let _ = parse_entry_at(&bytes, offset, version);
        }
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_read_segment_exactly_max_size_allowed() {
        // 🛡️ Sentry Test: Verify read_segment allows file of exactly MAX_SEGMENT_SIZE.
        // This targets mutants that change `>` to `>=`.
        let dir = TempDir::new().unwrap();
        let segment_path = dir.path().join("max_size.log");

        let mut file = File::create(&segment_path).unwrap();

        // Write header
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION]).unwrap();

        // Seek to exact MAX_SEGMENT_SIZE (sparse file)
        file.set_len(MAX_SEGMENT_SIZE).unwrap();

        drop(file);

        // Read segment - should succeed (return empty entries or corruption error, but NOT "too large").
        let result = read_segment(&segment_path, LSN(1));

        match result {
            Ok(_) => {
                // Success is fine (e.g. if sparse zeros are skipped or interpreted as empty)
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    !msg.contains("too large"),
                    "Should not reject max size file. Error was: {}",
                    msg
                );
            }
        }
    }

    #[test]
    fn test_read_segment_header_only() {
        // 🛡️ Sentry Test: Verify read_segment handles file with ONLY header (5 bytes).
        // This targets mutants that change `>=` to `>` in header size check.
        let dir = TempDir::new().unwrap();
        let segment_path = dir.path().join("header_only.log");

        let mut file = File::create(&segment_path).unwrap();
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION]).unwrap();
        // Total size = 5 bytes.
        drop(file);

        let result = read_segment(&segment_path, LSN(1));

        assert!(result.is_ok(), "Should accept header-only segment");
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_parse_entry_at_exact_header_size() {
        // 🛡️ Sentry Test: Verify parse_entry_at behavior with exactly 24 bytes (header size).
        // Targets `>` vs `>=` in `if current_offset.checked_add(24)? > buffer.len()`.

        // 24 bytes buffer
        let buffer = vec![0u8; 24];

        let result = parse_entry_at(&buffer, 0, WAL_VERSION);

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        // We expect it to pass the first check (24 !> 24) and fail the op-type check.
        assert!(
            msg.contains("operation type"),
            "Should fail at op type check, not header check. Got: {}",
            msg
        );
    }

    #[test]
    fn test_parse_entry_at_exact_header_and_op_type() {
        // 🛡️ Sentry Test: Verify parse_entry_at behavior with exactly 25 bytes (header + op type).
        // Targets `>` vs `>=` in `if current_offset >= buffer.len()`.

        let mut buffer = vec![0u8; 25];
        // LSN=0, TS=0, Checksum=0.
        // OpType = 255 (Unknown) at index 24.
        buffer[24] = 255;

        let result = parse_entry_at(&buffer, 0, WAL_VERSION);

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Unknown WAL operation type"),
            "Should read op type and fail validation. Got: {}",
            msg
        );
    }

    #[test]
    fn test_bolt_pre_allocate_segment_capacity() {
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        let file_path = dir.path().join("1.log");
        let mut file = std::fs::File::create(&file_path).unwrap();

        // Write magic header and some dummy data to make buffer large enough
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&super::WAL_MAGIC);
        buffer.push(super::WAL_VERSION); // Version

        // Add padding to make the file size larger (e.g., 1024 bytes)
        buffer.extend(vec![0; 1024 - buffer.len()]);
        file.write_all(&buffer).unwrap();
        file.sync_all().unwrap();

        let entries = read_segment(&file_path, crate::storage::LSN(1)).unwrap();

        // 1024 / 128 = 8. Since we expect capacity_hint = buffer.len() / 128
        assert!(
            entries.capacity() >= 8,
            "⚡ Bolt: Vector should be pre-allocated with capacity based on file size. Capacity was {}",
            entries.capacity()
        );
    }
}
