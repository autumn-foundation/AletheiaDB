//! Hybrid Logical Clock (HLC) implementation for distributed temporal consistency.
//!
//! HLCs combine physical wallclock time with logical counters to provide:
//! - Monotonic ordering despite clock skew
//! - Causality preservation across distributed nodes
//! - Human-readable wallclock semantics for temporal queries

use crate::core::temporal::MAX_VALID_TIMESTAMP;
use crate::utils::error::{StorageError, TemporalError};

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
    /// Create a new HybridTimestamp with validation.
    ///
    /// # Errors
    /// Returns `TemporalError::InvalidTimestamp` if wallclock exceeds `MAX_VALID_TIMESTAMP`.
    /// This prevents DoS attacks from extreme timestamp values.
    #[inline]
    pub fn new(wallclock: i64, logical: u32) -> Result<Self, TemporalError> {
        if wallclock > MAX_VALID_TIMESTAMP {
            // Construct invalid timestamp for error reporting
            let invalid_ts = HybridTimestamp { wallclock, logical };
            return Err(TemporalError::InvalidTimestamp {
                timestamp: invalid_ts,
                reason: format!(
                    "Wallclock {} exceeds MAX_VALID_TIMESTAMP ({})",
                    wallclock, MAX_VALID_TIMESTAMP
                ),
            });
        }
        Ok(HybridTimestamp { wallclock, logical })
    }

    /// Create a new HybridTimestamp without validation.
    ///
    /// # Internal Use Only
    /// This function does not validate the wallclock value. Use only with trusted data
    /// from internal sources (WAL recovery, storage deserialization with external validation).
    ///
    /// Public constructors should use `new()` which validates inputs.
    #[allow(dead_code)] // Reserved for Phase 2 WAL/storage integration
    #[inline]
    pub(crate) const fn new_unchecked(wallclock: i64, logical: u32) -> Self {
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

    /// Helper: Increment logical counter with overflow check.
    ///
    /// This helper reduces duplication in `send()` and `receive()` methods.
    #[inline]
    fn increment_logical(logical: u32, wallclock: i64) -> Result<u32, TemporalError> {
        logical
            .checked_add(1)
            .ok_or(TemporalError::LogicalCounterOverflow {
                wallclock,
                current_logical: logical,
            })
    }

    /// Generate a new timestamp for a send event.
    ///
    /// # HLC Algorithm
    /// - If `new_wallclock` > `self.wallclock`: Use new wallclock, reset logical to 0
    /// - Otherwise: Keep max(wallclock, new_wallclock), increment logical
    ///
    /// This ensures monotonicity while preserving wallclock semantics.
    ///
    /// # Errors
    /// - Returns `TemporalError::InvalidTimestamp` if `new_wallclock` exceeds `MAX_VALID_TIMESTAMP`.
    /// - Returns `TemporalError::LogicalCounterOverflow` if the logical counter would exceed u32::MAX.
    ///   This theoretically requires 4+ billion events at the same microsecond, indicating severe
    ///   clock drift or pathological workload.
    #[inline]
    pub fn send(&self, new_wallclock: i64) -> Result<Self, TemporalError> {
        // Validate new_wallclock to prevent invalid timestamps
        if new_wallclock > MAX_VALID_TIMESTAMP {
            return Err(TemporalError::InvalidTimestamp {
                timestamp: Self {
                    wallclock: new_wallclock,
                    logical: 0,
                },
                reason: format!(
                    "Send wallclock {} exceeds MAX_VALID_TIMESTAMP ({})",
                    new_wallclock, MAX_VALID_TIMESTAMP
                ),
            });
        }

        if new_wallclock > self.wallclock {
            // Wallclock advanced - reset logical counter
            Ok(HybridTimestamp {
                wallclock: new_wallclock,
                logical: 0,
            })
        } else {
            // Wallclock didn't advance - increment logical counter
            let logical = Self::increment_logical(self.logical, self.wallclock)?;
            Ok(HybridTimestamp {
                wallclock: self.wallclock,
                logical,
            })
        }
    }

    /// Generate a new timestamp for a receive event (distributed message reception).
    ///
    /// # HLC Algorithm for Message Reception
    /// When receiving a message with timestamp `msg` from a remote node:
    /// 1. `new_wallclock = max(self.wallclock, msg.wallclock, physical_wallclock)`
    /// 2. If wallclock advances beyond both: reset logical to 0
    /// 3. If wallclock matches both: logical = max(self.logical, msg.logical) + 1
    /// 4. If wallclock matches only one: increment that timestamp's logical counter
    ///
    /// This preserves causality: if message A → message B, then timestamp(A) < timestamp(B).
    ///
    /// # Arguments
    /// - `msg`: The timestamp from the received message
    /// - `physical_wallclock`: Current physical clock reading
    ///
    /// # Errors
    /// - Returns `TemporalError::InvalidTimestamp` if the resulting wallclock exceeds `MAX_VALID_TIMESTAMP`.
    /// - Returns `TemporalError::LogicalCounterOverflow` if the logical counter would exceed u32::MAX.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::core::hlc::HybridTimestamp;
    ///
    /// // Receiving a message from a remote node
    /// let local_time = HybridTimestamp::new(1000, 5).unwrap();
    /// let message_time = HybridTimestamp::new(2000, 10).unwrap();
    /// let physical_clock = 1500;
    ///
    /// let updated = local_time.receive(message_time, physical_clock).unwrap();
    /// assert!(updated > local_time);
    /// assert!(updated > message_time);
    /// ```
    ///
    /// Receiving multiple messages maintains causality:
    ///
    /// ```
    /// use aletheiadb::core::hlc::HybridTimestamp;
    ///
    /// let mut local = HybridTimestamp::new(1000, 0).unwrap();
    /// let msg1 = HybridTimestamp::new(1100, 0).unwrap();
    /// let msg2 = HybridTimestamp::new(1050, 0).unwrap();
    ///
    /// // Receive msg1, update local time
    /// local = local.receive(msg1, 1000).unwrap();
    /// assert!(local > msg1); // Causality: local happened-after msg1
    ///
    /// // Receive msg2 (older wallclock but causally after local)
    /// local = local.receive(msg2, 1000).unwrap();
    /// assert!(local > msg2); // Causality preserved despite clock skew
    /// ```
    #[inline]
    pub fn receive(
        &self,
        msg: HybridTimestamp,
        physical_wallclock: i64,
    ) -> Result<Self, TemporalError> {
        // Compute maximum wallclock across all three values
        let new_wallclock = self.wallclock.max(msg.wallclock).max(physical_wallclock);

        // Validate resulting wallclock
        if new_wallclock > MAX_VALID_TIMESTAMP {
            return Err(TemporalError::InvalidTimestamp {
                timestamp: Self {
                    wallclock: new_wallclock,
                    logical: 0,
                },
                reason: format!(
                    "Receive wallclock {} exceeds MAX_VALID_TIMESTAMP ({})",
                    new_wallclock, MAX_VALID_TIMESTAMP
                ),
            });
        }

        // Determine logical counter based on which wallclock(s) were chosen
        let logical = if new_wallclock > self.wallclock && new_wallclock > msg.wallclock {
            // Physical clock advanced beyond both - reset to 0
            0
        } else if new_wallclock == self.wallclock && new_wallclock == msg.wallclock {
            // Both local and message have same wallclock - use max logical + 1
            Self::increment_logical(self.logical.max(msg.logical), new_wallclock)?
        } else if new_wallclock == self.wallclock {
            // Local wallclock was chosen - increment local logical
            Self::increment_logical(self.logical, new_wallclock)?
        } else {
            // Message wallclock was chosen - increment message logical
            Self::increment_logical(msg.logical, new_wallclock)?
        };

        Ok(HybridTimestamp {
            wallclock: new_wallclock,
            logical,
        })
    }

    /// Serialize this HybridTimestamp to bytes.
    ///
    /// # Binary Format
    /// ```text
    /// [wallclock:8][logical:4]
    /// ```
    /// Total: 12 bytes, little-endian
    ///
    /// # Performance Note
    /// This allocates a new `Vec<u8>`. For better performance when serializing
    /// multiple timestamps, consider using `serialize_into()` with a reused buffer.
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

    /// Deserialize a HybridTimestamp from bytes with validation.
    ///
    /// Returns the HybridTimestamp and number of bytes consumed (always 12).
    ///
    /// # Errors
    /// - Returns `StorageError::CorruptedData` if buffer is too short
    /// - Returns `StorageError::CorruptedData` if wallclock exceeds `MAX_VALID_TIMESTAMP`
    pub fn deserialize(bytes: &[u8]) -> Result<(Self, usize), StorageError> {
        if bytes.len() < 12 {
            return Err(StorageError::CorruptedData(format!(
                "Buffer too short for HybridTimestamp: {} bytes (need 12)",
                bytes.len()
            )));
        }

        // Use split_at and try_into for cleaner, safer byte array conversion
        let (wallclock_bytes, rest) = bytes.split_at(std::mem::size_of::<i64>());
        let wallclock = i64::from_le_bytes(wallclock_bytes.try_into().unwrap());

        let (logical_bytes, _) = rest.split_at(std::mem::size_of::<u32>());
        let logical = u32::from_le_bytes(logical_bytes.try_into().unwrap());

        // Validate wallclock to prevent corrupted data from injecting invalid timestamps
        // Allow i64::MAX as a special sentinel value for TIMESTAMP_MAX (represents infinity/"still current")
        if wallclock > MAX_VALID_TIMESTAMP && wallclock != i64::MAX {
            return Err(StorageError::CorruptedData(format!(
                "Deserialized wallclock {} exceeds MAX_VALID_TIMESTAMP ({})",
                wallclock, MAX_VALID_TIMESTAMP
            )));
        }

        Ok((HybridTimestamp { wallclock, logical }, 12))
    }
}

