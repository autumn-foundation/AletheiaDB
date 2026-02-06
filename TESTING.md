# Testing, Coverage, and Profiling Guide

This document explains how to use the testing, coverage, and profiling tools for AletheiaDB.

## Prerequisites

Install required tools:

```bash
# Install cargo-llvm-cov for coverage
cargo install cargo-llvm-cov

# Install just for running commands
cargo install just

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
