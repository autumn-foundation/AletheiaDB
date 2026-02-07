//! Temporal primitives for bi-temporal graph database.
//!
//! This module implements the core temporal types that enable bi-temporality:
//! - Valid time: when facts were true in the real world
//! - Transaction time: when facts were recorded in the database
//!
//! Every graph element (node/edge version) has a BiTemporalInterval that tracks
//! both dimensions of time.

use std::fmt;

use crate::core::hlc::HybridTimestamp;
use crate::utils::error::{StorageError, TemporalError};

/// Timestamp represented as Hybrid Logical Clock (HLC).
///
/// Phase 2: Migrated from i64 to HybridTimestamp for distributed temporal consistency.
///
/// HybridTimestamp combines:
/// - Wallclock component (i64 microseconds since Unix epoch)
/// - Logical counter (u32) for ordering events at same wallclock
///
/// This provides:
/// - Monotonic ordering despite clock skew
/// - Causality preservation across distributed nodes
/// - Human-readable wallclock semantics for temporal queries
pub type Timestamp = HybridTimestamp;

/// Sentinel value representing "infinity" or "current" timestamp.
///
/// Used for open-ended time ranges that extend to the present.
/// For HybridTimestamp, this is (i64::MAX, 0) - maximum wallclock, zero logical counter.
pub const TIMESTAMP_MAX: Timestamp = HybridTimestamp::new_unchecked(i64::MAX, 0);

/// Maximum valid timestamp wallclock value for user data.
///
/// Similar to MAX_VALID_ID, this reserves the upper 1000 i64 values for internal use
/// (sentinel values, metadata). Timestamps exceeding this value are rejected to prevent
/// DoS attacks and ensure system integrity.
///
/// The reserved range provides a safety margin without meaningfully restricting the
/// timestamp space (still covers ~290,000 years before/after epoch).
///
/// Note: This is the maximum wallclock value. The logical counter can be any u32 value.
pub const MAX_VALID_TIMESTAMP: i64 = i64::MAX - 1000;

/// Represents a continuous range of time [start, end).
///
/// The range includes the start timestamp but excludes the end timestamp.
/// An end value of TIMESTAMP_MAX represents an open-ended range (still current).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeRange {
    start: Timestamp,
    end: Timestamp,
}

impl TimeRange {
    /// Create a new time range.
    ///
    /// # Errors
    /// Returns `TemporalError::InvalidTimeRange` if start > end.
    #[inline]
    pub fn new(start: Timestamp, end: Timestamp) -> Result<Self, TemporalError> {
        if start > end {
            return Err(TemporalError::InvalidTimeRange { start, end });
        }
        Ok(TimeRange { start, end })
    }

    /// Create a time range that starts at the given timestamp and is still current.
    #[inline]
    pub fn from(start: Timestamp) -> Self {
        TimeRange {
            start,
            end: TIMESTAMP_MAX,
        }
    }

    /// Create a time range that is bounded on both ends.
    #[inline]
    pub fn between(start: Timestamp, end: Timestamp) -> Result<Self, TemporalError> {
        Self::new(start, end)
    }

    /// Create a point-in-time range (instant with zero duration).
    #[inline]
    pub fn at(timestamp: Timestamp) -> Self {
        TimeRange {
            start: timestamp,
            end: timestamp,
        }
    }

    /// Get the start timestamp.
    #[inline]
    pub const fn start(&self) -> Timestamp {
        self.start
    }

    /// Get the end timestamp.
    #[inline]
    pub const fn end(&self) -> Timestamp {
        self.end
    }

    /// Returns true if this range is currently open-ended (end == TIMESTAMP_MAX).
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn is_current(&self) -> bool {
        self.end == TIMESTAMP_MAX
    }

    /// Returns true if this range has been closed (end < TIMESTAMP_MAX).
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.end < TIMESTAMP_MAX
    }

    /// Returns true if the given timestamp is contained within this range [start, end).
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn contains(&self, timestamp: Timestamp) -> bool {
        timestamp >= self.start && timestamp < self.end
    }

    /// Returns true if the given timestamp is at or after the start of this range.
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn contains_or_after(&self, timestamp: Timestamp) -> bool {
        timestamp >= self.start
    }

    /// Returns true if this range overlaps with another range.
    ///
    /// Two ranges overlap if there exists any timestamp that is in both ranges.
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn overlaps(&self, other: &TimeRange) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Returns true if this range completely contains another range.
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn contains_range(&self, other: &TimeRange) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// Close this range at the given timestamp.
    ///
    /// Returns a new TimeRange with the same start but the specified end.
    #[inline]
    pub const fn close_at(self, end: Timestamp) -> Self {
        TimeRange {
            start: self.start,
            end,
        }
    }

    /// Returns the duration of this range in microseconds.
    ///
    /// Returns None if the range is open-ended (current).
    /// Duration is calculated using wallclock components only.
    /// Note: Phase 2 - removed const due to is_current() needing HybridTimestamp comparison.
    #[inline]
    pub fn duration_micros(&self) -> Option<i64> {
        if self.is_current() {
            None
        } else {
            Some(self.end.wallclock() - self.start.wallclock())
        }
    }

    /// Serialize this TimeRange to bytes.
    ///
    /// # Binary Format (Phase 2: HybridTimestamp)
    /// ```text
    /// [start.wallclock:8][start.logical:4][end.wallclock:8][end.logical:4]
    /// ```
    /// Total: 24 bytes (2 HybridTimestamps × 12 bytes each)
    pub fn serialize(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(24);
        self.serialize_into(&mut buffer);
        buffer
    }

    /// Serialize into an existing buffer.
    pub fn serialize_into(&self, buffer: &mut Vec<u8>) {
        self.start.serialize_into(buffer);
        self.end.serialize_into(buffer);
    }

    /// Deserialize a TimeRange from bytes.
    ///
    /// Returns the TimeRange and number of bytes consumed (always 24).
    pub fn deserialize(bytes: &[u8]) -> Result<(Self, usize), StorageError> {
        if bytes.len() < 24 {
            return Err(StorageError::CorruptedData(format!(
                "Buffer too short for TimeRange: {} bytes (need 24)",
                bytes.len()
            )));
        }
        let (start, _) = HybridTimestamp::deserialize(&bytes[0..12])?;
        let (end, _) = HybridTimestamp::deserialize(&bytes[12..24])?;
        Ok((TimeRange { start, end }, 24))
    }
}

