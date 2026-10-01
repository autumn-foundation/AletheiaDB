//! Latency statistics: p50 / p95 / p99 percentile math.
//!
//! Percentiles use the **nearest-rank** method on the ascending-sorted
//! samples: for percentile `p` over `n` samples, `rank = ceil(p/100 * n)` and
//! the reported value is `samples[rank - 1]` (with `p = 0` mapping to the
//! minimum). This method needs no interpolation, is exactly reproducible, and
//! is the standard choice for latency reporting where every reported number is
//! an actually-observed sample.

use serde::{Deserialize, Serialize};

/// Compute a percentile over an **ascending-sorted** slice using the
/// nearest-rank method.
///
/// # Panics
///
/// Panics if `sorted_ascending` is empty. `p` is clamped to `[0.0, 100.0]`.
#[must_use]
pub fn percentile(sorted_ascending: &[f64], p: f64) -> f64 {
    assert!(
        !sorted_ascending.is_empty(),
        "percentile requires a non-empty sample set"
    );
    let n = sorted_ascending.len();
    if n == 1 {
        return sorted_ascending[0];
    }
    let p = p.clamp(0.0, 100.0);
    // Nearest-rank: rank in 0..=n. rank 0 -> index 0 (the minimum).
    let rank = (p / 100.0 * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted_ascending[idx]
}

/// Summary latency statistics for one operation, in microseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyStats {
    /// Number of measured samples.
    pub count: usize,
    /// Minimum observed latency (µs).
    pub min_us: f64,
    /// Maximum observed latency (µs).
    pub max_us: f64,
    /// Arithmetic mean latency (µs).
    pub mean_us: f64,
    /// Median latency (µs).
    pub p50_us: f64,
    /// 95th percentile latency (µs).
    pub p95_us: f64,
    /// 99th percentile latency (µs).
    pub p99_us: f64,
}

impl LatencyStats {
    /// Compute statistics from a set of per-iteration latencies (µs). The input
    /// is copied and sorted internally, so callers may pass unsorted samples.
    ///
    /// # Panics
    ///
    /// Panics if `samples_us` is empty.
    #[must_use]
    pub fn from_samples(samples_us: &[f64]) -> Self {
        assert!(
            !samples_us.is_empty(),
            "LatencyStats requires at least one sample"
        );
        let mut sorted = samples_us.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let count = sorted.len();
        let sum: f64 = sorted.iter().sum();
        Self {
            count,
            min_us: sorted[0],
            max_us: sorted[count - 1],
            mean_us: sum / count as f64,
            p50_us: percentile(&sorted, 50.0),
            p95_us: percentile(&sorted, 95.0),
            p99_us: percentile(&sorted, 99.0),
        }
    }
}
