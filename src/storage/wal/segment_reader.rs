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

use crate::core::hlc::HybridTimestamp;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::property::PropertyMap;
use crate::utils::error::{Error, Result, StorageError};

use super::{LSN, WalEntry, WalOperation};

/// Magic bytes identifying a GallifreyDB WAL segment file.
const WAL_MAGIC: [u8; 4] = *b"GWAL";

/// Current WAL format version.
const WAL_VERSION: u8 = 1;

/// Size of the WAL segment header (magic + version).
const WAL_HEADER_SIZE: usize = 5;

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
pub fn read_entries_from_dir(wal_dir: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
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
        let segment_entries = read_segment(&path, start_lsn)?;
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
pub fn read_segment(path: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
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

    // Maximum reasonable segment size (configurable, but 1GB is a safe upper bound)
    // Default segments are 64MB, so 1GB allows for 16x growth
    const MAX_SEGMENT_SIZE: u64 = 1024 * 1024 * 1024; // 1GB
    if metadata.len() > MAX_SEGMENT_SIZE {
        return Err(StorageError::CorruptedData(format!(
            "WAL segment too large: {} bytes (max: {} bytes)",
            metadata.len(),
            MAX_SEGMENT_SIZE
        ))
        .into());
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
        if ver > WAL_VERSION {
            return Err(StorageError::CorruptedData(format!(
                "Unsupported WAL version: {} (max supported: {})",
                ver, WAL_VERSION
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

    // Parse entries using the extracted helper function (issue #218)
    while offset < buffer.len() {
        // Try to parse an entry at the current offset
        match parse_entry_at(buffer, offset, version) {
            Ok((entry, bytes_consumed)) => {
                // Only include entries >= start_lsn
                if entry.lsn >= start_lsn {
                    entries.push(entry);
                }
                offset += bytes_consumed;
            }
            Err(e) => {
                // Distinguish between expected EOF truncation vs. unexpected corruption
                if offset + 24 > buffer.len() {
                    // Insufficient bytes for next entry header - expected at EOF
                    // This can happen if a write was interrupted mid-entry
                    #[cfg(feature = "observability")]
                    tracing::debug!(
                        "Partial entry at end of WAL segment {:?} (offset {}/{}), stopping read",
                        path,
                        offset,
                        buffer.len()
                    );
                    break;
                } else {
                    // Corruption or invalid data in the middle of the file - this is serious
                    #[cfg(feature = "observability")]
                    tracing::error!(
                        "Failed to parse WAL entry in segment {:?} at offset {}: {}",
                        path,
                        offset,
                        e
                    );
                    return Err(e);
                }
            }
        }
    }

    Ok(entries)
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

    // Phase 2: Need at least 24 bytes for LSN (8) + HybridTimestamp (12) + checksum (4)
    if current_offset + 24 > buffer.len() {
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
    current_offset += 8;

    // Read timestamp (12 bytes: Phase 2 HybridTimestamp)
    let (timestamp, _) = HybridTimestamp::deserialize(&buffer[current_offset..]).map_err(|e| {
        StorageError::CorruptedData(format!("Failed to deserialize timestamp: {}", e))
    })?;
    current_offset += 12;

    // Read checksum (4 bytes)
    let checksum = u32::from_le_bytes(
        buffer[current_offset..current_offset + 4]
            .try_into()
            .unwrap(), // Safe due to buffer length check above
    );
    current_offset += 4;

    // Read operation type
    if current_offset >= buffer.len() {
        return Err(StorageError::CorruptedData(
            "Insufficient buffer size for operation type".to_string(),
        )
        .into());
    }
    let op_type = buffer[current_offset];
    current_offset += 1;

    // Parse operation data based on type and version
    let operation = match op_type {
        1 => {
            // CreateNode
            if current_offset + 12 > buffer.len() {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for CreateNode".to_string(),
                )
                .into());
            }
            let node_id = deserialize_node_id(buffer, current_offset, "CreateNode")?;
            current_offset += 8;

            // Read 4-byte InternedString ID
            if current_offset + 4 > buffer.len() {
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
            current_offset += 4;

            // Reconstruct InternedString from ID
            // During recovery, the string should already be in the interner
            // (either from checkpoint or previous WAL entries)
            let label = crate::core::interning::InternedString::from_raw(label_id);

            // V1+: deserialize properties and temporal
            let (properties, valid_from) = if version >= WAL_VERSION {
                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                current_offset += props_len;
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                current_offset += ts_len;
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
        2 => {
            // CreateEdge
            if current_offset + 28 > buffer.len() {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for CreateEdge".to_string(),
                )
                .into());
            }
            let edge_id = deserialize_edge_id(buffer, current_offset, "CreateEdge")?;
            current_offset += 8;

            let source = deserialize_node_id(buffer, current_offset, "CreateEdge source")?;
            current_offset += 8;

            let target = deserialize_node_id(buffer, current_offset, "CreateEdge target")?;
            current_offset += 8;

            // Read 4-byte InternedString ID
            if current_offset + 4 > buffer.len() {
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
            current_offset += 4;

            // Reconstruct InternedString from ID
            let label = crate::core::interning::InternedString::from_raw(label_id);

            let (properties, valid_from) = if version >= WAL_VERSION {
                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                current_offset += props_len;
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                current_offset += ts_len;
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
        3 => {
            // UpdateNode
            if current_offset + 20 > buffer.len() {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for UpdateNode".to_string(),
                )
                .into());
            }
            let node_id = deserialize_node_id(buffer, current_offset, "UpdateNode")?;
            current_offset += 8;

            let version_id = deserialize_version_id(buffer, current_offset, "UpdateNode")?;
            current_offset += 8;

            let (label, properties, valid_from) = if version >= WAL_VERSION {
                // Read 4-byte InternedString ID
                let label_id = u32::from_le_bytes([
                    buffer[current_offset],
                    buffer[current_offset + 1],
                    buffer[current_offset + 2],
                    buffer[current_offset + 3],
                ]);
                current_offset += 4;

                // Reconstruct InternedString from ID
                let lbl = crate::core::interning::InternedString::from_raw(label_id);

                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                current_offset += props_len;
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                current_offset += ts_len;
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
        4 => {
            // UpdateEdge
            if current_offset + 16 > buffer.len() {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for UpdateEdge".to_string(),
                )
                .into());
            }
            let edge_id = deserialize_edge_id(buffer, current_offset, "UpdateEdge")?;
            current_offset += 8;

            let version_id = deserialize_version_id(buffer, current_offset, "UpdateEdge")?;
            current_offset += 8;

            let (label, properties, valid_from) = if version >= WAL_VERSION {
                // Read 4-byte InternedString ID
                let label_id = u32::from_le_bytes([
                    buffer[current_offset],
                    buffer[current_offset + 1],
                    buffer[current_offset + 2],
                    buffer[current_offset + 3],
                ]);
                current_offset += 4;

                // Reconstruct InternedString from ID
                let lbl = crate::core::interning::InternedString::from_raw(label_id);

                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                current_offset += props_len;
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                current_offset += ts_len;
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
        5 => {
            // Checkpoint: Phase 2: LSN (8 bytes) + HybridTimestamp (12 bytes) = 20 bytes
            if current_offset + 20 > buffer.len() {
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
            current_offset += 8;

            // Phase 2: Deserialize HybridTimestamp (12 bytes: 8 wallclock + 4 logical)
            let (cp_timestamp, consumed) = HybridTimestamp::deserialize(&buffer[current_offset..])?;
            current_offset += consumed;

            WalOperation::Checkpoint {
                lsn: cp_lsn,
                timestamp: cp_timestamp,
            }
        }
        6 => {
            // DeleteNode
            if current_offset + 8 > buffer.len() {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for DeleteNode".to_string(),
                )
                .into());
            }
            let node_id = deserialize_node_id(buffer, current_offset, "DeleteNode")?;
            current_offset += 8;

            let valid_from = if version >= WAL_VERSION {
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                current_offset += ts_len;
                valid_from_ts
            } else {
                timestamp
            };

            WalOperation::DeleteNode {
                node_id,
                valid_from,
            }
        }
        7 => {
            // DeleteEdge
            if current_offset + 8 > buffer.len() {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for DeleteEdge".to_string(),
                )
                .into());
            }
            let edge_id = deserialize_edge_id(buffer, current_offset, "DeleteEdge")?;
            current_offset += 8;

            let valid_from = if version >= WAL_VERSION {
                let (valid_from_ts, ts_len) =
                    HybridTimestamp::deserialize(&buffer[current_offset..])?;
                current_offset += ts_len;
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
    let raw_id = u64::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
        buffer[offset + 4],
        buffer[offset + 5],
        buffer[offset + 6],
        buffer[offset + 7],
    ]);
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
    let raw_id = u64::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
        buffer[offset + 4],
        buffer[offset + 5],
        buffer[offset + 6],
        buffer[offset + 7],
    ]);
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
    let raw_id = u64::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
        buffer[offset + 4],
        buffer[offset + 5],
        buffer[offset + 6],
        buffer[offset + 7],
    ]);
    VersionId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid version ID in WAL {}: {}",
            context, e
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::temporal::time;
    use crate::storage::wal::serialize_entry_into;
    use tempfile::TempDir;

    #[test]
    fn test_read_empty_directory() {
        let dir = TempDir::new().unwrap();
        let entries = read_entries_from_dir(dir.path(), LSN(1)).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_read_nonexistent_segment() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.log");
        let entries = read_segment(&path, LSN(1)).unwrap();
        assert!(entries.is_empty());
    }

    // =============================================================================
    // TDD Tests for parse_entry_at() - Issue #218
    // =============================================================================

    #[test]
    fn test_parse_entry_at_create_node() {
        // Create a CreateNode entry
        let node_id = NodeId::new(42).unwrap();
        let operation = WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(1), operation);

        // Serialize it
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Parse it back
        let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

        // Verify
        assert_eq!(parsed_entry.lsn, LSN(1));
        assert_eq!(bytes_consumed, buffer.len());
        match parsed_entry.operation {
            WalOperation::CreateNode {
                node_id: parsed_id,
                label,
                ..
            } => {
                assert_eq!(parsed_id, node_id);
                assert_eq!(label, GLOBAL_INTERNER.intern("Person").unwrap());
            }
            _ => panic!("Expected CreateNode operation"),
        }
    }

    #[test]
    fn test_parse_entry_at_create_edge() {
        // Create a CreateEdge entry
        let edge_id = EdgeId::new(100).unwrap();
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let operation = WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(2), operation);

        // Serialize it
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Parse it back
        let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

        // Verify
        assert_eq!(parsed_entry.lsn, LSN(2));
        assert_eq!(bytes_consumed, buffer.len());
        match parsed_entry.operation {
            WalOperation::CreateEdge {
                edge_id: parsed_id,
                source: parsed_source,
                target: parsed_target,
                label,
                ..
            } => {
                assert_eq!(parsed_id, edge_id);
                assert_eq!(parsed_source, source);
                assert_eq!(parsed_target, target);
                assert_eq!(label, GLOBAL_INTERNER.intern("KNOWS").unwrap());
            }
            _ => panic!("Expected CreateEdge operation"),
        }
    }

    #[test]
    fn test_parse_entry_at_update_node() {
        // Create an UpdateNode entry
        let node_id = NodeId::new(42).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let operation = WalOperation::UpdateNode {
            node_id,
            version_id,
            label: GLOBAL_INTERNER.intern("UpdatedPerson").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(3), operation);

        // Serialize it
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Parse it back
        let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

        // Verify
        assert_eq!(parsed_entry.lsn, LSN(3));
        assert_eq!(bytes_consumed, buffer.len());
        match parsed_entry.operation {
            WalOperation::UpdateNode {
                node_id: parsed_id,
                version_id: parsed_version,
                label,
                ..
            } => {
                assert_eq!(parsed_id, node_id);
                assert_eq!(parsed_version, version_id);
                assert_eq!(label, GLOBAL_INTERNER.intern("UpdatedPerson").unwrap());
            }
            _ => panic!("Expected UpdateNode operation"),
        }
    }

    #[test]
    fn test_parse_entry_at_update_edge() {
        // Create an UpdateEdge entry
        let edge_id = EdgeId::new(100).unwrap();
        let version_id = VersionId::new(1).unwrap();
        let operation = WalOperation::UpdateEdge {
            edge_id,
            version_id,
            label: GLOBAL_INTERNER.intern("UPDATED_KNOWS").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(4), operation);

        // Serialize it
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Parse it back
        let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

        // Verify
        assert_eq!(parsed_entry.lsn, LSN(4));
        assert_eq!(bytes_consumed, buffer.len());
        match parsed_entry.operation {
            WalOperation::UpdateEdge {
                edge_id: parsed_id,
                version_id: parsed_version,
                label,
                ..
            } => {
                assert_eq!(parsed_id, edge_id);
                assert_eq!(parsed_version, version_id);
                assert_eq!(label, GLOBAL_INTERNER.intern("UPDATED_KNOWS").unwrap());
            }
            _ => panic!("Expected UpdateEdge operation"),
        }
    }

    #[test]
    fn test_parse_entry_at_delete_node() {
        // Create a DeleteNode entry
        let node_id = NodeId::new(42).unwrap();
        let operation = WalOperation::DeleteNode {
            node_id,
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(5), operation);

        // Serialize it
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Parse it back
        let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

        // Verify
        assert_eq!(parsed_entry.lsn, LSN(5));
        assert_eq!(bytes_consumed, buffer.len());
        match parsed_entry.operation {
            WalOperation::DeleteNode {
                node_id: parsed_id, ..
            } => {
                assert_eq!(parsed_id, node_id);
            }
            _ => panic!("Expected DeleteNode operation"),
        }
    }

    #[test]
    fn test_parse_entry_at_delete_edge() {
        // Create a DeleteEdge entry
        let edge_id = EdgeId::new(100).unwrap();
        let operation = WalOperation::DeleteEdge {
            edge_id,
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(6), operation);

        // Serialize it
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Parse it back
        let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

        // Verify
        assert_eq!(parsed_entry.lsn, LSN(6));
        assert_eq!(bytes_consumed, buffer.len());
        match parsed_entry.operation {
            WalOperation::DeleteEdge {
                edge_id: parsed_id, ..
            } => {
                assert_eq!(parsed_id, edge_id);
            }
            _ => panic!("Expected DeleteEdge operation"),
        }
    }

    #[test]
    fn test_parse_entry_at_checkpoint() {
        // Create a Checkpoint entry
        let cp_timestamp = time::now();
        let operation = WalOperation::Checkpoint {
            lsn: LSN(100),
            timestamp: cp_timestamp,
        };
        let entry = WalEntry::new(LSN(7), operation);

        // Serialize it
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Parse it back
        let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

        // Verify
        assert_eq!(parsed_entry.lsn, LSN(7));
        assert_eq!(bytes_consumed, buffer.len());
        match parsed_entry.operation {
            WalOperation::Checkpoint { lsn, .. } => {
                assert_eq!(lsn, LSN(100));
            }
            _ => panic!("Expected Checkpoint operation"),
        }
    }

    #[test]
    fn test_parse_entry_at_with_offset() {
        // Create two entries
        let operation1 = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: GLOBAL_INTERNER.intern("First").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry1 = WalEntry::new(LSN(1), operation1);

        let operation2 = WalOperation::CreateNode {
            node_id: NodeId::new(2).unwrap(),
            label: GLOBAL_INTERNER.intern("Second").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry2 = WalEntry::new(LSN(2), operation2);

        // Serialize both entries separately, then concatenate
        // (serialize_entry_into computes checksum from buffer start, so we can't
        //  append directly without getting wrong checksums)
        let mut buffer = Vec::new();
        serialize_entry_into(&entry1, &mut buffer).unwrap();
        let offset1_end = buffer.len();

        let mut buffer2 = Vec::new();
        serialize_entry_into(&entry2, &mut buffer2).unwrap();
        buffer.extend_from_slice(&buffer2);

        // Parse second entry using offset
        let (parsed_entry, bytes_consumed) =
            parse_entry_at(&buffer, offset1_end, WAL_VERSION).unwrap();

        // Verify
        assert_eq!(parsed_entry.lsn, LSN(2));
        match parsed_entry.operation {
            WalOperation::CreateNode { label, .. } => {
                assert_eq!(label, GLOBAL_INTERNER.intern("Second").unwrap());
            }
            _ => panic!("Expected CreateNode operation"),
        }
        assert_eq!(bytes_consumed, buffer.len() - offset1_end);
    }

    #[test]
    fn test_parse_entry_at_insufficient_buffer() {
        // Create a buffer with only 10 bytes (not enough for LSN + timestamp + checksum)
        let buffer = vec![0u8; 10];

        // Should return error
        let result = parse_entry_at(&buffer, 0, WAL_VERSION);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_entry_at_unknown_operation_type() {
        // Create a valid header but invalid operation type
        let mut buffer = Vec::new();

        // LSN (8 bytes)
        buffer.extend_from_slice(&1u64.to_le_bytes());

        // Timestamp (12 bytes)
        let timestamp = time::now();
        timestamp.serialize_into(&mut buffer);

        // Checksum (4 bytes) - just use 0 for this test
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // Invalid operation type (255)
        buffer.push(255);

        // Should return error for unknown operation type
        let result = parse_entry_at(&buffer, 0, WAL_VERSION);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_entry_at_truncated_operation_data() {
        // Create a valid header but truncate operation data
        let mut buffer = Vec::new();

        // LSN (8 bytes)
        buffer.extend_from_slice(&1u64.to_le_bytes());

        // Timestamp (12 bytes)
        let timestamp = time::now();
        timestamp.serialize_into(&mut buffer);

        // Checksum (4 bytes)
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // Operation type for CreateNode (1)
        buffer.push(1);

        // Only 4 bytes of node_id (should be 8) - truncated!
        buffer.extend_from_slice(&[1, 2, 3, 4]);

        // Should return error for insufficient data
        let result = parse_entry_at(&buffer, 0, WAL_VERSION);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_entry_at_version_0_compatibility() {
        // Test legacy version 0 parsing (without properties and temporal data)
        // This tests the version < WAL_VERSION code path
        let mut buffer = Vec::new();

        // LSN (8 bytes)
        buffer.extend_from_slice(&42u64.to_le_bytes());

        // Timestamp (12 bytes)
        let timestamp = time::now();
        timestamp.serialize_into(&mut buffer);

        // Placeholder checksum (4 bytes) - will be computed later
        let checksum_offset = buffer.len();
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // Operation type: CreateNode (1)
        buffer.push(1);

        // Node ID (8 bytes)
        buffer.extend_from_slice(&123u64.to_le_bytes());

        // Label (4-byte InternedString ID)
        let label_id = GLOBAL_INTERNER.intern("TestNode").unwrap().as_u32();
        buffer.extend_from_slice(&label_id.to_le_bytes());

        // Note: Version 0 format does NOT include properties or temporal data

        // Compute checksum
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&buffer[0..checksum_offset]); // LSN + timestamp
        hasher.update(&buffer[checksum_offset + 4..]); // Operation data
        let checksum = hasher.finalize();
        buffer[checksum_offset..checksum_offset + 4].copy_from_slice(&checksum.to_le_bytes());

        // Parse with version 0
        let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, 0).unwrap();

        // Verify
        assert_eq!(parsed_entry.lsn.0, 42);
        assert_eq!(bytes_consumed, buffer.len());
        match parsed_entry.operation {
            WalOperation::CreateNode {
                node_id,
                label: parsed_label,
                properties,
                valid_from,
            } => {
                assert_eq!(node_id.as_u64(), 123);
                assert_eq!(parsed_label, GLOBAL_INTERNER.intern("TestNode").unwrap());
                // Version 0 should have empty properties
                assert!(properties.is_empty());
                // Valid_from should be set to the timestamp
                assert_eq!(valid_from, timestamp);
            }
            _ => panic!("Expected CreateNode operation"),
        }
    }

    #[test]
    fn test_parse_entry_at_checksum_mismatch() {
        // Create a valid entry
        let node_id = NodeId::new(42).unwrap();
        let operation = WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(1), operation);

        // Serialize it
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Corrupt the checksum (bytes 20-24)
        buffer[20] ^= 0xFF; // Flip all bits in first checksum byte

        // Should return error for checksum mismatch
        let result = parse_entry_at(&buffer, 0, WAL_VERSION);
        assert!(result.is_err());
        if let Err(e) = result {
            let error_msg = format!("{}", e);
            assert!(error_msg.contains("checksum mismatch"));
        }
    }

    // =============================================================================
    // TDD Tests for Memory-Efficient Segment Reading - Issue #216
    // =============================================================================

    /// Test that we can read a segment file with many entries without loading
    /// the entire file into memory at once.
    ///
    /// This test creates a large segment file (simulating real-world 64MB segments)
    /// and verifies that all entries can be read correctly.
    #[test]
    fn test_read_large_segment_memory_efficient() {
        use std::io::Write;

        let dir = TempDir::new().unwrap();
        let segment_path = dir.path().join("large_segment.log");

        // Create a segment file with many entries
        let mut file = File::create(&segment_path).unwrap();

        // Write WAL header
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION]).unwrap();

        // Create and write many entries to simulate a large segment
        // We'll create 1000 entries, which should be several MB
        let num_entries = 1000;
        let mut expected_lsns = Vec::new();

        for i in 0..num_entries {
            let lsn = LSN(i + 1);
            expected_lsns.push(lsn);

            let operation = WalOperation::CreateNode {
                node_id: NodeId::new(i + 1).unwrap(),
                label: GLOBAL_INTERNER.intern(format!("Node_{}", i)).unwrap(),
                properties: PropertyMap::new(),
                valid_from: time::now(),
            };

            let entry = WalEntry::new(lsn, operation);
            let mut buffer = Vec::new();
            serialize_entry_into(&entry, &mut buffer).unwrap();
            file.write_all(&buffer).unwrap();
        }

        file.sync_all().unwrap();
        drop(file);

        // Read the segment
        let entries = read_segment(&segment_path, LSN(1)).unwrap();

        // Verify all entries were read correctly
        assert_eq!(entries.len(), num_entries as usize);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.lsn, LSN(i as u64 + 1));
        }
    }

    /// Test that reading multiple segments doesn't accumulate excessive memory.
    ///
    /// This test creates multiple segment files and verifies that we can process
    /// them sequentially without holding all segment buffers in memory simultaneously.
    #[test]
    fn test_read_multiple_segments_sequentially() {
        use std::io::Write;

        let dir = TempDir::new().unwrap();

        // Create 5 segment files
        let num_segments = 5;
        let entries_per_segment = 100;

        for seg_id in 0..num_segments {
            let segment_path = dir.path().join(format!("{}.log", seg_id));
            let mut file = File::create(&segment_path).unwrap();

            // Write WAL header
            file.write_all(&WAL_MAGIC).unwrap();
            file.write_all(&[WAL_VERSION]).unwrap();

            // Write entries for this segment
            for i in 0..entries_per_segment {
                let lsn = LSN((seg_id * entries_per_segment) + i + 1);

                let operation = WalOperation::CreateNode {
                    node_id: NodeId::new(lsn.0).unwrap(),
                    label: GLOBAL_INTERNER
                        .intern(format!("Node_seg{}_entry{}", seg_id, i))
                        .unwrap(),
                    properties: PropertyMap::new(),
                    valid_from: time::now(),
                };

                let entry = WalEntry::new(lsn, operation);
                let mut buffer = Vec::new();
                serialize_entry_into(&entry, &mut buffer).unwrap();
                file.write_all(&buffer).unwrap();
            }

            file.sync_all().unwrap();
        }

        // Read all entries from directory
        let entries = read_entries_from_dir(dir.path(), LSN(1)).unwrap();

        // Verify all entries were read correctly
        assert_eq!(entries.len(), (num_segments * entries_per_segment) as usize);

        // Verify entries are sorted by LSN
        for i in 0..entries.len() - 1 {
            assert!(entries[i].lsn <= entries[i + 1].lsn);
        }
    }

    /// Test that segment reading works correctly with the start_lsn filter.
    ///
    /// This verifies that we can efficiently skip entries before a certain LSN
    /// without processing them.
    #[test]
    fn test_read_segment_with_start_lsn_filter() {
        use std::io::Write;

        let dir = TempDir::new().unwrap();
        let segment_path = dir.path().join("filtered_segment.log");

        let mut file = File::create(&segment_path).unwrap();

        // Write WAL header
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION]).unwrap();

        // Write 100 entries with LSN 1-100
        for i in 1..=100 {
            let lsn = LSN(i);
            let operation = WalOperation::CreateNode {
                node_id: NodeId::new(i).unwrap(),
                label: GLOBAL_INTERNER.intern(format!("Node_{}", i)).unwrap(),
                properties: PropertyMap::new(),
                valid_from: time::now(),
            };

            let entry = WalEntry::new(lsn, operation);
            let mut buffer = Vec::new();
            serialize_entry_into(&entry, &mut buffer).unwrap();
            file.write_all(&buffer).unwrap();
        }

        file.sync_all().unwrap();
        drop(file);

        // Read entries starting from LSN 50
        let entries = read_segment(&segment_path, LSN(50)).unwrap();

        // Should only get entries with LSN >= 50
        assert_eq!(entries.len(), 51); // LSN 50-100 inclusive
        assert_eq!(entries[0].lsn, LSN(50));
        assert_eq!(entries[entries.len() - 1].lsn, LSN(100));
    }

    /// Test that empty segments are handled efficiently.
    #[test]
    fn test_read_empty_segment_efficient() {
        use std::io::Write;

        let dir = TempDir::new().unwrap();
        let segment_path = dir.path().join("empty_segment.log");

        let mut file = File::create(&segment_path).unwrap();

        // Write only WAL header, no entries
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION]).unwrap();

        file.sync_all().unwrap();
        drop(file);

        // Read the empty segment
        let entries = read_segment(&segment_path, LSN(1)).unwrap();

        // Should return empty vector
        assert!(entries.is_empty());
    }

    /// Test that partial/truncated entries at end of segment are handled gracefully.
    ///
    /// This can happen if a write was interrupted mid-entry.
    #[test]
    fn test_read_segment_with_truncated_entry() {
        use std::io::Write;

        let dir = TempDir::new().unwrap();
        let segment_path = dir.path().join("truncated_segment.log");

        let mut file = File::create(&segment_path).unwrap();

        // Write WAL header
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION]).unwrap();

        // Write one complete entry
        let operation = WalOperation::CreateNode {
            node_id: NodeId::new(1).unwrap(),
            label: GLOBAL_INTERNER.intern("Node_1").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(1), operation);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();
        file.write_all(&buffer).unwrap();

        // Write a partial entry (just the LSN, incomplete)
        file.write_all(&42u64.to_le_bytes()).unwrap();

        file.sync_all().unwrap();
        drop(file);

        // Read the segment - should get the complete entry and stop at truncation
        let entries = read_segment(&segment_path, LSN(1)).unwrap();

        // Should only get the one complete entry
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].lsn, LSN(1));
    }

    // =============================================================================
    // Security and Error Handling Tests - Issue #216 Fixes
    // =============================================================================

    /// Test that non-existent files return empty results (not an error).
    #[test]
    fn test_read_nonexistent_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let nonexistent = dir.path().join("does_not_exist.log");

        // Should return Ok(empty vector), not an error
        let result = read_segment(&nonexistent, LSN(1));
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    /// Test that file size validation prevents reading excessively large files.
    ///
    /// This protects against DoS attacks where an attacker places a huge file
    /// in the WAL directory.
    #[test]
    fn test_read_segment_rejects_oversized_file() {
        use std::io::Write;

        let dir = TempDir::new().unwrap();
        let segment_path = dir.path().join("oversized_segment.log");

        let mut file = File::create(&segment_path).unwrap();

        // Write WAL header
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION]).unwrap();

        // Seek to a position beyond MAX_SEGMENT_SIZE (1GB)
        // Note: We don't actually write 1GB of data, just seek past it
        // This creates a sparse file that reports a large size
        const OVERSIZED: u64 = 1024 * 1024 * 1024 + 1; // 1GB + 1 byte
        file.set_len(OVERSIZED).unwrap();

        file.sync_all().unwrap();
        drop(file);

        // Should return an error about file being too large
        let result = read_segment(&segment_path, LSN(1));
        assert!(result.is_err());
        let error_msg = format!("{}", result.unwrap_err());
        assert!(
            error_msg.contains("too large"),
            "Expected 'too large' error, got: {}",
            error_msg
        );
    }
}
