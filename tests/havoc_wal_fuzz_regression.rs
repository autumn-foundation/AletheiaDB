#[test]
fn test_repro_fuzz_crash_oob_index() {
    // 👺 Havoc: "Input string with length 0xFFFFFF caused buffer overflow."
    // Crash input from CI failure:
    // [71, 87, 65, 76, 1, 71, 87, 63, 76, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 177, 0, 71, 0, 14, 76, 0, 4, 5, 10, 0, 126, 126, 126, 3, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255]

    use aletheiadb::storage::wal::segment_reader::parse_entry_at;

    let crash_input = vec![
        71, 87, 65, 76, 1, 71, 87, 63, 76, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        177, 0, 71, 0, 14, 76, 0, 4, 5, 10, 0, 126, 126, 126, 3, 0, 0, 0, 0, 0,
        0, 0, 255, 255, 255
    ];

    // This should return an Err, not panic
    let result = parse_entry_at(&crash_input, 5, 1);

    // We expect it to be an error (CorruptedData), but NOT a panic
    assert!(result.is_err());
}
