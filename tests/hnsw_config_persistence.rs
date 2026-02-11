use aletheiadb::index::vector::{DistanceMetric, HnswConfig, Quantization, StorageMode};
use proptest::prelude::*;
use std::path::PathBuf;

// 🛡️ Sentry: HNSW Config Serialization Tests
//
// 🎯 Target: `HnswConfig::serialize_into` and `deserialize_from`.
// 💣 Risk: Configuration loss during persistence/recovery, especially with version changes.
// 🧪 Strategy: Property-based testing for round-trip correctness and specific regression tests for edge cases.

proptest! {
    #[test]
    fn test_hnsw_config_roundtrip(
        dimensions in 1usize..4096,
        m in 2usize..64,
        ef_construction in 10usize..500,
        ef_search in 10usize..500,
        capacity in 0usize..100_000,
        metric_idx in 0usize..6,
        quant_idx in 0usize..3,
    ) {
        let metric = match metric_idx {
            0 => DistanceMetric::Cosine,
            1 => DistanceMetric::Euclidean,
            2 => DistanceMetric::DotProduct,
            3 => DistanceMetric::Haversine,
            4 => DistanceMetric::Hamming,
            5 => DistanceMetric::Tanimoto,
            _ => unreachable!(),
        };

        let quantization = match quant_idx {
            0 => Quantization::F32,
            1 => Quantization::F16,
            2 => Quantization::I8,
            _ => unreachable!(),
        };

        // Create config with non-default values and transient fields set
        let config = HnswConfig::new(dimensions, metric)
            .with_m(m)
            .with_ef_construction(ef_construction)
            .with_ef_search(ef_search)
            .with_capacity(capacity)
            .with_quantization(quantization)
            // Transient fields (should not be serialized or should be reset)
            .with_storage(StorageMode::MemoryMapped { path: PathBuf::from("/tmp/test") });

        let mut buffer = Vec::new();
        config.serialize_into(&mut buffer).unwrap();

        let mut reader = &buffer[..];
        let loaded_config = HnswConfig::deserialize_from(&mut reader).unwrap();

        // Check persisted fields
        assert_eq!(loaded_config.dimensions, config.dimensions, "Dimensions mismatch");
        assert_eq!(loaded_config.metric, config.metric, "Metric mismatch");
        assert_eq!(loaded_config.m, config.m, "M mismatch");
        assert_eq!(loaded_config.ef_construction, config.ef_construction, "ef_construction mismatch");
        assert_eq!(loaded_config.ef_search, config.ef_search, "ef_search mismatch");
        assert_eq!(loaded_config.capacity, config.capacity, "Capacity mismatch");
        assert_eq!(loaded_config.quantization, config.quantization, "Quantization mismatch");

        // Verify transient fields are reset to defaults
        assert!(matches!(loaded_config.storage, StorageMode::InMemory), "Storage should reset to InMemory");
        assert!(loaded_config.custom_metric.is_none(), "Custom metric should be None");
    }
}

#[test]
fn test_hnsw_config_custom_metric_handling() {
    // 🧪 Strategy: Verify that having a custom metric doesn't break serialization,
    // and that it is correctly ignored (not persisted).

    let config = HnswConfig::new(128, DistanceMetric::Cosine)
        .with_quantization(Quantization::F32) // Custom metrics require F32
        .with_custom_metric("test_metric", |a, b| {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
        });

    let mut buffer = Vec::new();
    config.serialize_into(&mut buffer).unwrap();

    let mut reader = &buffer[..];
    let loaded_config = HnswConfig::deserialize_from(&mut reader).unwrap();

    // Basic fields preserved
    assert_eq!(loaded_config.dimensions, 128);
    assert_eq!(loaded_config.metric, DistanceMetric::Cosine);

    // Custom metric lost (as expected)
    assert!(loaded_config.custom_metric.is_none());
}

#[test]
fn test_hnsw_config_v1_compatibility() {
    // 🧪 Strategy: Manually construct a V1 buffer (missing quantization byte)
    // and verify it deserializes correctly with default quantization.

    use std::io::Write;

    let mut buffer = Vec::new();

    // V1 fields:
    // dimensions (u64)
    buffer.write_all(&(128u64).to_le_bytes()).unwrap();
    // metric (u8)
    buffer
        .write_all(&[DistanceMetric::Euclidean.to_u8()])
        .unwrap();
    // m (u64)
    buffer.write_all(&(16u64).to_le_bytes()).unwrap();
    // ef_construction (u64)
    buffer.write_all(&(100u64).to_le_bytes()).unwrap();
    // ef_search (u64)
    buffer.write_all(&(50u64).to_le_bytes()).unwrap();
    // capacity (u64)
    buffer.write_all(&(1000u64).to_le_bytes()).unwrap();

    // STOP here. No quantization byte.

    let mut reader = &buffer[..];
    let loaded_config = HnswConfig::deserialize_from(&mut reader).unwrap();

    assert_eq!(loaded_config.dimensions, 128);
    assert_eq!(loaded_config.metric, DistanceMetric::Euclidean);
    assert_eq!(loaded_config.m, 16);
    assert_eq!(loaded_config.ef_construction, 100);
    assert_eq!(loaded_config.ef_search, 50);
    assert_eq!(loaded_config.capacity, 1000);

    // Check fallback to default
    assert_eq!(loaded_config.quantization, Quantization::default());
}