impl fmt::Display for TimeRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_current() {
            write!(f, "[{}, current)", self.start)
        } else {
            write!(f, "[{}, {})", self.start, self.end)
        }
    }
}

/// Bi-temporal interval tracking both valid time and transaction time.
///
/// This is the core temporal primitive that enables time-traveling queries:
/// - **Valid time**: When the fact was true in the real world
/// - **Transaction time**: When the fact was recorded in the database
///
/// This creates a 2D time space that allows queries like:
/// - "What did we know about X at time T?" (transaction time)
/// - "What was true about X in reality at time T?" (valid time)
/// - "When did we record that X was true at time T?" (both)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BiTemporalInterval {
    /// When the fact was true in the real world.
    valid_time: TimeRange,
    /// When the fact was recorded in the database.
    transaction_time: TimeRange,
}

impl BiTemporalInterval {
    /// Create a new bi-temporal interval.
    #[inline]
    pub const fn new(valid_time: TimeRange, transaction_time: TimeRange) -> Self {
        BiTemporalInterval {
            valid_time,
            transaction_time,
        }
    }

    /// Create a bi-temporal interval where both dimensions start at the same time
    /// and are currently open.
    #[inline]
    pub fn current(timestamp: Timestamp) -> Self {
        let range = TimeRange::from(timestamp);
        BiTemporalInterval {
            valid_time: range,
            transaction_time: range,
        }
    }

    /// Create a bi-temporal interval for a fact that is currently valid and was just recorded.
    ///
    /// This is the most common case: recording a fact that is true now.
    #[inline]
    pub fn now(valid_start: Timestamp, tx_timestamp: Timestamp) -> Self {
        BiTemporalInterval {
            valid_time: TimeRange::from(valid_start),
            transaction_time: TimeRange::from(tx_timestamp),
        }
    }

    /// Create a bi-temporal interval with separate valid_from and transaction_time.
    ///
    /// This enables true bi-temporal semantics where:
    /// - `valid_from`: when the fact became true in reality (user-controlled)
    /// - `tx_time`: when the fact was recorded in the database (system-managed)
    ///
    /// Both dimensions are open-ended (current), but start at different times.
    /// This allows backdating facts - recording at time T2 that something was
    /// valid since time T1 (where T1 < T2).
    ///
    /// # Example
    /// ```
    /// use aletheiadb::core::temporal::BiTemporalInterval;
    /// use aletheiadb::core::hlc::HybridTimestamp;
    ///
    /// // Record today that Alice joined the company last month
    /// let last_month = HybridTimestamp::new(1704067200_000_000, 0).unwrap();
    /// let today = HybridTimestamp::new(1706745600_000_000, 0).unwrap();
    ///
    /// let interval = BiTemporalInterval::with_valid_time(last_month, today);
    /// // Alice's membership was valid since last month
    /// // But we only recorded it today
    /// ```
    #[inline]
    pub fn with_valid_time(valid_from: Timestamp, tx_time: Timestamp) -> Self {
        BiTemporalInterval {
            valid_time: TimeRange::from(valid_from),
            transaction_time: TimeRange::from(tx_time),
        }
    }

    /// Get the valid time range.
    #[inline]
    pub const fn valid_time(&self) -> TimeRange {
        self.valid_time
    }

    /// Get the transaction time range.
    #[inline]
    pub const fn transaction_time(&self) -> TimeRange {
        self.transaction_time
    }

    /// Returns true if this interval is currently valid (valid time is open).
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn is_currently_valid(&self) -> bool {
        self.valid_time.is_current()
    }

    /// Returns true if this interval is currently in the database (transaction time is open).
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn is_currently_recorded(&self) -> bool {
        self.transaction_time.is_current()
    }

    /// Returns true if this interval is current in both dimensions.
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn is_current(&self) -> bool {
        self.is_currently_valid() && self.is_currently_recorded()
    }

    /// Check if this interval is visible at the given valid time.
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn is_valid_at(&self, timestamp: Timestamp) -> bool {
        self.valid_time.contains(timestamp)
    }

    /// Check if this interval was recorded by the given transaction time.
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn is_recorded_at(&self, timestamp: Timestamp) -> bool {
        self.transaction_time.contains(timestamp)
    }

    /// Check if this interval is visible in both dimensions at the given times.
    ///
    /// This answers: "At transaction time T1, did we believe this fact was true at valid time T2?"
    /// Note: Phase 2 - removed const due to HybridTimestamp comparison.
    #[inline]
    pub fn is_visible_at(&self, valid_time: Timestamp, tx_time: Timestamp) -> bool {
        self.valid_time.contains(valid_time) && self.transaction_time.contains(tx_time)
    }

    /// Close the valid time dimension at the given timestamp.
    ///
    /// This marks a fact as no longer valid in the real world.
    #[inline]
    pub fn close_valid_time(self, end: Timestamp) -> Self {
        BiTemporalInterval {
            valid_time: self.valid_time.close_at(end),
            transaction_time: self.transaction_time,
        }
    }

    /// Close the transaction time dimension at the given timestamp.
    ///
    /// This marks when we stopped believing this version of the fact.
    #[inline]
    pub fn close_transaction_time(self, end: Timestamp) -> Self {
        BiTemporalInterval {
            valid_time: self.valid_time,
            transaction_time: self.transaction_time.close_at(end),
        }
    }

