use aletheiadb::index::vector::{DistanceMetric, HnswConfig, Quantization, StorageMode};
use aletheiadb::core::error::Result;
use std::io::Cursor;

#[test]
fn test_config_serialization_round_trip() -> Result<()> {
    // Create a config with non-default values, especially quantization
    let original = HnswConfig::new(128, DistanceMetric::Cosine)
        .with_m(32)
        .with_ef_construction(200)
        .with_ef_search(100)
        .with_capacity(5000)
        .with_quantization(Quantization::F16) // The critical part
        .with_storage(StorageMode::InMemory); // Storage mode is not serialized, but quantization should be

    // Serialize to buffer
    let mut buffer = Vec::new();
    original.serialize_into(&mut buffer)?;

    // Deserialize from buffer
    let mut cursor = Cursor::new(buffer);
    let deserialized = HnswConfig::deserialize_from(&mut cursor)?;

    // Assert equality
    assert_eq!(original.dimensions, deserialized.dimensions);
    assert_eq!(original.metric, deserialized.metric);
    assert_eq!(original.m, deserialized.m);
    assert_eq!(original.ef_construction, deserialized.ef_construction);
    assert_eq!(original.ef_search, deserialized.ef_search);
    assert_eq!(original.capacity, deserialized.capacity);

    // This assertion is expected to fail currently
    assert_eq!(
        original.quantization, deserialized.quantization,
        "Quantization should be preserved after round-trip serialization"
    );

    Ok(())
}