impl std::fmt::Display for HybridTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Display as wallclock.logical for human readability
        write!(f, "{}.{}", self.wallclock, self.logical)
    }
}

/// Convert an i64 wallclock timestamp to HybridTimestamp with logical counter = 0.
///
/// This enables seamless migration from Phase 1 (i64 timestamps) to Phase 2 (HybridTimestamp).
/// The conversion is primarily used in:
/// - Test code using integer literals
/// - Legacy APIs that accept i64 timestamps
/// - WAL/storage deserialization where logical counter is unknown
///
/// # Examples
/// ```
/// # use aletheiadb::core::hlc::HybridTimestamp;
/// let ts: HybridTimestamp = 1000_i64.into();
/// assert_eq!(ts.wallclock(), 1000);
/// assert_eq!(ts.logical(), 0);
/// ```
impl From<i64> for HybridTimestamp {
    fn from(wallclock: i64) -> Self {
        // Use new_unchecked for performance - caller responsible for validation
        // In test code and deserialization contexts, values are trusted
        HybridTimestamp::new_unchecked(wallclock, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_validation() {
        // Valid timestamp
        assert!(HybridTimestamp::new(1000, 0).is_ok());

        // Edge case: Exact MAX_VALID_TIMESTAMP
        assert!(HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).is_ok());

        // Invalid: Exceeds MAX_VALID_TIMESTAMP
        let result = HybridTimestamp::new(MAX_VALID_TIMESTAMP + 1, 0);
        assert!(result.is_err());
        match result {
            Err(TemporalError::InvalidTimestamp { .. }) => (),
            _ => panic!("Expected InvalidTimestamp error"),
        }
    }

    #[test]
    fn test_send_logic() {
        let ts = HybridTimestamp::new(1000, 5).unwrap();

        // Case 1: Wallclock advanced -> reset logical
        let next = ts.send(1001).unwrap();
        assert_eq!(next.wallclock(), 1001);
        assert_eq!(next.logical(), 0);

        // Case 2: Wallclock same -> increment logical
        let next = ts.send(1000).unwrap();
        assert_eq!(next.wallclock(), 1000);
        assert_eq!(next.logical(), 6);

        // Case 3: Wallclock regression (clock skew) -> increment logical, keep old wallclock
        let next = ts.send(999).unwrap();
        assert_eq!(next.wallclock(), 1000);
        assert_eq!(next.logical(), 6);
    }

    #[test]
    fn test_send_overflow() {
        let ts = HybridTimestamp::new(1000, u32::MAX).unwrap();

        // Should error on overflow when wallclock doesn't advance
        let result = ts.send(1000);
        assert!(matches!(
            result,
            Err(TemporalError::LogicalCounterOverflow { .. })
        ));

        // Should NOT error if wallclock advances (logical resets)
        let result = ts.send(1001);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().logical(), 0);
    }

