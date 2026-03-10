use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex; // Import trait
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, Quantization};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn test_custom_metric_panic_resilience() {
    let panic_trigger = Arc::new(AtomicBool::new(false));
    let panic_trigger_clone = panic_trigger.clone();

    // Use a custom metric that panics when triggered
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("panicker", move |a, b| {
            if panic_trigger_clone.load(Ordering::SeqCst) {
                panic!("Havoc injected panic!");
            }
            // Standard Euclidean distance
            a.iter()
                .zip(b.iter())
                .map(|(x, y)| (x - y).powi(2))
                .sum::<f32>()
                .sqrt()
        })
        .build()
        .expect("Failed to build index");

    // Add some vectors
    for i in 1..=10 {
        let vec = vec![i as f32, 0.0, 0.0, 0.0];
        index.add(NodeId::new(i).unwrap(), &vec).unwrap();
    }

    // 1. Search normally - should work
    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5).unwrap();
    assert!(!results.is_empty());

    // 2. Enable panic mode
    panic_trigger.store(true, Ordering::SeqCst);

    // 3. Search again - should NOT crash/abort
    // The wrapper catches the panic and returns f32::MAX.
    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5);

    // If we survived, test passes.
    println!("Survived panic! Result: {:?}", results);
}

#[test]
fn test_builder_dos_limits() {
    // Verify that we cannot create an index with massive parameters (DoS prevention)

    // ef_construction too large (max 4096)
    let res = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .ef_construction(5000)
        .build();
    assert!(res.is_err());

    // ef_search too large (max 4096)
    let res = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .ef_search(5000)
        .build();
    assert!(res.is_err());
}

#[test]
fn test_import_csr_invalid_id() {
    let label = aletheiadb::core::interning::GLOBAL_INTERNER
        .intern("KNOWS")
        .unwrap();

    // We create a node ID larger than MAX_VALID_ID
    let invalid_id: u64 = aletheiadb::core::id::MAX_VALID_ID + 1;

    let nodes = vec![invalid_id];
    let offsets = vec![0, 1];
    let edge_ids = vec![100];
    let mut edge_map = std::collections::HashMap::default();
    edge_map.insert(
        aletheiadb::core::id::EdgeId::new(100).unwrap(),
        (aletheiadb::core::id::NodeId::new(2).unwrap(), label),
    );

    // This should panic or return an error because it violates the MAX_VALID_ID invariant.
    // If it succeeds, the unsafe transmute bypassed the checks, which is a bug.

    // We expect a panic, since validate_csr_invariants returns a Result, and import_csr unwraps it.
    let result = std::panic::catch_unwind(|| {
        aletheiadb::index::AdjacencyIndex::import_csr(nodes, offsets, edge_ids, &edge_map)
    });

    assert!(
        result.is_err(),
        "Expected import_csr to panic due to invalid NodeId"
    );
}
