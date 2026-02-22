//! Hybrid Logical Clock (HLC) implementation for distributed temporal consistency.
//!
//! HLCs combine physical wallclock time with logical counters to provide:
//! - Monotonic ordering despite clock skew
//! - Causality preservation across distributed nodes
//! - Human-readable wallclock semantics for temporal queries

use crate::core::error::{StorageError, TemporalError};
use crate::core::temporal::MAX_VALID_TIMESTAMP;
#[cfg(test)]
use std::cell::Cell;
use std::sync::OnceLock;

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

/// Maximum allowed backward clock drift in microseconds (5 minutes).
///
/// Backward drift indicates local wallclock moved behind the last committed HLC wallclock.
/// Beyond this threshold we either reject the write or self-heal by clamping to the frontier.
pub const MAX_BACKWARD_DRIFT_US: i64 = 5 * 60 * 1_000_000;

/// Maximum allowed forward clock jump in microseconds (1 hour).
///
/// Callers may choose to disable this check (for example when idle time is expected and
/// forward drift is measured against the last commit timestamp).
pub const MAX_FORWARD_JUMP_US: i64 = 60 * 60 * 1_000_000;

#[cfg(test)]
thread_local! {
    static CLOCK_SKEW_AUTO_HEAL_TEST_OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Test-only guard to force auto-heal behavior for the current thread.
///
/// This lets unit tests stay deterministic regardless of ambient process environment.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ClockSkewAutoHealTestGuard {
    previous: Option<bool>,
}

#[cfg(test)]
impl ClockSkewAutoHealTestGuard {
    pub(crate) fn force(enabled: bool) -> Self {
        let previous = CLOCK_SKEW_AUTO_HEAL_TEST_OVERRIDE.with(|cell| {
            let prev = cell.get();
            cell.set(Some(enabled));
            prev
        });
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for ClockSkewAutoHealTestGuard {
    fn drop(&mut self) {
        CLOCK_SKEW_AUTO_HEAL_TEST_OVERRIDE.with(|cell| cell.set(self.previous));
    }
}

/// Whether clock-skew self-healing is enabled via environment variable.
///
/// Enabled values: `1`, `true`, `on`, `yes`, `enabled` (case-insensitive).
pub fn is_clock_skew_self_heal_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = CLOCK_SKEW_AUTO_HEAL_TEST_OVERRIDE.with(|cell| cell.get()) {
        return forced;
    }

    static CLOCK_SKEW_AUTO_HEAL: OnceLock<Option<bool>> = OnceLock::new();
    CLOCK_SKEW_AUTO_HEAL
        .get_or_init(|| {
            std::env::var("ALETHEIADB_AUTO_HEAL_CLOCK_SKEW")
                .ok()
                .map(|value| {
                    matches!(
                        value.to_ascii_lowercase().as_str(),
                        "1" | "true" | "on" | "yes" | "enabled"
                    )
                })
        })
        .unwrap_or(false)
}

/// Direction of wallclock drift relative to the HLC frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSkewDirection {
    /// Current wallclock is behind frontier by more than allowed.
    Backward,
    /// Current wallclock is ahead of frontier by more than allowed.
    Forward,
}

impl ClockSkewDirection {
    /// Human-readable direction label for logs and error messages.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Backward => "backward",
            Self::Forward => "forward",
        }
    }
}

/// Result of evaluating wallclock drift against configured skew policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSkewDecision {
    /// Signed drift in microseconds (`current_wallclock - frontier_wallclock`).
    pub drift_us: i64,
    /// Wallclock to pass into `HybridTimestamp::send`.
    pub effective_wallclock: i64,
    /// Drift direction that was self-healed (if any).
    pub healed_direction: Option<ClockSkewDirection>,
}

/// Drift exceeded configured threshold and self-healing was disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSkewViolation {
    /// Signed drift in microseconds (`current_wallclock - frontier_wallclock`).
    pub drift_us: i64,
    /// Threshold used for this direction (signed to preserve caller semantics).
    pub max_allowed: i64,
    /// Direction of the violation.
    pub direction: ClockSkewDirection,
}

