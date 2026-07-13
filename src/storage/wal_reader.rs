//! WAL segment reader.
//!
//! This module provides a standalone function for reading WAL segments
//! from disk for recovery purposes.

use std::path::Path;
use std::sync::Arc;

use super::wal::segment_reader;
use super::wal::{LSN, WalEntry};
use crate::core::error::Result;
use crate::encryption::cipher::Cipher;

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

/// Like [`read_wal_entries`], but decrypts version-2 (encrypted) WAL segments
/// with the supplied cipher. Passing `None` reproduces [`read_wal_entries`].
///
/// Recovery replay must use this when the WAL was written encrypted, otherwise
/// encrypted segments cannot be read back (the recovery read path would
/// otherwise fail closed on the first encrypted segment).
///
/// # Arguments
///
/// * `wal_dir` - Path to the WAL directory containing segment files
/// * `start_lsn` - Only entries with LSN >= this value are returned
/// * `cipher` - Optional cipher for decrypting version-2 segments
pub fn read_wal_entries_with_cipher(
    wal_dir: &Path,
    start_lsn: LSN,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<Vec<WalEntry>> {
    segment_reader::read_entries_from_dir_with_cipher(wal_dir, start_lsn, cipher)
}
