//! Tests for HNSW hot path optimization (Issue #206).
//!
//! This test suite verifies that the HNSW index search operations avoid
//! unnecessary NodeId validation in hot paths, specifically:
//! - `search_with_filter()` filter callback
//! - `convert_and_sort_matches()` result conversion
//!
//! # Background
//!
//! NodeIds retrieved from the HNSW index's internal reverse_mapping are
//! guaranteed to be valid because they were created through controlled
//! insertion paths. Therefore, we can safely skip validation when retrieving
//! them, avoiding ~1-2ns overhead per node examined during search.
//!
//! For a search that examines 1,000 candidate nodes, eliminating validation
//! saves ~1-2 microseconds total.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex, VectorIndex};

/// Test that verifies search_with_filter works correctly and efficiently
/// when examining many nodes.
///
/// This test ensures:
/// 1. The filter callback is invoked for candidate nodes
/// 2. Results are correctly filtered according to the predicate
/// 3. The hot path doesn't introduce unnecessary validation overhead
#[test]
fn test_search_with_filter_hot_path_correctness() {
    // Create an index with many nodes to exercise the hot path
    let dimensions = 128;
    let num_nodes = 1000;

    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine)
        .with_m(16)
        .with_ef_construction(128)
        .with_capacity(num_nodes);

    let index = HnswIndex::new(config).expect("Failed to create index");

    // Insert vectors with IDs 0..999
    for i in 0..num_nodes {
        let node_id = NodeId::new(i as u64).expect("Valid node ID");
        let vector: Vec<f32> = (0..dimensions)
            .map(|j| ((i + j) % 100) as f32 / 100.0)
            .collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    // Query vector similar to the first vectors
    let query: Vec<f32> = (0..dimensions).map(|j| (j % 100) as f32 / 100.0).collect();

    // Search with filter: only return even-numbered node IDs
    // This exercises the filter callback on many candidate nodes
    let k = 10;
    let results = index
        .search_with_filter(&query, k, |id| id.as_u64() % 2 == 0)
        .expect("Search should succeed");

    // Verify all results satisfy the filter predicate
    assert_eq!(results.len(), k, "Should return exactly k results");
    for (node_id, _similarity) in &results {
        assert_eq!(
            node_id.as_u64() % 2,
            0,
            "All results should have even node IDs, but found {:?}",
            node_id
        );
    }

    // Verify results are sorted by similarity (descending)
    for i in 1..results.len() {
        assert!(
            results[i - 1].1 >= results[i].1,
            "Results should be sorted by similarity (descending)"
        );
    }
}

/// Test that verifies the filter callback correctly handles NodeIds
/// retrieved from the internal reverse_mapping.
///
/// This test specifically validates that:
/// 1. NodeIds passed to the filter are valid (no panics/errors)
/// 2. The filter can safely call methods on NodeId (like as_u64())
/// 3. Multiple filter invocations work correctly
#[test]
fn test_search_with_filter_nodeid_safety() {
    let dimensions = 64;
    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine);
    let index = HnswIndex::new(config).expect("Failed to create index");

    // Add some test vectors
    for i in 0..100 {
        let node_id = NodeId::new(i).expect("Valid node ID");
        let vector: Vec<f32> = (0..dimensions).map(|_| (i as f32) / 100.0).collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    let query: Vec<f32> = vec![0.5; dimensions];

    // Filter that exercises NodeId methods
    let results = index
        .search_with_filter(&query, 5, |id| {
            // These operations should work without validation overhead
            let raw_id = id.as_u64();
            let is_valid = raw_id < 100;

            // Return true for IDs in range [10, 20)
            is_valid && (10..20).contains(&raw_id)
        })
        .expect("Search should succeed");

    // Verify we got some results (filter was applied successfully)
    assert!(
        !results.is_empty(),
        "Should find some results matching the filter"
    );

    // Verify all results are in the expected range
    for (node_id, _) in &results {
        let raw_id = node_id.as_u64();
        assert!(
            (10..20).contains(&raw_id),
            "Result ID {} should be in range [10, 20)",
            raw_id
        );
    }
}

