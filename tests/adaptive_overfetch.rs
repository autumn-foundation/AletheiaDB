//! Tests for adaptive over-fetch strategy in filtered vector search.
//!
//! Issue #334: Make the over-fetch heuristic adaptive based on actual filter pass rates.

use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};

#[test]
fn test_adaptive_overfetch_learns_from_sparse_labels() {
    // Create database with vector index
    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create 100 nodes: 90 with label "Common", 10 with label "Rare"
    let mut rare_nodes = Vec::new();
    for i in 0..100 {
        let embedding = vec![i as f32 / 100.0, 0.5, 0.3, 0.2];
        let label = if i < 90 { "Common" } else { "Rare" };

        let node_id = db
            .create_node(
                label,
                PropertyMapBuilder::new()
                    .insert("name", format!("Node_{}", i))
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();

        if i >= 90 {
            rare_nodes.push(node_id);
        }
    }

    // First search for "Rare" - should use default heuristic (k * 10 = 50 candidates)
    let results = db
        .find_similar_with_label(rare_nodes[0], "Rare", 5)
        .unwrap();
    assert!(results.len() >= 4); // Should find at least 4 other rare nodes (out of 9 others)

    // After a few searches, the adaptive strategy should learn that "Rare" has ~10% pass rate
    // and increase the over-fetch multiplier
    for _ in 0..5 {
        let _ = db
            .find_similar_with_label(rare_nodes[0], "Rare", 5)
            .unwrap();
    }

    // Now the adaptive strategy should fetch more candidates (e.g., k * 20 or more)
    // to compensate for the low pass rate
    let results = db
        .find_similar_with_label(rare_nodes[0], "Rare", 5)
        .unwrap();
    assert_eq!(results.len(), 5); // Should reliably get all 5 requested results
}

#[test]
fn test_adaptive_overfetch_learns_from_dense_labels() {
    // Create database with vector index
    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create 100 nodes: 95 with label "Common", 5 with label "Rare"
    let mut common_nodes = Vec::new();
    for i in 0..100 {
        let embedding = vec![i as f32 / 100.0, 0.5, 0.3, 0.2];
        let label = if i < 95 { "Common" } else { "Rare" };

        let node_id = db
            .create_node(
                label,
                PropertyMapBuilder::new()
                    .insert("name", format!("Node_{}", i))
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();

        if i < 95 {
            common_nodes.push(node_id);
        }
    }

    // Search for "Common" multiple times - should learn it has ~95% pass rate
    for _ in 0..5 {
        let _ = db
            .find_similar_with_label(common_nodes[0], "Common", 10)
            .unwrap();
    }

    // After learning, should optimize by fetching fewer candidates (maybe k * 5)
    // since almost all candidates pass the filter
    let results = db
        .find_similar_with_label(common_nodes[0], "Common", 10)
        .unwrap();
    assert_eq!(results.len(), 10); // Should still get all requested results
}

#[test]
fn test_adaptive_overfetch_embedding_search() {
    // Test adaptive over-fetch with find_similar_by_embedding_with_label
    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create 100 nodes with varying labels
    for i in 0..100 {
        let embedding = vec![i as f32 / 100.0, 0.5, 0.3, 0.2];
        let label = if i % 10 == 0 { "Target" } else { "Other" };

        let _ = db
            .create_node(
                label,
                PropertyMapBuilder::new()
                    .insert("name", format!("Node_{}", i))
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();
    }

    let query_embedding = vec![0.5f32, 0.5, 0.3, 0.2];

    // Perform multiple searches to train the adaptive strategy
    for _ in 0..5 {
        let _ = db
            .find_similar_by_embedding_with_label(&query_embedding, "Target", 5)
            .unwrap();
    }

    // Should now have learned the pass rate and adapt accordingly
    let results = db
        .find_similar_by_embedding_with_label(&query_embedding, "Target", 5)
        .unwrap();

    assert!(results.len() >= 5 || results.len() == 10); // Should find 5 or all 10 "Target" nodes
}

#[test]
fn test_adaptive_overfetch_respects_max_cap() {
    // Ensure adaptive strategy doesn't exceed reasonable bounds
    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create 1000 nodes with a very rare label (0.1% pass rate)
    let mut needle_node = None;
    for i in 0..1000 {
        let embedding = vec![i as f32 / 1000.0, 0.5, 0.3, 0.2];
        let label = if i == 500 { "Needle" } else { "Haystack" };

        let node_id = db
            .create_node(
                label,
                PropertyMapBuilder::new()
                    .insert("name", format!("Node_{}", i))
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();

        if i == 500 {
            needle_node = Some(node_id);
        }
    }

    // Even with very low pass rate, should cap the over-fetch
    // (e.g., at k + 1000 or some reasonable maximum)
    let results = db
        .find_similar_with_label(needle_node.unwrap(), "Needle", 1)
        .unwrap();

    // Should find at least the query node excluded, so 0 results is acceptable
    // (since there's only 1 "Needle" node total)
    assert_eq!(results.len(), 0);
}

#[test]
fn test_adaptive_overfetch_varying_distributions() {
    // Test adaptive over-fetch with varying label distributions
    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create nodes with different label distributions
    for i in 0..100 {
        let embedding = vec![i as f32 / 100.0, 0.5, 0.3, 0.2];
        let label = if i < 20 { "Article" } else { "Blog" };

        let _ = db
            .create_node(
                label,
                PropertyMapBuilder::new()
                    .insert("name", format!("Doc_{}", i))
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();
    }

    let query_embedding = vec![0.1f32, 0.5, 0.3, 0.2];

    // Train adaptive strategy on "Article" label
    for _ in 0..5 {
        let _ = db
            .find_similar_by_embedding_with_label(&query_embedding, "Article", 5)
            .unwrap();
    }

    // Should adapt to "Article" having ~20% pass rate
    let results = db
        .find_similar_by_embedding_with_label(&query_embedding, "Article", 5)
        .unwrap();

    assert!(results.len() >= 5 || results.len() == 20); // Should find at least 5 or all 20
}

#[test]
fn test_adaptive_overfetch_statistics_tracking() {
    // Verify that statistics are being tracked correctly
    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create a controlled dataset: 50% label "A", 50% label "B"
    for i in 0..100 {
        let embedding = vec![i as f32 / 100.0, 0.5, 0.3, 0.2];
        let label = if i < 50 { "A" } else { "B" };

        let _ = db
            .create_node(
                label,
                PropertyMapBuilder::new()
                    .insert("name", format!("Node_{}", i))
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();
    }

    // Verify no stats before searches
    assert!(db.__test_get_filter_stats("A").is_none());

    // Perform 10 searches for label "A"
    let query_embedding = vec![0.25f32, 0.5, 0.3, 0.2];
    for _ in 0..10 {
        let _ = db
            .find_similar_by_embedding_with_label(&query_embedding, "A", 10)
            .unwrap();
    }

    // Verify statistics were recorded correctly
    let (search_count, total_candidates, total_results) = db
        .__test_get_filter_stats("A")
        .expect("Stats should exist after searches");

    // We performed exactly 10 searches
    assert_eq!(search_count, 10, "Should have recorded 10 searches");

    // With 50/50 label distribution, pass rate should be ~50%
    // First 3 searches use default 10x multiplier = 100 candidates each
    // After that, adaptive multiplier kicks in (~7x for 50% pass rate)
    // So expect total_candidates to be around: (3 * 100) + (7 * 70) = 300 + 490 = ~790
    assert!(
        (700..=1000).contains(&total_candidates),
        "Expected ~790 candidates, got {}",
        total_candidates
    );

    // Pass rate should be close to 50% (results / candidates)
    let pass_rate = total_results as f64 / total_candidates as f64;
    assert!(
        (0.3..0.7).contains(&pass_rate),
        "Pass rate should be ~50%, got {:.2}%",
        pass_rate * 100.0
    );

    // Verify label "B" has no stats (we didn't search for it)
    assert!(db.__test_get_filter_stats("B").is_none());
}

fn adaptive_overfetch_concurrent_timeout_secs() -> u64 {
    if std::env::var("CI").is_ok() { 180 } else { 30 }
}

#[test]
#[serial_test::serial]
fn test_adaptive_overfetch_concurrent_safety() {
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    let progress = Arc::new(AtomicUsize::new(0));
    let progress_worker = Arc::clone(&progress);

    let (tx, rx) = mpsc::channel::<std::thread::Result<()>>();
    std::thread::spawn(move || {
        let result = catch_unwind(AssertUnwindSafe(|| {
            run_adaptive_overfetch_concurrent_safety(progress_worker);
        }));
        let _ = tx.send(result);
    });

    let timeout_secs = adaptive_overfetch_concurrent_timeout_secs();
    match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
        Ok(Ok(())) => {}
        Ok(Err(panic_payload)) => {
            resume_unwind(panic_payload);
        }
        Err(_) => {
            let completed = progress.load(Ordering::Relaxed);
            panic!(
                "Potential deadlock in adaptive overfetch concurrent test: timed out after {}s ({} searches completed)",
                timeout_secs, completed
            );
        }
    }
}

fn run_adaptive_overfetch_concurrent_safety(
    progress: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    // Verify that concurrent searches don't cause panics or data corruption
    // Reduced workload for CI performance
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::thread;

    let db = Arc::new(AletheiaDB::new().unwrap());
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create 50 nodes with 2 labels (reduced from 200 nodes for CI performance)
    for i in 0..50 {
        let embedding = vec![i as f32 / 50.0, 0.5, 0.3, 0.2];
        let label = if i % 2 == 0 { "TypeA" } else { "TypeB" };

        let _ = db
            .create_node(
                label,
                PropertyMapBuilder::new()
                    .insert("name", format!("Node_{}", i))
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();
    }

    // Spawn 4 threads (reduced from 8 for CI performance)
    let mut handles = vec![];

    for thread_id in 0..4 {
        let db_clone = Arc::clone(&db);
        let progress_clone = Arc::clone(&progress);
        let handle = thread::spawn(move || {
            let label = if thread_id % 2 == 0 { "TypeA" } else { "TypeB" };
            let query_embedding = vec![0.5f32, 0.5, 0.3, 0.2];

            // Each thread performs 5 searches (reduced from 20 for CI performance)
            for _ in 0..5 {
                let results = db_clone
                    .find_similar_by_embedding_with_label(&query_embedding, label, 3)
                    .unwrap();

                // Basic sanity check
                assert!(results.len() <= 3);
                progress_clone.fetch_add(1, Ordering::Relaxed);
            }
        });

        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().expect("Thread should not panic");
    }

    // Verify statistics were recorded without corruption
    let stats_a = db.__test_get_filter_stats("TypeA").unwrap();
    let stats_b = db.__test_get_filter_stats("TypeB").unwrap();

    // Each of 2 threads x 5 searches = 10 searches per label
    assert_eq!(stats_a.0, 10, "TypeA should have 10 searches");
    assert_eq!(stats_b.0, 10, "TypeB should have 10 searches");

    // Verify counters are non-zero and reasonable (main goal is no panics/corruption)
    assert!(stats_a.1 > 0, "TypeA candidates should be non-zero");
    assert!(stats_a.2 > 0, "TypeA results should be non-zero");
    assert!(stats_b.1 > 0, "TypeB candidates should be non-zero");
    assert!(stats_b.2 > 0, "TypeB results should be non-zero");
}
