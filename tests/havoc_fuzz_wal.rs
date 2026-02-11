use aletheiadb::storage::wal::segment_reader::read_segment;
use aletheiadb::storage::wal::LSN;
use std::io::Write;
use tempfile::NamedTempFile;
use proptest::prelude::*;

proptest! {
    #[test]
    fn fuzz_read_segment(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();
        let path = file.path();

        // Should not panic
        let _ = read_segment(path, LSN(0));
    }
}
