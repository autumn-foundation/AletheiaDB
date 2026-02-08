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
use std::path::{Path, PathBuf};

use crate::core::hlc::HybridTimestamp;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::property::PropertyMap;
use crate::utils::error::{Error, Result, StorageError};

use super::{LSN, WalEntry, WalOperation};

/// Magic bytes identifying a AletheiaDB WAL segment file.
const WAL_MAGIC: [u8; 4] = *b"GWAL";

/// Current WAL format version.
const WAL_VERSION: u8 = 1;

/// Size of the WAL segment header (magic + version).
const WAL_HEADER_SIZE: usize = 5;

/// Iterator over WAL entries in a single segment file.
///
/// Buffers and sorts entries within the segment to ensure LSN ordering
/// (handling intra-segment disorder from concurrent writes), while
/// keeping memory usage bounded to the size of one segment.
pub struct WalSegmentIterator {
    entries: std::vec::IntoIter<WalEntry>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl WalSegmentIterator {
    /// Create a new iterator for a segment file.
    pub fn new(path: &Path, start_lsn: LSN) -> Result<Self> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StorageError::IoError(format!(
                    "Failed to open WAL segment {:?}: {}",
                    path, e
                ))
                .into());
            }
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
        const MAX_SEGMENT_SIZE: u64 = 1024 * 1024 * 1024; // 1GB
        if metadata.len() > MAX_SEGMENT_SIZE {
            return Err(StorageError::CorruptedData(format!(
                "WAL segment too large: {} bytes (max: {} bytes)",
                metadata.len(),
                MAX_SEGMENT_SIZE
            ))
            .into());
        }

        if metadata.len() == 0 {
            return Ok(Self {
                entries: Vec::new().into_iter(),
                path: path.to_path_buf(),
            });
        }

        // Memory-map the file
        // SAFETY: We only read from the memory map, never write. The file is opened read-only.
        let mmap = unsafe {
            memmap2::Mmap::map(&file).map_err(|e| {
                StorageError::IoError(format!("Failed to memory-map WAL segment: {}", e))
            })?
        };

        let mut entries = Vec::new();
        let mut current_offset = 0;
        let mut version = 0;

        // Initialize header
        if mmap.len() >= WAL_HEADER_SIZE && mmap[0..4] == WAL_MAGIC {
            version = mmap[4];
            if version > WAL_VERSION {
                return Err(StorageError::CorruptedData(format!(
                    "Unsupported WAL version: {} (max supported: {})",
                    version, WAL_VERSION
                ))
                .into());
            }
            current_offset = WAL_HEADER_SIZE;
        } else if !mmap.is_empty() {
            return Err(StorageError::CorruptedData(
                "Invalid WAL segment: missing GWAL magic header".to_string(),
            )
            .into());
        }

        // Parse all entries
        while current_offset < mmap.len() {
            match parse_entry_at(&mmap, current_offset, version) {
                Ok((entry, bytes_consumed)) => {
                    current_offset += bytes_consumed;
                    if entry.lsn >= start_lsn {
                        entries.push(entry);
                    }
                }
                Err(e) => {
                    // Check for expected EOF (truncation) vs corruption
                    if current_offset + 24 > mmap.len() {
                        // Truncated at EOF - treat as end of stream
                        #[cfg(feature = "observability")]
                        tracing::debug!(
                            "Partial entry at end of WAL segment {:?} (offset {}/{}), stopping read",
                            path,
                            current_offset,
                            mmap.len()
                        );
                        break;
                    } else {
                        // Actual corruption
                        #[cfg(feature = "observability")]
                        tracing::error!(
                            "Failed to parse WAL entry in segment {:?} at offset {}: {}",
                            path,
                            current_offset,
                            e
                        );
                        return Err(e);
                    }
                }
            }
        }

        // Sort entries by LSN to handle intra-segment disorder
        entries.sort_by_key(|e| e.lsn);

        Ok(Self {
            entries: entries.into_iter(),
            path: path.to_path_buf(),
        })
    }
}

impl Iterator for WalSegmentIterator {
    type Item = Result<WalEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.entries.next().map(Ok)
    }
}

/// Iterator over all WAL segments in a directory.
pub struct WalDirectoryIterator {
    segments: Vec<PathBuf>,
    current_segment_idx: usize,
    current_segment_iter: Option<WalSegmentIterator>,
    start_lsn: LSN,
}

impl WalDirectoryIterator {
    /// Create a new directory iterator.
    pub fn new(wal_dir: &Path, start_lsn: LSN) -> Result<Self> {
        let mut segments = Vec::new();
        if let Ok(dir_entries) = std::fs::read_dir(wal_dir) {
            for entry in dir_entries.flatten() {
                if let Some(seg_id) = entry
                    .file_name()
                    .to_str()
                    .filter(|name| name.ends_with(".log"))
                    .and_then(|name| name.strip_suffix(".log"))
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    segments.push((seg_id, entry.path()));
                }
            }
        }

        // Sort by segment ID
        segments.sort_by_key(|(id, _)| *id);
        let segment_paths = segments.into_iter().map(|(_, path)| path).collect();

        let mut iter = Self {
            segments: segment_paths,
            current_segment_idx: 0,
            current_segment_iter: None,
            start_lsn,
        };

        // Initialize first segment
        iter.advance_segment()?;

        Ok(iter)
    }

    fn advance_segment(&mut self) -> Result<()> {
        if self.current_segment_idx < self.segments.len() {
            let path = &self.segments[self.current_segment_idx];
            match WalSegmentIterator::new(path, self.start_lsn) {
                Ok(iter) => {
                    self.current_segment_iter = Some(iter);
                    self.current_segment_idx += 1;
                }
                Err(e) => {
                    // If we fail to open a segment, that's a critical error for recovery
                    return Err(e);
                }
            }
        } else {
            self.current_segment_iter = None;
        }
        Ok(())
    }
}

