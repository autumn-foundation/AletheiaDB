//! `ldbc-bench` — one-command runner for the LDBC-style benchmark suite.
//!
//! Generates a synthetic SNB-shaped graph, loads it into AletheiaDB, runs the
//! SNB-subset workloads plus the temporal and vector extensions, and emits a
//! machine-readable JSON report plus a human summary.
//!
//! ## Usage
//!
//! ```text
//! ldbc-bench [--scale smoke|sf0.1|sf1] [--seed N] [--iterations N] [--warmup N]
//!            [--vector-count N] [--vector-dim D]
//!            [--out results.json]
//!            [--write-baseline path.json]
//!            [--check-gate --baseline path.json [--threshold PCT]]
//! ```
//!
//! `--vector-count` / `--vector-dim` dial the vector-extension corpus
//! independently of `--scale` (e.g. `--scale sf0.1 --vector-count 1000000
//! --vector-dim 384` for a 1M-vector k-NN run over an SF0.1-sized graph).
//!
//! Exit codes: `0` success (and gate passed, if checked); `1` runtime error;
//! `2` gate regression detected.

use aletheia_bench_ldbc::gate::{
    DEFAULT_MIN_ABS_DELTA_US, DEFAULT_THRESHOLD_PCT, evaluate_gate_with_floor,
};
use aletheia_bench_ldbc::generator::Scale;
use aletheia_bench_ldbc::report::BenchmarkReport;
use aletheia_bench_ldbc::runner::{RunOptions, run_suite};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let cli = match Cli::parse(&args[1..]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}\n");
            eprintln!("{USAGE}");
            return ExitCode::from(1);
        }
    };

    if cli.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let opts = RunOptions {
        scale: cli.scale,
        seed: cli.seed,
        warmup: cli.warmup,
        iterations: cli.iterations,
        vector_count: cli.vector_count,
        vector_dim: cli.vector_dim,
    };

    eprintln!(
        "Running LDBC-style suite: scale={}, seed={}, warmup={}, iterations={}",
        cli.scale.label(),
        cli.seed,
        cli.warmup,
        cli.iterations
    );

    let report = match run_suite(&opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: benchmark run failed: {e}");
            return ExitCode::from(1);
        }
    };

    // Emit JSON report.
    match serde_json::to_string_pretty(&report) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&cli.out, &json) {
                eprintln!("error: could not write report to {}: {e}", cli.out);
                return ExitCode::from(1);
            }
            eprintln!("Wrote JSON report to {}", cli.out);
        }
        Err(e) => {
            eprintln!("error: could not serialize report: {e}");
            return ExitCode::from(1);
        }
    }

    print_human_summary(&report);

    // Optionally write a baseline snapshot.
    if let Some(path) = &cli.write_baseline {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => match std::fs::write(path, json) {
                Ok(()) => eprintln!("Wrote baseline to {path}"),
                Err(e) => {
                    eprintln!("error: could not write baseline to {path}: {e}");
                    return ExitCode::from(1);
                }
            },
            Err(e) => {
                eprintln!("error: could not serialize baseline: {e}");
                return ExitCode::from(1);
            }
        }
    }

    // Optionally run the regression gate.
    if cli.check_gate {
        let Some(baseline_path) = &cli.baseline else {
            eprintln!("error: --check-gate requires --baseline <path>");
            return ExitCode::from(1);
        };
        let baseline: BenchmarkReport = match std::fs::read_to_string(baseline_path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: could not load baseline {baseline_path}: {e}");
                return ExitCode::from(1);
            }
        };

        let result =
            evaluate_gate_with_floor(&baseline, &report, cli.threshold, cli.min_abs_delta_us);
        print_gate_result(&result);
        if !result.passed() {
            eprintln!(
                "GATE FAILED: {} operation(s) regressed beyond {:.1}% p99 (or missing).",
                result.regressions().len() + result.missing_in_current.len(),
                cli.threshold
            );
            return ExitCode::from(2);
        }
        eprintln!(
            "GATE PASSED: no p99 regression beyond {:.1}%.",
            cli.threshold
        );
    }

    ExitCode::SUCCESS
}

