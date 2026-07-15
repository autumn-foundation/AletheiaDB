//! `aletheia-eval` binary (Issue #3366).
//!
//! Usage:
//!
//! ```text
//! aletheia-eval run <manifest.toml> [--json <out.json>] [--quiet]
//! ```
//!
//! Loads the manifest, runs the paired full-vs-baseline evaluation, prints a
//! human summary to stdout (and the full JSON report to `--json` if given),
//! and exits non-zero if any regression gate is breached — so CI can gate on
//! it. This mirrors the shape of the future `aletheia eval run` subcommand.

use std::path::PathBuf;
use std::process::ExitCode;

use aletheia_eval::report::run_manifest;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(gates_passed) => {
            if gates_passed {
                ExitCode::SUCCESS
            } else {
                // Gate breach: non-zero exit for CI.
                ExitCode::from(2)
            }
        }
        Err(e) => {
            eprintln!("aletheia-eval: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<bool, Box<dyn std::error::Error>> {
    let mut iter = args.iter();
    let command = iter.next().map(String::as_str);
    match command {
        Some("run") => {}
        Some(other) => return Err(format!("unknown subcommand '{other}' (expected 'run')").into()),
        None => return Err(usage().into()),
    }

    let mut manifest: Option<PathBuf> = None;
    let mut json_out: Option<PathBuf> = None;
    let mut quiet = false;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => {
                let path = iter.next().ok_or("--json requires a path argument")?;
                json_out = Some(PathBuf::from(path));
            }
            "--quiet" => quiet = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown flag '{other}'").into());
            }
            other => {
                if manifest.is_some() {
                    return Err(format!("unexpected argument '{other}'").into());
                }
                manifest = Some(PathBuf::from(other));
            }
        }
    }

    let manifest = manifest.ok_or_else(|| usage().to_string())?;
    let report = run_manifest(&manifest)?;

    if !quiet {
        print!("{}", report.human_summary());
    }

    if let Some(path) = json_out {
        std::fs::write(&path, report.to_json())?;
        if !quiet {
            println!("Wrote JSON report to {}", path.display());
        }
    } else if quiet {
        // In quiet mode with no --json, still emit the machine report.
        println!("{}", report.to_json());
    }

    Ok(report.gates_passed)
}

fn usage() -> &'static str {
    "usage: aletheia-eval run <manifest.toml> [--json <out.json>] [--quiet]"
}
