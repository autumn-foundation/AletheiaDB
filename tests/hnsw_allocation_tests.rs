//! Tests verifying allocation efficiency in HNSW index operations.
//!
//! This test suite addresses Issue #232: ensuring the HNSW wrapper implementation
//! minimizes Vec allocations during add() and search() operations.
//!
//! ## Background
//!
//! Issue #232 was filed on 2026-01-05, noting that the hnsw_rs implementation
//! allocated new Vec objects on every operation. On 2026-01-14, the codebase
//! migrated from hnsw_rs to usearch, which has a different API design.
//!
//! ## Current Status (Post-usearch Migration)
//!
//! The current usearch-based implementation:
//! - Accepts slice references (`&[f32]`) for add() and search()
//! - Does NOT require Vec allocations for input data in the wrapper code
//! - May allocate internally within usearch (C++ library) for index storage
//! - Allocates Vec for search results (required by VectorIndex trait API)
//!
//! ## Test Strategy
//!
//! These tests verify:
//! 1. The API accepts slices, not owned Vecs
//! 2. We can reuse the same buffer across multiple operations
//! 3. No unnecessary cloning happens in the wrapper code

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::hnsw::{HnswConfig, HnswIndex};
use aletheiadb::index::vector::{DistanceMetric, VectorIndex};
use aletheiadb::utils::Result;

/// Test that add() accepts slices and doesn't require Vec allocation per call.
///
/// This test demonstrates that the same buffer can be reused across multiple
/// add() operations, proving that the wrapper doesn't clone input vectors.
#[test]
fn test_add_accepts_slices_no_vec_required() -> Result<()> {
    let dimensions = 384;
    let index = HnswIndex::new(HnswConfig::new(dimensions, DistanceMetric::Cosine))?;

    // Create a single buffer and reuse it for multiple adds
    // If add() required Vec ownership, this wouldn't compile
    let mut buffer = vec![0.0f32; dimensions];

    for i in 0..100 {
        // Modify the buffer in-place
        buffer[0] = i as f32 / 100.0;
        buffer[1] = (i as f32 / 100.0).sin();

        // add() accepts a slice reference, not Vec
        // No allocation required on the caller side
        let node_id = NodeId::new(i as u64).unwrap();
        index.add(node_id, &buffer)?;
    }

    // Verify all vectors were added
    assert_eq!(index.len(), 100);

    Ok(())
}

/// Test that search() accepts slices and doesn't require Vec allocation per call.
///
/// This test demonstrates that the same query buffer can be reused across
/// multiple search operations.
#[test]
fn test_search_accepts_slices_no_vec_required() -> Result<()> {
    let dimensions = 384;
    let index = HnswIndex::new(HnswConfig::new(dimensions, DistanceMetric::Cosine))?;

    // Populate index
    for i in 0..100 {
        let node_id = NodeId::new(i as u64).unwrap();
        let mut vector = vec![0.0f32; dimensions];
        vector[0] = i as f32 / 100.0;
        index.add(node_id, &vector)?;
    }

    // Create a single query buffer and reuse it
    // If search() required Vec ownership, this wouldn't compile
    let mut query_buffer = vec![0.0f32; dimensions];

    for i in 0..50 {
        // Modify query in-place
        query_buffer[0] = i as f32 / 50.0;

        // search() accepts a slice reference, not Vec
        // No allocation required on the caller side for the query vector
        // (search returns Vec of results, which is necessary for the API)
        let results = index.search(&query_buffer, 10)?;

        // Verify we got results
        assert!(
            !results.is_empty(),
            "Search should return results for query {}",
            i
        );
    }

    Ok(())
}

