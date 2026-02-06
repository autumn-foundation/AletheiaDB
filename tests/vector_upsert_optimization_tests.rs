//! Tests for Issue #207: Optimize vector updates to avoid unconditional remove() before add()
//!
//! The goal is to verify that updating a vector (re-adding with same NodeId) does not
//! perform unnecessary FFI calls. The optimized implementation should use a try-add-first
//! approach instead of unconditionally removing before adding.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};

/// Test that updating a vector works correctly.
///
/// This test verifies the FUNCTIONAL correctness of vector updates.
/// The optimization in Issue #207 should not change this behavior.
#[test]
fn test_vector_update_correctness() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Add initial vector
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Update with different vector (this should NOT increase the count)
    index.add(node, &[0.0, 1.0, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1, "Update should not increase vector count");

    // Search should find the UPDATED vector, not the original
    let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node);
    assert!(
        results[0].1 > 0.99,
        "Updated vector should have high similarity to query"
    );

    // Verify old vector is NOT findable
    let old_results = index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(old_results.len(), 1);
    assert_eq!(old_results[0].0, node);
    // Should have LOW similarity since we updated to a different vector
    assert!(
        old_results[0].1 < 0.5,
        "Old vector should have low similarity after update"
    );
}

/// Test multiple consecutive updates to the same node.
///
/// Verifies that multiple updates work correctly without accumulating vectors.
#[test]
fn test_multiple_consecutive_updates() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(42).unwrap();

    // Perform 10 updates
    for i in 0..10 {
        let mut vector = vec![0.0f32; 4];
        vector[i % 4] = 1.0;
        index.add(node, &vector).unwrap();

        // Should always have exactly 1 vector
        assert_eq!(
            index.len(),
            1,
            "After {} updates, should still have 1 vector",
            i + 1
        );
    }

    // Final verification - should find the last vector
    let query = vec![0.0, 1.0, 0.0, 0.0]; // Last update was i=9, so 9%4=1
    let results = index.search(&query, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node);
}

/// Test concurrent updates to different nodes.
///
/// Ensures that updates to different nodes don't interfere with each other.
#[test]
fn test_concurrent_updates_different_nodes() {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(
        HnswIndexBuilder::new(8, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    // Add initial vectors for 10 nodes
    for i in 1..=10 {
        let node = NodeId::new(i).unwrap();
        let vector = vec![i as f32; 8];
        index.add(node, &vector).unwrap();
    }
    assert_eq!(index.len(), 10);

    // Spawn threads to update each node concurrently
    let handles: Vec<_> = (1..=10)
        .map(|i| {
            let index_clone = Arc::clone(&index);
            thread::spawn(move || {
                let node = NodeId::new(i).unwrap();
                // Update 5 times
                for j in 0..5 {
                    let mut vector = vec![0.0f32; 8];
                    vector[(i + j) as usize % 8] = 1.0;
                    index_clone.add(node, &vector).unwrap();
                }
            })
        })
        .collect();

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    // Should still have exactly 10 vectors
    assert_eq!(
        index.len(),
        10,
        "After concurrent updates, should still have 10 vectors"
    );
}

/// Test that update after delete works correctly.
///
/// This ensures the optimization handles the delete -> add -> add pattern.
#[test]
fn test_update_after_delete() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Add initial vector
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Delete it
    index.remove(node).unwrap();
    assert_eq!(index.len(), 0);

    // Re-add with same NodeId (should allocate new key)
    index.add(node, &[0.0, 1.0, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Update the re-added vector
    index.add(node, &[0.0, 0.0, 1.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Verify we can find the final vector
    let results = index.search(&[0.0, 0.0, 1.0, 0.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node);
    assert!(results[0].1 > 0.99);
}

/// Benchmark-style test to verify optimization intent.
///
/// This test documents the expected behavior: updating should be efficient.
/// While we can't directly measure FFI calls in a unit test, we can verify
/// that updates complete in reasonable time even with many iterations.
#[test]
fn test_update_performance_intention() {
    let index = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Perform 1000 updates
    // With the optimization, this should be ~1000 FFI calls (just adds)
    // Without optimization, this would be ~2000 FFI calls (1000 removes + 1000 adds)
    let start = std::time::Instant::now();

    for i in 0..1000 {
        let mut vector = vec![0.0f32; 128];
        vector[i % 128] = 1.0;
        index.add(node, &vector).unwrap();
    }

    let duration = start.elapsed();

    // Should still have exactly 1 vector
    assert_eq!(index.len(), 1);

    // Sanity check: 1000 updates should complete reasonably fast
    // Even with FFI overhead, this should be well under 1 second
    assert!(
        duration.as_secs() < 5,
        "1000 updates took {:?}, may indicate performance issue",
        duration
    );

    println!(
        "1000 vector updates completed in {:?} ({:.2} µs/update)",
        duration,
        duration.as_micros() as f64 / 1000.0
    );
}
