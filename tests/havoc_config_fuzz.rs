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
            // Guard against OOM in CI:
            // Random bytes can produce huge dimensions or capacity values that cause
            // large allocations when building the index. We skip testing build()
            // for these extreme cases as they are likely to be rejected by production
            // limits anyway, but here we want to avoid crashing the test process.
            if config.dimensions > 4096 || config.capacity > 100_000 {
                return Ok(());
            }

            // If deserialization succeeds and config is within reasonable bounds,
            // try to build an index with it.
            let _ = HnswIndexBuilder::from_config(&config).build();
        }
    }
}
