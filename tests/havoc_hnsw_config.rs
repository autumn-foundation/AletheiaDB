use aletheiadb::index::vector::{HnswConfig, DistanceMetric, Quantization};
use std::io::Cursor;

#[test]
fn test_hnsw_zero_dimensions_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u64.to_le_bytes()); // dimensions: 0
    bytes.push(DistanceMetric::Cosine.to_u8());
    bytes.extend_from_slice(&16u64.to_le_bytes()); // m
    bytes.extend_from_slice(&128u64.to_le_bytes()); // ef_construction
    bytes.extend_from_slice(&64u64.to_le_bytes()); // ef_search
    bytes.extend_from_slice(&1000u64.to_le_bytes()); // capacity
    bytes.push(Quantization::F32.to_u8());

    let mut cursor = Cursor::new(bytes);

    let config_result = HnswConfig::deserialize_from(&mut cursor);

    if let Err(e) = config_result {
        if e.to_string().contains("dimensions must be > 0") {
             return; // Passed
        }
        panic!("Deserialization failed but with unexpected error: {:?}", e);
    }

    panic!("Deserialization SUCCEEDED with 0 dimensions! Security regression.");
}
