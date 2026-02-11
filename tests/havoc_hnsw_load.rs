use aletheiadb::core::property::PropertyMap;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex};
use proptest::prelude::*;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn test_hnsw_load_fuzz(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fuzz.index");

        {
            let mut file = File::create(&path).unwrap();
            file.write_all(&bytes).unwrap();
        }

        let config = HnswConfig::new(4, DistanceMetric::Cosine);

        // This should return Error, NOT panic/crash
        let _ = HnswIndex::load(&path, config);
    }

    #[test]
    fn test_property_map_deserialize_fuzz(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        // This should return Error, NOT panic
        let _ = PropertyMap::deserialize(&bytes);
    }
}
