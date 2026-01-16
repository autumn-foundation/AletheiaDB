//! Hybrid Logical Clock (HLC) implementation for distributed temporal consistency.
//!
//! HLCs combine physical wallclock time with logical counters to provide:
//! - Monotonic ordering despite clock skew
//! - Causality preservation across distributed nodes
//! - Human-readable wallclock semantics for temporal queries

use crate::utils::error::StorageError;

/// Hybrid Logical Clock timestamp combining wallclock and logical components.
///
/// # Structure
/// - `wallclock`: Physical time in microseconds since Unix epoch
/// - `logical`: Counter incremented when wallclock doesn't advance
///
/// # Ordering
/// HLCs are ordered lexicographically: first by wallclock, then by logical counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HybridTimestamp {
    /// Physical wallclock time in microseconds since Unix epoch.
    wallclock: i64,
    /// Logical counter for ordering events with identical wallclock.
    logical: u32,
}

impl HybridTimestamp {
    /// Create a new HybridTimestamp with the given wallclock and logical components.
    #[inline]
    pub const fn new(wallclock: i64, logical: u32) -> Self {
        HybridTimestamp { wallclock, logical }
    }

    /// Get the wallclock component.
    #[inline]
    pub const fn wallclock(&self) -> i64 {
        self.wallclock
    }

    /// Get the logical component.
    #[inline]
    pub const fn logical(&self) -> u32 {
        self.logical
    }

    /// Generate a new timestamp for a send event.
    ///
    /// # HLC Algorithm
    /// - If `new_wallclock` > `self.wallclock`: Use new wallclock, reset logical to 0
    /// - Otherwise: Keep max(wallclock, new_wallclock), increment logical
    ///
    /// This ensures monotonicity while preserving wallclock semantics.
    #[inline]
    pub fn send(&self, new_wallclock: i64) -> Self {
        if new_wallclock > self.wallclock {
            // Wallclock advanced - reset logical counter
            HybridTimestamp {
                wallclock: new_wallclock,
                logical: 0,
            }
        } else {
            // Wallclock didn't advance - increment logical counter
            HybridTimestamp {
                wallclock: self.wallclock,
                logical: self.logical + 1,
            }
        }
    }

    /// Serialize this HybridTimestamp to bytes.
    ///
    /// # Binary Format
    /// ```text
    /// [wallclock:8][logical:4]
    /// ```
    /// Total: 12 bytes, little-endian
    pub fn serialize(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(12);
        self.serialize_into(&mut buffer);
        buffer
    }

    /// Serialize into an existing buffer.
    pub fn serialize_into(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.wallclock.to_le_bytes());
        buffer.extend_from_slice(&self.logical.to_le_bytes());
    }

    /// Deserialize a HybridTimestamp from bytes.
    ///
    /// Returns the HybridTimestamp and number of bytes consumed (always 12).
    pub fn deserialize(bytes: &[u8]) -> Result<(Self, usize), StorageError> {
        if bytes.len() < 12 {
            return Err(StorageError::CorruptedData(format!(
                "Buffer too short for HybridTimestamp: {} bytes (need 12)",
                bytes.len()
            )));
        }
        let wallclock = i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        let logical = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        Ok((HybridTimestamp { wallclock, logical }, 12))
    }
}