    /// Close both time dimensions.
    #[inline]
    pub fn close_both(self, valid_end: Timestamp, tx_end: Timestamp) -> Self {
        BiTemporalInterval {
            valid_time: self.valid_time.close_at(valid_end),
            transaction_time: self.transaction_time.close_at(tx_end),
        }
    }

    /// Serialize this BiTemporalInterval to bytes.
    ///
    /// # Binary Format (Phase 2: HybridTimestamp)
    /// ```text
    /// [valid_time: 24 bytes][transaction_time: 24 bytes]
    /// ```
    /// Total: 48 bytes (2 TimeRanges × 24 bytes each)
    pub fn serialize(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(48);
        self.serialize_into(&mut buffer);
        buffer
    }

    /// Serialize into an existing buffer.
    pub fn serialize_into(&self, buffer: &mut Vec<u8>) {
        self.valid_time.serialize_into(buffer);
        self.transaction_time.serialize_into(buffer);
    }

    /// Deserialize a BiTemporalInterval from bytes.
    ///
    /// Returns the BiTemporalInterval and number of bytes consumed (always 48).
    pub fn deserialize(bytes: &[u8]) -> Result<(Self, usize), StorageError> {
        if bytes.len() < 48 {
            return Err(StorageError::CorruptedData(format!(
                "Buffer too short for BiTemporalInterval: {} bytes (need 48)",
                bytes.len()
            )));
        }
        let (valid_time, _) = TimeRange::deserialize(&bytes[0..24])?;
        let (transaction_time, _) = TimeRange::deserialize(&bytes[24..48])?;
        Ok((
            BiTemporalInterval {
                valid_time,
                transaction_time,
            },
            48,
        ))
    }
}

impl fmt::Display for BiTemporalInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BiTemporal[valid: {}, tx: {}]",
            self.valid_time, self.transaction_time
        )
    }
}

/// Helper functions for working with timestamps.
pub mod time {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Get the current system time as a Timestamp (HybridTimestamp).
    ///
    /// Returns HybridTimestamp with current wallclock and logical counter = 0.
    /// For monotonic HLC generation with causality, use HLC's send() method.
    ///
    /// # Panics
    /// Panics if the system clock is set before Unix epoch. Use [`try_now`] for
    /// a fallible version that returns `Result` instead.
    pub fn now() -> Timestamp {
        try_now().expect("System clock is before Unix epoch")
    }

    /// Fallible version of [`now`] that returns `Result` instead of panicking.
    ///
    /// Returns an error if the system clock is set before Unix epoch.
    /// Most callers should use [`now`] instead, since a pre-epoch clock
    /// is an unrecoverable system-level error.
    pub fn try_now() -> Result<Timestamp, crate::utils::error::TemporalError> {
        let wallclock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| crate::utils::error::TemporalError::TemporalParadox {
                reason: "System clock is before Unix epoch".to_string(),
            })?
            .as_micros() as i64;