/// Evaluate clock skew and derive the effective wallclock for `send()`.
///
/// When self-healing is enabled and drift exceeds threshold, this clamps to the frontier
/// wallclock so HLC remains monotonic without aborting.
pub fn evaluate_clock_skew(
    current_wallclock: i64,
    frontier_wallclock: i64,
    max_forward_jump_us: Option<i64>,
    self_heal_clock_skew: bool,
) -> Result<ClockSkewDecision, ClockSkewViolation> {
    let drift = current_wallclock - frontier_wallclock;
    let mut effective_wallclock = current_wallclock;
    let mut healed_direction = None;

    if drift < -MAX_BACKWARD_DRIFT_US {
        if !self_heal_clock_skew {
            return Err(ClockSkewViolation {
                drift_us: drift,
                max_allowed: -MAX_BACKWARD_DRIFT_US,
                direction: ClockSkewDirection::Backward,
            });
        }

        effective_wallclock = frontier_wallclock;
        healed_direction = Some(ClockSkewDirection::Backward);
    }

    if let Some(max_forward_jump_us) = max_forward_jump_us
        && drift > max_forward_jump_us
    {
        if !self_heal_clock_skew {
            return Err(ClockSkewViolation {
                drift_us: drift,
                max_allowed: max_forward_jump_us,
                direction: ClockSkewDirection::Forward,
            });
        }

        effective_wallclock = frontier_wallclock;
        healed_direction = Some(ClockSkewDirection::Forward);
    }

    Ok(ClockSkewDecision {
        drift_us: drift,
        effective_wallclock,
        healed_direction,
    })
}

/// Error stage from `send_with_overflow_self_heal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendWithSelfHealError {
    /// Initial call to `send()` failed.
    InitialSend(TemporalError),
    /// Logical overflow was detected but `wallclock + 1` overflowed i64.
    FallbackWallclockOverflow {
        /// Wallclock from the original overflow error.
        wallclock: i64,
        /// Logical counter from the original overflow error.
        current_logical: u32,
    },
    /// Fallback call to `send(wallclock + 1)` failed.
    FallbackSend(TemporalError),
}

