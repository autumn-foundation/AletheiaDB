# Testing, Coverage, and Profiling Guide

This document explains how to use the testing, coverage, and profiling tools for AletheiaDB.

## Prerequisites

Install required tools:

```bash
# Install cargo-llvm-cov for coverage
cargo install cargo-llvm-cov

# Install just for running commands
cargo install just

# Install cargo-fuzz for coverage-guided fuzzing
cargo install cargo-fuzz

# Install criterion for benchmarks (included in dev-dependencies)
# Already included when you run cargo build

# Optional: Install cargo-flamegraph for CPU profiling
cargo install flamegraph

# Optional: Download Tracy profiler for advanced profiling
# https://github.com/wolfpld/tracy/releases
```

## Quick Reference

```bash
# Run all tests
just test

# Run tests with coverage (HTML report)
just coverage

# Check if coverage meets thresholds (80% lines, 75% functions, 70% branches)
just coverage-check

# Run all pre-commit checks (format, lint, test)
just pre-commit

# Run one fuzz target for 60 seconds
cargo +nightly fuzz run wal_entry_parsing -- -max_total_time=60

# Full check including coverage
just check-all
```

## Testing

### Running Tests

```bash
# Run all tests
just test
# or: cargo test

# Run tests with detailed output
just test-verbose

# Run a specific test
just test-one test_node_id_creation

# Run tests in a specific module
cargo test core::temporal
```

For a reference index of crash-, corruption-, and recovery-scenario tests (what
each simulates, the invariant asserted, and how to run it), see
[docs/testing/crash-scenarios.md](docs/testing/crash-scenarios.md).

### Writing Tests

Follow these guidelines:

1. **Unit tests** in each module test individual functions in isolation
2. **Integration tests** in `tests/` directory test complete workflows
3. **Property-based tests** use `proptest` for invariant checking
4. **Benchmarks** in `benches/` measure performance

Example unit test:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature() {
        let result = my_function();
        assert_eq!(result, expected);
    }
}
```

Example property-based test:
```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn temporal_consistency(timestamp in 0i64..1_000_000) {
        let range = TimeRange::from(timestamp);
        assert!(range.contains(timestamp));
    }
}
```

## Fuzz Testing

AletheiaDB uses `cargo-fuzz` for coverage-guided fuzzing of crash recovery and
temporal reconstruction paths. The fuzz package lives in `fuzz/` and is not part
of normal builds; it enables the crate's internal `fuzzing` feature to access
small parser hooks and Arbitrary implementations.

### Fuzz Targets

| Target | Focus |
|--------|-------|
| `wal_entry_parsing` | Raw WAL entry parser, checksums, malformed/truncated entries |
| `wal_replay` | Structured WAL mutation streams and crash-recovery replay |
| `temporal_reconstruction` | Anchor/delta version reconstruction and visibility lookups |
| `property_serialization` | PropertyMap and PropertyValue serialization/deserialization |
| `timestamp_arithmetic` | HLC ordering, TimeRange, and BiTemporalInterval boundaries |

### Running Locally

```bash
# Install once
cargo install cargo-fuzz

# List targets
cargo fuzz list

# Run individual targets
cargo +nightly fuzz run wal_entry_parsing -- -max_total_time=60
cargo +nightly fuzz run wal_replay -- -max_total_time=60
cargo +nightly fuzz run temporal_reconstruction -- -max_total_time=60
cargo +nightly fuzz run property_serialization -- -max_total_time=60
cargo +nightly fuzz run timestamp_arithmetic -- -max_total_time=60

# Convenience wrappers
just fuzz wal_entry_parsing 60
just fuzz-smoke 30
```

Use sanitizers when investigating memory or concurrency failures:

```bash
cargo +nightly fuzz run wal_entry_parsing --sanitizer address -- -max_total_time=300
cargo +nightly fuzz run wal_replay --sanitizer leak -- -max_total_time=300
cargo +nightly fuzz run temporal_reconstruction --sanitizer thread -- -max_total_time=300
```

Nightly CI runs the initial target set and writes a per-target summary to the
GitHub Actions job summary. Treat any crash artifact under `fuzz/artifacts/` as
a required regression test before landing the fix.

On Windows/MSVC, sanitizer-backed fuzzing may require the matching sanitizer
runtime DLL in `PATH`; the CI fuzz job runs on Ubuntu to avoid that local toolchain
trap.

## Code Coverage

### Coverage Thresholds

AletheiaDB enforces minimum coverage thresholds:
- **80%** line coverage
- **75%** function coverage
- **70%** branch coverage

These are configured in `Cargo.toml` under `[package.metadata.llvm-cov]`.

### Generating Coverage Reports

```bash
# HTML report (opens in browser)
just coverage

# Check against thresholds (CI)
just coverage-check

# Generate lcov format for CI/tooling
just coverage-ci

# Detailed report showing missing lines
just coverage-detailed

