use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::MAX_VECTOR_DIMENSIONS;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndex, HnswIndexBuilder};
use tempfile::tempdir;

#[test]
fn test_hnsw_max_dimensions_load() {
    // 🎯 Target: HnswIndex::validate_metadata boundary check
    // 💣 Risk: Off-by-one error (replace > with >=) causing rejection of MAX_VECTOR_DIMENSIONS
    // 🧪 Strategy: Create an index with exactly MAX_VECTOR_DIMENSIONS, save, and load.

    let dir = tempdir().unwrap();
    let path = dir.path().join("max_dims.index");

    let dims = MAX_VECTOR_DIMENSIONS;

    // 1. Build index with max dims
    let index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
        .build()
        .expect("Should support MAX_VECTOR_DIMENSIONS");

    // 2. Add a vector to ensure mappings are written
    let vec = vec![0.0f32; dims];
    index.add(NodeId::new(1).unwrap(), &vec).unwrap();

    // 3. Save
    index.save(&path).expect("Save should succeed");

    // 4. Load
    let config_template = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
        .build()
        .unwrap()
        .config();

    let loaded = HnswIndex::load(&path, config_template);

    assert!(
        loaded.is_ok(),
        "Should successfully load index with MAX_VECTOR_DIMENSIONS"
    );
    let loaded_index = loaded.unwrap();
    assert_eq!(loaded_index.config().dimensions, dims);
}