impl Iterator for WalDirectoryIterator {
    type Item = Result<WalEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(iter) = &mut self.current_segment_iter {
                match iter.next() {
                    Some(result) => return Some(result),
                    None => {
                        // Current segment finished, move to next
                        if let Err(e) = self.advance_segment() {
                            return Some(Err(e));
                        }
                    }
                }
            } else {
                // No more segments
                return None;
            }
        }
    }
}

/// Read all WAL entries from a directory, returning an iterator.
pub fn read_entries_from_dir(wal_dir: &Path, start_lsn: LSN) -> Result<WalDirectoryIterator> {
    WalDirectoryIterator::new(wal_dir, start_lsn)
}

/// Parse a single WAL entry from a buffer at the specified offset.
///
/// This function extracts the parsing logic that was previously duplicated
/// in multiple places (issue #218). It handles all WAL operation types and
/// returns both the parsed entry and the number of bytes consumed.
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
        1 => {
            // CreateNode
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
        2 => {
            // CreateEdge
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
        3 => {
            // UpdateNode
            if current_offset.checked_add(20).ok_or_else(|| {
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
        4 => {
            // UpdateEdge
            if current_offset.checked_add(16).ok_or_else(|| {
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
        5 => {
            // Checkpoint: Phase 2: LSN (8 bytes) + HybridTimestamp (12 bytes) = 20 bytes
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
        6 => {
            // DeleteNode
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
        7 => {
            // DeleteEdge
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
    use crate::storage::wal::serialization::serialize_entry_into;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_read_empty_directory() {
        let dir = TempDir::new().unwrap();
        let mut iter = read_entries_from_dir(dir.path(), LSN(1)).unwrap();
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_read_entries_from_dir_streaming() {
        let dir = TempDir::new().unwrap();

        // Create 2 segments
        for seg_id in 0..2 {
            let segment_path = dir.path().join(format!("{}.log", seg_id));
            let mut file = File::create(&segment_path).unwrap();

            file.write_all(&WAL_MAGIC).unwrap();
            file.write_all(&[WAL_VERSION]).unwrap();

            // Write 5 entries per segment
            for i in 0..5 {
                let lsn = LSN((seg_id * 5) + i + 1);
                let operation = WalOperation::CreateNode {
                    node_id: NodeId::new(lsn.0).unwrap(),
                    label: GLOBAL_INTERNER.intern("Test").unwrap(),
                    properties: PropertyMap::new(),
                    valid_from: time::now(),
                };
                let entry = WalEntry::new(lsn, operation);
                let mut buffer = Vec::new();
                serialize_entry_into(&entry, &mut buffer).unwrap();
                file.write_all(&buffer).unwrap();
            }
        }

        let iter = read_entries_from_dir(dir.path(), LSN(1)).unwrap();
        let entries: Vec<WalEntry> = iter.collect::<Result<Vec<_>>>().unwrap();

        assert_eq!(entries.len(), 10);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.lsn, LSN((i + 1) as u64));
        }
    }

    #[test]
    fn test_read_entries_from_dir_with_corrupt_segment() {
        let dir = TempDir::new().unwrap();
        let segment_path = dir.path().join("0.log");
        let mut file = File::create(&segment_path).unwrap();

        // Write magic and version
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION]).unwrap();

        // Write corrupted data (enough to pass length check but fail parsing/checksum)
        // Need > 24 bytes to trigger corruption check instead of truncation
        let garbage = [0u8; 30];
        file.write_all(&garbage).unwrap();

         // Since we parse eagerly to sort, we expect immediate error for the first segment
         let result = read_entries_from_dir(dir.path(), LSN(1));
         assert!(result.is_err());
    }

    // =============================================================================
    // TDD Tests for parse_entry_at() - Restored
    // =============================================================================

    #[test]
    fn test_parse_entry_at_create_node() {
        let node_id = NodeId::new(42).unwrap();
        let operation = WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(1), operation);

        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

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
    fn test_parse_entry_at_checksum_mismatch() {
        let node_id = NodeId::new(42).unwrap();
        let operation = WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
        };
        let entry = WalEntry::new(LSN(1), operation);

        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();

        // Corrupt the checksum (bytes 20-24)
        buffer[20] ^= 0xFF; // Flip all bits in first checksum byte

        let result = parse_entry_at(&buffer, 0, WAL_VERSION);
        assert!(result.is_err());
        if let Err(e) = result {
            let error_msg = format!("{}", e);
            assert!(error_msg.contains("checksum mismatch"));
        }
    }

    #[test]
    fn test_parse_entry_at_insufficient_buffer() {
        let buffer = vec![0u8; 10];
        let result = parse_entry_at(&buffer, 0, WAL_VERSION);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_entry_at_unknown_operation_type() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&1u64.to_le_bytes());
        let timestamp = time::now();
        timestamp.serialize_into(&mut buffer);
        buffer.extend_from_slice(&0u32.to_le_bytes());
        buffer.push(255); // Invalid op type

        let result = parse_entry_at(&buffer, 0, WAL_VERSION);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_entry_at_truncated_operation_data() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&1u64.to_le_bytes());
        let timestamp = time::now();
        timestamp.serialize_into(&mut buffer);
        buffer.extend_from_slice(&0u32.to_le_bytes());
        buffer.push(1); // CreateNode
        buffer.extend_from_slice(&[1, 2, 3, 4]); // Truncated node_id

        let result = parse_entry_at(&buffer, 0, WAL_VERSION);
        assert!(result.is_err());
    }
}
