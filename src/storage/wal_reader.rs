//! WAL segment reader.
//!
//! This module provides a standalone function for reading WAL segments
//! from disk for recovery purposes.

use std::path::Path;

use super::wal::WalEntry;
use super::wal::segment_reader;
use crate::utils::error::Result;

/// Read WAL entries from a directory, starting from the specified LSN.
///
/// This is a standalone function that can be used for recovery without
/// requiring an active WAL writer. It reads all segment files in the directory
/// and parses entries that have LSN >= start_lsn.
///
/// # Arguments
///
/// * `wal_dir` - Path to the WAL directory containing segment files
/// * `start_lsn` - Only entries with LSN >= this value are returned
///
/// # Returns
///
/// An iterator over WAL entries.
pub fn read_wal_entries(
    wal_dir: &Path,
    start_lsn: super::wal::LSN,
) -> Result<impl Iterator<Item = Result<WalEntry>>> {
    segment_reader::read_entries_from_dir(wal_dir, start_lsn)
}
