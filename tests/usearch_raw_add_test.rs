//! Direct test of usearch's add() behavior with duplicate keys.
//!
//! This test directly manipulates the usearch index to understand if add()
//! with an existing key performs an upsert or creates a duplicate.

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

#[test]
fn test_usearch_raw_duplicate_key_behavior() {
    // Create a usearch index directly (bypassing our wrapper)
    let options = IndexOptions {
        dimensions: 4,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        connectivity: 16,
        expansion_add: 128,
        expansion_search: 64,
        multi: false, // Single vector per key
    };

    let index = Index::new(&options).unwrap();
    index.reserve(1024).unwrap();

    let key: u64 = 42;
    let vector1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let vector2 = vec![0.0f32, 1.0, 0.0, 0.0];

    // Add first vector with key=42
    index.add(key, &vector1).unwrap();
    println!("After first add: size = {}", index.size());
    assert_eq!(index.size(), 1);

    // Add SECOND vector with SAME key=42 (WITHOUT removing first)
    // This is the critical test to understand usearch's behavior
    // Expected: This should FAIL with "Duplicate keys not allowed"
    let result = index.add(key, &vector2);

    // Verify that add() with duplicate key fails
    assert!(
        result.is_err(),
        "Expected add() with duplicate key to fail, but it succeeded"
    );

    let error_msg = result.unwrap_err().to_string();
    println!("Error message: {}", error_msg);

    assert!(
        error_msg.contains("Duplicate keys not allowed"),
        "Expected 'Duplicate keys not allowed' error, got: {}",
        error_msg
    );

    println!("✓ Confirmed: usearch requires remove() before re-adding with same key");
    println!("  This validates our optimization in Issue #207:");
    println!("  We check if key exists before calling remove() to avoid wasteful FFI calls");
}