    #[test]
    fn test_receive_logic() {
        let local = HybridTimestamp::new(1000, 10).unwrap();

        // Case 1: Physical clock advanced beyond both -> reset logical
        let msg = HybridTimestamp::new(1000, 20).unwrap();
        let next = local.receive(msg, 2000).unwrap();
        assert_eq!(next.wallclock(), 2000);
        assert_eq!(next.logical(), 0);

        // Case 2: Wallclocks match -> max(logical) + 1
        let msg = HybridTimestamp::new(1000, 20).unwrap();
        let next = local.receive(msg, 1000).unwrap();
        assert_eq!(next.wallclock(), 1000);
        assert_eq!(next.logical(), 21); // 20 + 1

        // Case 3: Local wallclock chosen -> local.logical + 1
        let msg = HybridTimestamp::new(900, 5).unwrap();
        let next = local.receive(msg, 950).unwrap();
        assert_eq!(next.wallclock(), 1000);
        assert_eq!(next.logical(), 11); // 10 + 1

        // Case 4: Message wallclock chosen -> msg.logical + 1
        let msg = HybridTimestamp::new(1500, 5).unwrap();
        let next = local.receive(msg, 1200).unwrap();
        assert_eq!(next.wallclock(), 1500);
        assert_eq!(next.logical(), 6); // 5 + 1
    }

