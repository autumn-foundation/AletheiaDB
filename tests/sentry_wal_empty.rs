
#[cfg(test)]
mod reproduction_test {
    use aletheiadb::storage::wal::segment_reader::read_segment;
    use aletheiadb::storage::wal::LSN;
    use tempfile::TempDir;
    use std::fs::File;

    #[test]
    fn test_read_zero_byte_segment_panic() {
        let dir = TempDir::new().unwrap();
        let segment_path = dir.path().join("zero_byte.log");

        // Create empty file
        File::create(&segment_path).unwrap();

        // This should not error/panic
        let result = read_segment(&segment_path, LSN(0));

        match result {
            Ok(entries) => assert!(entries.is_empty(), "Should return empty entries for zero-byte file"),
            Err(e) => panic!("read_segment failed on zero-byte file: {}", e),
        }
    }
}
