//! Benchmarks for PropertyDelta hot path operations (Issue #214)
//!
//! These benchmarks measure the performance of PropertyDelta::from_diff and apply
//! operations which are on the hot path for temporal queries. With anchor_interval=10,
//! 90% of versions are deltas requiring repeated apply() calls during time-travel.
//!
//! Key performance characteristics to verify:
//! 1. PropertyKey cloning is O(1) (InternedString ID copy)
//! 2. PropertyValue cloning is O(1) (Arc refcount increment)
//! 3. from_diff scales linearly with number of changed properties
//! 4. apply scales linearly with total properties + changes

mod common;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::core::property::{PropertyMapBuilder, PropertyValue};
use gallifreydb::storage::version::PropertyDelta;

/// Benchmark: PropertyDelta::from_diff with varying numbers of changed properties
///
/// This tests the cost of computing deltas when different percentages of
/// properties have changed. Expected: O(n) where n = total properties.
fn bench_property_delta_from_diff(c: &mut Criterion) {
    let mut group = c.benchmark_group("property_delta_from_diff");

    // Test with varying numbers of properties: 10, 50, 100, 500
    for num_props in [10, 50, 100, 500] {
        // Test with different change percentages: 1%, 10%, 50%, 90%
        for change_pct in [1, 10, 50, 90] {
            let num_changed = (num_props * change_pct) / 100;
            let bench_name = format!("{}_props_{}%_changed", num_props, change_pct);

            group.bench_with_input(
                BenchmarkId::from_parameter(&bench_name),
                &(num_props, num_changed),
                |b, &(total, changed)| {
                    b.iter_batched(
                        || {
                            // Setup: Create two property maps with some differences
                            let mut old_builder = PropertyMapBuilder::new();
                            let mut new_builder = PropertyMapBuilder::new();

                            // Add unchanged properties
                            for i in 0..total {
                                let key = format!("prop_{}", i);
                                old_builder = old_builder.insert(&key, i as i64);

                                if i < changed {
                                    // Changed property (different value)
                                    new_builder = new_builder.insert(&key, (i + 1000) as i64);
                                } else {
                                    // Unchanged property
                                    new_builder = new_builder.insert(&key, i as i64);
                                }
                            }

                            let old = old_builder.build();
                            let new = new_builder.build();
                            (old, new)
                        },
                        |(old, new)| {
                            // Benchmark: Compute the delta
                            let delta = black_box(PropertyDelta::from_diff(&old, &new));
                            black_box(delta);
                        },
                        criterion::BatchSize::SmallInput,
                    );
                },
            );
        }
    }
    group.finish();
}

/// Benchmark: PropertyDelta::from_diff with vector properties (sparse delta optimization)
///
/// This tests the sparse vector delta optimization (Issue #215) which should
/// be much faster than storing full vectors when only a few elements change.
fn bench_property_delta_from_diff_with_vectors(c: &mut Criterion) {
    let mut group = c.benchmark_group("property_delta_from_diff_vectors");

    // Test with different vector dimensions
    for dimensions in [384, 768, 1536] {
        // Test with different numbers of changed elements
        for num_changed in [1, 5, 10, 50] {
            let bench_name = format!("{}_dim_{}_changed", dimensions, num_changed);

            group.bench_with_input(
                BenchmarkId::from_parameter(&bench_name),
                &(dimensions, num_changed),
                |b, &(dim, changed)| {
                    b.iter_batched(
                        || {
                            // Setup: Create property maps with vector embeddings
                            let old_embedding = vec![0.1f32; dim];
                            let mut new_embedding = old_embedding.clone();

                            // Change a few elements
                            for item in new_embedding.iter_mut().take(changed.min(dim)) {
                                *item = 0.9f32;
                            }

                            let old = PropertyMapBuilder::new()
                                .insert("name", "TestNode")
                                .insert("age", 30i64)
                                .insert("embedding", PropertyValue::vector(&old_embedding))
                                .build();

                            let new = PropertyMapBuilder::new()
                                .insert("name", "TestNode")
                                .insert("age", 31i64) // Changed
                                .insert("embedding", PropertyValue::vector(&new_embedding))
                                .build();

                            (old, new)
                        },
                        |(old, new)| {
                            // Benchmark: Compute delta with vector optimization
                            let delta = black_box(PropertyDelta::from_diff(&old, &new));
                            black_box(delta);
                        },
                        criterion::BatchSize::SmallInput,
                    );
                },
            );
        }
    }
    group.finish();
}