fn print_human_summary(report: &BenchmarkReport) {
    println!(
        "\n=== LDBC-style benchmark summary ({}) ===",
        report.config.scale
    );
    println!("{}", report.disclaimer);
    println!(
        "hardware: {} {} / {} logical CPUs{}",
        report.hardware.arch,
        report.hardware.os,
        report.hardware.logical_cpus,
        report
            .hardware
            .cpu_model
            .as_deref()
            .map(|m| format!(" / {m}"))
            .unwrap_or_default()
    );
    println!(
        "graph: {} nodes, {} edges, {} vectors (dim {})\n",
        report.config.node_count,
        report.config.edge_count,
        report.config.vector_count,
        report.config.vector_dim
    );
    println!(
        "{:<38} {:>8} {:>10} {:>10} {:>10}",
        "operation (SNB)", "iters", "p50 (µs)", "p95 (µs)", "p99 (µs)"
    );
    println!("{}", "-".repeat(80));
    for o in &report.operations {
        println!(
            "{:<38} {:>8} {:>10.2} {:>10.2} {:>10.2}",
            format!("{} [{}]", o.key, o.snb_analogue),
            o.iterations,
            o.stats.p50_us,
            o.stats.p95_us,
            o.stats.p99_us,
        );
    }
    // Extension highlight lines.
    if let Some(t) = report.operation("ext_temporal_reconstruction") {
        println!(
            "\ntemporal reconstruction: p50={:.2}µs p99={:.2}µs  (target <10ms => {})",
            t.stats.p50_us,
            t.stats.p99_us,
            if t.stats.p99_us < 10_000.0 {
                "PASS"
            } else {
                "FAIL"
            }
        );
    }
    if let Some(v) = report.operation("ext_vector_knn") {
        println!(
            "vector k-NN (k=10, {} vectors): p50={:.2}µs p99={:.2}µs  (target <10ms => {})",
            report.config.vector_count,
            v.stats.p50_us,
            v.stats.p99_us,
            if v.stats.p99_us < 10_000.0 {
                "PASS"
            } else {
                "FAIL"
            }
        );
    }
    println!();
}

fn print_gate_result(result: &aletheia_bench_ldbc::gate::GateResult) {
    println!(
        "\n=== Regression gate (threshold {:.1}% p99, noise floor {:.1}µs) ===",
        result.threshold_pct, result.min_abs_delta_us
    );
    println!(
        "{:<40} {:>12} {:>12} {:>9} {:>10} {:>6}",
        "operation", "base p99", "cur p99", "delta%", "delta µs", "regr"
    );
    println!("{}", "-".repeat(94));
    for c in &result.comparisons {
        println!(
            "{:<40} {:>12.2} {:>12.2} {:>+8.1}% {:>+10.2} {:>6}",
            c.key,
            c.baseline_p99_us,
            c.current_p99_us,
            c.delta_pct,
            c.delta_us,
            if c.regressed { "YES" } else { "no" }
        );
    }
    for m in &result.missing_in_current {
        println!("{m:<40} MISSING IN CURRENT RUN (treated as failure)");
    }
    println!();
}

/// Parsed command-line options.
struct Cli {
    scale: Scale,
    seed: u64,
    warmup: usize,
    iterations: usize,
    out: String,
    write_baseline: Option<String>,
    check_gate: bool,
    baseline: Option<String>,
    threshold: f64,
    min_abs_delta_us: f64,
    vector_count: Option<usize>,
    vector_dim: Option<usize>,
    help: bool,
}

