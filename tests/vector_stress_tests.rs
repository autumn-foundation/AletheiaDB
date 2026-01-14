//! Stress tests for concurrent vector index operations.

use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::Arc;
use std::thread;

/// Stress test: concurrent add/search/delete operations.
#[test]
fn stress_concurrent_operations() {
    let index = Arc::new(
        HnswIndexBuilder::new(64, DistanceMetric::Cosine)
            .initial_capacity(10000)
            .build()
            .unwrap(),
    );

    let num_threads = 8;
    let ops_per_thread = 1000;

    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let index = Arc::clone(&index);

        let handle = thread::spawn(move || {
            let base_id = thread_id * ops_per_thread;

            for i in 0..ops_per_thread {
                let node_id = NodeId::new((base_id + i) as u64 + 1).unwrap();
                let vector: Vec<f32> = (0..64).map(|j| (i + j) as f32 / 1000.0).collect();

                // Add
                index.add(node_id, &vector).unwrap();

                // Search (every 10th operation)
                if i % 10 == 0 {
                    let query: Vec<f32> = (0..64).map(|j| (i + j + 1) as f32 / 1000.0).collect();
                    let _ = index.search(&query, 10);
                }

                // Delete (every 5th operation)
                if i % 5 == 0 && i > 0 {
                    let delete_id = NodeId::new((base_id + i - 1) as u64 + 1).unwrap();
                    let _ = index.remove(delete_id);
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Verify index is still usable
    let results = index.search(&vec![0.5f32; 64], 10);
    assert!(results.is_ok());
}

/// Stress test: rapid add/remove cycles.
#[test]
fn stress_rapid_add_remove() {
    let index = HnswIndexBuilder::new(32, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    let vector = vec![0.5f32; 32];

    // Rapid add/remove cycles
    for _ in 0..1000 {
        index.add(node, &vector).unwrap();
        index.remove(node).unwrap();
    }

    // Final state should be empty
    assert_eq!(index.len(), 0);

    // Should be able to add again
    index.add(node, &vector).unwrap();
    assert_eq!(index.len(), 1);
}

/// Stress test: many searches on large index.
#[test]
fn stress_search_throughput() {
    let index = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .initial_capacity(10000)
        .build()
        .unwrap();

    // Build index with 10k vectors
    for i in 0..10000 {
        let node = NodeId::new(i as u64 + 1).unwrap();
        let vector: Vec<f32> = (0..128)
            .map(|j| ((i * 17 + j * 31) % 1000) as f32 / 1000.0)
            .collect();
        index.add(node, &vector).unwrap();
    }

    // Perform 1000 searches
    let start = std::time::Instant::now();

    for i in 0..1000 {
        let query: Vec<f32> = (0..128)
            .map(|j| ((i * 13 + j * 29) % 1000) as f32 / 1000.0)
            .collect();
        let results = index.search(&query, 10).unwrap();
        assert!(!results.is_empty());
    }

    let elapsed = start.elapsed();
    let qps = 1000.0 / elapsed.as_secs_f64();

    println!("Search throughput: {:.0} queries/second", qps);

    // Should achieve at least 100 QPS
    assert!(qps > 100.0, "Search throughput {:.0} QPS is too low", qps);
}
