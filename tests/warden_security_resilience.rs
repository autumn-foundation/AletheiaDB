use aletheiadb::index::vector::{HnswIndexBuilder, DistanceMetric, Quantization};
use aletheiadb::index::VectorIndex;
use aletheiadb::core::id::NodeId;

#[test]
fn test_metric_panic_resilience() {
    // This test verifies that a panic in a custom metric function does not abort the process.
    // Instead, it should be caught at the FFI boundary, logged, and return a safe fallback value.

    // Define a panicking metric
    let metric_fn = |_: &[f32], _: &[f32]| -> f32 {
        panic!("Malicious metric panic!");
    };

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("panic_metric", metric_fn)
        .build()
        .unwrap();

    // Add nodes
    let n1 = NodeId::new(1).unwrap();
    let n2 = NodeId::new(2).unwrap();

    // Note: add() might also trigger distance calculations during insertion if ef_construction > 0
    // So we should verify add() is also safe.
    let res1 = index.add(n1, &[1.0, 0.0, 0.0, 0.0]);
    let res2 = index.add(n2, &[0.0, 1.0, 0.0, 0.0]);

    // We expect add() to succeed (or at least not crash), even if the metric panics.
    // Useach might return an error if distance calculation fails?
    // No, we return f32::MAX, so usearch sees a valid (large) distance.
    assert!(res1.is_ok(), "add() failed during panic scenario: {:?}", res1.err());
    assert!(res2.is_ok(), "add() failed during panic scenario: {:?}", res2.err());

    // Trigger search which calls the metric
    println!("Starting search with panicking metric...");
    let result = index.search(&[1.0, 0.0, 0.0, 0.0], 2);

    match result {
        Ok(search_res) => {
            println!("Search returned successfully: {:?}", search_res);
            // We expect results, but with very low similarity (due to f32::MAX distance)
            // Cosine similarity = 1.0 - distance = 1.0 - f32::MAX = -inf
            if !search_res.is_empty() {
                assert!(search_res[0].1 < -1.0e30, "Expected extremely low similarity due to panic fallback");
            }
        }
        Err(e) => {
            panic!("Search returned error instead of handling panic gracefully: {}", e);
        }
    }
}