const USAGE: &str = "ldbc-bench — LDBC-style benchmark suite for AletheiaDB (Issue #3373)\n\
\n\
USAGE:\n    \
ldbc-bench [OPTIONS]\n\
\n\
OPTIONS:\n    \
--scale <smoke|sf0.1|sf1>  Scale point (default: smoke)\n    \
--seed <N>                 PRNG seed (default: 42)\n    \
--iterations <N>           Measured iterations per op (default: 200)\n    \
--warmup <N>               Warmup iterations discarded (default: 20)\n    \
--vector-count <N>         Override the dedicated vector-extension corpus size,\n    \
                           independent of --scale (e.g. 1000000 for a 1M-vector\n    \
                           k-NN run). Default: preset (no extra corpus)\n    \
--vector-dim <D>           Override the embedding dimensionality (must be > 0,\n    \
                           e.g. 384). Default: preset dim\n    \
--out <PATH>               JSON report output (default: ldbc_results.json)\n    \
--write-baseline <PATH>    Also write this run as a baseline JSON\n    \
--check-gate               Compare against --baseline and exit 2 on regression\n    \
--baseline <PATH>          Baseline JSON for --check-gate\n    \
--threshold <PCT>          p99 regression threshold percent (default: 10)\n    \
--min-abs-delta-us <US>    Absolute p99 noise floor in µs; a regression must\n    \
                           exceed BOTH the percent threshold and this (default: 5)\n    \
-h, --help                 Show this help";

impl Cli {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut cli = Cli {
            scale: Scale::Smoke,
            seed: 42,
            warmup: 20,
            iterations: 200,
            out: "ldbc_results.json".to_string(),
            write_baseline: None,
            check_gate: false,
            baseline: None,
            threshold: DEFAULT_THRESHOLD_PCT,
            min_abs_delta_us: DEFAULT_MIN_ABS_DELTA_US,
            vector_count: None,
            vector_dim: None,
            help: false,
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => cli.help = true,
                "--scale" => {
                    let v = it.next().ok_or("--scale requires a value")?;
                    cli.scale = Scale::parse(v).ok_or_else(|| format!("unknown scale: {v}"))?;
                }
                "--seed" => {
                    cli.seed = it
                        .next()
                        .ok_or("--seed requires a value")?
                        .parse()
                        .map_err(|_| "invalid --seed")?;
                }
                "--iterations" => {
                    cli.iterations = it
                        .next()
                        .ok_or("--iterations requires a value")?
                        .parse()
                        .map_err(|_| "invalid --iterations")?;
                }
                "--warmup" => {
                    cli.warmup = it
                        .next()
                        .ok_or("--warmup requires a value")?
                        .parse()
                        .map_err(|_| "invalid --warmup")?;
                }
                "--out" => cli.out = it.next().ok_or("--out requires a value")?.clone(),
                "--write-baseline" => {
                    cli.write_baseline = Some(
                        it.next()
                            .ok_or("--write-baseline requires a value")?
                            .clone(),
                    );
                }
                "--check-gate" => cli.check_gate = true,
                "--baseline" => {
                    cli.baseline = Some(it.next().ok_or("--baseline requires a value")?.clone());
                }
                "--threshold" => {
                    cli.threshold = it
                        .next()
                        .ok_or("--threshold requires a value")?
                        .parse()
                        .map_err(|_| "invalid --threshold")?;
                }
                "--min-abs-delta-us" => {
                    cli.min_abs_delta_us = it
                        .next()
                        .ok_or("--min-abs-delta-us requires a value")?
                        .parse()
                        .map_err(|_| "invalid --min-abs-delta-us")?;
                }
                "--vector-count" => {
                    let n = it
                        .next()
                        .ok_or("--vector-count requires a value")?
                        .parse()
                        .map_err(|_| "invalid --vector-count")?;
                    cli.vector_count = Some(n);
                }
                "--vector-dim" => {
                    let d: usize = it
                        .next()
                        .ok_or("--vector-dim requires a value")?
                        .parse()
                        .map_err(|_| "invalid --vector-dim")?;
                    if d == 0 {
                        return Err("--vector-dim must be > 0".to_string());
                    }
                    cli.vector_dim = Some(d);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        if cli.iterations == 0 {
            return Err("--iterations must be > 0".to_string());
        }
        Ok(cli)
    }
}
