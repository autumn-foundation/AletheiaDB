//! Benchmarks for HistoricalStorage anchor creation and version counting
//!
//! Issue #208: Measures the performance impact of count_versions_since_anchor
//! walking the chain on every add operation.

mod common;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::id::{NodeId, VersionId};
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::temporal::BiTemporalInterval;
use gallifreydb::storage::historical::HistoricalStorage;
use gallifreydb::storage::version::AnchorConfig;

/// Benchmark: Add node versions with varying anchor intervals
///
/// This benchmark measures the performance impact of count_versions_since_anchor
/// walking the chain on every add. With the default anchor_interval=10, each add
/// performs up to 9 HashMap lookups just for counting.
///
/// Expected before fix: O(anchor_interval) - linear with anchor interval
/// Expected after fix: O(1) - constant time regardless of anchor interval
fn bench_add_node_version_with_varying_anchor_intervals(c: &mut Criterion) {
    let mut group = c.benchmark_group("add_node_version_anchor_counting");

    // Test with anchor intervals: 5, 10, 20, 50
    // Before fix: Higher intervals = more lookups per add
    // After fix: Should be constant regardless of interval
    for anchor_interval in [5u32, 10, 20, 50] {
        group.bench_with_input(
            BenchmarkId::new("anchor_interval", anchor_interval),
            &anchor_interval,
            |b, &interval| {
                b.iter_batched(
                    || {
                        // Setup: Create storage with specific anchor interval
                        let config = AnchorConfig {
                            anchor_interval: interval,
                            max_delta_chain: 100,
                        };
                        let storage = HistoricalStorage::with_config(config);
                        let node_id = NodeId::new(1).unwrap();
                        (storage, node_id)
                    },
                    |(mut storage, node_id)| {
                        // Benchmark: Add 100 versions
                        // This simulates a bulk insert scenario
                        let label = GLOBAL_INTERNER.intern("TestLabel").unwrap();
                        for i in 0..100u64 {
                            let version_id = VersionId::new(i).unwrap();
                            let temporal =
                                BiTemporalInterval::current((1000 + (i as i64) * 100).into());
                            let properties = PropertyMapBuilder::new()
                                .insert("version", i as i64)
                                .build();

                            black_box(storage.add_node_version(
                                node_id, version_id, temporal, label, properties,
                            ))
                            .unwrap();
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

/// Benchmark: Single add operation performance with different chain positions
///
/// Measures the cost of a single add_node_version at different positions in the
/// version chain (early vs late in chain before anchor).
///
/// Expected before fix: Performance degrades as chain length increases
/// Expected after fix: Constant performance regardless of position
fn bench_single_add_at_chain_positions(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_add_chain_position");

    let config = AnchorConfig {
        anchor_interval: 10,
        max_delta_chain: 100,
    };

    // Test adding at different positions: 1, 3, 5, 7, 9 (deltas before anchor)
    for position in [1, 3, 5, 7, 9] {
        group.bench_with_input(
            BenchmarkId::new("deltas_before_anchor", position),
            &position,
            |b, &pos| {
                b.iter_batched(
                    || {
                        // Setup: Create storage and add 'pos' versions
                        let mut storage = HistoricalStorage::with_config(config.clone());
                        let node_id = NodeId::new(1).unwrap();
                        let label = GLOBAL_INTERNER.intern("TestLabel").unwrap();

                        // Pre-populate with 'pos' versions
                        for i in 0..pos {
                            let version_id = VersionId::new(i as u64).unwrap();
                            let temporal =
                                BiTemporalInterval::current((1000 + (i as i64) * 100).into());
                            let properties = PropertyMapBuilder::new()
                                .insert("version", i as i64)
                                .build();

                            storage
                                .add_node_version(node_id, version_id, temporal, label, properties)
                                .unwrap();
                        }

                        (storage, node_id, pos)
                    },
                    |(mut storage, node_id, pos)| {
                        // Benchmark: Add one more version at this position
                        let label = GLOBAL_INTERNER.intern("TestLabel").unwrap();
                        let version_id = VersionId::new(pos as u64).unwrap();
                        let temporal =
                            BiTemporalInterval::current((1000 + (pos as i64) * 100).into());
                        let properties = PropertyMapBuilder::new()
                            .insert("version", pos as i64)
                            .build();

                        black_box(
                            storage
                                .add_node_version(node_id, version_id, temporal, label, properties),
                        )
                        .unwrap();
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

/// Benchmark: Bulk insert performance across multiple entities
///
/// Simulates a realistic workload with multiple entities being updated concurrently.
/// This stresses the version counting logic across different entity chains.
fn bench_bulk_insert_multiple_entities(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_insert_multiple_entities");

    for entity_count in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("entities", entity_count),
            &entity_count,
            |b, &count| {
                b.iter_batched(
                    || {
                        // Setup: Create storage with default anchor interval (10)
                        let storage = HistoricalStorage::new();
                        (storage, count)
                    },
                    |(mut storage, entity_count)| {
                        // Benchmark: Add 20 versions to each entity
                        let label = GLOBAL_INTERNER.intern("TestLabel").unwrap();
                        for entity_id in 0..entity_count {
                            let node_id = NodeId::new(entity_id).unwrap();

                            for version in 0..20u64 {
                                let version_id =
                                    VersionId::new(entity_id * 1000 + version).unwrap();
                                let temporal = BiTemporalInterval::current(
                                    (1000 + (version as i64) * 100).into(),
                                );
                                let properties = PropertyMapBuilder::new()
                                    .insert("entity_id", entity_id as i64)
                                    .insert("version", version as i64)
                                    .build();

                                black_box(storage.add_node_version(
                                    node_id, version_id, temporal, label, properties,
                                ))
                                .unwrap();
                            }
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

/// Benchmark: stats() performance with varying version counts
///
/// Issue #212: This benchmark demonstrates that stats() is now O(1) instead of O(versions).
/// Before the fix, stats() iterated through all versions to count anchors vs deltas.
/// After the fix, it returns cached counters maintained during version insertion.
///
/// Expected before fix: Linear time with version count
/// Expected after fix: Constant time regardless of version count
fn bench_stats_with_varying_version_counts(c: &mut Criterion) {
    let mut group = c.benchmark_group("stats_performance");

    // Test with different version counts: 1K, 10K, 100K, 1M
    // Before fix: Performance degrades linearly
    // After fix: Constant time regardless of count
    for version_count in [1_000, 10_000, 100_000, 1_000_000] {
        group.bench_with_input(
            BenchmarkId::new("versions", version_count),
            &version_count,
            |b, &count| {
                // Setup: Create storage with many versions
                let config = AnchorConfig {
                    anchor_interval: 10,
                    max_delta_chain: 100,
                };
                let mut storage = HistoricalStorage::with_config(config);
                let label = GLOBAL_INTERNER.intern("TestLabel").unwrap();

                // Pre-populate with versions across multiple nodes to avoid hitting
                // retention limits (max_versions_per_entity)
                for i in 0..count {
                    let node_idx = i / 100; // 100 versions per node
                    let node_id = NodeId::new(node_idx).unwrap();
                    let version_id = VersionId::new(i).unwrap();
                    let temporal = BiTemporalInterval::current((1000 + (i as i64) * 100).into());
                    let properties = PropertyMapBuilder::new().insert("value", i as i64).build();

                    storage
                        .add_node_version(node_id, version_id, temporal, label, properties)
                        .unwrap();
                }

                // Benchmark: Call stats() repeatedly
                b.iter(|| {
                    black_box(storage.stats());
                });
            },
        );
    }
    group.finish();
}

/// Benchmark: Delta creation with property caching (Issue #210)
///
/// This benchmark measures the performance improvement from caching properties
/// at write-time for delta versions. Before the fix, creating each delta required
/// reconstructing the previous version's properties. After the fix, properties are
/// cached when added, eliminating reconstructions during consecutive writes.
///
/// Expected before fix: Each delta creation triggers property reconstruction
/// Expected after fix: Zero reconstructions during consecutive delta writes
fn bench_delta_creation_with_caching(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_creation_property_caching");

    // Use large anchor interval to create many consecutive deltas
    let config = AnchorConfig {
        anchor_interval: 1000,
        max_delta_chain: 2000,
    };

    // Test with different numbers of consecutive deltas
    for delta_count in [10, 50, 100, 200] {
        group.bench_with_input(
            BenchmarkId::new("consecutive_deltas", delta_count),
            &delta_count,
            |b, &count| {
                b.iter_batched(
                    || {
                        // Setup: Create fresh storage
                        let storage = HistoricalStorage::with_config(config.clone());
                        let node_id = NodeId::new(1).unwrap();
                        (storage, node_id)
                    },
                    |(mut storage, node_id)| {
                        // Benchmark: Add many consecutive delta versions
                        // With fix: All properties cached at write-time (0 reconstructions)
                        // Without fix: Each delta reconstructs previous properties
                        let label = GLOBAL_INTERNER.intern("TestLabel").unwrap();
                        for i in 0..count {
                            let version_id = VersionId::new(i).unwrap();
                            let temporal =
                                BiTemporalInterval::current((1000 + (i as i64) * 100).into());
                            let properties = PropertyMapBuilder::new()
                                .insert("data", format!("version_{}", i))
                                .insert("counter", i as i64)
                                .build();

                            black_box(storage.add_node_version(
                                node_id, version_id, temporal, label, properties,
                            ))
                            .unwrap();
                        }

                        // Verify cache effectiveness
                        let metrics = storage.cache_metrics();
                        // After fix: full_reconstructions should be 0
                        assert_eq!(
                            metrics.full_reconstructions, 0,
                            "Expected 0 reconstructions with caching fix"
                        );
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

/// Benchmark: Interleaved updates to multiple entities (Issue #210 real-world scenario)
///
/// Simulates a realistic write-heavy workload where multiple entities are updated
/// in an interleaved fashion, typical of real-time data streams or event sourcing.
///
/// This stresses the property cache as properties from multiple entities compete
/// for cache space. The fix ensures recently-written properties stay cached.
fn bench_interleaved_multi_entity_updates(c: &mut Criterion) {
    let mut group = c.benchmark_group("interleaved_entity_updates");

    let config = AnchorConfig {
        anchor_interval: 50,
        max_delta_chain: 100,
    };

    // Simulate updating 10, 50, or 100 entities in interleaved fashion
    for entity_count in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("entities_interleaved", entity_count),
            &entity_count,
            |b, &count| {
                b.iter_batched(
                    || {
                        let storage = HistoricalStorage::with_config(config.clone());
                        storage
                    },
                    |mut storage| {
                        // Add 10 versions to each entity in round-robin fashion
                        let label = GLOBAL_INTERNER.intern("Sensor").unwrap();
                        for round in 0..10 {
                            for entity_id in 0..count {
                                let node_id = NodeId::new(entity_id).unwrap();
                                let version_id =
                                    VersionId::new((round * count + entity_id) as u64).unwrap();
                                let temporal = BiTemporalInterval::current(
                                    (1000 + (round as i64) * 100).into(),
                                );
                                let properties = PropertyMapBuilder::new()
                                    .insert("sensor_id", entity_id as i64)
                                    .insert("reading", (round * 10 + entity_id) as i64)
                                    .build();

                                black_box(storage.add_node_version(
                                    node_id, version_id, temporal, label, properties,
                                ))
                                .unwrap();
                            }
                        }

                        // Verify that no reconstructions occurred. With the default cache size
                        // (10,000 entries), all recently written versions should remain cached,
                        // even with interleaved updates across multiple entities.
                        let metrics = storage.cache_metrics();
                        assert_eq!(
                            metrics.full_reconstructions, 0,
                            "Expected 0 reconstructions for interleaved updates with sufficient cache"
                        );
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_add_node_version_with_varying_anchor_intervals,
    bench_single_add_at_chain_positions,
    bench_bulk_insert_multiple_entities,
    bench_stats_with_varying_version_counts,
    bench_delta_creation_with_caching,
    bench_interleaved_multi_entity_updates
);
criterion_main!(benches);
