//! WAL Segment Reader.
//!
//! This module provides standalone functions for reading WAL segments from disk
//! for recovery purposes. It does not require any WAL writer state.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::core::hlc::HybridTimestamp;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::property::PropertyMap;
use crate::core::temporal::BiTemporalInterval;
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

    Ok(entries)
}

/// Read WAL entries from a single segment file.
///
/// # Arguments
///
/// * `path` - Path to the segment file
/// * `start_lsn` - Only entries with LSN >= this value are returned
///
/// # Returns
///
/// A vector of WAL entries from this segment.
pub fn read_segment(path: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()), // File doesn't exist yet, return empty
    };

    let mut reader = std::io::BufReader::new(file);
    let mut entries = Vec::new();
    let mut buffer = Vec::new();

    // Read entire file into buffer
    reader
        .read_to_end(&mut buffer)
        .map_err(|e| StorageError::IoError(format!("Failed to read WAL segment: {}", e)))?;

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
        match parse_entry_at(&buffer, offset, version) {
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

            let label_len = u32::from_le_bytes(
                buffer[current_offset..current_offset + 4]
                    .try_into()
                    .unwrap(), // Safe due to buffer length check above
            ) as usize;
            current_offset += 4;

            // Use saturating_add to prevent overflow
            if current_offset.saturating_add(label_len) > buffer.len() {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for CreateNode label".to_string(),
                )
                .into());
            }
            let label =
                String::from_utf8_lossy(&buffer[current_offset..current_offset + label_len])
                    .to_string();
            current_offset += label_len;

            // V1+: deserialize properties and temporal
            let (properties, temporal) = if version >= WAL_VERSION {
                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                current_offset += props_len;
                let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[current_offset..])?;
                current_offset += temp_len;
                (props, temp)
            } else {
                (PropertyMap::new(), BiTemporalInterval::current(timestamp))
            };

            WalOperation::CreateNode {
                node_id,
                label,
                properties,
                temporal,
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

            let label_len = u32::from_le_bytes(
                buffer[current_offset..current_offset + 4]
                    .try_into()
                    .unwrap(), // Safe due to buffer length check above
            ) as usize;
            current_offset += 4;

            // Use saturating_add to prevent overflow
            if current_offset.saturating_add(label_len) > buffer.len() {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for CreateEdge label".to_string(),
                )
                .into());
            }
            let label =
                String::from_utf8_lossy(&buffer[current_offset..current_offset + label_len])
                    .to_string();
            current_offset += label_len;

            let (properties, temporal) = if version >= WAL_VERSION {
                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                current_offset += props_len;
                let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[current_offset..])?;
                current_offset += temp_len;
                (props, temp)
            } else {
                (PropertyMap::new(), BiTemporalInterval::current(timestamp))
            };

            WalOperation::CreateEdge {
                edge_id,
                source,
                target,
                label,
                properties,
                temporal,
            }
        }
        3 => {
            // UpdateNode
            if current_offset + 16 > buffer.len() {
                return Err(StorageError::CorruptedData(
                    "Insufficient buffer size for UpdateNode".to_string(),
                )
                .into());
            }
            let node_id = deserialize_node_id(buffer, current_offset, "UpdateNode")?;
            current_offset += 8;

            let version_id = deserialize_version_id(buffer, current_offset, "UpdateNode")?;
            current_offset += 8;

            let (label, properties, temporal) = if version >= WAL_VERSION {
                let label_len = u32::from_le_bytes([
                    buffer[current_offset],
                    buffer[current_offset + 1],
                    buffer[current_offset + 2],
                    buffer[current_offset + 3],
                ]) as usize;
                current_offset += 4;

                if current_offset + label_len > buffer.len() {
                    return Err(StorageError::CorruptedData(
                        "Insufficient buffer size for UpdateNode label".to_string(),
                    )
                    .into());
                }
                let lbl =
                    String::from_utf8_lossy(&buffer[current_offset..current_offset + label_len])
                        .to_string();
                current_offset += label_len;

                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                current_offset += props_len;
                let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[current_offset..])?;
                current_offset += temp_len;
                (lbl, props, temp)
            } else {
                (
                    String::new(),
                    PropertyMap::new(),
                    BiTemporalInterval::current(timestamp),
                )
            };

            WalOperation::UpdateNode {
                node_id,
                version_id,
                label,
                properties,
                temporal,
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

            let (label, properties, temporal) = if version >= WAL_VERSION {
                let label_len = u32::from_le_bytes([
                    buffer[current_offset],
                    buffer[current_offset + 1],
                    buffer[current_offset + 2],
                    buffer[current_offset + 3],
                ]) as usize;
                current_offset += 4;

                if current_offset + label_len > buffer.len() {
                    return Err(StorageError::CorruptedData(
                        "Insufficient buffer size for UpdateEdge label".to_string(),
                    )
                    .into());
                }
                let lbl =
                    String::from_utf8_lossy(&buffer[current_offset..current_offset + label_len])
                        .to_string();
                current_offset += label_len;

                let (props, props_len) = PropertyMap::deserialize(&buffer[current_offset..])?;
                current_offset += props_len;
                let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[current_offset..])?;
                current_offset += temp_len;
                (lbl, props, temp)
            } else {
                (
                    String::new(),
                    PropertyMap::new(),
                    BiTemporalInterval::current(timestamp),
                )
            };

            WalOperation::UpdateEdge {
                edge_id,
                version_id,
                label,
                properties,
                temporal,
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

            let temporal = if version >= WAL_VERSION {
                let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[current_offset..])?;
                current_offset += temp_len;
                temp
            } else {
                BiTemporalInterval::current(timestamp)
            };

            WalOperation::DeleteNode { node_id, temporal }
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

            let temporal = if version >= WAL_VERSION {
                let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[current_offset..])?;
                current_offset += temp_len;
                temp
            } else {
                BiTemporalInterval::current(timestamp)
            };

            WalOperation::DeleteEdge { edge_id, temporal }
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
            label: "Person".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
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
                assert_eq!(label, "Person");
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
            label: "KNOWS".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
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
                assert_eq!(label, "KNOWS");
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
            label: "UpdatedPerson".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
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
                assert_eq!(label, "UpdatedPerson");
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
            label: "UPDATED_KNOWS".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
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
                assert_eq!(label, "UPDATED_KNOWS");
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
            temporal: BiTemporalInterval::current(time::now()),
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
            temporal: BiTemporalInterval::current(time::now()),
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
            label: "First".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
        };
        let entry1 = WalEntry::new(LSN(1), operation1);

        let operation2 = WalOperation::CreateNode {
            node_id: NodeId::new(2).unwrap(),
            label: "Second".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
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
                assert_eq!(label, "Second");
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

        // Label
        let label = "TestNode";
        buffer.extend_from_slice(&(label.len() as u32).to_le_bytes());
        buffer.extend_from_slice(label.as_bytes());

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
                temporal,
            } => {
                assert_eq!(node_id.as_u64(), 123);
                assert_eq!(parsed_label, "TestNode");
                // Version 0 should have empty properties
                assert!(properties.is_empty());
                // Temporal should be set to current(timestamp)
                assert_eq!(temporal.transaction_time().start(), timestamp);
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
            label: "Person".to_string(),
            properties: PropertyMap::new(),
            temporal: BiTemporalInterval::current(time::now()),
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
}
