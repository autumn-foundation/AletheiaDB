#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::wal::segment_reader::parse_entry_at;

    #[test]
    fn test_repro_update_edge_panic() {
        // Failing input from fuzzer:
        // [71, 87, 65, 76, 1, 190, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 40, 1, 1, 1, 1, 1, 71, 87, 65, 76, 0, 4, 0, 0, 0, 1, 40, 1, 1, 1, 1, 1, 71, 87, 65, 76, 76, 0, 0, 0]
        let data = vec![
            71, 87, 65, 76, 1, // Header: GWAL, Ver 1
            190, 0, 0, 0, 0, 0, 0, 0, // LSN: 190
            0, 1, 1, 1, 1, 40, 1, 1, 1, 1, 1, 71, // Timestamp (12 bytes)
            87, 65, 76, 0, // Checksum (4 bytes)
            4, // OpType: 4 (UpdateEdge)
            0, 0, 0, 1, 40, 1, 1, 1, // EdgeId (8 bytes)
            1, 1, 71, 87, 65, 76, 76, 0 // VersionId (8 bytes)
            // Total length: 48 bytes
            // 0, 0 // Missing LabelId (4 bytes)
        ];

        // Ensure we catch the panic
        let result = std::panic::catch_unwind(|| {
            // Offset 5 to skip header
            let _ = parse_entry_at(&data, 5, 1);
        });

        if let Err(err) = result {
            panic!("Panicked as expected: {:?}", err);
        } else {
            // Ideally we want it to return an Err, not panic.
            // But before fix, it panics.
            // After fix, it should return Result::Err.
            let res = parse_entry_at(&data, 5, 1);
            assert!(res.is_err(), "Should return error for truncated buffer, got {:?}", res);
        }
    }
}
