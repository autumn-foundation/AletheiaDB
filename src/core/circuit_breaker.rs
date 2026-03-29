#![allow(clippy::collapsible_if)]
//! Circuit breaker implementation for protecting against cascading failures.
//!
//! A circuit breaker acts as a proxy for operations that might fail.
//! It monitors failures and when they reach a certain threshold, it "trips" (opens),
//! returning errors immediately without attempting the operation.
//! After a timeout, it allows a limited number of test requests to pass through
//! (half-open state). If they succeed, it closes the circuit and resumes normal operation.

use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Circuit is closed (normal operation).
    Closed,
    /// Circuit is open (rejecting requests).
    Open,
    /// Circuit is half-open (testing if service recovered).
    HalfOpen,
}

/// Configuration for circuit breaker.
///
/// The `CircuitBreakerConfig` defines the thresholds and durations for the
/// [`CircuitBreaker`] to transition between `Closed`, `Open`, and `HalfOpen` states.
/// It helps prevent cascading failures in a distributed setup when remote nodes are unresponsive.
///
/// # Examples
///
/// ```rust
/// # #[cfg(feature = "nova")]
/// # fn main() {
/// use aletheiadb::core::circuit_breaker::CircuitBreakerConfig;
/// use std::time::Duration;
///
/// let config = CircuitBreakerConfig {
///     failure_threshold: 5,
///     open_duration: Duration::from_secs(30),
///     success_threshold: 3,
///     failure_window: Duration::from_secs(60),
/// };
/// # }
/// # #[cfg(not(feature = "nova"))]
/// # fn main() {}
/// ```
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening circuit.
    pub failure_threshold: usize,
    /// Duration to keep circuit open.
    pub open_duration: Duration,
    /// Number of successes in half-open to close circuit.
    pub success_threshold: usize,
    /// Window for counting failures.
    pub failure_window: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            open_duration: Duration::from_secs(30),
            success_threshold: 3,
            failure_window: Duration::from_secs(60),
        }
    }
}

