//! Benchmarks for cold storage backend (Issue #120).
//!
//! This benchmark suite validates the performance targets from SCALE-002:
//! - Read latency: <1ms (p50), <10ms (p99)
//! - Write throughput: >10k versions/sec
//! - Compression ratio: >3x
//!
//! The benchmarks cover:
//! - Individual read/write operations
//! - Batch operations
//! - Different compression algorithms (None, Zstd, Fast)
//! - Both node and edge versions
//!
//! # Running Benchmarks
//!
//! ```bash
//! # Run all cold storage benchmarks
//! cargo bench --bench cold_storage
//!
//! # Run specific benchmark group
//! cargo bench --bench cold_storage -- read_latency
//!
//! # Fast CI mode
//! BENCH_SAMPLE_SIZE=10 BENCH_MEASUREMENT_TIME=1 cargo bench --bench cold_storage
//! ```

mod common;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::{NodeId, VersionId};
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::temporal::BiTemporalInterval;
use gallifreydb::storage::cold_storage::{
    ColdStorage, ColdStorageConfig, CompressionAlgorithm, FileColdStorage,
};
use gallifreydb::storage::version::NodeVersion;
use std::time::Instant;
use tempfile::TempDir;

/// Create a test node version with realistic property data.
///
/// The version includes:
/// - String property (name)
/// - Integer property (age)
/// - Realistic temporal intervals
fn create_test_node_version(id: u64, node_id: u64) -> NodeVersion {
    let properties = PropertyMapBuilder::new()
        .insert("name", "Alice Johnson")
        .insert("age", 30i64)
        .insert("email", "alice@example.com")
        .insert("score", 98.5f64)
        .build();

    NodeVersion::new_anchor(
        VersionId::new(id).unwrap(),
        NodeId::new(node_id).unwrap(),
        BiTemporalInterval::current((1000 + id as i64).into()),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        properties,
    )
}

/// Create a batch of test node versions.
fn create_test_versions(count: usize) -> Vec<NodeVersion> {
    (0..count)
        .map(|i| create_test_node_version(i as u64, (i % 1000) as u64))
        .collect()
}

/// Benchmark: Individual write operations.
///
/// Measures the latency of storing individual versions.
fn bench_write_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_write_latency");

    for compression in [
        CompressionAlgorithm::None,
        CompressionAlgorithm::Fast,
        CompressionAlgorithm::Zstd,
    ] {
        let temp_dir = TempDir::new().unwrap();
        let config = ColdStorageConfig {
            compression,
            sync_writes: false,
            batch_size: 1000,
            enable_checksums: true,
        };
        let storage = FileColdStorage::new(temp_dir.path(), config).unwrap();

        let compression_name = match compression {
            CompressionAlgorithm::None => "no_compression",
            CompressionAlgorithm::Fast => "fast_compression",
            CompressionAlgorithm::Zstd => "zstd_compression",
        };

        let mut version_id = 0u64;
        group.bench_function(BenchmarkId::new("single_write", compression_name), |b| {
            b.iter(|| {
                let version = create_test_node_version(version_id, version_id % 1000);
                version_id += 1;
                storage.store_node_version(black_box(&version)).unwrap();
            });
        });
    }

    group.finish();
}

/// Benchmark: Individual read operations.
///
/// Measures the latency of retrieving individual versions.
/// Target: <1ms (p50), <10ms (p99)
fn bench_read_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_read_latency");

    for compression in [
        CompressionAlgorithm::None,
        CompressionAlgorithm::Fast,
        CompressionAlgorithm::Zstd,
    ] {
        // Setup: Create storage with pre-populated data
        let temp_dir = TempDir::new().unwrap();
        let config = ColdStorageConfig {
            compression,
            sync_writes: false,
            batch_size: 1000,
            enable_checksums: true,
        };
        let storage = FileColdStorage::new(temp_dir.path(), config).unwrap();

        // Pre-populate with 1000 versions
        let versions = create_test_versions(1000);
        for version in &versions {
            storage.store_node_version(version).unwrap();
        }

        let compression_name = match compression {
            CompressionAlgorithm::None => "no_compression",
            CompressionAlgorithm::Fast => "fast_compression",
            CompressionAlgorithm::Zstd => "zstd_compression",
        };

        let mut read_idx = 0usize;
        group.bench_function(BenchmarkId::new("single_read", compression_name), |b| {
            b.iter(|| {
                let version_id = VersionId::new((read_idx % 1000) as u64).unwrap();
                read_idx += 1;
                let result = storage.get_node_version(black_box(version_id)).unwrap();
                black_box(result)
            });
        });
    }

    group.finish();
}

