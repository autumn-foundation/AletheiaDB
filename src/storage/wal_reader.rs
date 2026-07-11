//! WAL segment reader.
//!
//! This module provides a standalone function for reading WAL segments
//! from disk for recovery purposes.

use std::path::Path;

use super::wal::segment_reader;
use super::wal::{LSN, WalEntry};
use crate::core::error::Result;

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
/// A vector of WAL entries sorted by LSN.
pub fn read_wal_entries(wal_dir: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    segment_reader::read_entries_from_dir(wal_dir, start_lsn)
}

/// Read WAL entries from a directory with an explicit crash-torn-tail recovery
/// policy (Issue #3433).
///
/// When `tolerate_torn_tail` is `true` (the default recovery behavior), an
/// undecodable trailing entry in the final segment stops replay there and the
/// intact prefix is applied; when `false`, any parse failure hard-errors
/// (fail-stop recovery). See
/// [`segment_reader::read_entries_from_dir_with_options`].
pub fn read_wal_entries_with_options(
    wal_dir: &Path,
    start_lsn: LSN,
    tolerate_torn_tail: bool,
) -> Result<Vec<WalEntry>> {
    segment_reader::read_entries_from_dir_with_options(wal_dir, start_lsn, None, tolerate_torn_tail)
}