# Quick summary in terminal
just coverage-summary
```

### Coverage Output

HTML reports are generated in `target/llvm-cov/html/index.html`.

Example output:
```
Filename                      Regions    Missed Regions     Cover   Functions  Missed Functions  Executed
-----------------------------------------------------------------------------------------------------------------
src/core/id.rs                    24                 0   100.00%          15                 0   100.00%
src/core/temporal.rs              45                 2    95.56%          28                 0   100.00%
src/core/property.rs              38                 3    92.11%          22                 1    95.45%
-----------------------------------------------------------------------------------------------------------------
TOTAL                            107                 5    95.33%          65                 1    98.46%
```

### Excluding Code from Coverage

Mark code that shouldn't count toward coverage:

```rust
#[cfg(not(tarpaulin_include))]
fn debug_only_function() {
    // This won't be counted in coverage
}
```

Or exclude entire modules in `Cargo.toml`:
```toml
[package.metadata.llvm-cov]
exclude = [
    "src/main.rs",
    "benches/*",
]
```

## Mutation Testing

Mutation testing validates that tests actually *assert correctness*, not just execute code. [cargo-mutants](https://mutants.rs/) introduces small changes (mutants) to source code and checks whether at least one test fails. A surviving mutant means code was executed but not meaningfully verified.

See [ADR-0035](docs/adr/0035-mutation-testing.md) for the full rationale.

### Installation

```bash
cargo install cargo-mutants
# or faster with pre-built binaries:
cargo binstall cargo-mutants
```

### Running Locally

```bash
# Mutation test only uncommitted changes (fast)
just mutants-diff

# Mutation test changes vs trunk (useful before opening a PR)
just mutants-branch

