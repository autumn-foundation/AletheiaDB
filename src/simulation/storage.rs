//! In-memory simulated storage with optional fault injection.
//!
//! `SimulatedStorage` stores key → byte-vector pairs in a `HashMap`, tracking
//! cumulative bytes written so that crash-at-offset faults fire at the right
//! point in a scenario.

use std::collections::HashMap;

use crate::simulation::fault::{FaultConfig, FaultInjector};

/// Error type for simulated storage operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimStorageError {
    /// The write was blocked by a simulated crash at the configured byte offset.
    SimulatedCrash {
        /// Cumulative byte offset at which the crash was triggered.
        at_byte_offset: u64,
    },
}

impl std::fmt::Display for SimStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SimulatedCrash { at_byte_offset } => {
                write!(f, "simulated crash at byte offset {at_byte_offset}")
            }
        }
    }
}

impl std::error::Error for SimStorageError {}

/// An in-memory key-value store with configurable fault injection.
///
/// Designed to simulate the storage layer for DST scenarios without touching
/// the real file system or WAL.
#[derive(Debug)]
pub struct SimulatedStorage {
    data: HashMap<String, Vec<u8>>,
    bytes_written: u64,
    faults: Option<FaultInjector>,
}

impl SimulatedStorage {
    /// Create a fault-free in-memory storage instance.
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
            bytes_written: 0,
            faults: None,
        }
    }

    /// Create a storage instance with the given fault configuration and seed.
    pub fn with_faults(config: FaultConfig, seed: u64) -> Self {
        Self {
            data: HashMap::new(),
            bytes_written: 0,
            faults: Some(FaultInjector::new(config, seed)),
        }
    }

    /// Write `data` under `key`.
    ///
    /// Returns `Err(SimStorageError::SimulatedCrash)` if the write would push
    /// the cumulative byte count past the crash-at-offset threshold.
    pub fn write(&mut self, key: &str, data: &[u8]) -> Result<(), SimStorageError> {
        let new_total = self.bytes_written + data.len() as u64;

        // Check crash threshold *before* writing.
        if let Some(ref fi) = self.faults
            && fi.should_crash_at(new_total)
        {
            return Err(SimStorageError::SimulatedCrash {
                at_byte_offset: new_total,
            });
        }

        let mut payload = data.to_vec();
        if let Some(ref fi) = self.faults {
            fi.try_corrupt(&mut payload);
        }

        self.data.insert(key.to_owned(), payload);
        self.bytes_written = new_total;
        Ok(())
    }

    /// Read the bytes stored under `key`, or `None` if absent.
    pub fn read(&self, key: &str) -> Option<&[u8]> {
        self.data.get(key).map(Vec::as_slice)
    }

    /// Total bytes submitted to `write` across all successful calls.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }
}

impl Default for SimulatedStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_roundtrip() {
        let mut s = SimulatedStorage::new();
        s.write("k", b"hello").unwrap();
        assert_eq!(s.read("k").unwrap(), b"hello");
    }

    #[test]
    fn bytes_written_accumulates() {
        let mut s = SimulatedStorage::new();
        s.write("a", &[1, 2]).unwrap();
        s.write("b", &[3]).unwrap();
        assert_eq!(s.bytes_written(), 3);
    }

    #[test]
    fn crash_at_offset_blocks_write() {
        let cfg = FaultConfig::default().with_crash_at_offset(4);
        let mut s = SimulatedStorage::with_faults(cfg, 0);
        s.write("x", &[0; 3]).unwrap(); // 3 bytes → ok
        let err = s.write("y", &[0; 3]).unwrap_err(); // would reach 6 → crash
        assert!(matches!(err, SimStorageError::SimulatedCrash { .. }));
    }
}
