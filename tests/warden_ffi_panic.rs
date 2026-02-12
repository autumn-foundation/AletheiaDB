use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};

#[test]
fn test_ffi_panic_in_custom_metric() {
    // This test attempts to panic inside a custom metric callback invoked by usearch (C++).
    // If unwinding crosses the FFI boundary, this is UB and might cause a hard crash/abort.
    //
    // The fix should catch the panic inside the callback, log an error, and return f32::MAX.
    // The process should NOT abort, and the operation should complete (likely successfully from the caller's perspective).

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .with_custom_metric("panic_metric", |_a, _b| {
            panic!("Panic inside FFI callback!");
        })
        .build()
        .expect("Failed to build index");

    let node1 = NodeId::new(1).unwrap();

    // First node might not trigger metric if there are no comparisons
    index
        .add(node1, &[1.0, 0.0, 0.0, 0.0])
        .expect("Failed to add node1");

    let node2 = NodeId::new(2).unwrap();

    // This add should trigger distance calculation against node1.
    // The panic inside the metric should be caught, printed to stderr, and ignored.
    let result = index.add(node2, &[0.0, 1.0, 0.0, 0.0]);

    // We expect success because the index swallows the panic and returns MAX distance.
    assert!(result.is_ok(), "Operation failed: {:?}", result.err());

    println!("Test passed: Panic was caught and process did not abort.");
}