/// Benchmark: Batch write throughput.
///
/// Measures versions per second for batch operations.
/// Target: >10k versions/sec
fn bench_batch_write_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_batch_write_throughput");

    for batch_size in [100, 1000, 10000] {
        for compression in [CompressionAlgorithm::Fast, CompressionAlgorithm::Zstd] {
            let temp_dir = TempDir::new().unwrap();
            let config = ColdStorageConfig {
                compression,
                sync_writes: false,
                batch_size,
                enable_checksums: true,
            };
            let storage = FileColdStorage::new(temp_dir.path(), config).unwrap();

            let versions = create_test_versions(batch_size);

            let compression_name = match compression {
                CompressionAlgorithm::None => "no_compression",
                CompressionAlgorithm::Fast => "fast",
                CompressionAlgorithm::Zstd => "zstd",
            };

            group.throughput(Throughput::Elements(batch_size as u64));
            group.bench_function(BenchmarkId::new(compression_name, batch_size), |b| {
                b.iter(|| {
                    storage
                        .store_node_versions_batch(black_box(&versions))
                        .unwrap();
                });
            });
        }
    }

    group.finish();
}

/// Benchmark: Read latency percentiles.
///
/// Measures p50, p95, p99 latencies for cold reads.
/// Validates targets: p50 <1ms, p99 <10ms
fn bench_read_latency_percentiles(c: &mut Criterion) {
    // Setup: Create storage with realistic dataset
    let temp_dir = TempDir::new().unwrap();
    let config = ColdStorageConfig {
        compression: CompressionAlgorithm::Zstd,
        sync_writes: false,
        batch_size: 1000,
        enable_checksums: true,
    };
    let storage = FileColdStorage::new(temp_dir.path(), config).unwrap();

    // Pre-populate with 10k versions
    let versions = create_test_versions(10000);
    storage.store_node_versions_batch(&versions).unwrap();

    c.bench_function("read_latency_p50_p99", |b| {
        b.iter_custom(|iters| {
            let mut latencies = Vec::with_capacity(iters as usize);

            for i in 0..iters {
                let version_id = VersionId::new(i % 10000).unwrap();
                let start = Instant::now();
                let _ = storage.get_node_version(black_box(version_id)).unwrap();
                latencies.push(start.elapsed());
            }

            // Calculate percentiles
            latencies.sort_unstable();
            let p50_idx = (latencies.len() as f64 * 0.50) as usize;
            let p95_idx = (latencies.len() as f64 * 0.95) as usize;
            let p99_idx = (latencies.len() as f64 * 0.99) as usize;

            let p50 = latencies[p50_idx];
            let p95 = latencies[p95_idx];
            let p99 = latencies[p99_idx];

            // Print percentiles for manual verification
            if iters == 100 {
                eprintln!("\nRead Latency Percentiles:");
                eprintln!("  p50: {:?}", p50);
                eprintln!("  p95: {:?}", p95);
                eprintln!("  p99: {:?}", p99);
                eprintln!("  Target: p50 <1ms, p99 <10ms");
            }

            // Return median for criterion
            p50
        });
    });
}

