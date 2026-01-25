//! Test to understand usearch's behavior when adding with existing keys.
//!
//! This test helps us determine the correct optimization strategy for Issue #207.

use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};

/// Test what happens when we add with the same NodeId without removing first.
///
/// This test will help us understand if usearch supports implicit upsert or if
/// we need to explicitly remove before adding.
#[test]
fn test_usearch_add_without_remove() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();

    // Add initial vector
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    println!("After first add: len = {}", index.len());
    assert_eq!(index.len(), 1);

    // Try adding again WITHOUT removing first
    // This bypasses our current implementation's remove() call
    // to see what usearch does natively.
    //
    // Expected behavior:
    // - If usearch supports upsert: len stays 1, vector is updated
    // - If usearch doesn't support upsert: len becomes 2 (duplicate) or error
    index.add(node, &[0.0, 1.0, 0.0, 0.0]).unwrap();
    println!("After second add: len = {}", index.len());

    // Check the length - this tells us if usearch created a duplicate
    let final_len = index.len();
    println!("Final length: {}", final_len);

    // Search for the second vector
    let results = index.search(&[0.0, 1.0, 0.0, 0.0], 2).unwrap();
    println!("Search results: {:?}", results);

    // The actual behavior will determine our optimization strategy
    if final_len == 1 {
        println!("✓ usearch supports implicit upsert - we can skip remove()!");
    } else {
        println!("✗ usearch does NOT support upsert - we must call remove() first");
        println!("  (This is the expected behavior based on usearch architecture)");
    }
}
