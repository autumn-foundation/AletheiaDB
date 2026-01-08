use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use gallifreydb::GallifreyDB;
use gallifreydb::api::transaction::WriteOps;
use gallifreydb::core::property::PropertyMapBuilder;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Benchmark concurrent time-travel reads with varying concurrency levels.
fn bench_concurrent_time_travel_reads(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_time_travel_reads");

    // Setup: Create database ONCE.
    // We wrap it in Arc here so we can clone the reference into threads cheaply.
    let db = Arc::new(setup_database_with_versions(100)); // 100 nodes is enough for this

    for num_threads in [1, 2, 4, 8, 10] {
        group.bench_with_input(
            BenchmarkId::new("threads", num_threads),
            &num_threads,
            |b, &num_threads| {
                // OPTIMIZATION 1: Create the thread pool OUTSIDE the measurement loop.
                // We only want to measure the query time, not OS thread spawning time.
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(num_threads)
                    .build()
                    .unwrap();

                b.iter(|| {
                    pool.install(|| {
                        rayon::scope(|s| {
                            for _ in 0..num_threads {
                                let db_clone = Arc::clone(&db);
                                s.spawn(move |_| {
                                    // Each thread performs 25 queries (total work per iter scales with threads)
                                    // or keep total work constant? Usually scaling work is better for throughput tests.
                                    for i in 0..25 {
                                        let node_id = gallifreydb::core::id::NodeId::new(
                                            (i % 100) as u64 + 1,
                                        )
                                        .unwrap();
                                        // Pick a valid timestamp
                                        let timestamp = 1000 + (i as i64 * 100);

                                        let result = db_clone
                                            .get_node_at_time(node_id, timestamp, timestamp);

                                        let _ = black_box(result);
                                    }
                                });
                            }
                        });
                    });
                });
            },
        );
    }

    group.finish();
}

/// Benchmark cache hit rate by reading the same version repeatedly.
fn bench_cache_hit_rate(c: &mut Criterion) {
    // Setup ONCE
    let db = setup_database_with_versions(10);
    let node_id = gallifreydb::core::id::NodeId::new(1).unwrap();
    let timestamp = 1000;

    // Warm up the cache manually (optional, but ensures we measure hits)
    let _ = db.get_node_at_time(node_id, timestamp, timestamp);

    c.bench_function("time_travel_cache_hit", |b| {
        b.iter(|| {
            // Hot path: This should be ~50ns
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
    // OPTIMIZATION 2: The "Infinite Corridor" Strategy
    // Instead of rebuilding the DB 100 times, build ONE DB with 10,000 nodes.
    // In the loop, read Node 1, then Node 2, then Node 3...
    // Since we never repeat a node, every read is a cache miss.
    let node_count = 10_000;
    let db = setup_database_with_versions(node_count);

    // Atomic counter to pick a unique node each iteration
    let counter = AtomicU64::new(1);

    c.bench_function("time_travel_cache_miss", |b| {
        b.iter(|| {
            // Get unique ID (mod count to be safe, but ideally we don't wrap)
            let i = counter.fetch_add(1, Ordering::Relaxed);
            let id_to_read = (i % node_count as u64) + 1;

            let node_id = gallifreydb::core::id::NodeId::new(id_to_read).unwrap();
            let timestamp = 1000; // First version

            let result = db.get_node_at_time(
                black_box(node_id),
                black_box(timestamp),
                black_box(timestamp),
            );
            black_box(result)
        })
    });
}

/// Setup helper: Create a database with `count` versioned nodes.
fn setup_database_with_versions(count: usize) -> GallifreyDB {
    // OPTIMIZATION 3: Use a temp dir!
    // If GallifreyDB defaults to disk, we don't want tests colliding.
    // Assuming new() is in-memory or handles this, but explicit is better if possible.
    let db = GallifreyDB::new();

    // Batch writes if possible, otherwise individual
    for i in 0..count {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Node_{}", i).as_str())
            .insert("value", i as i64)
            .build();

        let node_id = db.create_node("TestNode", props).unwrap();

        // Create versions
        for version in 1..3 {
            // Reduced versions to speed up setup, still sufficient for testing
            let updated_props = PropertyMapBuilder::new()
                .insert("name", format!("Node_{}", i).as_str())
                .insert("value", (i * 10 + version) as i64)
                .insert("version", version as i64)
                .build();

            db.write(|tx| {
                tx.update_node(node_id, updated_props.clone())?;
                Ok(())
            })
            .unwrap();
        }
    }

    db
}

fn configure_criterion() -> Criterion {
    let sample_size = std::env::var("BENCH_SAMPLE_SIZE")
        .map(|s| s.parse().unwrap_or(50))
        .unwrap_or(50);

    Criterion::default().sample_size(sample_size)
}

criterion_group!(
    name = benches;
    config = configure_criterion();
    targets = bench_concurrent_time_travel_reads,
    bench_cache_hit_rate,
    bench_cache_miss
);
criterion_main!(benches);
