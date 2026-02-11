use aletheiadb::storage::wal::segment_reader::read_segment;
use aletheiadb::storage::wal::LSN;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn test_wal_segment_reader_oob_panic_regression() {
    let data = [
        71, 87, 65, 76, 1, 0, 0, 0, 71, 87, 65, 76, 0, 71, 87, 76, 65, 82, 0, 0, 0, 76, 255, 255,
        255, 255, 255, 255, 255, 4, 2, 2, 7, 3, 255, 2, 71, 87, 2, 2, 2, 2, 2, 2, 2, 3, 255, 0,
        255,
    ];

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&data).unwrap();

    // This should return an error, not panic
    let result = read_segment(file.path(), LSN(1));
    assert!(result.is_err());
}