# Full mutation test run (slow — tests every function in scope)
just mutants
```

### Interpreting Results

Results are written to `mutants.out/`:

| File | Meaning |
|------|---------|
| `caught.txt` | Mutants killed by tests (good) |
| `missed.txt` | Mutants that survived (investigate these) |
| `timeout.txt` | Mutants that caused test timeouts |
| `unviable.txt` | Mutants that failed to compile (neutral) |

Focus on **`missed.txt`** — each entry is a code location where a behavioral change went undetected. Common fixes:

- Add an assertion for the return value or side effect
- Add a test case that exercises the specific branch or condition
- If the mutant is genuinely equivalent (behavior can't differ), it can be ignored

### CI Integration

The "Mutation Testing" workflow (`.github/workflows/mutants.yml`) has two paths:

- **On PRs (blocking gate)**: `cargo mutants --in-diff` runs on the code the PR
  touches, then `.github/scripts/mutants_gate.py gate` enforces a minimum
  mutation score. The check name is **`Mutants (PR diff)`**; it fails the PR
  when the diff's score is below the threshold.
- **Weekly (informational rotating sample)**: every Sunday, 12 of 96
  round-robin shards run (~12.5% of all mutants). cargo-mutants' *default*
  sharding is `slice` (contiguous chunks of the mutant list in file order —
  a biased sample); we pass `--sharding round-robin` (mutant `i` runs on
  shard `i % k`) so any subset of shards is a systematic, project-wide
  sample. Each weekly run therefore publishes an unbiased project-wide score
  estimate (`mutants-weekly-score` artifact containing `score.json`), and
  the rotating start index walks through all 96 shards so the whole project
  is covered **approximately** every 8 weeks (approximately: GitHub pauses
  cron on repository inactivity, a failed week skips its window, and code
  churn reassigns round-robin positions between weeks). Shard indexes are
  0-based (`--shard 0/96` … `--shard 95/96`). The epoch-week number and
  rotating start are pinned **once per run** by a small setup job and passed
  to the shard jobs and the aggregate via job outputs, so re-running a
  failed shard hours later (even across the epoch-week boundary) stays in
  the same weekly window. If any shard artifact is missing or unusable, the
  published summary and `score.json` are marked **partial** rather than
  passed off as a full weekly estimate.
- Both paths run tests under **cargo-nextest** (`--test-tool nextest`) for
  faster per-mutant test runs. Note: nextest does not run doctests, so a
  mutant only caught by a doctest would be reported as missed.
- Results are uploaded as artifacts and summarized in the GitHub job summary.

### The Mutation-Score Gate

The gate's tuning knobs live in **`.github/mutants-gate.toml`**:

- `threshold` — minimum mutation score (%) for the mutants produced by a PR
  diff. Score formula:

  ```
  score = (caught + timeout) / (caught + timeout + missed) × 100
  ```

  Timeouts count as caught (the mutant made the test suite hang, i.e. it was
  detected). Unviable mutants (failed to compile) are excluded entirely.
- `min_mutants` — the gate only enforces when the diff produces at least this
  many viable mutants; below the floor, results are reported but never fail
  the check (small-sample noise).

A PR whose diff produces zero mutants (docs-only, CI-only, test-only) always
passes. The gate distinguishes real gate failures (exit 2) from
infrastructure errors (exit 1, e.g. unreadable output or malformed config).
Run it locally against any `mutants.out` with `just mutants-gate`.

### Configuration

Mutant exclusions are configured in `.cargo/mutants.toml`. Currently excluded:

- `fmt` methods of `Display`/`Debug` impls (cosmetic output; asserting exact
  strings adds brittleness, not correctness) — 73 mutants at the time of
  writing (12,301 → 12,228)

`src/mcp/**` is deliberately **not** excluded — it is the actively developed
LLM-facing surface and must stay under mutation pressure.

## Profiling

### Tracy Profiling

Tracy is a real-time profiler for detailed performance analysis.

#### Setup

1. Download Tracy profiler GUI from [releases](https://github.com/wolfpld/tracy/releases)
2. Run the Tracy profiler application
3. Build and run AletheiaDB with Tracy enabled:

```bash
# Build with Tracy support
cargo build --release --features tracy

# Run with profiling (Tracy must be running)
just profile-tracy
```

#### Adding Tracy Zones

Instrument your code with Tracy zones:

```rust
#[cfg(feature = "tracy")]
use tracy_client::span;

pub fn expensive_function() {
    #[cfg(feature = "tracy")]
    let _span = span!("expensive_function");

    // Your code here
    // Tracy will measure execution time of this function
}
```

For more granular profiling:

```rust
pub fn complex_operation() {
    #[cfg(feature = "tracy")]
    {
        let _span1 = span!("phase_1");
        phase_1();
    }

    #[cfg(feature = "tracy")]
    {
        let _span2 = span!("phase_2");
        phase_2();
    }
}
```

#### Tracy Features

- **Real-time visualization**: See function calls as they happen
- **Frame profiling**: Measure frame-to-frame performance
- **Memory tracking**: Monitor allocations and deallocations
- **Lock contention**: Identify synchronization bottlenecks
- **Statistical analysis**: Aggregate data over multiple runs

### CPU Profiling with Flamegraph

For quick CPU profiling without Tracy:

```bash
# Generate flamegraph for benchmarks
just flamegraph

# Or manually:
cargo flamegraph --bench current_state
```

This creates `flamegraph.svg` showing where CPU time is spent.

### Benchmarking with Criterion

```bash
# Run all benchmarks
just bench

# Run specific benchmark
cargo bench --bench current_state

# Compare with baseline
cargo bench -- --save-baseline main
cargo bench -- --baseline main
```

Criterion generates detailed HTML reports in `target/criterion/`.

## Continuous Integration

The CI workflow (`just ci`) runs:
1. Format check (`cargo fmt --check`)
2. Lint check (`cargo clippy`)
3. All tests (`cargo test`)
4. Coverage check with thresholds

```bash
# Run what CI runs locally
just ci
```

## Performance Testing Workflow

1. **Write a benchmark** in `benches/`
2. **Establish baseline**: `cargo bench -- --save-baseline main`
3. **Make changes** to your code
4. **Compare**: `cargo bench -- --baseline main`
5. **Profile if slower**: Use Tracy or flamegraph to find bottlenecks
6. **Iterate** until performance targets are met

## Coverage Goals by Component

Based on CLAUDE.md targets:

| Component | Target Coverage | Rationale |
|-----------|----------------|-----------|
| Core primitives | 100% | Foundation - must be bulletproof |
| Storage layer | 95% | Critical path - high reliability |
| Query engine | 90% | Complex logic - thorough testing |
| API layer | 85% | User-facing - good coverage |
| Utilities | 80% | Supporting code - adequate coverage |

## Debugging Test Failures

```bash
# Run failing test with output
just test-verbose

# Run single test with backtrace
RUST_BACKTRACE=1 cargo test failing_test_name -- --nocapture

# Run with debug logging (if using tracing/log)
RUST_LOG=debug cargo test
```

## Best Practices

1. **Write tests first** (TDD approach recommended)
2. **Run coverage locally** before pushing
3. **Profile performance-critical code** as you write it
4. **Use property-based tests** for invariants
5. **Keep benchmarks up to date** with new features
6. **Check coverage reports** to find untested paths
7. **Use Tracy** for hot path optimization

## Troubleshooting

### Coverage not working
```bash
# Make sure llvm-cov is installed
cargo install cargo-llvm-cov

# Clean and retry
cargo clean
just coverage
```

### Tracy not connecting
- Ensure Tracy profiler GUI is running first
- Check firewall isn't blocking connection
- Verify `--features tracy` is enabled in build

### Benchmarks failing
```bash
# Clear benchmark cache
rm -rf target/criterion

# Run with verbose output
cargo bench -- --verbose
```

## Additional Resources

- [Criterion.rs User Guide](https://bheisler.github.io/criterion.rs/book/)
- [Tracy Profiler Manual](https://github.com/wolfpld/tracy/releases)
- [cargo-llvm-cov Documentation](https://github.com/taiki-e/cargo-llvm-cov)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
