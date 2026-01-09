//! WAL segment reader.
//!
//! This module provides a standalone function for reading WAL segments
//! from disk for recovery purposes.

use std::path::Path;

use super::wal::{DurabilityMode, LSN, WalConfig, WalEntry, WriteAheadLog};
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
/// A vector of WAL entries sorted by LSN.
pub fn read_wal_entries(wal_dir: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    // Create a minimal config just for reading
    let config = WalConfig {
        wal_dir: wal_dir.to_path_buf(),
        segment_size: 64 * 1024 * 1024, // Default, not used for reading
        segments_to_retain: 10,         // Default, not used for reading
        durability_mode: DurabilityMode::Synchronous, // Not used for reading
    };

    // Create a minimal WAL instance for reading
    // Note: WriteAheadLog::new() doesn't create segment files, just initializes the struct
    let wal = WriteAheadLog::new(config)?;
    wal.read_from(start_lsn)
}
