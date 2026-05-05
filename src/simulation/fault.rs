//! Fault injection for deterministic simulation testing.
//!
//! `FaultConfig` describes what faults to inject; `FaultInjector` applies them
//! deterministically using a seeded PRNG so every run with the same seed
//! reproduces the identical fault sequence.

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::cell::RefCell;

/// Taxonomy of injectable faults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultType {
    /// Flip random bits in storage data.
    StorageCorruption,
    /// Simulate slow I/O by recording an artificial latency.
    StorageLatency {
        /// Simulated latency in microseconds.
        micros: u64,
    },
    /// Simulate a process crash once `byte_offset` bytes have been written.
    CrashAtOffset {
        /// Cumulative byte offset at which the crash is triggered.
        byte_offset: u64,
    },
    /// Inject a clock discontinuity.
    ClockJump {
        /// Microseconds to jump (positive = forward, negative = backward).
        delta_micros: i64,
    },
}

/// Configuration for the fault injector.
///
/// Build with the builder methods:
/// ```ignore
/// let cfg = FaultConfig::default()
///     .with_corruption_rate(0.1)
///     .with_crash_at_offset(4096);
/// ```
#[derive(Debug, Clone)]
pub struct FaultConfig {
    /// Probability [0.0, 1.0] that a storage write is corrupted.
    pub corruption_rate: f64,
    /// If set, writing past this cumulative byte offset triggers a simulated crash.
    pub crash_at_byte_offset: Option<u64>,
}

impl Default for FaultConfig {
    fn default() -> Self {
        Self {
            corruption_rate: 0.0,
            crash_at_byte_offset: None,
        }
    }
}

impl FaultConfig {
    /// Set the probability that any individual storage write is corrupted.
    pub fn with_corruption_rate(mut self, rate: f64) -> Self {
        assert!(
            (0.0..=1.0).contains(&rate),
            "corruption_rate must be in [0.0, 1.0]"
        );
        self.corruption_rate = rate;
        self
    }

    /// Trigger a simulated crash once the cumulative bytes written reach `offset`.
    pub fn with_crash_at_offset(mut self, offset: u64) -> Self {
        self.crash_at_byte_offset = Some(offset);
        self
    }
}

/// Applies faults described by a [`FaultConfig`] using a seeded PRNG.
///
/// All decisions are drawn from the same PRNG in order, so the same seed
/// always produces the same fault sequence.
#[derive(Debug)]
pub struct FaultInjector {
    config: FaultConfig,
    /// Interior-mutable so `try_corrupt` can be called via `&self`.
    rng: RefCell<SmallRng>,
}

impl FaultInjector {
    /// Create a new injector from `config` and a deterministic `seed`.
    pub fn new(config: FaultConfig, seed: u64) -> Self {
        Self {
            config,
            rng: RefCell::new(SmallRng::seed_from_u64(seed)),
        }
    }

    /// Return the underlying fault configuration.
    pub fn config(&self) -> &FaultConfig {
        &self.config
    }

    /// Possibly corrupt `data` in-place according to the configured corruption rate.
    ///
    /// At `rate = 0.0` the data is never modified.
    /// At `rate = 1.0` every byte is XOR'd with a random non-zero mask.
    pub fn try_corrupt(&self, data: &mut [u8]) {
        if data.is_empty() || self.config.corruption_rate == 0.0 {
            return;
        }

        let mut rng = self.rng.borrow_mut();
        if self.config.corruption_rate >= 1.0
            || rng.gen_range(0.0_f64..1.0_f64) < self.config.corruption_rate
        {
            // Flip a random bit in each byte to guarantee at least one change.
            for byte in data.iter_mut() {
                let mask: u8 = rng.gen_range(1_u8..=255_u8);
                *byte ^= mask;
            }
        }
    }

    /// Return `true` if `bytes_written_so_far` has reached or exceeded the
    /// configured crash-at-byte-offset threshold.
    pub fn should_crash_at(&self, bytes_written_so_far: u64) -> bool {
        match self.config.crash_at_byte_offset {
            Some(threshold) => bytes_written_so_far >= threshold,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_rate_never_corrupts() {
        let fi = FaultInjector::new(FaultConfig::default(), 0);
        let mut data = vec![0xAB_u8; 32];
        let original = data.clone();
        for _ in 0..100 {
            fi.try_corrupt(&mut data);
        }
        assert_eq!(data, original);
    }

    #[test]
    fn full_rate_always_corrupts() {
        let cfg = FaultConfig::default().with_corruption_rate(1.0);
        let fi = FaultInjector::new(cfg, 1);
        let mut data = vec![0x00_u8; 8];
        fi.try_corrupt(&mut data);
        assert!(data.iter().any(|&b| b != 0));
    }

    #[test]
    fn crash_threshold_triggers_at_offset() {
        let cfg = FaultConfig::default().with_crash_at_offset(100);
        let fi = FaultInjector::new(cfg, 0);
        assert!(!fi.should_crash_at(99));
        assert!(fi.should_crash_at(100));
        assert!(fi.should_crash_at(101));
    }

    #[test]
    fn same_seed_same_output() {
        let cfg = FaultConfig::default().with_corruption_rate(0.5);
        let fi_a = FaultInjector::new(cfg.clone(), 42);
        let fi_b = FaultInjector::new(cfg, 42);
        let mut da = vec![0xFF_u8; 16];
        let mut db = da.clone();
        fi_a.try_corrupt(&mut da);
        fi_b.try_corrupt(&mut db);
        assert_eq!(da, db);
    }
}
