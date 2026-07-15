//! Regression-gate decision tests, including the mandated synthetic 2x
//! slowdown that MUST fail the gate.

use aletheia_bench_ldbc::gate::{evaluate_gate, evaluate_gate_with_floor};
use aletheia_bench_ldbc::report::{
    BenchmarkReport, Category, HardwareInfo, OperationResult, RunConfig, SCHEMA_VERSION,
};
use aletheia_bench_ldbc::stats::LatencyStats;

/// Build a report whose operations have the given (key, p99) values. All other
/// percentiles are set equal to p99 (sufficient for gate tests, which only
/// read p99).
fn report_with(ops: &[(&str, f64)]) -> BenchmarkReport {
    let operations = ops
        .iter()
        .map(|(key, p99)| OperationResult {
            key: (*key).to_string(),
            description: String::new(),
            snb_analogue: "TEST".to_string(),
            category: Category::ShortRead,
            iterations: 100,
            stats: LatencyStats {
                count: 100,
                min_us: *p99,
                max_us: *p99,
                mean_us: *p99,
                p50_us: *p99,
                p95_us: *p99,
                p99_us: *p99,
            },
            extra: serde_json::Map::new(),
        })
        .collect();
    BenchmarkReport {
        schema_version: SCHEMA_VERSION,
        suite: "test".to_string(),
        disclaimer: "test".to_string(),
        generated_at: "1970-01-01T00:00:00Z".to_string(),
        generated_at_unix: 0,
        hardware: HardwareInfo {
            arch: "x".to_string(),
            os: "y".to_string(),
            logical_cpus: 1,
            cpu_model: None,
            total_mem_mib: None,
        },
        config: RunConfig {
            scale: "smoke".to_string(),
            seed: 1,
            warmup: 0,
            iterations: 100,
            node_count: 0,
            edge_count: 0,
            vector_count: 0,
            vector_dim: 0,
        },
        operations,
    }
}

#[test]
fn within_threshold_passes() {
    let baseline = report_with(&[("a", 100.0), ("b", 200.0)]);
    // +5% and +9% — both within the 10% threshold.
    let current = report_with(&[("a", 105.0), ("b", 218.0)]);
    let result = evaluate_gate(&baseline, &current, 10.0);
    assert!(
        result.passed(),
        "within-threshold run should pass: {result:?}"
    );
    assert!(result.regressions().is_empty());
}

#[test]
fn just_over_threshold_fails() {
    let baseline = report_with(&[("a", 100.0)]);
    // +10.1% exceeds the 10% threshold.
    let current = report_with(&[("a", 110.1)]);
    let result = evaluate_gate(&baseline, &current, 10.0);
    assert!(!result.passed(), "10.1% regression must fail the gate");
    assert_eq!(result.regressions().len(), 1);
    assert_eq!(result.regressions()[0].key, "a");
}

#[test]
fn exactly_at_threshold_passes() {
    let baseline = report_with(&[("a", 100.0)]);
    // Exactly +10% is NOT beyond the threshold (strictly greater fails).
    let current = report_with(&[("a", 110.0)]);
    let result = evaluate_gate(&baseline, &current, 10.0);
    assert!(result.passed(), "exactly at threshold should pass");
}

#[test]
fn synthetic_2x_slowdown_fails_the_gate() {
    // The mandated sensitivity check: a synthetic 2x-slower run must fail.
    let baseline = report_with(&[
        ("is1", 100.0),
        ("is3", 150.0),
        ("ext_temporal_reconstruction", 500.0),
    ]);
    let current = report_with(&[
        ("is1", 200.0),
        ("is3", 300.0),
        ("ext_temporal_reconstruction", 1000.0),
    ]);
    let result = evaluate_gate(&baseline, &current, 10.0);
    assert!(!result.passed(), "a 2x slowdown must fail the gate");
    assert_eq!(
        result.regressions().len(),
        3,
        "every 2x-slower op must be flagged"
    );
    for c in result.regressions() {
        assert!((c.delta_pct - 100.0).abs() < 1e-6, "2x == +100%");
    }
}

#[test]
fn faster_run_passes() {
    let baseline = report_with(&[("a", 100.0)]);
    let current = report_with(&[("a", 50.0)]); // 2x faster
    let result = evaluate_gate(&baseline, &current, 10.0);
    assert!(result.passed(), "an improvement must never fail the gate");
    assert!(result.comparisons[0].delta_pct < 0.0);
}

#[test]
fn missing_operation_is_a_failure() {
    let baseline = report_with(&[("a", 100.0), ("b", 100.0)]);
    let current = report_with(&[("a", 100.0)]); // dropped "b"
    let result = evaluate_gate(&baseline, &current, 10.0);
    assert!(!result.passed(), "a dropped baseline op must fail the gate");
    assert_eq!(result.missing_in_current, vec!["b".to_string()]);
}

#[test]
fn sub_microsecond_jitter_is_suppressed_by_noise_floor() {
    // A sub-µs read: 0.63µs baseline -> 0.84µs current is +33% relative but only
    // +0.21µs absolute — pure scheduler jitter. The default 5µs floor must
    // suppress it so the CI gate does not flap red on noise.
    let baseline = report_with(&[("is1", 0.63)]);
    let current = report_with(&[("is1", 0.84)]);
    let result = evaluate_gate(&baseline, &current, 10.0); // default 5µs floor
    assert!(
        result.passed(),
        "sub-µs jitter (+33% but +0.21µs) must not fail the default gate: {result:?}"
    );
    // The comparison still records the (large) relative delta honestly.
    assert!(result.comparisons[0].delta_pct > 30.0);
    assert!(!result.comparisons[0].regressed);
}

#[test]
fn noise_floor_does_not_mask_a_real_large_regression() {
    // A meaningful op: 100µs -> 130µs is +30% AND +30µs absolute — a real
    // regression that must fail even with the noise floor active.
    let baseline = report_with(&[("ic9", 100.0)]);
    let current = report_with(&[("ic9", 130.0)]);
    let result = evaluate_gate(&baseline, &current, 10.0);
    assert!(!result.passed(), "a +30µs/+30% regression must fail");
    assert_eq!(result.regressions().len(), 1);
}

#[test]
fn explicit_zero_floor_restores_pure_relative_gate() {
    // With the floor set to 0, the pure >10% relative contract applies even to
    // sub-µs ops (available for callers who want the strict relative gate).
    let baseline = report_with(&[("is1", 0.63)]);
    let current = report_with(&[("is1", 0.84)]);
    let result = evaluate_gate_with_floor(&baseline, &current, 10.0, 0.0);
    assert!(!result.passed(), "with a 0µs floor, +33% must fail");
}

#[test]
fn new_operation_is_informational_not_a_failure() {
    let baseline = report_with(&[("a", 100.0)]);
    let current = report_with(&[("a", 100.0), ("new_op", 999.0)]);
    let result = evaluate_gate(&baseline, &current, 10.0);
    assert!(result.passed(), "a new op cannot regress");
    assert_eq!(result.new_in_current, vec!["new_op".to_string()]);
}