/// Benchmark: PropertyDelta::apply (hot path for time-travel queries)
///
/// This is the critical hot path: during time-travel queries with anchor_interval=10,
/// we need to apply up to 9 deltas in sequence to reconstruct historical state.
/// Expected: O(total_props + changes) where changes << total_props typically.
fn bench_property_delta_apply(c: &mut Criterion) {
    let mut group = c.benchmark_group("property_delta_apply");

    // Test with varying base property map sizes
    for num_base_props in [10, 50, 100, 500] {
        // Test with different numbers of changes in the delta
        for num_changes in [1, 5, 10, 50] {
            let bench_name = format!("{}_base_{}_changes", num_base_props, num_changes);

            group.bench_with_input(
                BenchmarkId::from_parameter(&bench_name),
                &(num_base_props, num_changes),
                |b, &(base_size, changes)| {
                    b.iter_batched(
                        || {
                            // Setup: Create base property map
                            let mut base_builder = PropertyMapBuilder::new();
                            for i in 0..base_size {
                                let key = format!("prop_{}", i);
                                base_builder = base_builder.insert(&key, i as i64);
                            }
                            let base = base_builder.build();

                            // Create a delta with some changes
                            let mut delta = PropertyDelta::new();
                            for i in 0..changes.min(base_size) {
                                let key = GLOBAL_INTERNER.intern(format!("prop_{}", i)).unwrap();
                                delta
                                    .changed
                                    .insert(key, PropertyValue::Int((i + 1000) as i64));
                            }

                            (base, delta)
                        },
                        |(base, delta)| {
                            // Benchmark: Apply the delta
                            let result = black_box(delta.apply(&base));
                            black_box(result);
                        },
                        criterion::BatchSize::SmallInput,
                    );
                },
            );
        }
    }
    group.finish();
}

/// Benchmark: Sequential delta application (realistic time-travel scenario)
///
/// This simulates the realistic hot path: applying 9 deltas in sequence
/// (worst case before hitting an anchor with anchor_interval=10).
/// This is what happens during every time-travel query.
fn bench_sequential_delta_application(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequential_delta_application");

    // Test with different numbers of sequential deltas (typical: 1-9 before anchor)
    for num_deltas in [1, 3, 5, 9] {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_deltas),
            &num_deltas,
            |b, &deltas| {
                b.iter_batched(
                    || {
                        // Setup: Create base property map with 100 properties
                        let mut base_builder = PropertyMapBuilder::new();
                        for i in 0..100 {
                            let key = format!("prop_{}", i);
                            base_builder = base_builder.insert(&key, i as i64);
                        }
                        let base = base_builder.build();

                        // Create a sequence of deltas, each changing 5 properties
                        let mut delta_chain = Vec::new();
                        for delta_idx in 0..deltas {
                            let mut delta = PropertyDelta::new();
                            // Each delta changes 5 properties
                            for i in 0..5 {
                                let prop_idx = (delta_idx * 5 + i) % 100;
                                let key = GLOBAL_INTERNER
                                    .intern(format!("prop_{}", prop_idx))
                                    .unwrap();
                                delta
                                    .changed
                                    .insert(key, PropertyValue::Int((delta_idx * 1000 + i) as i64));
                            }
                            delta_chain.push(delta);
                        }

                        (base, delta_chain)
                    },
                    |(base, deltas)| {
                        // Benchmark: Apply all deltas in sequence (hot path!)
                        let mut current = base;
                        for delta in &deltas {
                            current = black_box(delta.apply(&current));
                        }
                        black_box(current);
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

/// Benchmark: PropertyKey cloning cost (should be O(1) with InternedString)
///
/// This verifies that PropertyKey cloning is cheap (just copying an ID),
/// not expensive (heap allocation). This is the optimization from Issue #202.
fn bench_property_key_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("property_key_clone");

    // Intern a bunch of keys first
    let keys: Vec<_> = (0..1000)
        .map(|i| GLOBAL_INTERNER.intern(format!("key_{}", i)).unwrap())
        .collect();

    group.bench_function("clone_1000_interned_keys", |b| {
        b.iter(|| {
            // Benchmark: Clone all keys (should be O(1) per key)
            let cloned: Vec<_> = keys.iter().map(|k| black_box(*k)).collect();
            black_box(cloned);
        });
    });

    group.finish();
}

/// Benchmark: PropertyValue cloning cost (should be O(1) with Arc)
///
/// This verifies that PropertyValue cloning is cheap (Arc refcount increment),
/// not expensive (deep copy). This validates the Arc optimization.
fn bench_property_value_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("property_value_clone");

    // Create various property values
    let string_value = PropertyValue::string("A reasonably long string value for testing");
    let vector_value = PropertyValue::vector(vec![0.1f32; 1536]); // Large embedding
    let array_value = PropertyValue::array(vec![PropertyValue::Int(42); 100]);

    group.bench_function("clone_string_arc", |b| {
        b.iter(|| {
            let cloned = black_box(string_value.clone());
            black_box(cloned);
        });
    });

    group.bench_function("clone_vector_arc_1536_dim", |b| {
        b.iter(|| {
            let cloned = black_box(vector_value.clone());
            black_box(cloned);
        });
    });

    group.bench_function("clone_array_arc_100_elements", |b| {
        b.iter(|| {
            let cloned = black_box(array_value.clone());
            black_box(cloned);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_property_delta_from_diff,
    bench_property_delta_from_diff_with_vectors,
    bench_property_delta_apply,
    bench_sequential_delta_application,
    bench_property_key_clone,
    bench_property_value_clone,
);

criterion_main!(benches);
