//! Process-level regression-gate sensitivity test.
//!
//! The gate math is unit-tested in `src/gate.rs`, but the CLI's "exit code 2 on
//! a regression" contract is a property of the *binary*, not the library. These
//! tests spawn the actual `ldbc-bench` process against synthetic baselines and
//! assert the real process exit code, closing the sensitivity claim end-to-end:
//!
//! * against a deliberately **fast** baseline (every p99 driven near zero), a
//!   fresh run regresses and the process must exit **2**;
//! * against a deliberately **slow** baseline (every p99 enormous), a fresh run
//!   cannot regress and the process must exit **0**.

use aletheia_bench_ldbc::generator::Scale;
use aletheia_bench_ldbc::report::BenchmarkReport;
use aletheia_bench_ldbc::runner::{RunOptions, run_suite};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// A tiny suite run so the test stays fast; keys are scale-independent so they
/// line up with the binary's own `--scale smoke` run.
fn tiny_opts() -> RunOptions {
    RunOptions {
        scale: Scale::Smoke,
        seed: 42,
        warmup: 2,
        iterations: 5,
    }
}

/// Unique temp path within this test process (no external tempfile dependency).
fn unique_temp_path(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("aletheia_ldbc_gate_{pid}_{tag}_{n}.json"))
}

/// Build a synthetic baseline from a real run, overriding every operation's p99
/// to `p99_us`, and write it to a fresh temp file. Returns the path.
fn write_baseline_with_p99(tag: &str, p99_us: f64) -> PathBuf {
    let mut report = run_suite(&tiny_opts()).expect("suite should run");
    for op in &mut report.operations {
        op.stats.p99_us = p99_us;
    }
    let path = unique_temp_path(tag);
    let json = serde_json::to_string_pretty(&report).expect("serialize baseline");
    std::fs::write(&path, json).expect("write baseline");
    path
}

/// Run the `ldbc-bench` binary in `--check-gate` mode against `baseline` and
/// return its process exit code.
fn run_gate_against(baseline: &PathBuf) -> i32 {
    let out = unique_temp_path("out");
    let status = Command::new(env!("CARGO_BIN_EXE_ldbc-bench"))
        .args([
            "--scale",
            "smoke",
            "--iterations",
            "5",
            "--warmup",
            "2",
            "--out",
        ])
        .arg(&out)
        .arg("--check-gate")
        .arg("--baseline")
        .arg(baseline)
        .status()
        .expect("failed to spawn ldbc-bench");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(baseline);
    status.code().expect("process exited via signal, not code")
}

#[test]
fn regression_makes_the_process_exit_2() {
    // A baseline whose every p99 is a tiny positive value: a real run is orders
    // of magnitude slower (insert/update ops alone are hundreds of µs), so it
    // regresses past both the relative threshold and the absolute noise floor.
    let baseline = write_baseline_with_p99("fast", 0.001);
    let code = run_gate_against(&baseline);
    assert_eq!(
        code, 2,
        "a run against a near-zero baseline must exit 2 (regression detected), got {code}"
    );
}

#[test]
fn no_regression_makes_the_process_exit_0() {
    // A baseline whose every p99 is enormous: a real run is always faster, so no
    // operation can regress and the process must exit 0.
    let baseline = write_baseline_with_p99("slow", 1.0e9);
    let code = run_gate_against(&baseline);
    assert_eq!(
        code, 0,
        "a run against an enormous baseline must exit 0 (no regression), got {code}"
    );
}

/// Guard the assumption the exit-2 test depends on: a real report must contain
/// at least one operation whose p99 clears the absolute noise floor, so the
/// near-zero baseline is guaranteed to produce a floor-clearing regression.
#[test]
fn real_run_has_an_op_above_the_noise_floor() {
    let report = run_suite(&tiny_opts()).expect("suite should run");
    let max_p99 = report
        .operations
        .iter()
        .map(|o| o.stats.p99_us)
        .fold(0.0_f64, f64::max);
    assert!(
        max_p99 > aletheia_bench_ldbc::gate::DEFAULT_MIN_ABS_DELTA_US,
        "expected at least one op p99 above the {}µs noise floor, max was {max_p99:.3}µs",
        aletheia_bench_ldbc::gate::DEFAULT_MIN_ABS_DELTA_US
    );
}

/// Verify the parsed baseline (post-serialization) really is a
/// [`BenchmarkReport`] with the overridden p99, guarding the test's own fixture.
#[test]
fn synthetic_baseline_round_trips() {
    let path = write_baseline_with_p99("roundtrip", 0.001);
    let parsed: BenchmarkReport =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read baseline"))
            .expect("parse baseline");
    let _ = std::fs::remove_file(&path);
    assert!(!parsed.operations.is_empty());
    assert!(parsed.operations.iter().all(|o| o.stats.p99_us == 0.001));
}
