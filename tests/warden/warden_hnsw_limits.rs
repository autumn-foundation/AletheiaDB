use aletheiadb::core::property::MAX_VECTOR_DIMENSIONS;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, Quantization};
use std::io::Cursor;

#[test]
fn test_hnsw_config_deserialize_limit() {
    let huge_dims = (MAX_VECTOR_DIMENSIONS + 1) as u64;
    let mut buffer = Vec::new();

    // Format: dimensions(8) + metric(1) + m(8) + ef_c(8) + ef_s(8) + cap(8) + quant(1)
    buffer.extend_from_slice(&huge_dims.to_le_bytes()); // dimensions
    buffer.push(DistanceMetric::Cosine.to_u8());
    buffer.extend_from_slice(&16u64.to_le_bytes()); // m
    buffer.extend_from_slice(&128u64.to_le_bytes()); // ef_construction
    buffer.extend_from_slice(&64u64.to_le_bytes()); // ef_search
    buffer.extend_from_slice(&1000u64.to_le_bytes()); // capacity
    buffer.push(Quantization::F32.to_u8()); // quantization

    let mut cursor = Cursor::new(buffer);
    let result = HnswConfig::deserialize_from(&mut cursor);

    assert!(
        result.is_err(),
        "Should fail deserialization with huge dimensions"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("dimensions"),
        "Error should mention dimensions"
    );
    assert!(
        msg.contains("exceeds maximum allowed"),
        "Error should mention limit"
    );
}
