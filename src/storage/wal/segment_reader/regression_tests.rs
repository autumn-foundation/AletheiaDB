use super::*;

#[test]
fn test_repro_fuzz_update_edge_panic() {
    // Failing input from fuzzer:
    // [71, 87, 65, 76, 1, 190, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 40, 1, 1, 1, 1, 1, 71, 87, 65, 76, 0, 4, 0, 0, 0, 1, 40, 1, 1, 1, 1, 1, 71, 87, 65, 76, 76, 0, 0, 0]
    let data = vec![
        71, 87, 65, 76, 1, // Header: GWAL, Ver 1
        190, 0, 0, 0, 0, 0, 0, 0, // LSN: 190
        0, 1, 1, 1, 1, 40, 1, 1, 1, 1, 1, 71, // Timestamp (12 bytes)
        87, 65, 76, 0, // Checksum (4 bytes)
        4, // OpType: 4 (UpdateEdge)
        0, 0, 0, 1, 40, 1, 1, 1, // EdgeId (8 bytes)
        1, 1, 71, 87, 65, 76, 76,
        0, // VersionId (8 bytes)
           // Total length: 48 bytes
           // Missing LabelId (4 bytes) required for Ver 1
    ];

    // Offset 5 to skip header
    let result = parse_entry_at(&data, 5, 1);

    // Before fix: Panics with index out of bounds
    // After fix: Returns Error
    assert!(
        result.is_err(),
        "Should return error for truncated buffer, got {:?}",
        result
    );
}