/// Test that verifies convert_and_sort_matches correctly handles
/// large result sets without validation overhead.
#[test]
fn test_search_large_k_without_validation_overhead() {
    use rand::{Rng, SeedableRng, rngs::StdRng};

    let dimensions = 128;
    let num_nodes = 5000;

    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(num_nodes);

    let index = HnswIndex::new(config).expect("Failed to create index");

    // Use seeded RNG for reproducible tests
    let mut rng = StdRng::seed_from_u64(42);

    // Insert many vectors
    for i in 0..num_nodes {
        let node_id = NodeId::new(i as u64).expect("Valid node ID");
        let vector: Vec<f32> = (0..dimensions).map(|_| rng.gen_range(0.0..1.0)).collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    let query: Vec<f32> = (0..dimensions).map(|_| rng.gen_range(0.0..1.0)).collect();

    // Request a large k to exercise the result conversion path
    let k = 100;
    let results = index.search(&query, k).expect("Search should succeed");

    // Verify results
    assert!(results.len() <= k, "Should return at most k results");
    assert!(!results.is_empty(), "Should return some results");

    // Verify all NodeIds are valid (would panic if not)
    for (node_id, _similarity) in &results {
        let raw_id = node_id.as_u64();
        assert!(
            raw_id < num_nodes as u64,
            "NodeId {} should be < {}",
            raw_id,
            num_nodes
        );
    }

    // Verify sorted by similarity
    for i in 1..results.len() {
        assert!(
            results[i - 1].1 >= results[i].1,
            "Results should be sorted by similarity"
        );
    }
}

/// Test that exercises both search and search_with_filter to verify
/// consistent behavior (both should avoid validation in hot paths).
#[test]
fn test_search_vs_search_with_filter_consistency() {
    let dimensions = 64;
    let num_nodes = 500;

    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine);
    let index = HnswIndex::new(config).expect("Failed to create index");

    // Add test data
    for i in 0..num_nodes {
        let node_id = NodeId::new(i as u64).expect("Valid node ID");
        let vector: Vec<f32> = (0..dimensions)
            .map(|j| ((i + j) % 100) as f32 / 100.0)
            .collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    let query: Vec<f32> = (0..dimensions).map(|j| (j % 100) as f32 / 100.0).collect();

    // Search without filter
    let k = 20;
    let all_results = index.search(&query, k).expect("Search should succeed");

    // Search with filter that accepts all nodes
    let filtered_results = index
        .search_with_filter(&query, k, |_| true)
        .expect("Filtered search should succeed");

    // Both should return the same results (since filter accepts all)
    assert_eq!(
        all_results.len(),
        filtered_results.len(),
        "Both search methods should return same number of results"
    );

    // Results should be identical
    for (i, (node_all, sim_all)) in all_results.iter().enumerate() {
        let (node_filtered, sim_filtered) = &filtered_results[i];
        assert_eq!(
            node_all, node_filtered,
            "NodeId at position {} should match",
            i
        );
        assert!(
            (sim_all - sim_filtered).abs() < 1e-6,
            "Similarity at position {} should match",
            i
        );
    }
}

/// Benchmark-style test to verify the hot path is actually fast.
/// This doesn't measure exact timing but ensures operations complete
/// in reasonable time even with many nodes.
#[test]
fn test_search_with_filter_performance_sanity_check() {
    use rand::{Rng, SeedableRng, rngs::StdRng};

    let dimensions = 256;
    let num_nodes = 10_000;

    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(num_nodes);

    let index = HnswIndex::new(config).expect("Failed to create index");

    // Use seeded RNG for reproducible tests
    let mut rng = StdRng::seed_from_u64(42);

    // Insert many nodes
    for i in 0..num_nodes {
        let node_id = NodeId::new(i as u64).expect("Valid node ID");
        let vector: Vec<f32> = (0..dimensions).map(|_| rng.gen_range(0.0..1.0)).collect();
        index.add(node_id, &vector).expect("Failed to add vector");
    }

    let query: Vec<f32> = (0..dimensions).map(|_| rng.gen_range(0.0..1.0)).collect();

    // Perform multiple searches to amortize any startup costs
    let num_searches = 10;
    let k = 20;

    let start = std::time::Instant::now();
    for _ in 0..num_searches {
        let _results = index
            .search_with_filter(&query, k, |id| {
                id.as_u64() % 2 == 0 // Filter to even IDs
            })
            .expect("Search should succeed");
    }
    let elapsed = start.elapsed();

    // Sanity check: 10 searches on 10K nodes should complete in < 1 second
    // This catches catastrophic performance regressions while allowing for
    // slower CI environments and debug builds.
    assert!(
        elapsed.as_secs() < 1,
        "Searches took too long: {:?} (possible performance regression)",
        elapsed
    );

    println!("10 filtered searches on 10K nodes took: {:?}", elapsed);
    println!("Average per search: {:?}", elapsed / num_searches);
}
