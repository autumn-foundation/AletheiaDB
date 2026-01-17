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

    while offset < buffer.len() {
        // Phase 2: Need at least 24 bytes for LSN (8) + HybridTimestamp (12) + checksum (4)
        if offset + 24 > buffer.len() {
            break;
        }

        // Read LSN (8 bytes)
        let lsn = LSN(u64::from_le_bytes([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
            buffer[offset + 4],
            buffer[offset + 5],
            buffer[offset + 6],
            buffer[offset + 7],
        ]));
        offset += 8;

        // Read timestamp (12 bytes: Phase 2 HybridTimestamp)
        let (timestamp, _) = HybridTimestamp::deserialize(&buffer[offset..])
            .map_err(|e| StorageError::CorruptedData(format!("Failed to deserialize timestamp: {}", e)))?;
        offset += 12;

        // Read checksum (4 bytes)
        let checksum = u32::from_le_bytes([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
        ]);
        offset += 4;

        // Read operation type
        if offset >= buffer.len() {
            break;
        }
        let op_type = buffer[offset];
        offset += 1;

        // Parse operation data based on type and version
        let operation = match op_type {
            1 => {
                // CreateNode
                if offset + 12 > buffer.len() {
                    break;
                }
                let node_id = deserialize_node_id(&buffer, offset, "CreateNode")?;
                offset += 8;

                let label_len = u32::from_le_bytes([
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                ]) as usize;
                offset += 4;

                if offset + label_len > buffer.len() {
                    break;
                }
                let label =
                    String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                offset += label_len;

                // V1+: deserialize properties and temporal
                let (properties, temporal) = if version >= WAL_VERSION {
                    let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                    offset += props_len;
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
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
                if offset + 28 > buffer.len() {
                    break;
                }
                let edge_id = deserialize_edge_id(&buffer, offset, "CreateEdge")?;
                offset += 8;

                let source = deserialize_node_id(&buffer, offset, "CreateEdge source")?;
                offset += 8;

                let target = deserialize_node_id(&buffer, offset, "CreateEdge target")?;
                offset += 8;

                let label_len = u32::from_le_bytes([
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                ]) as usize;
                offset += 4;

                if offset + label_len > buffer.len() {
                    break;
                }
                let label =
                    String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                offset += label_len;

                let (properties, temporal) = if version >= WAL_VERSION {
                    let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                    offset += props_len;
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
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
                if offset + 16 > buffer.len() {
                    break;
                }
                let node_id = deserialize_node_id(&buffer, offset, "UpdateNode")?;
                offset += 8;

                let version_id = deserialize_version_id(&buffer, offset, "UpdateNode")?;
                offset += 8;

                let (label, properties, temporal) = if version >= WAL_VERSION {
                    let label_len = u32::from_le_bytes([
                        buffer[offset],
                        buffer[offset + 1],
                        buffer[offset + 2],
                        buffer[offset + 3],
                    ]) as usize;
                    offset += 4;

                    if offset + label_len > buffer.len() {
                        break;
                    }
                    let lbl =
                        String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                    offset += label_len;

                    let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                    offset += props_len;
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
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
                if offset + 16 > buffer.len() {
                    break;
                }
                let edge_id = deserialize_edge_id(&buffer, offset, "UpdateEdge")?;
                offset += 8;

                let version_id = deserialize_version_id(&buffer, offset, "UpdateEdge")?;
                offset += 8;

                let (label, properties, temporal) = if version >= WAL_VERSION {
                    let label_len = u32::from_le_bytes([
                        buffer[offset],
                        buffer[offset + 1],
                        buffer[offset + 2],
                        buffer[offset + 3],
                    ]) as usize;
                    offset += 4;

                    if offset + label_len > buffer.len() {
                        break;
                    }
                    let lbl =
                        String::from_utf8_lossy(&buffer[offset..offset + label_len]).to_string();
                    offset += label_len;

                    let (props, props_len) = PropertyMap::deserialize(&buffer[offset..])?;
                    offset += props_len;
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
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
                // Checkpoint
                if offset + 16 > buffer.len() {
                    break;
                }
                let cp_lsn = LSN(u64::from_le_bytes([
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                    buffer[offset + 4],
                    buffer[offset + 5],
                    buffer[offset + 6],
                    buffer[offset + 7],
                ]));
                offset += 8;

                let cp_timestamp = i64::from_le_bytes([
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                    buffer[offset + 4],
                    buffer[offset + 5],
                    buffer[offset + 6],
                    buffer[offset + 7],
                ]);
                offset += 8;

                WalOperation::Checkpoint {
                    lsn: cp_lsn,
                    timestamp: HybridTimestamp::new_unchecked(cp_timestamp, 0),
                }
            }
            6 => {
                // DeleteNode
                if offset + 8 > buffer.len() {
                    break;
                }
                let node_id = deserialize_node_id(&buffer, offset, "DeleteNode")?;
                offset += 8;

                let temporal = if version >= WAL_VERSION {
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
                    temp
                } else {
                    BiTemporalInterval::current(timestamp)
                };

                WalOperation::DeleteNode { node_id, temporal }
            }
            7 => {
                // DeleteEdge
                if offset + 8 > buffer.len() {
                    break;
                }
                let edge_id = deserialize_edge_id(&buffer, offset, "DeleteEdge")?;
                offset += 8;

                let temporal = if version >= WAL_VERSION {
                    let (temp, temp_len) = BiTemporalInterval::deserialize(&buffer[offset..])?;
                    offset += temp_len;
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

        // Only include entries >= start_lsn
        if lsn >= start_lsn {
            let entry = WalEntry {
                lsn,
                timestamp,
                operation,
                checksum,
            };
            entries.push(entry);
        }
    }

    Ok(entries)
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
}