        // Return HybridTimestamp with logical counter = 0
        // Use new_unchecked for performance (system clock is trusted)
        Ok(HybridTimestamp::new_unchecked(wallclock, 0))
    }

    /// Convert a Timestamp to a human-readable ISO 8601 string (UTC).
    ///
    /// Returns "current" for TIMESTAMP_MAX.
    /// Uses the wallclock component for display.
    pub fn to_iso8601(timestamp: Timestamp) -> String {
        if timestamp == TIMESTAMP_MAX {
            return "current".to_string();
        }

        let wallclock = timestamp.wallclock();
        // Convert microseconds to seconds and nanoseconds
        let secs = wallclock / 1_000_000;
        let nanos = ((wallclock % 1_000_000) * 1000) as u32;

        // This is a simplified conversion - for production use chrono crate
        let datetime = UNIX_EPOCH + std::time::Duration::new(secs as u64, nanos);
        format!("{:?}", datetime) // Simplified - use chrono for proper formatting
    }

    /// Create a timestamp from seconds since Unix epoch.
    /// Logical counter is set to 0.
    #[inline]
    pub const fn from_secs(secs: i64) -> Timestamp {
        HybridTimestamp::new_unchecked(secs * 1_000_000, 0)
    }

    /// Create a timestamp from milliseconds since Unix epoch.
    /// Logical counter is set to 0.
    #[inline]
    pub const fn from_millis(millis: i64) -> Timestamp {
        HybridTimestamp::new_unchecked(millis * 1_000, 0)
    }

    /// Convert a timestamp to seconds since Unix epoch.
    /// Uses the wallclock component.
    #[inline]
    pub const fn to_secs(timestamp: Timestamp) -> i64 {
        timestamp.wallclock() / 1_000_000
    }

    /// Convert a timestamp to milliseconds since Unix epoch.
    /// Uses the wallclock component.
    #[inline]
    pub const fn to_millis(timestamp: Timestamp) -> i64 {
        timestamp.wallclock() / 1_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_time_range_creation() {
        let range = TimeRange::new(100.into(), 200.into()).unwrap();
        assert_eq!(range.start(), 100.into());
        assert_eq!(range.end(), 200.into());
        assert!(!range.is_current());
        assert!(range.is_closed());
    }

    #[test]
    fn test_time_range_current() {
        let range = TimeRange::from(100.into());
        assert_eq!(range.start(), 100.into());
        assert_eq!(range.end(), TIMESTAMP_MAX);
        assert!(range.is_current());
        assert!(!range.is_closed());
    }

    #[test]
    fn test_time_range_contains() {
        let range = TimeRange::new(100.into(), 200.into()).unwrap();
        assert!(!range.contains(99.into()));
        assert!(range.contains(100.into()));
        assert!(range.contains(150.into()));
        assert!(range.contains(199.into()));
        assert!(!range.contains(200.into())); // Exclusive end
    }

    #[test]
    fn test_time_range_overlaps() {
        let r1 = TimeRange::new(100.into(), 200.into()).unwrap();
        let r2 = TimeRange::new(150.into(), 250.into()).unwrap();
        let r3 = TimeRange::new(200.into(), 300.into()).unwrap();
        let r4 = TimeRange::new(50.into(), 75.into()).unwrap();

        assert!(r1.overlaps(&r2));
        assert!(r2.overlaps(&r1));
        assert!(!r1.overlaps(&r3)); // Touching but not overlapping
        assert!(!r1.overlaps(&r4));
    }

    #[test]
    fn test_time_range_contains_range() {
        let outer = TimeRange::new(100.into(), 300.into()).unwrap();
        let inner = TimeRange::new(150.into(), 250.into()).unwrap();
        let overlapping = TimeRange::new(150.into(), 350.into()).unwrap();

        assert!(outer.contains_range(&inner));
        assert!(!inner.contains_range(&outer));
        assert!(!outer.contains_range(&overlapping));
    }

    #[test]
    fn test_time_range_close_at() {
        let open = TimeRange::from(100.into());
        let closed = open.close_at(200.into());

        assert!(open.is_current());
        assert!(!closed.is_current());
        assert_eq!(closed.start(), 100.into());
        assert_eq!(closed.end(), 200.into());
    }

    #[test]
    fn test_time_range_duration() {
        let range = TimeRange::new(100.into(), 500.into()).unwrap();
        assert_eq!(range.duration_micros(), Some(400.into()));

        let open = TimeRange::from(100.into());
        assert_eq!(open.duration_micros(), None);
    }

    #[test]
    fn test_bitemporal_current() {
        let interval = BiTemporalInterval::current(1000.into());
        assert!(interval.is_currently_valid());
        assert!(interval.is_currently_recorded());
        assert!(interval.is_current());
    }

    #[test]
    fn test_bitemporal_now() {
        let interval = BiTemporalInterval::now(1000.into(), 2000.into());
        assert_eq!(interval.valid_time().start(), 1000.into());
        assert_eq!(interval.transaction_time().start(), 2000.into());
        assert!(interval.is_currently_valid());
        assert!(interval.is_currently_recorded());
    }

    #[test]
    fn test_bitemporal_visibility() {
        let interval = BiTemporalInterval::new(
            TimeRange::new(1000.into(), 2000.into()).unwrap(), // Valid from 1000 to 2000
            TimeRange::new(3000.into(), 4000.into()).unwrap(), // Recorded from 3000 to 4000
        );

        // Visible if both dimensions are in range
        assert!(interval.is_visible_at(1500.into(), 3500.into()));
        assert!(!interval.is_visible_at(500.into(), 3500.into())); // Before valid time
        assert!(!interval.is_visible_at(1500.into(), 2500.into())); // Before transaction time
        assert!(!interval.is_visible_at(2500.into(), 3500.into())); // After valid time
        assert!(!interval.is_visible_at(1500.into(), 4500.into())); // After transaction time
    }

    #[test]
    fn test_bitemporal_close() {
        let interval = BiTemporalInterval::now(1000.into(), 2000.into());

        let closed_valid = interval.close_valid_time(1500.into());
        assert!(!closed_valid.is_currently_valid());
        assert!(closed_valid.is_currently_recorded());
        assert_eq!(closed_valid.valid_time().end(), 1500.into());

        let closed_tx = interval.close_transaction_time(2500.into());
        assert!(closed_tx.is_currently_valid());
        assert!(!closed_tx.is_currently_recorded());
        assert_eq!(closed_tx.transaction_time().end(), 2500.into());

        let closed_both = interval.close_both(1500.into(), 2500.into());
        assert!(!closed_both.is_currently_valid());
        assert!(!closed_both.is_currently_recorded());
    }

    #[test]
    fn test_time_helpers() {
        let secs = 1234567890i64;
        let timestamp = time::from_secs(secs);
        assert_eq!(time::to_secs(timestamp), secs);

        let millis = 1234567890123i64;
        let timestamp = time::from_millis(millis);
        assert_eq!(time::to_millis(timestamp), millis);
    }

    #[test]
    fn test_time_now() {
        let timestamp = time::now();
        // Should be after 2020-01-01 and before 2100-01-01
        assert!(timestamp > time::from_secs(1577836800));
        assert!(timestamp < time::from_secs(4102444800));
    }

    #[test]
    fn test_time_range_invalid_returns_error() {
        // TimeRange::new should return an error for invalid ranges (start > end)
        let result = TimeRange::new(200.into(), 100.into());
        assert!(result.is_err());
        if let Err(crate::utils::error::TemporalError::InvalidTimeRange { start, end }) = result {
            assert_eq!(start, 200.into());
            assert_eq!(end, 100.into());
        } else {
            panic!("Expected InvalidTimeRange error");
        }
    }

    #[test]
    fn test_time_range_valid_returns_ok() {
        // TimeRange::new should return Ok for valid ranges
        let result = TimeRange::new(100.into(), 200.into());
        assert!(result.is_ok());
        let range = result.unwrap();
        assert_eq!(range.start(), 100.into());
        assert_eq!(range.end(), 200.into());
    }

    #[test]
    fn test_time_range_equal_start_end_returns_ok() {
        // TimeRange::new should return Ok when start == end (point-in-time)
        let result = TimeRange::new(100.into(), 100.into());
        assert!(result.is_ok());
        let range = result.unwrap();
        assert_eq!(range.start(), 100.into());
        assert_eq!(range.end(), 100.into());
    }

    // Serialization tests

    #[test]
    fn test_timerange_serialize_roundtrip() {
        let ranges = [
            TimeRange::new(100.into(), 200.into()).unwrap(),
            TimeRange::from(1000.into()),
            TimeRange::at(500.into()),
            TimeRange::new(i64::MIN.into(), i64::MAX.into()).unwrap(),
            TimeRange::new(0.into(), 0.into()).unwrap(),
        ];
        for range in ranges {
            let bytes = range.serialize();
            assert_eq!(bytes.len(), 24); // Phase 2: 2 x 12-byte HybridTimestamp
            let (deserialized, consumed) = TimeRange::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, range);
            assert_eq!(consumed, 24);
        }
    }

    #[test]
    fn test_timerange_serialize_into() {
        let range = TimeRange::new(100.into(), 200.into()).unwrap();
        let mut buffer = Vec::new();
        range.serialize_into(&mut buffer);
        assert_eq!(buffer.len(), 24); // Phase 2: 2 x 12-byte HybridTimestamp
        let (deserialized, _) = TimeRange::deserialize(&buffer).unwrap();
        assert_eq!(deserialized, range);
    }

    #[test]
    fn test_timerange_deserialize_truncated() {
        let result = TimeRange::deserialize(&[0; 23]); // Phase 2: 24 bytes needed
        assert!(result.is_err());
    }

    #[test]
    fn test_bitemporal_serialize_roundtrip() {
        let intervals = [
            BiTemporalInterval::current(1000.into()),
            BiTemporalInterval::now(500.into(), 600.into()),
            BiTemporalInterval::new(
                TimeRange::new(100.into(), 200.into()).unwrap(),
                TimeRange::new(300.into(), 400.into()).unwrap(),
            ),
            BiTemporalInterval::new(TimeRange::from(0.into()), TimeRange::from(0.into())),
        ];
        for interval in intervals {
            let bytes = interval.serialize();
            assert_eq!(bytes.len(), 48); // Phase 2: 2 x 24-byte TimeRange
            let (deserialized, consumed) = BiTemporalInterval::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, interval);
            assert_eq!(consumed, 48);
        }
    }

    #[test]
    fn test_bitemporal_serialize_into() {
        let interval = BiTemporalInterval::new(
            TimeRange::new(100.into(), 200.into()).unwrap(),
            TimeRange::new(300.into(), 400.into()).unwrap(),
        );
        let mut buffer = Vec::new();
        interval.serialize_into(&mut buffer);
        assert_eq!(buffer.len(), 48); // Phase 2: 2 x 24-byte TimeRange
        let (deserialized, _) = BiTemporalInterval::deserialize(&buffer).unwrap();
        assert_eq!(deserialized, interval);
    }

    #[test]
    fn test_bitemporal_deserialize_truncated() {
        let result = BiTemporalInterval::deserialize(&[0; 47]); // Phase 2: 48 bytes needed
        assert!(result.is_err());
    }

    #[test]
    fn test_serialization_endianness() {
        // Phase 2: Verify HybridTimestamp little-endian format
        // Format: [start_wallclock:8][start_logical:4][end_wallclock:8][end_logical:4]
        let range =
            TimeRange::new(0x0102030405060708i64.into(), 0x1112131415161718i64.into()).unwrap();
        let bytes = range.serialize();

        assert_eq!(bytes.len(), 24); // Total size: 24 bytes

        // Start timestamp wallclock (bytes 0-7) - little-endian
        assert_eq!(bytes[0], 0x08, "start.wallclock LSB");
        assert_eq!(bytes[7], 0x01, "start.wallclock MSB");

        // Start timestamp logical (bytes 8-11) - should be 0
        assert_eq!(&bytes[8..12], &[0, 0, 0, 0], "start.logical should be 0");

        // End timestamp wallclock (bytes 12-19) - little-endian
        assert_eq!(bytes[12], 0x18, "end.wallclock LSB");
        assert_eq!(bytes[19], 0x11, "end.wallclock MSB");

        // End timestamp logical (bytes 20-23) - should be 0
        assert_eq!(&bytes[20..24], &[0, 0, 0, 0], "end.logical should be 0");
    }

    // =========================================================================
    // Phase 2: HybridTimestamp Integration Tests
    // =========================================================================

    #[test]
    fn test_timestamp_is_hybrid_timestamp() {
        // After Phase 2, Timestamp should be HybridTimestamp
        // time::now() should return HybridTimestamp
        let ts = time::now();

        // Should have wallclock component
        assert!(ts.wallclock() > 0);

        // Should have logical component (starts at 0)
        assert_eq!(ts.logical(), 0);
    }

    #[test]
    fn test_time_range_with_hybrid_timestamps() {
        use crate::core::hlc::HybridTimestamp;

        // Create timestamps with different wallclocks
        let ts1 = HybridTimestamp::new(1000, 0).unwrap();
        let ts2 = HybridTimestamp::new(2000, 0).unwrap();

        // TimeRange should work with HybridTimestamp
        let range = TimeRange::new(ts1, ts2).unwrap();

        assert_eq!(range.start(), ts1);
        assert_eq!(range.end(), ts2);
    }

    #[test]
    fn test_time_range_ordering_with_logical_component() {
        use crate::core::hlc::HybridTimestamp;

        // Same wallclock, different logical counters
        let ts1 = HybridTimestamp::new(1000, 0).unwrap();
        let ts2 = HybridTimestamp::new(1000, 1).unwrap();
        let ts3 = HybridTimestamp::new(1000, 2).unwrap();

        // HybridTimestamp ordering is lexicographic: wallclock first, then logical
        assert!(ts1 < ts2);
        assert!(ts2 < ts3);

        // TimeRange should respect this ordering
        let range = TimeRange::new(ts1, ts3).unwrap();
        assert!(range.contains(ts1));
        assert!(range.contains(ts2));
        assert!(!range.contains(ts3)); // Exclusive end
    }

    #[test]
    fn test_bitemporal_with_hybrid_timestamps() {
        use crate::core::hlc::HybridTimestamp;

        let valid_start = HybridTimestamp::new(1000, 0).unwrap();
        let tx_start = HybridTimestamp::new(2000, 0).unwrap();

        let interval = BiTemporalInterval::now(valid_start, tx_start);

        assert_eq!(interval.valid_time().start(), valid_start);
        assert_eq!(interval.transaction_time().start(), tx_start);
    }

    #[test]
    fn test_hybrid_timestamp_serialization_size() {
        use crate::core::hlc::HybridTimestamp;

        // HybridTimestamp is 12 bytes (8 wallclock + 4 logical)
        let ts = HybridTimestamp::new(1000, 5).unwrap();
        let serialized = ts.serialize();
        assert_eq!(serialized.len(), 12);

        // TimeRange with HybridTimestamp should be 24 bytes (2 × 12)
        let range = TimeRange::new(
            HybridTimestamp::new(1000, 0).unwrap(),
            HybridTimestamp::new(2000, 0).unwrap(),
        )
        .unwrap();
        let serialized = range.serialize();
        assert_eq!(serialized.len(), 24);

        // BiTemporalInterval should be 48 bytes (4 × 12)
        let interval = BiTemporalInterval::now(
            HybridTimestamp::new(1000, 0).unwrap(),
            HybridTimestamp::new(2000, 0).unwrap(),
        );
        let serialized = interval.serialize();
        assert_eq!(serialized.len(), 48);
    }

    #[test]
    fn test_timestamp_max_with_hybrid_timestamp() {
        // TIMESTAMP_MAX should be representable as HybridTimestamp
        // It represents "infinity" or "current"
        assert_eq!(TIMESTAMP_MAX.wallclock(), i64::MAX);
        assert_eq!(TIMESTAMP_MAX.logical(), 0);
    }

    // =========================================================================
    // Phase 1: True Bi-Temporal Support Tests (with_valid_time)
    // =========================================================================

    #[test]
    fn test_with_valid_time_creates_separate_dimensions() {
        use crate::core::hlc::HybridTimestamp;

        let valid_from = HybridTimestamp::new(1000, 0).unwrap();
        let tx_time = HybridTimestamp::new(2000, 0).unwrap();

        let interval = BiTemporalInterval::with_valid_time(valid_from, tx_time);

        assert_eq!(interval.valid_time().start(), valid_from);
        assert_eq!(interval.transaction_time().start(), tx_time);
        assert_ne!(
            interval.valid_time().start(),
            interval.transaction_time().start()
        );
    }

    #[test]
    fn test_with_valid_time_creates_open_ended_ranges() {
        use crate::core::hlc::HybridTimestamp;

        let valid_from = HybridTimestamp::new(1000, 0).unwrap();
        let tx_time = HybridTimestamp::new(2000, 0).unwrap();

        let interval = BiTemporalInterval::with_valid_time(valid_from, tx_time);

        // Both should be open-ended (end = TIMESTAMP_MAX)
        assert!(interval.valid_time().is_current());
        assert!(interval.transaction_time().is_current());
    }

    #[test]
    fn test_with_valid_time_backdated_visibility() {
        use crate::core::hlc::HybridTimestamp;

        // Create interval: valid from Jan 1, recorded on Feb 1
        let jan_1 = HybridTimestamp::new(1_704_067_200_000_000, 0).unwrap(); // 2024-01-01
        let feb_1 = HybridTimestamp::new(1_706_745_600_000_000, 0).unwrap(); // 2024-02-01
        let jan_15 = HybridTimestamp::new(1_705_276_800_000_000, 0).unwrap(); // 2024-01-15

        let interval = BiTemporalInterval::with_valid_time(jan_1, feb_1);

        // Visible at valid_time=Jan 15, tx_time=Feb 1 (after recording)
        assert!(interval.is_visible_at(jan_15, feb_1));

        // NOT visible at valid_time=Jan 15, tx_time=Jan 15 (before recording)
        assert!(!interval.is_visible_at(jan_15, jan_15));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for generating valid wallclock values within safe bounds.
    /// We use a range well within MAX_VALID_TIMESTAMP to avoid overflow in
    /// arithmetic operations and to stay away from sentinel values.
    fn valid_wallclock() -> impl Strategy<Value = i64> {
        0..=(MAX_VALID_TIMESTAMP / 2)
    }

    /// Strategy for generating HybridTimestamps with valid values.
    fn valid_timestamp() -> impl Strategy<Value = Timestamp> {
        (valid_wallclock(), any::<u32>())
            .prop_map(|(wc, lc)| HybridTimestamp::new_unchecked(wc, lc))
    }

    /// Strategy for generating three ordered wallclock values (a <= b <= c).
    fn three_ordered_wallclocks() -> impl Strategy<Value = (i64, i64, i64)> {
        (valid_wallclock(), valid_wallclock(), valid_wallclock()).prop_map(|(a, b, c)| {
            let mut vals = [a, b, c];
            vals.sort();
            (vals[0], vals[1], vals[2])
        })
    }

    proptest! {
        // ====================================================================
        // Timestamp Ordering Properties
        // ====================================================================

        /// Property: Timestamp ordering is transitive.
        /// If a < b and b < c then a < c.
        #[test]
        fn prop_timestamp_ordering_is_transitive(
            (wc_a, wc_b, wc_c) in three_ordered_wallclocks()
        ) {
            let a = HybridTimestamp::new_unchecked(wc_a, 0);
            let b = HybridTimestamp::new_unchecked(wc_b, 0);
            let c = HybridTimestamp::new_unchecked(wc_c, 0);

            if a < b && b < c {
                prop_assert!(a < c, "Transitivity violated: {:?} < {:?} < {:?} but a >= c", a, b, c);
            }
        }

        /// Property: Timestamp ordering is transitive with logical counters.
        #[test]
        fn prop_timestamp_ordering_transitive_with_logical(
            wc in valid_wallclock(),
            lc_a in 0u32..1000,
            lc_b in 0u32..1000,
            lc_c in 0u32..1000,
        ) {
            let a = HybridTimestamp::new_unchecked(wc, lc_a);
            let b = HybridTimestamp::new_unchecked(wc, lc_b);
            let c = HybridTimestamp::new_unchecked(wc, lc_c);

            if a < b && b < c {
                prop_assert!(a < c, "Logical counter transitivity violated");
            }
        }

        /// Property: Timestamp ordering is antisymmetric.
        /// If a <= b and b <= a then a == b.
        #[test]
        fn prop_timestamp_ordering_antisymmetric(
            a in valid_timestamp(),
            b in valid_timestamp(),
        ) {
            if a <= b && b <= a {
                prop_assert_eq!(a, b);
            }
        }

        /// Property: Timestamp ordering is total.
        /// For any a, b: exactly one of a < b, a == b, a > b holds.
        #[test]
        fn prop_timestamp_ordering_total(
            a in valid_timestamp(),
            b in valid_timestamp(),
        ) {
            let lt = a < b;
            let eq = a == b;
            let gt = a > b;
            let exactly_one = (lt as u8 + eq as u8 + gt as u8) == 1;
            prop_assert!(exactly_one, "Total ordering violated: lt={}, eq={}, gt={}", lt, eq, gt);
        }

        // ====================================================================
        // Timestamp Monotonicity Properties
        // ====================================================================

        /// Property: time::now() produces timestamps after a known past time.
        #[test]
        fn prop_time_now_is_after_epoch(_dummy in 0..10u8) {
            let ts = time::now();
            let epoch = HybridTimestamp::new_unchecked(0, 0);
            prop_assert!(ts > epoch, "time::now() should be after epoch");
        }

        /// Property: Sequential calls to time::now() produce non-decreasing timestamps.
        #[test]
        fn prop_time_now_monotonic(_dummy in 0..10u8) {
            let t1 = time::now();
            let t2 = time::now();
            prop_assert!(t2 >= t1, "Sequential now() calls should be non-decreasing: {:?} vs {:?}", t1, t2);
        }

        // ====================================================================
        // TimeRange Properties
        // ====================================================================

        /// Property: Valid time ranges always have start <= end.
        #[test]
        fn prop_time_range_well_formed(
            start_wc in valid_wallclock(),
            end_wc in valid_wallclock(),
        ) {
            let start = HybridTimestamp::new_unchecked(start_wc, 0);
            let end = HybridTimestamp::new_unchecked(end_wc, 0);
            let result = TimeRange::new(start, end);

            if start <= end {
                let range = result.expect("Valid range should succeed");
                prop_assert!(range.start() <= range.end(),
                    "TimeRange should have start <= end");
            } else {
                prop_assert!(result.is_err(),
                    "TimeRange::new should reject start > end");
            }
        }

        /// Property: TimeRange::from() always creates a current (open-ended) range.
        #[test]
        fn prop_time_range_from_is_current(ts in valid_timestamp()) {
            let range = TimeRange::from(ts);
            prop_assert!(range.is_current(), "TimeRange::from should be current");
            prop_assert_eq!(range.start(), ts);
            prop_assert_eq!(range.end(), TIMESTAMP_MAX);
        }

        /// Property: TimeRange::at() creates a point-in-time range where start == end.
        #[test]
        fn prop_time_range_at_is_point(ts in valid_timestamp()) {
            let range = TimeRange::at(ts);
            prop_assert_eq!(range.start(), range.end());
            prop_assert_eq!(range.start(), ts);
        }

        /// Property: TimeRange contains its start but not its end (half-open interval).
        #[test]
        fn prop_time_range_half_open(
            (wc_start, wc_end) in (0i64..1_000_000, 1i64..1_000_000)
                .prop_map(|(s, delta)| (s, s + delta))
        ) {
            let start = HybridTimestamp::new_unchecked(wc_start, 0);
            let end = HybridTimestamp::new_unchecked(wc_end, 0);
            let range = TimeRange::new(start, end).unwrap();

            prop_assert!(range.contains(start), "Range should contain its start");
            prop_assert!(!range.contains(end), "Range should not contain its end (exclusive)");
        }

        /// Property: TimeRange::contains is consistent with the [start, end) semantics.
        #[test]
        fn prop_time_range_contains_semantics(
            wc_start in 0i64..500_000,
            wc_end in 500_001i64..1_000_000,
            wc_point in 0i64..1_000_000,
        ) {
            let start = HybridTimestamp::new_unchecked(wc_start, 0);
            let end = HybridTimestamp::new_unchecked(wc_end, 0);
            let point = HybridTimestamp::new_unchecked(wc_point, 0);
            let range = TimeRange::new(start, end).unwrap();

            let expected = point >= start && point < end;
            prop_assert_eq!(range.contains(point), expected,
                "contains({:?}) mismatch for range [{:?}, {:?})", point, start, end);
        }

        /// Property: TimeRange overlaps is symmetric.
        #[test]
        fn prop_time_range_overlaps_symmetric(
            wc_a in 0i64..500_000,
            len_a in 1i64..500_000,
            wc_b in 0i64..500_000,
            len_b in 1i64..500_000,
        ) {
            let start_a = HybridTimestamp::new_unchecked(wc_a, 0);
            let end_a = HybridTimestamp::new_unchecked(wc_a + len_a, 0);
            let start_b = HybridTimestamp::new_unchecked(wc_b, 0);
            let end_b = HybridTimestamp::new_unchecked(wc_b + len_b, 0);

            let range_a = TimeRange::new(start_a, end_a).unwrap();
            let range_b = TimeRange::new(start_b, end_b).unwrap();

            prop_assert_eq!(range_a.overlaps(&range_b), range_b.overlaps(&range_a),
                "overlaps should be symmetric");
        }

        /// Property: A range always contains itself (reflexive containment).
        #[test]
        fn prop_time_range_contains_self(
            wc_start in 0i64..500_000,
            wc_end in 500_001i64..1_000_000,
        ) {
            let start = HybridTimestamp::new_unchecked(wc_start, 0);
            let end = HybridTimestamp::new_unchecked(wc_end, 0);
            let range = TimeRange::new(start, end).unwrap();

            prop_assert!(range.contains_range(&range),
                "A range should always contain itself");
        }

        /// Property: TimeRange serialization roundtrip preserves values.
        #[test]
        fn prop_time_range_serialization_roundtrip(
            wc_start in valid_wallclock(),
            wc_end in valid_wallclock(),
        ) {
            let (s, e) = if wc_start <= wc_end { (wc_start, wc_end) } else { (wc_end, wc_start) };
            let start = HybridTimestamp::new_unchecked(s, 0);
            let end = HybridTimestamp::new_unchecked(e, 0);
            let range = TimeRange::new(start, end).unwrap();

            let bytes = range.serialize();
            prop_assert_eq!(bytes.len(), 24);
            let (deserialized, consumed) = TimeRange::deserialize(&bytes).unwrap();
            prop_assert_eq!(consumed, 24);
            prop_assert_eq!(deserialized, range);
        }

        // ====================================================================
        // BiTemporalInterval Properties
        // ====================================================================

        /// Property: BiTemporalInterval::current() is always current in both dimensions.
        #[test]
        fn prop_bitemporal_current_is_current(ts in valid_timestamp()) {
            let interval = BiTemporalInterval::current(ts);
            prop_assert!(interval.is_currently_valid());
            prop_assert!(interval.is_currently_recorded());
            prop_assert!(interval.is_current());
        }

        /// Property: BiTemporalInterval serialization roundtrip preserves values.
        #[test]
        fn prop_bitemporal_serialization_roundtrip(
            wc_vs in valid_wallclock(),
            wc_ve in valid_wallclock(),
            wc_ts in valid_wallclock(),
            wc_te in valid_wallclock(),
        ) {
            let (vs, ve) = if wc_vs <= wc_ve { (wc_vs, wc_ve) } else { (wc_ve, wc_vs) };
            let (ts, te) = if wc_ts <= wc_te { (wc_ts, wc_te) } else { (wc_te, wc_ts) };

            let vt = TimeRange::new(
                HybridTimestamp::new_unchecked(vs, 0),
                HybridTimestamp::new_unchecked(ve, 0),
            ).unwrap();
            let tt = TimeRange::new(
                HybridTimestamp::new_unchecked(ts, 0),
                HybridTimestamp::new_unchecked(te, 0),
            ).unwrap();

            let interval = BiTemporalInterval::new(vt, tt);
            let bytes = interval.serialize();
            prop_assert_eq!(bytes.len(), 48);
            let (deserialized, consumed) = BiTemporalInterval::deserialize(&bytes).unwrap();
            prop_assert_eq!(consumed, 48);
            prop_assert_eq!(deserialized, interval);
        }

        /// Property: Closing valid time makes the interval no longer currently valid.
        #[test]
        fn prop_closing_valid_time_makes_not_current(
            wc_start in 0i64..500_000,
            wc_close in 500_001i64..1_000_000,
        ) {
            let start = HybridTimestamp::new_unchecked(wc_start, 0);
            let close = HybridTimestamp::new_unchecked(wc_close, 0);

            let interval = BiTemporalInterval::current(start);
            prop_assert!(interval.is_currently_valid());

            let closed = interval.close_valid_time(close);
            prop_assert!(!closed.is_currently_valid());
            prop_assert_eq!(closed.valid_time().end(), close);
        }

        /// Property: Closing transaction time makes the interval no longer currently recorded.
        #[test]
        fn prop_closing_tx_time_makes_not_recorded(
            wc_start in 0i64..500_000,
            wc_close in 500_001i64..1_000_000,
        ) {
            let start = HybridTimestamp::new_unchecked(wc_start, 0);
            let close = HybridTimestamp::new_unchecked(wc_close, 0);

            let interval = BiTemporalInterval::current(start);
            prop_assert!(interval.is_currently_recorded());

            let closed = interval.close_transaction_time(close);
            prop_assert!(!closed.is_currently_recorded());
            prop_assert_eq!(closed.transaction_time().end(), close);
        }

        /// Property: TimeRange duration is non-negative for valid ranges.
        #[test]
        fn prop_time_range_duration_non_negative(
            wc_start in valid_wallclock(),
            wc_end in valid_wallclock(),
        ) {
            let (s, e) = if wc_start <= wc_end { (wc_start, wc_end) } else { (wc_end, wc_start) };
            let start = HybridTimestamp::new_unchecked(s, 0);
            let end = HybridTimestamp::new_unchecked(e, 0);
            let range = TimeRange::new(start, end).unwrap();

            if let Some(duration) = range.duration_micros() {
                prop_assert!(duration >= 0,
                    "Duration should be non-negative, got {}", duration);
            }
        }

        /// Property: time::from_secs roundtrips through to_secs.
        #[test]
        fn prop_time_secs_roundtrip(secs in 0i64..1_000_000_000) {
            let ts = time::from_secs(secs);
            let result = time::to_secs(ts);
            prop_assert_eq!(result, secs);
        }

        /// Property: time::from_millis roundtrips through to_millis.
        #[test]
        fn prop_time_millis_roundtrip(millis in 0i64..1_000_000_000_000) {
            let ts = time::from_millis(millis);
            let result = time::to_millis(ts);
            prop_assert_eq!(result, millis);
        }
    }
}
