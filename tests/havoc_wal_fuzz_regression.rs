#[test]
fn test_fuzz_crash_update_edge_bounds_check() {
    use aletheiadb::storage::wal::segment_reader::read_segment;
    use aletheiadb::storage::wal::LSN;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Failing input from fuzzer
    let data: Vec<u8> = vec![
        71, 87, 65, 76, 1, // Header: GWAL + Version 1
        1, 47, 65, 43, 0, 4, 4, 4, // LSN (8 bytes)
        4, 4, 1, 1, 1, 1, 1, 80, 0, 3, 1, 1, 1, 1, // Timestamp (12 bytes)
        4, 4, 4, 4, // Checksum (4 bytes)
        4, // OpType: 4 (UpdateEdge)
        4, 0, 0, 0, 0, 0, 0, 0, // EdgeId (8 bytes)
        7, 4, 4, 0, 0, 0, 2, 1, // VersionId (8 bytes)
        58 // Trailing byte? or start of LabelId?
    ];
    // Total len: 5 + 8 + 12 + 4 + 1 + 8 + 8 + 1 = 47?
    // Let's count properly.
    // Header: 5
    // LSN: 8
    // TS: 12
    // CS: 4
    // Op: 1
    // EdgeId: 8
    // VerId: 8
    // Total: 5 + 8 + 12 + 4 + 1 + 8 + 8 = 46.
    // Input len is 49.
    // So 3 extra bytes at end.
    // UpdateEdge tries to read LabelId (4 bytes).
    // It has 3 bytes available (indices 46, 47, 48).
    // It tries to read index 46, 47, 48, 49.
    // Index 49 is out of bounds (len is 49).

    // Note: The input in the log is:
    // [71, 87, 65, 76, 1, 1, 47, 65, 43, 0, 4, 4, 4, 4, 4, 1, 1, 1, 1, 1, 80, 0, 3, 1, 1, 1, 1, 4, 4, 4, 4, 4, 4, 0, 0, 0, 0, 0, 0, 0, 7, 4, 4, 0, 0, 0, 2, 1, 58]
    // Let's rely on the Vec literal.

    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&data).unwrap();

    // This should return an error (CorruptedData or IoError), NOT panic
    let result = read_segment(file.path(), LSN(0));

    // We expect an error because checksum likely won't match, or buffer too short.
    // But definitely NOT a panic.
    assert!(result.is_err());
}
