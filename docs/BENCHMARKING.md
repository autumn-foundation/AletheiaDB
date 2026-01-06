# Benchmarking Guide

GallifreyDB uses [Criterion.rs](https://github.com/bheisler/criterion.rs) for performance benchmarking with custom HTML table formatting for easy viewing.

## Running Benchmarks

### Quick Start

```bash
# Run all benchmarks
just bench

# Run benchmarks and generate HTML tables
just bench-tables

# Run specific benchmark suite
cargo bench --bench gallifreydb

# Run specific benchmark
cargo bench --bench gallifreydb -- node_lookup
```

### Available Benchmark Suites

| Suite | Description | Key Metrics |
|-------|-------------|-------------|
| `current_state` | Fast-path current state queries | Node lookup, edge traversal |
| `gallifreydb` | Core database operations with versioning | Node/edge creation, multi-hop traversal |
| `persistence` | WAL and durability operations | Write throughput, recovery time |
| `transactions` | Transaction management | Commit latency, concurrency |
| `vector_similarity` | Vector operations | Distance calculations |
| `hnsw_index` | HNSW k-NN search | Index build time, query performance |
| `id_generation` | ID allocation | ID generation throughput |
| `string_interning` | String deduplication | Intern performance |
| `temporal_vector` | Temporal vector queries | Time-travel with vectors |

## Viewing Results

### Local Development

After running `just bench-tables`, open `benchmark-results/index.html` in your browser:

```bash
# Windows
start benchmark-results/index.html

# macOS
open benchmark-results/index.html

# Linux
xdg-open benchmark-results/index.html
```

The generated page includes:
- **Performance targets** - GallifreyDB's performance goals
- **Benchmark tables** - Organized by suite with mean, std dev, and median
- **Link to detailed Criterion reports** - For statistical analysis and historical comparisons

### GitHub Pages

Benchmark results are automatically published to GitHub Pages on every push to `trunk`:

**URL**: https://madmax983.github.io/GallifreyDB/benchmarks/

Features:
- Automatically updated on each push to trunk
- Historical trend tracking (via Criterion's built-in charts)
- Clean table-based overview
- Detailed statistical reports

### CI/CD

Benchmarks run automatically in two scenarios:

1. **On push to trunk** - Results published to GitHub Pages
2. **On pull requests** - Summary posted as PR comment
3. **Weekly schedule** - Monday at 00:00 UTC for regression tracking

## Performance Targets

GallifreyDB aims to meet these performance targets:

| Metric | Target | Rationale |
|--------|--------|-----------|
| Current-state single-hop traversal | <1µs | Zero temporal overhead |
| Current-state 3-hop traversal | <100µs | Competitive with non-temporal DBs |
| Time-travel reconstruction | <10ms | Fast enough for audit queries |
| Batch insertion throughput | >100k edges/sec | High-volume data ingestion |
| Storage overhead | <2X vs non-temporal | Acceptable for bi-temporal tracking |

## Interpreting Results

### Mean vs Median

- **Mean**: Average performance across all iterations
- **Median**: Middle value, less affected by outliers
- **Std Dev**: Variability in measurements (lower is better)

Use median for typical performance, mean for overall picture.

### Statistical Significance

Criterion uses statistical analysis to detect performance changes:

- **Green**: Performance improved
- **Yellow**: No significant change
- **Red**: Performance regressed (>5% slower)

Criterion requires multiple runs to establish statistical confidence.

### Noise and Variance

Benchmarks can be affected by:
- System load (other processes running)
- CPU frequency scaling
- Thermal throttling
- Cache state

For accurate comparisons:
- Close unnecessary applications
- Run benchmarks multiple times
- Check CI results for consistency

## Adding New Benchmarks

### Basic Structure

```rust
use criterion::{criterion_group, criterion_main, Criterion, black_box};
use gallifreydb::GallifreyDB;

fn bench_my_operation(c: &mut Criterion) {
    c.bench_function("my_operation", |b| {
        let db = GallifreyDB::new();

        b.iter(|| {
            // Operation to benchmark
            let result = db.some_operation(black_box(42));
            black_box(result)
        });
    });
}

criterion_group!(benches, bench_my_operation);
criterion_main!(benches);
```

### Best Practices

1. **Use `black_box`** - Prevents compiler from optimizing away code
   ```rust
   black_box(db.operation(black_box(input)))
   ```

2. **Setup outside benchmark** - Only measure the operation
   ```rust
   b.iter_batched(
       || setup_data(),      // Setup (not measured)
       |data| operation(data), // Measured code
       criterion::BatchSize::SmallInput
   )
   ```

3. **Parameterized benchmarks** - Test different scales
   ```rust
   for size in [100, 1000, 10000] {
       group.bench_with_input(
           BenchmarkId::from_parameter(size),
           &size,
           |b, &s| { /* benchmark with size s */ }
       );
   }
   ```

4. **Name conventions**
   - Use snake_case for benchmark functions
   - Group related benchmarks in same file
   - Prefix with component: `gallifreydb_node_creation`

### Registering New Benchmarks

Add to `Cargo.toml`:

```toml
[[bench]]
name = "my_benchmark"
harness = false
path = "benches/my_benchmark.rs"
required-features = []
```

## Troubleshooting

### Benchmarks Taking Too Long

Criterion runs each benchmark many times for statistical accuracy. To speed up during development:

```bash
# Quick mode (fewer iterations)
cargo bench -- --quick

# Sample size (fewer measurements)
cargo bench -- --sample-size 10
```

### Inconsistent Results

If results vary significantly:

1. Check system load: `htop` or Task Manager
2. Disable CPU frequency scaling (Linux):
   ```bash
   sudo cpupower frequency-set --governor performance
   ```
3. Run multiple times and compare trends
4. Use `--save-baseline` to compare against stable baseline

### Script Errors

If `generate_benchmark_tables.py` fails:

```bash
# Check Python version (requires 3.7+)
python --version

# Verify Criterion output exists
ls target/criterion/

# Run script manually with verbose output
python scripts/generate_benchmark_tables.py --input target/criterion
```

## Advanced Usage

### Baseline Comparison

```bash
# Save current results as baseline
cargo bench -- --save-baseline main

# Compare against baseline
cargo bench -- --baseline main
```

### Profiling Benchmarks

```bash
# Run with Tracy profiling
just bench-profile

# Generate flamegraph
just flamegraph
```

### Custom Output Format

Criterion supports multiple output formats:

```bash
# Bencher format (for github-action-benchmark)
cargo bench -- --output-format bencher

# JSON output
cargo bench -- --output-format json
```

## GitHub Actions Integration

The benchmark workflow (`.github/workflows/benchmark.yml`) automatically:

1. Runs all benchmark suites
2. Generates HTML tables using `generate_benchmark_tables.py`
3. Copies Criterion's detailed reports
4. Publishes to GitHub Pages at `/benchmarks/`
5. Posts summary to PRs (if applicable)

No manual intervention required - just push to trunk!

## References

- [Criterion.rs Documentation](https://bheisler.github.io/criterion.rs/book/)
- [Criterion.rs User Guide](https://bheisler.github.io/criterion.rs/book/user_guide/user_guide.html)
- [Rust Performance Book](https://nnethercote.github.io/perf-book/)
- [CLAUDE.md Performance Targets](../CLAUDE.md#performance-benchmarks)