/// Benchmark: Write throughput over time.
///
/// Measures sustained write throughput.
/// Target: >10k versions/sec
fn bench_sustained_write_throughput(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let config = ColdStorageConfig {
        compression: CompressionAlgorithm::Zstd,
        sync_writes: false,
        batch_size: 1000,
        enable_checksums: true,
    };
    let storage = FileColdStorage::new(temp_dir.path(), config).unwrap();

    let batch_size = 10000;
    let versions = create_test_versions(batch_size);

    c.bench_function("sustained_write_10k_versions", |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _ in 0..iters {
                storage
                    .store_node_versions_batch(black_box(&versions))
                    .unwrap();
            }
            let elapsed = start.elapsed();

            // Print throughput for manual verification
            let total_versions = batch_size * iters as usize;
            let throughput = total_versions as f64 / elapsed.as_secs_f64();
            if iters == 1 {
                eprintln!("\nWrite Throughput:");
                eprintln!("  {:.0} versions/sec", throughput);
                eprintln!("  Target: >10,000 versions/sec");
            }

            elapsed
        });
    });
}

/// Benchmark: Compression ratio measurement.
///
/// Measures actual compression ratios achieved.
/// Target: >3x compression ratio
fn bench_compression_ratio(c: &mut Criterion) {
    let mut group = c.benchmark_group("compression_ratio");

    for compression in [
        CompressionAlgorithm::None,
        CompressionAlgorithm::Fast,
        CompressionAlgorithm::Zstd,
    ] {
        let temp_dir = TempDir::new().unwrap();
        let config = ColdStorageConfig {
            compression,
            sync_writes: false,
            batch_size: 1000,
            enable_checksums: true,
        };
        let storage = FileColdStorage::new(temp_dir.path(), config).unwrap();

        // Store 1000 versions to get stable compression metrics
        let versions = create_test_versions(1000);
        storage.store_node_versions_batch(&versions).unwrap();

        // Get statistics
        let stats = storage.stats();
        let ratio = stats.compression_ratio();

        let compression_name = match compression {
            CompressionAlgorithm::None => "no_compression",
            CompressionAlgorithm::Fast => "fast",
            CompressionAlgorithm::Zstd => "zstd",
        };

        eprintln!("\nCompression Ratio ({}):", compression_name);
        eprintln!("  Ratio: {:.2}x", ratio);
        eprintln!("  Raw: {} bytes", stats.bytes_written_raw);
        eprintln!("  Compressed: {} bytes", stats.bytes_written_compressed);
        if matches!(compression, CompressionAlgorithm::Zstd) {
            eprintln!("  Target: >3x compression ratio");
        }

        // Benchmark the compression operation itself
        group.bench_function(BenchmarkId::new("compress", compression_name), |b| {
            let versions = create_test_versions(100);
            b.iter(|| {
                for version in &versions {
                    storage.store_node_version(black_box(version)).unwrap();
                }
            });
        });
    }

    group.finish();
}

/// Benchmark: Mixed read/write workload.
///
/// Simulates a realistic workload with both reads and writes.
fn bench_mixed_workload(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let config = ColdStorageConfig {
        compression: CompressionAlgorithm::Zstd,
        sync_writes: false,
        batch_size: 1000,
        enable_checksums: true,
    };
    let storage = FileColdStorage::new(temp_dir.path(), config).unwrap();

    // Pre-populate with 1000 versions
    let initial_versions = create_test_versions(1000);
    storage
        .store_node_versions_batch(&initial_versions)
        .unwrap();

    let mut write_id = 1000u64;
    let mut read_id = 0u64;

    c.bench_function("mixed_read_write_80_20", |b| {
        b.iter(|| {
            // 80% reads, 20% writes
            for i in 0..100 {
                if i % 5 == 0 {
                    // Write
                    let version = create_test_node_version(write_id, write_id % 1000);
                    write_id += 1;
                    storage.store_node_version(black_box(&version)).unwrap();
                } else {
                    // Read
                    let version_id = VersionId::new(read_id % 1000).unwrap();
                    read_id += 1;
                    let _ = storage.get_node_version(black_box(version_id)).unwrap();
                }
            }
        });
    });
}

criterion_group! {
    name = cold_storage_benches;
    config = common::configure_criterion();
    targets =
        bench_write_latency,
        bench_read_latency,
        bench_batch_write_throughput,
        bench_read_latency_percentiles,
        bench_sustained_write_throughput,
        bench_compression_ratio,
        bench_mixed_workload,
}

criterion_main!(cold_storage_benches);
