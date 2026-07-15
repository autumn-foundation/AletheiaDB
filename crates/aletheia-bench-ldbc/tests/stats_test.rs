//! Percentile math tests (p50/p95/p99 on known inputs).

use aletheia_bench_ldbc::stats::{LatencyStats, percentile};

fn one_to_hundred() -> Vec<f64> {
    (1..=100).map(|x| x as f64).collect()
}

#[test]
fn percentile_nearest_rank_on_1_to_100() {
    let s = one_to_hundred();
    // Nearest-rank: rank = ceil(p/100 * n); value = s[rank-1].
    assert_eq!(percentile(&s, 50.0), 50.0, "p50");
    assert_eq!(percentile(&s, 95.0), 95.0, "p95");
    assert_eq!(percentile(&s, 99.0), 99.0, "p99");
    assert_eq!(percentile(&s, 100.0), 100.0, "p100 == max");
    assert_eq!(percentile(&s, 0.0), 1.0, "p0 == min");
}

#[test]
fn percentile_small_odd_set() {
    // [1,2,3,4,5]: p50 -> ceil(2.5)=3 -> index 2 -> value 3.
    let s = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    assert_eq!(percentile(&s, 50.0), 3.0);
    assert_eq!(percentile(&s, 99.0), 5.0);
}

#[test]
fn percentile_single_element() {
    assert_eq!(percentile(&[42.0], 50.0), 42.0);
    assert_eq!(percentile(&[42.0], 99.0), 42.0);
}

#[test]
fn latency_stats_from_unsorted_samples() {
    // Deliberately unsorted input; from_samples must sort internally.
    let samples = vec![5.0, 1.0, 3.0, 2.0, 4.0];
    let stats = LatencyStats::from_samples(&samples);
    assert_eq!(stats.count, 5);
    assert_eq!(stats.min_us, 1.0);
    assert_eq!(stats.max_us, 5.0);
    assert_eq!(stats.mean_us, 3.0);
    assert_eq!(stats.p50_us, 3.0);
    assert_eq!(stats.p99_us, 5.0);
}

#[test]
fn latency_stats_full_range() {
    let stats = LatencyStats::from_samples(&one_to_hundred());
    assert_eq!(stats.count, 100);
    assert_eq!(stats.p50_us, 50.0);
    assert_eq!(stats.p95_us, 95.0);
    assert_eq!(stats.p99_us, 99.0);
    assert!((stats.mean_us - 50.5).abs() < 1e-9);
}
