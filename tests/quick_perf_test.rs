//! Quick performance comparison test for AsyncBatched mode
//!
//! This is not a benchmark - just a quick test to measure relative performance
//! Run with: cargo test quick_perf_test -- --nocapture --ignored

use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, PropertyMapBuilder, WalConfigBuilder, WriteOps};
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

fn create_db_with_mode(mode: DurabilityMode) -> (AletheiaDB, TempDir) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config = WalConfigBuilder::new()
        .wal_dir(temp_dir.path().to_path_buf())
        .segment_size(10 * 1024 * 1024)
        .unwrap()
        .segments_to_retain(3)
        .unwrap()
        .durability_mode(mode)
        .build();
    let db = AletheiaDB::with_wal_config(config).unwrap();
    (db, temp_dir)
}

#[test]
#[ignore]
fn quick_perf_test() {
    println!("\n=== Quick Performance Comparison ===");
    println!("Testing 1000 single-node writes per mode...\n");

    let test_count = 1000;

    // Test Synchronous (baseline)
    {
        let (db, _guard) = create_db_with_mode(DurabilityMode::Synchronous);
        let start = Instant::now();

        for i in 0..test_count {
            db.write(|tx| {
                tx.create_node(
                    "PerfTest",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
            })
            .unwrap();
        }

        let elapsed = start.elapsed();
        let avg_latency = elapsed.as_micros() / test_count;
        let ops_per_sec = (test_count as f64 / elapsed.as_secs_f64()) as u64;

        println!("Synchronous:");
        println!("  Total time: {:?}", elapsed);
        println!("  Avg latency: {}µs", avg_latency);
        println!("  Throughput: {} ops/sec", ops_per_sec);
        thread::sleep(Duration::from_millis(200)); // Let background threads settle
    }

    // Test Async
    {
        let (db, _guard) = create_db_with_mode(DurabilityMode::Async {
            flush_interval_ms: 100,
        });
        let start = Instant::now();

        for i in 0..test_count {
            db.write(|tx| {
                tx.create_node(
                    "PerfTest",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
            })
            .unwrap();
        }

        let elapsed = start.elapsed();
        let avg_latency = elapsed.as_micros() / test_count;
        let ops_per_sec = (test_count as f64 / elapsed.as_secs_f64()) as u64;

        println!("\nAsync (100ms flush):");
        println!("  Total time: {:?}", elapsed);
        println!("  Avg latency: {}µs", avg_latency);
        println!("  Throughput: {} ops/sec", ops_per_sec);
        thread::sleep(Duration::from_millis(200)); // Let background threads settle
    }

    // Test GroupCommit
    {
        let (db, _guard) = create_db_with_mode(DurabilityMode::group_commit_default());
        let start = Instant::now();

        for i in 0..test_count {
            db.write(|tx| {
                tx.create_node(
                    "PerfTest",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
            })
            .unwrap();
        }

        let elapsed = start.elapsed();
        let avg_latency = elapsed.as_micros() / test_count;
        let ops_per_sec = (test_count as f64 / elapsed.as_secs_f64()) as u64;

        println!("\nGroupCommit (2ms, 200 batch):");
        println!("  Total time: {:?}", elapsed);
        println!("  Avg latency: {}µs", avg_latency);
        println!("  Throughput: {} ops/sec", ops_per_sec);
        thread::sleep(Duration::from_millis(200)); // Let background threads settle
    }

    // Test AsyncBatched default
    {
        let (db, _guard) = create_db_with_mode(DurabilityMode::async_batched_default());
        let start = Instant::now();

        for i in 0..test_count {
            db.write(|tx| {
                tx.create_node(
                    "PerfTest",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
            })
            .unwrap();
        }

        let elapsed = start.elapsed();
        let avg_latency = elapsed.as_micros() / test_count;
        let ops_per_sec = (test_count as f64 / elapsed.as_secs_f64()) as u64;

        println!("\nAsyncBatched (10ms, 100 batch):");
        println!("  Total time: {:?}", elapsed);
        println!("  Avg latency: {}µs", avg_latency);
        println!("  Throughput: {} ops/sec", ops_per_sec);
        println!("  Target: <100µs latency, >10,000 ops/sec");
    }

    // Test AsyncBatched aggressive
    {
        let (db, _guard) =
            create_db_with_mode(DurabilityMode::async_batched_validated(5, 50).unwrap());
        let start = Instant::now();

        for i in 0..test_count {
            db.write(|tx| {
                tx.create_node(
                    "PerfTest",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
            })
            .unwrap();
        }

        let elapsed = start.elapsed();
        let avg_latency = elapsed.as_micros() / test_count;
        let ops_per_sec = (test_count as f64 / elapsed.as_secs_f64()) as u64;

        println!("\nAsyncBatched Aggressive (5ms, 50 batch):");
        println!("  Total time: {:?}", elapsed);
        println!("  Avg latency: {}µs", avg_latency);
        println!("  Throughput: {} ops/sec", ops_per_sec);
        thread::sleep(Duration::from_millis(200)); // Let background threads settle
    }

    println!("\n=== Test Complete ===");
}
