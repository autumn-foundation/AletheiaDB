use aletheiadb::index::vector::{HnswConfig, HnswIndexBuilder};
use proptest::prelude::*;
use std::io::Cursor;

proptest! {
    // Generate random byte arrays of varying lengths
    #[test]
    fn fuzz_config_deserialization(data in prop::collection::vec(any::<u8>(), 0..1024)) {
        let mut cursor = Cursor::new(data);

        // Attempt to deserialize
        if let Ok(config) = HnswConfig::deserialize_from(&mut cursor) {
            // If deserialization succeeds, try to build an index with it
            // This catches cases where deserialization produces a valid struct but with
            // dangerous values (e.g. huge dimensions) that cause panic/OOM on build.
            let _ = HnswIndexBuilder::from_config(&config).build();
        }
    }
}