/// Circuit breaker for protecting against cascading failures.
///
/// The `CircuitBreaker` tracks failures and successes of requests. If failures exceed the configured
/// threshold, it transitions to an `Open` state, failing fast. After a timeout, it transitions
/// to a `HalfOpen` state to test if the service has recovered.
///
/// # Examples
///
/// ```rust
/// # #[cfg(feature = "nova")]
/// # fn main() {
/// use aletheiadb::core::circuit_breaker::{CircuitBreakerConfig, CircuitBreaker, CircuitState};
///
/// let config = CircuitBreakerConfig::default();
/// let breaker = CircuitBreaker::new(config);
///
/// // Initially, the circuit is closed and allows requests
/// assert_eq!(breaker.state(), CircuitState::Closed);
/// assert!(breaker.should_allow());
/// # }
/// # #[cfg(not(feature = "nova"))]
/// # fn main() {}
/// ```
#[derive(Debug)]
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    state: RwLock<CircuitState>,
    failure_count: AtomicUsize,
    success_count: AtomicUsize,
    last_failure: RwLock<Option<Instant>>,
    opened_at: RwLock<Option<Instant>>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            state: RwLock::new(CircuitState::Closed),
            failure_count: AtomicUsize::new(0),
            success_count: AtomicUsize::new(0),
            last_failure: RwLock::new(None),
            opened_at: RwLock::new(None),
        }
    }

    /// Get the current state.
    ///
    /// If the lock is poisoned, returns Closed (fail-open) to prevent
    /// cascading failures.
    pub fn state(&self) -> CircuitState {
        self.maybe_transition();
        self.state
            .read()
            .map(|s| *s)
            .unwrap_or(CircuitState::Closed)
    }

    /// Check if requests should be allowed.
    ///
    /// If the lock is poisoned, returns true (fail-open) to prevent
    /// cascading failures.
    pub fn should_allow(&self) -> bool {
        self.maybe_transition();
        let state = self
            .state
            .read()
            .map(|s| *s)
            .unwrap_or(CircuitState::Closed);
        matches!(state, CircuitState::Closed | CircuitState::HalfOpen)
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        let state = match self.state.read() {
            Ok(s) => *s,
            Err(_) => return, // Lock poisoned, silently skip
        };

        match state {
            CircuitState::Closed => {
                // Reset failure count on success
                self.failure_count.store(0, Ordering::SeqCst);
            }
            CircuitState::HalfOpen => {
                let successes = self.success_count.fetch_add(1, Ordering::SeqCst) + 1;
                if successes >= self.config.success_threshold {
                    // Transition to closed
                    if let Ok(mut s) = self.state.write() {
                        *s = CircuitState::Closed;
                    }
                    self.failure_count.store(0, Ordering::SeqCst);
                    self.success_count.store(0, Ordering::SeqCst);
                }
            }
            CircuitState::Open => {
                // Do not reset the timer when the circuit is already open.
                // This allows it to transition to HalfOpen after the original duration.
            }
        }
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        let state = match self.state.read() {
            Ok(s) => *s,
            Err(_) => return, // Lock poisoned, silently skip
        };

        match state {
            CircuitState::Closed => {
                // Check if we should reset due to window expiry
                if let Ok(last) = self.last_failure.read() {
                    if let Some(last_time) = *last {
                        if last_time.elapsed() > self.config.failure_window {
                            self.failure_count.store(0, Ordering::SeqCst);
                        }
                    }
                }

                let failures = self.failure_count.fetch_add(1, Ordering::SeqCst) + 1;

                if let Ok(mut last) = self.last_failure.write() {
                    *last = Some(Instant::now());
                }

                if failures >= self.config.failure_threshold {
                    // Transition to open
                    if let Ok(mut s) = self.state.write() {
                        *s = CircuitState::Open;
                    }
                    if let Ok(mut opened) = self.opened_at.write() {
                        *opened = Some(Instant::now());
                    }
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in half-open goes back to open
                if let Ok(mut s) = self.state.write() {
                    *s = CircuitState::Open;
                }
                if let Ok(mut opened) = self.opened_at.write() {
                    *opened = Some(Instant::now());
                }
                self.success_count.store(0, Ordering::SeqCst);
            }
            CircuitState::Open => {
                // Do not reset the timer when the circuit is already open.
                // This allows it to transition to HalfOpen after the original duration.
            }
        }
    }

    /// Check and perform state transitions based on time.
    fn maybe_transition(&self) {
        // Read opened_at first to avoid holding multiple locks (from distributed.rs)
        let should_transition = self
            .opened_at
            .read()
            .ok()
            .and_then(|opened| *opened)
            .is_some_and(|opened_time| opened_time.elapsed() >= self.config.open_duration);

        if !should_transition {
            return;
        }

        // Now acquire state write lock and verify state is still Open
        let mut state_guard = match self.state.write() {
            Ok(s) => s,
            Err(_) => return, // Lock poisoned, skip transition
        };

        // Double-check state is still Open (could have changed)
        if *state_guard == CircuitState::Open {
            *state_guard = CircuitState::HalfOpen;
            self.success_count.store(0, Ordering::SeqCst);
        }
    }

    /// Get remaining time before circuit can close.
    pub fn remaining_open_time(&self) -> Option<Duration> {
        let state = self.state.read().ok()?;
        if *state != CircuitState::Open {
            return None;
        }

        if let Ok(opened) = self.opened_at.read() {
            if let Some(opened_time) = *opened {
                let elapsed = opened_time.elapsed();
                if elapsed < self.config.open_duration {
                    return Some(self.config.open_duration - elapsed);
                }
            }
        }
        None
    }

    /// Reset the circuit breaker to closed state.
    pub fn reset(&self) {
        if let Ok(mut s) = self.state.write() {
            *s = CircuitState::Closed;
        }
        self.failure_count.store(0, Ordering::SeqCst);
        self.success_count.store(0, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_initial_state() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.should_allow());
    }

    #[test]
    fn test_circuit_breaker_opens_on_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        // Record failures
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.should_allow());
    }

    #[test]
    fn test_circuit_breaker_success_resets_count() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        cb.record_failure();
        cb.record_success(); // Should reset

        // These failures shouldn't trigger open
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure(); // Now it should open
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_half_open_transition() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            open_duration: Duration::from_millis(10),
            success_threshold: 2,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for open duration
        std::thread::sleep(Duration::from_millis(20));

        assert_eq!(cb.state(), CircuitState::HalfOpen);
        assert!(cb.should_allow());
    }

    #[test]
    fn test_circuit_breaker_closes_from_half_open() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            open_duration: Duration::from_millis(10),
            success_threshold: 2,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_failure_in_half_open() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            open_duration: Duration::from_millis(10),
            success_threshold: 2,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        cb.reset();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.should_allow());
    }

    #[test]
    fn test_circuit_breaker_remaining_time() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            open_duration: Duration::from_secs(30),
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        assert!(cb.remaining_open_time().is_none());

        cb.record_failure();
        let remaining = cb.remaining_open_time();
        assert!(remaining.is_some());
        assert!(remaining.unwrap() <= Duration::from_secs(30));
    }
}
