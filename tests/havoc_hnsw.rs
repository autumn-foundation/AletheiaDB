use aletheiadb::index::vector::HnswConfig;
use proptest::prelude::*;
use std::io::Cursor;

proptest! {
    #[test]
    fn fuzz_hnsw_config_deserialization(data in any::<Vec<u8>>()) {
        let mut cursor = Cursor::new(data);
        // We don't care about the result (Ok or Err), we just want to ensure it doesn't panic.
        let _ = HnswConfig::deserialize_from(&mut cursor);
    }
}
