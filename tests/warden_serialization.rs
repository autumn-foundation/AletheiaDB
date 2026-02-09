
use aletheiadb::index::vector::hnsw::HnswConfig;
use aletheiadb::index::vector::{DistanceMetric, Quantization};
use std::io::Cursor;

/// 🔒 Warden Security Regression Test
///
/// Threat: Metadata corruption via incomplete serialization
/// Description: HnswConfig serialization was dropping the `Quantization` setting,
/// effectively reverting any non-default quantization (e.g. I8) to F32 upon reload.
/// This could lead to buffer over-reads if a custom metric wrapper expects F32 pointers
/// but receives I8 pointers from the underlying index (which is correctly I8).
#[test]
fn test_warden_hnsw_config_quantization_persistence() {
    // 1. Create a config with non-default quantization (I8)
    let original = HnswConfig::new(128, DistanceMetric::Cosine)
        .with_quantization(Quantization::I8) // Explicitly set to I8
        .with_m(32)
        .with_ef_construction(200);

    // Verify setup
    assert_eq!(
        original.quantization,
        Quantization::I8,
        "Setup failed: quantization not set"
    );

    // 2. Serialize to buffer
    let mut buffer = Vec::new();
    original
        .serialize_into(&mut buffer)
        .expect("Serialization failed");

    // 3. Deserialize back
    let mut cursor = Cursor::new(buffer);
    let restored = HnswConfig::deserialize_from(&mut cursor).expect("Deserialization failed");

    // 4. Assert correctness (this is expected to FAIL before the fix)
    assert_eq!(
        restored.quantization, original.quantization,
        "SECURITY REGRESSION: Quantization setting lost during serialization! \
         Expected {:?}, got {:?}. This can lead to memory safety violations.",
        original.quantization, restored.quantization
    );

    // Verify other fields still work
    assert_eq!(restored.dimensions, 128);
    assert_eq!(restored.m, 32);
}
