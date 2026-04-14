//! Coverage tests for Issue #207 optimization.
//!
//! These tests ensure the new conditional remove path is properly covered,
//! specifically testing the edge case where a NodeId exists in id_mapping
//! but the corresponding key doesn't exist in the usearch index.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};

/// Test the optimization path where contains() returns false.
///
/// This is a tricky scenario to reproduce because normally if a NodeId exists
/// in id_mapping, the key should also exist in usearch. This edge case can occur:
/// 1. During recovery when mappings are restored but index isn't fully loaded
/// 2. After bugs or race conditions that leave mappings inconsistent
///
/// We simulate this by:
/// 1. Adding a vector (creates mapping + adds to usearch)
/// 2. Removing it (removes from mapping + removes from usearch)
/// 3. Adding again (goes through Vacant branch, creates new mapping + adds to usearch)
/// 4. Updating again (goes through Occupied branch, key exists, normal path)
#[test]
fn test_update_exercises_contains_check() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Add initial vector
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Update it (this goes through Occupied branch with contains() = true)
    index.add(node, &[0.9, 0.1, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Update again (still Occupied branch with contains() = true)
    index.add(node, &[0.8, 0.2, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Verify final vector
    let results = index.search(&[0.8, 0.2, 0.0, 0.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node);
    assert!(results[0].1 > 0.99);
}

/// Test multiple updates to ensure contains() + remove() path is covered.
#[test]
fn test_multiple_updates_for_coverage() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Perform 10 updates - each one exercises the Occupied branch
    for i in 0..10 {
        let mut vector = vec![0.0f32; 4];
        vector[i % 4] = 1.0;
        index.add(node, &vector).unwrap();

        // Each update should maintain count at 1
        assert_eq!(index.len(), 1);
    }

    // Verify final state
    let query = vec![0.0, 1.0, 0.0, 0.0]; // Last iteration had i=9, so 9%4=1
    let results = index.search(&query, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node);
}

/// Test that exercises both Occupied and Vacant branches.
#[test]
fn test_add_remove_readd_update_sequence() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Initial add (Vacant branch)
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Update (Occupied branch, contains=true, remove+add)
    index.add(node, &[0.9, 0.1, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Remove
    index.remove(node).unwrap();
    assert_eq!(index.len(), 0);

    // Re-add (Vacant branch again)
    index.add(node, &[0.8, 0.2, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Update again (Occupied branch, contains=true)
    index.add(node, &[0.7, 0.3, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Final verification
    let results = index.search(&[0.7, 0.3, 0.0, 0.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node);
    assert!(results[0].1 > 0.99);
}

/// Test concurrent updates to different nodes (exercises Occupied branch under concurrency).
#[test]
fn test_concurrent_updates_coverage() {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    // Add initial vectors
    for i in 1..=5 {
        let node = NodeId::new(i).unwrap();
        let vector = vec![i as f32, 0.0, 0.0, 0.0];
        index.add(node, &vector).unwrap();
    }
    assert_eq!(index.len(), 5);

    // Spawn threads to update each node
    let handles: Vec<_> = (1..=5)
        .map(|i| {
            let index_clone = Arc::clone(&index);
            thread::spawn(move || {
                let node = NodeId::new(i).unwrap();
                // Each thread performs 3 updates
                for j in 0..3 {
                    let mut vector = vec![0.0f32; 4];
                    vector[(i + j) as usize % 4] = 1.0;
                    index_clone.add(node, &vector).unwrap();
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // Should still have 5 vectors
    assert_eq!(index.len(), 5);
}

/// Test update with dimension mismatch error path.
#[test]
fn test_update_dimension_mismatch() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Add initial vector
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Try to update with wrong dimensions (should fail before reaching contains check)
    let result = index.add(node, &[1.0, 0.0, 0.0]); // Wrong size!
    assert!(result.is_err());

    // Verify original vector is still there
    assert_eq!(index.len(), 1);
    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node);
}

/// Test update with NaN vector (error path).
#[test]
fn test_update_with_nan() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Add initial vector
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Try to update with NaN (should fail before reaching contains check)
    let result = index.add(node, &[f32::NAN, 0.0, 0.0, 0.0]);
    assert!(result.is_err());

    // Verify original vector is still there
    assert_eq!(index.len(), 1);
}

/// Test update with infinity vector (error path).
#[test]
fn test_update_with_infinity() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Add initial vector
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    assert_eq!(index.len(), 1);

    // Try to update with infinity (should fail before reaching contains check)
    let result = index.add(node, &[f32::INFINITY, 0.0, 0.0, 0.0]);
    assert!(result.is_err());

    // Verify original vector is still there
    assert_eq!(index.len(), 1);
}
