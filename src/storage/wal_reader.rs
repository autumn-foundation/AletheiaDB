//! WAL segment reader.
//!
//! This module provides a standalone function for reading WAL segments
//! from disk for recovery purposes.

use std::path::Path;

use super::wal::segment_reader;
use super::wal::{LSN, WalEntry};
use crate::utils::error::Result;

/// Read WAL entries from a directory, starting from the specified LSN.
///
/// This is a standalone function that can be used for recovery without
/// requiring an active WAL writer. It reads all segment files in the directory
/// and parses entries that have LSN >= start_lsn.
///
/// # Memory Warning
///
/// This loads ALL entries into memory. For large WALs, use [`read_wal_entries_iter`] instead.
///
/// # Arguments
///
/// * `wal_dir` - Path to the WAL directory containing segment files
/// * `start_lsn` - Only entries with LSN >= this value are returned
///
/// # Returns
///
/// A vector of WAL entries sorted by LSN.
pub fn read_wal_entries(wal_dir: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    segment_reader::read_entries_from_dir(wal_dir, start_lsn)
}

/// Read WAL entries from a directory lazily.
///
/// This returns an iterator that reads segments one by one, preventing OOM
/// for large WALs.
pub fn read_wal_entries_iter(
    wal_dir: &Path,
    start_lsn: LSN,
) -> Result<segment_reader::WalDirectoryIterator> {
    segment_reader::read_entries_iter(wal_dir, start_lsn)
}