/// Call `send()` with optional logical-overflow self-healing.
///
/// If self-heal is enabled and logical overflow occurs, this retries at `wallclock + 1`
/// to preserve monotonicity under repeated clamping.
pub fn send_with_overflow_self_heal<E>(
    frontier: &HybridTimestamp,
    effective_wallclock: i64,
    self_heal_clock_skew: bool,
    map_error: impl Fn(SendWithSelfHealError) -> E,
) -> Result<HybridTimestamp, E> {
    match frontier.send(effective_wallclock) {
        Ok(next) => Ok(next),
        Err(TemporalError::LogicalCounterOverflow {
            wallclock,
            current_logical,
        }) if self_heal_clock_skew => {
            let fallback_wallclock = wallclock.checked_add(1).ok_or_else(|| {
                map_error(SendWithSelfHealError::FallbackWallclockOverflow {
                    wallclock,
                    current_logical,
                })
            })?;
            frontier
                .send(fallback_wallclock)
                .map_err(|error| map_error(SendWithSelfHealError::FallbackSend(error)))
        }
        Err(error) => Err(map_error(SendWithSelfHealError::InitialSend(error))),
    }
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

    /// Get the wallclock component in seconds since Unix epoch.
    #[inline]
    pub const fn as_secs(&self) -> i64 {
        self.wallclock / 1_000_000
    }

    /// Get the wallclock component in milliseconds since Unix epoch.
    #[inline]
    pub const fn as_millis(&self) -> i64 {
        self.wallclock / 1_000
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

        // Validate that TIMESTAMP_MAX sentinel (i64::MAX) must have logical counter 0
        if wallclock == i64::MAX && logical != 0 {
            return Err(StorageError::CorruptedData(format!(
                "Deserialized timestamp has wallclock i64::MAX but non-zero logical counter: {}",
                logical
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
    fn test_receive_collision_oracle() {
        // Oracle Test: Hardcoded scenario to verify collision logic without reconstructing the formula
        // Scenario: Local(1000, 10) receives Msg(1000, 20) with physical clock 1000.
        // Rule: max(1000, 1000, 1000) -> 1000.
        // Logical: max(10, 20) + 1 = 21.
        let local = HybridTimestamp::new(1000, 10).unwrap();
        let msg = HybridTimestamp::new(1000, 20).unwrap();
        let next = local.receive(msg, 1000).unwrap();

        assert_eq!(next.wallclock(), 1000);
        assert_eq!(next.logical(), 21);

        // Scenario: Local(1000, 20) receives Msg(1000, 10) with physical clock 1000.
        // Rule: max(1000, 1000, 1000) -> 1000.
        // Logical: max(20, 10) + 1 = 21.
        let local = HybridTimestamp::new(1000, 20).unwrap();
        let msg = HybridTimestamp::new(1000, 10).unwrap();
        let next = local.receive(msg, 1000).unwrap();

        assert_eq!(next.wallclock(), 1000);
        assert_eq!(next.logical(), 21);
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
    fn test_evaluate_clock_skew_backward_violation_without_self_heal() {
        let current_wallclock = 1_000_000;
        let frontier_wallclock = current_wallclock + MAX_BACKWARD_DRIFT_US + 1;

        let result = evaluate_clock_skew(
            current_wallclock,
            frontier_wallclock,
            Some(MAX_FORWARD_JUMP_US),
            false,
        );

        assert!(matches!(
            result,
            Err(ClockSkewViolation {
                direction: ClockSkewDirection::Backward,
                ..
            })
        ));
    }

    #[test]
    fn test_evaluate_clock_skew_allows_unbounded_forward_drift() {
        let current_wallclock = 10_000_000;
        let frontier_wallclock = 1_000;

        let result = evaluate_clock_skew(current_wallclock, frontier_wallclock, None, false)
            .expect("forward drift should be ignored when no forward bound is provided");

        assert_eq!(result.effective_wallclock, current_wallclock);
        assert!(result.healed_direction.is_none());
    }

    #[test]
    fn test_send_with_overflow_self_heal_uses_fallback_wallclock() {
        let ts = HybridTimestamp::new(1000, u32::MAX).unwrap();

        let next = send_with_overflow_self_heal(&ts, 1000, true, |error| error)
            .expect("self-heal should bump wallclock and recover from logical overflow");

        assert_eq!(next.wallclock(), 1001);
        assert_eq!(next.logical(), 0);
    }

    #[test]
    fn test_send_with_overflow_self_heal_reports_initial_error_when_disabled() {
        let ts = HybridTimestamp::new(1000, u32::MAX).unwrap();

        let err = send_with_overflow_self_heal(&ts, 1000, false, |error| error)
            .expect_err("overflow should fail when self-heal is disabled");

        assert!(matches!(
            err,
            SendWithSelfHealError::InitialSend(TemporalError::LogicalCounterOverflow { .. })
        ));
    }

    #[test]
    fn test_send_with_overflow_self_heal_reports_fallback_wallclock_overflow() {
        let ts = HybridTimestamp::new_unchecked(i64::MAX, u32::MAX);

        let err = send_with_overflow_self_heal(&ts, MAX_VALID_TIMESTAMP, true, |error| error)
            .expect_err("fallback wallclock increment should fail at i64::MAX");

        assert!(matches!(
            err,
            SendWithSelfHealError::FallbackWallclockOverflow {
                wallclock: i64::MAX,
                current_logical: u32::MAX
            }
        ));
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

    #[test]
    fn test_as_secs_and_millis() {
        // Test basic conversion
        let secs = 1234567890;
        let ts = HybridTimestamp::new(secs * 1_000_000, 0).unwrap();
        assert_eq!(ts.as_secs(), secs);
        assert_eq!(ts.as_millis(), secs * 1000);

        // Test with milliseconds precision
        let millis = 1234567890123;
        let ts = HybridTimestamp::new(millis * 1000, 0).unwrap();
        assert_eq!(ts.as_secs(), millis / 1000);
        assert_eq!(ts.as_millis(), millis);

        // Test with microseconds precision (should truncate)
        let micros = 1234567890123456;
        let ts = HybridTimestamp::new(micros, 0).unwrap();
        assert_eq!(ts.as_secs(), micros / 1_000_000);
        assert_eq!(ts.as_millis(), micros / 1000);
    }

    #[test]
    fn test_receive_when_physical_equals_local_and_ahead_of_msg() {
        // Scenario: Physical clock catches up to local clock, and both are ahead of message.
        // HLC should increment logical counter, not reset it.
        // This targets the specific gap where `new_wallclock == self.wallclock` but strict `>` was mutated to `>=`.

        let local = HybridTimestamp::new(1000, 5).unwrap();
        let msg = HybridTimestamp::new(900, 0).unwrap();
        let physical = 1000; // Equals local.wallclock

        // Expected: new_wallclock = 1000.
        // Logic should fall through to "new_wallclock == self.wallclock" branch.
        // Result logical should be local.logical + 1 = 6.

        let result = local.receive(msg, physical).unwrap();

        assert_eq!(result.wallclock(), 1000);
        assert_eq!(
            result.logical(),
            6,
            "Logical counter should increment, not reset"
        );
        assert!(
            result > local,
            "Result should be strictly greater than local"
        );
    }

    #[test]
    fn test_evaluate_clock_skew_self_heal_backward() {
        let current_wallclock = 1_000_000;
        let frontier_wallclock = current_wallclock + MAX_BACKWARD_DRIFT_US + 1;

        let result = evaluate_clock_skew(
            current_wallclock,
            frontier_wallclock,
            None,
            true, // self_heal = true
        )
        .expect("backward drift should be healed");

        assert_eq!(result.effective_wallclock, frontier_wallclock);
        assert_eq!(result.healed_direction, Some(ClockSkewDirection::Backward));
    }

    #[test]
    fn test_evaluate_clock_skew_self_heal_forward() {
        let current_wallclock = 1_000_000;
        let max_jump = 10_000;
        let frontier_wallclock = current_wallclock - max_jump - 1;
        // drift = current - frontier = max_jump + 1 > max_jump

        let result = evaluate_clock_skew(
            current_wallclock,
            frontier_wallclock,
            Some(max_jump),
            true, // self_heal = true
        )
        .expect("forward drift should be healed");

        assert_eq!(result.effective_wallclock, frontier_wallclock);
        assert_eq!(result.healed_direction, Some(ClockSkewDirection::Forward));
    }

    #[test]
    fn test_send_with_overflow_self_heal_fallback_send_error() {
        // Trigger FallbackSend error:
        // 1. Initial send fails with LogicalCounterOverflow.
        // 2. Fallback (wallclock + 1) fails with InvalidTimestamp (because wallclock + 1 > MAX_VALID_TIMESTAMP).

        // Setup:
        // wallclock = MAX_VALID_TIMESTAMP
        // logical = u32::MAX
        // send(MAX_VALID_TIMESTAMP) -> LogicalCounterOverflow
        // fallback = MAX_VALID_TIMESTAMP + 1 -> send(MAX_VALID_TIMESTAMP + 1) -> InvalidTimestamp

        let ts = HybridTimestamp::new_unchecked(MAX_VALID_TIMESTAMP, u32::MAX);

        let err = send_with_overflow_self_heal(&ts, MAX_VALID_TIMESTAMP, true, |error| error)
            .expect_err("should return FallbackSend error");

        assert!(matches!(
            err,
            SendWithSelfHealError::FallbackSend(TemporalError::InvalidTimestamp { .. })
        ));
    }

    #[test]
    fn test_receive_ignores_msg_logical_when_wallclock_behind() {
        // Scenario: Local clock is ahead of message clock, but message has a huge logical counter.
        // We must ensure that we don't accidentally pick up the message's logical counter.
        //
        // This targets a mutant that replaces `&&` with `||` in the collision check:
        // `if new_wallclock == self.wallclock && new_wallclock == msg.wallclock`
        // vs
        // `if new_wallclock == self.wallclock || new_wallclock == msg.wallclock`

        let local = HybridTimestamp::new(1000, 10).unwrap();
        let msg = HybridTimestamp::new(900, 999).unwrap(); // Wallclock behind, but logical huge
        let physical = 1000;

        // new_wallclock should be 1000 (local).
        // It matches local.wallclock, but NOT msg.wallclock.
        // Should increment local.logical (10 -> 11).
        // Should NOT consider msg.logical (999).

        let result = local.receive(msg, physical).unwrap();

        assert_eq!(result.wallclock(), 1000);
        assert_eq!(
            result.logical(),
            11,
            "Should increment local logical, ignoring msg logical"
        );
    }

    #[test]
    fn test_evaluate_clock_skew_exact_boundaries() {
        // Test exact boundaries for clock skew checks.
        // Targets mutants changing `>` to `>=`.

        let current = 1_000_000;

        // Forward boundary: drift == max_jump
        let max_jump = 100;
        let frontier_forward = current - max_jump; // drift = 100
        let result_forward = evaluate_clock_skew(current, frontier_forward, Some(max_jump), false);
        assert!(
            result_forward.is_ok(),
            "Exact max forward jump should be allowed"
        );

        // Backward boundary: drift == -MAX_BACKWARD_DRIFT_US
        // drift = -MAX_BACKWARD_DRIFT_US
        let frontier_backward = current + MAX_BACKWARD_DRIFT_US;
        let result_backward = evaluate_clock_skew(current, frontier_backward, None, false);
        assert!(
            result_backward.is_ok(),
            "Exact max backward drift should be allowed"
        );
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
            // However, we should handle the Result explicitly.
            match current.send(new_wallclock) {
                Ok(next) => {
                    prop_assert!(next > current);
                    prop_assert!(next.wallclock() >= new_wallclock);
                    prop_assert!(next.wallclock() >= current.wallclock());
                }
                Err(e) => {
                    // Only overflow errors are expected for valid inputs
                    prop_assert!(
                        matches!(e, TemporalError::LogicalCounterOverflow { .. }),
                        "Unexpected error: {:?}",
                        e
                    );
                }
            }
        }

        /// Property: receive() result is >= local, >= msg, and >= physical (wallclock).
        #[test]
        fn prop_receive_causality(
            local in valid_timestamp(),
            msg in valid_timestamp(),
            physical in valid_wallclock()
        ) {
            match local.receive(msg, physical) {
                Ok(next) => {
                    prop_assert!(next > local, "next > local");
                    prop_assert!(next > msg, "next > msg");
                    prop_assert!(next.wallclock() >= local.wallclock());
                    prop_assert!(next.wallclock() >= msg.wallclock());
                    prop_assert!(next.wallclock() >= physical);
                }
                Err(e) => {
                    // Only overflow errors are expected for valid inputs
                    prop_assert!(
                        matches!(e, TemporalError::LogicalCounterOverflow { .. }),
                        "Unexpected error: {:?}",
                        e
                    );
                }
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
            match local.receive(msg, wallclock) {
                Ok(next) => {
                    prop_assert!(next > local, "next > local (collision)");
                    prop_assert!(next > msg, "next > msg (collision)");
                    prop_assert_eq!(next.wallclock(), wallclock);

                    // Specifically verify the logical counter logic: max(l1, l2) + 1
                    let expected_logical = local_logical.max(msg_logical).checked_add(1);
                    if let Some(expected) = expected_logical {
                        prop_assert_eq!(next.logical(), expected);
                    } else {
                        prop_assert!(false, "Expected overflow error, but got Ok");
                    }
                }
                Err(e) => {
                    // Only overflow errors are expected
                    prop_assert!(
                        matches!(e, TemporalError::LogicalCounterOverflow { .. }),
                        "Unexpected error: {:?}",
                        e
                    );
                }
            }
        }
    }
}
