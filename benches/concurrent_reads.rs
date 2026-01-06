use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::GallifreyDB;
use gallifreydb::api::transaction::WriteOps;
use gallifreydb::core::property::PropertyMapBuilder;
use std::sync::Arc;
use std::thread;

/// Benchmark concurrent time-travel reads with varying concurrency levels.
///
/// This benchmark measures the impact of the TinyLFU cache on concurrent
/// time-travel query performance. Before the fix (issue #227), concurrent
/// reads would serialize due to lock contention during reconstruction.
/// After the fix, the cache allows concurrent reads to proceed in parallel.
fn bench_concurrent_time_travel_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_time_travel_reads");

    // Test with different concurrency levels
    for num_threads in [1, 2, 4, 8, 10] {
        group.bench_with_input(
            BenchmarkId::new("threads", num_threads),
            &num_threads,
            |b, &num_threads| {
                // Setup: Create database with versioned nodes
                let db = Arc::new(setup_database_with_versions());

                b.iter(|| {
                    let mut handles = vec![];

                    for _ in 0..num_threads {
                        let db_clone = Arc::clone(&db);
                        let handle = thread::spawn(move || {
                            // Each thread performs 100 time-travel queries
                            for i in 0..100 {
                                let node_id =
                                    gallifreydb::core::id::NodeId::new((i % 100) as u64 + 1)
                                        .unwrap();
                                let timestamp = 1000 + (i as i64 * 100);

                                let result =
                                    db_clone.get_node_at_time(node_id, timestamp, timestamp);

                                // Force the result to be used
                                black_box(result);
                            }
                        });
                        handles.push(handle);
                    }

                    // Wait for all threads to complete
                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark cache hit rate by reading the same version repeatedly.
fn bench_cache_hit_rate(c: &mut Criterion) {
    let db = setup_database_with_versions();
    let node_id = gallifreydb::core::id::NodeId::new(1).unwrap();
    let timestamp = 1000;

    c.bench_function("time_travel_cache_hit", |b| {
        b.iter(|| {
            // Same query repeatedly - should hit cache after first miss
            let result = db.get_node_at_time(
                black_box(node_id),
                black_box(timestamp),
                black_box(timestamp),
            );
            black_box(result)
        })
    });
}

/// Benchmark cache miss (first-time reconstruction).
fn bench_cache_miss(c: &mut Criterion) {
    c.bench_function("time_travel_cache_miss", |b| {
        b.iter(|| {
            // Create fresh database for each iteration to ensure cache miss
            let db = setup_database_with_versions();
            let node_id = gallifreydb::core::id::NodeId::new(1).unwrap();
            let timestamp = 1000;

            let result = db.get_node_at_time(
                black_box(node_id),
                black_box(timestamp),
                black_box(timestamp),
            );
            black_box(result)
        })
    });
}

/// Setup helper: Create a database with multiple versioned nodes.
fn setup_database_with_versions() -> GallifreyDB {
    let db = GallifreyDB::new();

    // Create 100 nodes, each with 5 versions
    for i in 0..100 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node_{}", i).as_str())
            .insert("value", i as i64)
            .build();

        let node_id = db.create_node("TestNode", props).unwrap();

        // Create additional versions by updating properties
        for version in 1..5 {
            let updated_props = PropertyMapBuilder::new()
                .insert("name", format!("Node_{}", i).as_str())
                .insert("value", (i * 10 + version) as i64)
                .insert("version", version as i64)
                .build();

            // Update node through write transaction
            db.write(|tx| {
                tx.update_node(node_id, updated_props.clone())?;
                Ok(())
            })
            .unwrap();
        }
    }

    db
}

criterion_group!(
    benches,
    bench_concurrent_time_travel_reads,
    bench_cache_hit_rate,
    bench_cache_miss
);
criterion_main!(benches);
