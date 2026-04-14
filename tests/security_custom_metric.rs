use aletheiadb::core::error::VectorError;
use aletheiadb::core::error::{Error, Result};
use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{
    DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder, Quantization, VectorIndex,
};

#[test]
fn test_custom_metric_with_quantization_crash() -> Result<()> {
    // This test attempts to trigger a buffer over-read by using I8 quantization
    // with a custom metric. It should now be prevented by a security check.

    let dims = 1000;

    // We can now use the builder directly since we added the method
    let index_res = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
        .quantization(Quantization::I8)
        .with_custom_metric("test", move |a: &[f32], b: &[f32]| {
            let mut sum = 0.0;
            for i in 0..a.len() {
                sum += a[i] + b[i];
            }
            sum
        })
        .build();

    // Assert that we get an error
    match index_res {
        Ok(_) => panic!("Expected build() to fail due to security check, but it succeeded!"),
        Err(Error::Vector(VectorError::InvalidVector { reason })) => {
            // Verify the error message contains relevant info
            assert!(
                reason.contains("only supported with F32 quantization"),
                "Unexpected error message: {}",
                reason
            );
            assert!(
                reason.contains("memory safety"),
                "Unexpected error message: {}",
                reason
            );
        }
        Err(e) => panic!("Expected InvalidVector error, got: {:?}", e),
    }

    Ok(())
}

#[test]
fn test_load_with_unsafe_config() -> Result<()> {
    // Create a temp file
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_index.usearch");

    // Create a valid index (without custom metric)
    let index = HnswIndexBuilder::new(10, DistanceMetric::Cosine)
        .quantization(Quantization::I8)
        .build()?;

    let node1 = NodeId::new(1).unwrap();
    let vec1 = vec![0.1f32; 10];
    index.add(node1, &vec1)?;

    index.save(&path)?;

    // Try to load it with a dangerous config
    let mut config = HnswConfig::new(10, DistanceMetric::Cosine);
    config = config.with_quantization(Quantization::I8);
    config = config.with_custom_metric("test", |_, _| 0.0);

    let res = HnswIndex::load(&path, config);

    assert!(res.is_err());
    if let Err(Error::Vector(VectorError::InvalidVector { reason })) = res {
        assert!(reason.contains("only supported with F32 quantization"));
    } else {
        panic!("Expected InvalidVector error");
    }

    Ok(())
}