    #[test]
    fn test_receive_overflow() {
        let local = HybridTimestamp::new(1000, u32::MAX).unwrap();
        let msg = HybridTimestamp::new(1000, 5).unwrap();

        // Overflow on local branch
        let result = local.receive(msg, 1000);
        assert!(matches!(
            result,
            Err(TemporalError::LogicalCounterOverflow { .. })
        ));
    }

    #[test]
    fn test_serialization() {
        let ts = HybridTimestamp::new(123456789, 42).unwrap();
        let bytes = ts.serialize();
        assert_eq!(bytes.len(), 12);

        let (deserialized, consumed) = HybridTimestamp::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, ts);
        assert_eq!(consumed, 12);
    }

    #[test]
    fn test_deserialize_validation() {
        // Buffer too short
        let bytes = vec![0u8; 11];
        assert!(matches!(
            HybridTimestamp::deserialize(&bytes),
            Err(StorageError::CorruptedData(_))
        ));

        // Invalid wallclock
        let invalid_ts = HybridTimestamp::new_unchecked(MAX_VALID_TIMESTAMP + 1, 0);
        let bytes = invalid_ts.serialize();
        assert!(matches!(
            HybridTimestamp::deserialize(&bytes),
            Err(StorageError::CorruptedData(_))
        ));

        // Sentinel value i64::MAX is allowed
        let sentinel = HybridTimestamp::new_unchecked(i64::MAX, 0);
        let bytes = sentinel.serialize();
        assert!(HybridTimestamp::deserialize(&bytes).is_ok());
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for generating valid wallclock values (within MAX_VALID_TIMESTAMP).
    fn valid_wallclock() -> impl Strategy<Value = i64> {
        0..=MAX_VALID_TIMESTAMP
    }

    /// Strategy for generating valid HybridTimestamp instances.
    fn valid_timestamp() -> impl Strategy<Value = HybridTimestamp> {
        (valid_wallclock(), any::<u32>()).prop_map(|(w, l)| HybridTimestamp::new(w, l).unwrap())
    }

    proptest! {
        /// Property: Timestamp ordering is lexicographic (wallclock, then logical).
        #[test]
        fn prop_ordering_lexicographic(
            w1 in valid_wallclock(), l1 in any::<u32>(),
            w2 in valid_wallclock(), l2 in any::<u32>()
        ) {
            let t1 = HybridTimestamp::new(w1, l1).unwrap();
            let t2 = HybridTimestamp::new(w2, l2).unwrap();
            prop_assert_eq!(t1.cmp(&t2), (w1, l1).cmp(&(w2, l2)));
        }

        /// Property: send() always produces a strictly greater timestamp.
        #[test]
        fn prop_send_monotonicity(
            current in valid_timestamp(),
            new_wallclock in valid_wallclock()
        ) {
            // send() might fail if logical overflows, but for random inputs probability is low.
            // However, we should handle the Result.
            if let Ok(next) = current.send(new_wallclock) {
                prop_assert!(next > current);
                prop_assert!(next.wallclock() >= new_wallclock);
                prop_assert!(next.wallclock() >= current.wallclock());
            }
        }

        /// Property: receive() result is >= local, >= msg, and >= physical (wallclock).
        #[test]
        fn prop_receive_causality(
            local in valid_timestamp(),
            msg in valid_timestamp(),
            physical in valid_wallclock()
        ) {
            if let Ok(next) = local.receive(msg, physical) {
                prop_assert!(next > local, "next > local");
                prop_assert!(next > msg, "next > msg");
                prop_assert!(next.wallclock() >= local.wallclock());
                prop_assert!(next.wallclock() >= msg.wallclock());
                prop_assert!(next.wallclock() >= physical);
            }
        }

        /// Property: Serialization roundtrip preserves equality.
        #[test]
        fn prop_serialization_roundtrip(ts in valid_timestamp()) {
            let bytes = ts.serialize();
            let (deserialized, consumed) = HybridTimestamp::deserialize(&bytes).unwrap();
            prop_assert_eq!(ts, deserialized);
            prop_assert_eq!(consumed, 12);
        }

        /// Property: send() rejects invalid wallclocks.
        #[test]
        fn prop_send_rejects_invalid(
            ts in valid_timestamp(),
            invalid_wc in (MAX_VALID_TIMESTAMP + 1)..i64::MAX
        ) {
            let result = ts.send(invalid_wc);
            let is_invalid = matches!(result, Err(TemporalError::InvalidTimestamp { .. }));
            prop_assert!(is_invalid);
        }

        /// Property: receive() rejects invalid physical clocks.
        #[test]
        fn prop_receive_rejects_invalid_physical(
            local in valid_timestamp(),
            msg in valid_timestamp(),
            invalid_phy in (MAX_VALID_TIMESTAMP + 1)..i64::MAX
        ) {
            let result = local.receive(msg, invalid_phy);
            let is_invalid = matches!(result, Err(TemporalError::InvalidTimestamp { .. }));
            prop_assert!(is_invalid);
        }

        /// Property: receive() preserves causality even when wallclocks collide.
        /// This specifically targets the "logical counter increment" logic when
        /// local.wallclock == msg.wallclock == physical_clock.
        #[test]
        fn prop_receive_causality_collision(
            wallclock in valid_wallclock(),
            local_logical in any::<u32>(),
            msg_logical in any::<u32>(),
        ) {
            let local = HybridTimestamp::new(wallclock, local_logical).unwrap();
            let msg = HybridTimestamp::new(wallclock, msg_logical).unwrap();

            // Force physical clock to match, triggering the collision path
            if let Ok(next) = local.receive(msg, wallclock) {
                prop_assert!(next > local, "next > local (collision)");
                prop_assert!(next > msg, "next > msg (collision)");
                prop_assert_eq!(next.wallclock(), wallclock);

                // Specifically verify the logical counter logic: max(l1, l2) + 1
                let expected_logical = local_logical.max(msg_logical).checked_add(1);
                if let Some(expected) = expected_logical {
                    prop_assert_eq!(next.logical(), expected);
                }
            }
        }
    }
}