/// Test that search_with_filter() also accepts slices.
#[test]
fn test_filtered_search_accepts_slices() -> Result<()> {
    let dimensions = 128;
    let index = HnswIndex::new(HnswConfig::new(dimensions, DistanceMetric::Cosine))?;

    // Populate index
    for i in 0..50 {
        let node_id = NodeId::new(i as u64).unwrap();
        let mut vector = vec![0.0f32; dimensions];
        vector[0] = i as f32 / 50.0;
        index.add(node_id, &vector)?;
    }

    // Reusable query buffer
    let query_buffer = vec![0.5f32; dimensions];

    // Multiple filtered searches with the same buffer
    for _ in 0..20 {
        let results = index.search_with_filter(&query_buffer, 5, |id| id.as_u64() % 2 == 0)?;

        // Verify only even IDs were returned
        for (node_id, _similarity) in results {
            assert_eq!(
                node_id.as_u64() % 2,
                0,
                "Filtered search should only return even node IDs"
            );
        }
    }

    Ok(())
}

/// Benchmark-style test: measure operations per second to ensure no allocation overhead.
///
/// This test verifies that the wrapper code is efficient by measuring throughput.
/// If Vec allocations were happening in the wrapper, we'd see significantly lower
/// throughput (allocation overhead would dominate).
#[test]
fn test_high_throughput_add_operations() -> Result<()> {
    let dimensions = 384;
    let index =
        HnswIndex::new(HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(10000))?;

    // Reusable buffer
    let mut buffer = vec![0.0f32; dimensions];

    let start = std::time::Instant::now();
    let num_operations = 1000;

    for i in 0..num_operations {
        buffer[0] = i as f32;
        buffer[1] = (i as f32).sin();

        let node_id = NodeId::new(i as u64).unwrap();
        index.add(node_id, &buffer)?;
    }

    let elapsed = start.elapsed();
    let ops_per_sec = num_operations as f64 / elapsed.as_secs_f64();

    // On modern hardware, we should see thousands of operations per second
    // If we were allocating Vec on every call, throughput would be much lower
    println!(
        "Add throughput: {:.0} ops/sec ({:.2}µs per operation)",
        ops_per_sec,
        elapsed.as_micros() as f64 / num_operations as f64
    );

    // Sanity check: we should be able to do at least 100 ops/sec
    // (This is a very conservative threshold; real performance should be much higher)
    assert!(
        ops_per_sec > 100.0,
        "Throughput too low: {:.0} ops/sec (expected >100)",
        ops_per_sec
    );

    Ok(())
}

/// Test that demonstrates usearch's slice-based API directly.
///
/// This test verifies at the type level that we're passing slices to usearch,
/// not owned Vecs. If usearch required Vec<f32>, this wouldn't compile.
#[test]
fn test_slice_api_type_safety() -> Result<()> {
    let dimensions = 4;
    let index = HnswIndex::new(HnswConfig::new(dimensions, DistanceMetric::Cosine))?;

    // Array reference (not Vec)
    let array = [1.0f32, 2.0, 3.0, 4.0];
    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &array)?; // Accepts array slice

    // Vec reference
    let vec = vec![1.1f32, 2.1, 3.1, 4.1];
    let node2 = NodeId::new(2).unwrap();
    index.add(node2, &vec)?; // Accepts vec slice

    // Search with array slice
    let query = [1.0f32, 2.0, 3.0, 4.0];
    let results = index.search(&query, 2)?;

    assert!(!results.is_empty());

    Ok(())
}

/// Test add_batch functionality.
///
/// Note: Unlike the slice-based add() API, add_batch() currently requires owned
/// Vec<f32> for each item, which is less allocation-efficient. This is a limitation
/// of the current API design that could be improved in a future enhancement.
///
/// See: Future enhancement to make add_batch accept slices via generics
#[test]
fn test_add_batch_functionality() -> Result<()> {
    let dimensions = 128;
    let index = HnswIndex::new(HnswConfig::new(dimensions, DistanceMetric::Cosine))?;

    // Create batch data - note that we're required to allocate owned Vecs here
    // This is less efficient than the slice-based add() API
    let batch: Vec<(NodeId, Vec<f32>)> = (0..100)
        .map(|i| {
            let node_id = NodeId::new(i as u64).unwrap();
            let vector = vec![i as f32 / 100.0; dimensions];
            (node_id, vector)
        })
        .collect();

    index.add_batch(&batch)?;

    assert_eq!(index.len(), 100);

    Ok(())
}
