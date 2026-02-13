use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, Quantization};

#[test]
fn test_ffi_panic_in_custom_metric() {
    let metric_fn = |_: &[f32], _: &[f32]| -> f32 {
        panic!("Intentionally panicking in custom metric");
    };

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("panicking_metric", metric_fn)
        .build()
        .expect("Failed to build index");

    let node1 = NodeId::new(1).unwrap();
    index
        .add(node1, &[1.0, 0.0, 0.0, 0.0])
        .expect("Failed to add vector");

    // This search should trigger the panic inside the metric
    let _ = index.search(&[1.0, 0.0, 0.0, 0.0], 1);
}
