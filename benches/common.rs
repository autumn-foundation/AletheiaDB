//! Common utilities for benchmarks.
//!
//! This module provides shared configuration and helper functions
//! used across all benchmark suites.

use criterion::Criterion;
use std::time::Duration;

/// Configure Criterion with environment variable overrides.
///
/// This function reads the following environment variables:
/// - `BENCH_SAMPLE_SIZE`: Number of samples per benchmark (default: 50)
/// - `BENCH_MEASUREMENT_TIME`: Measurement time in seconds (default: 5)
/// - `BENCH_WARMUP_TIME`: Warm-up time in seconds (default: 3)
///
/// # Examples
///
/// ```ignore
/// // In your benchmark file
/// mod common;
///
/// criterion_group!(
///     name = my_benches;
///     config = common::configure_criterion();
///     targets = my_benchmark_function
/// );
/// ```
///
/// # Environment Variable Usage
///
/// ```bash
/// # Fast CI benchmarks
/// BENCH_SAMPLE_SIZE=10 BENCH_MEASUREMENT_TIME=1 BENCH_WARMUP_TIME=1 cargo bench
///
/// # Production benchmarks
/// BENCH_SAMPLE_SIZE=100 BENCH_MEASUREMENT_TIME=10 BENCH_WARMUP_TIME=5 cargo bench
/// ```
#[allow(dead_code)]
pub fn configure_criterion() -> Criterion {
    let sample_size = std::env::var("BENCH_SAMPLE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    let measurement_time_secs = std::env::var("BENCH_MEASUREMENT_TIME")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let warmup_time_secs = std::env::var("BENCH_WARMUP_TIME")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    Criterion::default()
        .sample_size(sample_size)
        .measurement_time(Duration::from_secs(measurement_time_secs))
        .warm_up_time(Duration::from_secs(warmup_time_secs))
}
