use aletheiadb::storage::wal::LSN;
use aletheiadb::storage::wal::segment_reader::read_segment;
use std::fs::File;
use std::io::Write;
use tempfile::TempDir;

#[test]
fn test_fuzz_crash_578de513() {
    // Reproduction for crash discovered by fuzzer
    // Input: R1dBTAEAB1NB/wrh/////////////////0wAAAAEB+cFCuEmdgd5BwcHV0EHBwcH
    // Cause: Index out of bounds in deserialize_node_id

    let crash_data = vec![
        71, 87, 65, 76, 1, 0, 7, 83, 65, 255, 10, 225, 255, 255, 255, 255, 255, 255, 255, 255, 255,
        255, 255, 255, 255, 76, 0, 0, 0, 4, 7, 231, 5, 10, 225, 38, 118, 7, 121, 7, 7, 7, 87, 65,
        7, 7, 7, 7,
    ];

    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("crash.log");

    {
        let mut file = File::create(&segment_path).unwrap();
        file.write_all(&crash_data).unwrap();
    }

    // Should return an error, not panic
    let result = read_segment(&segment_path, LSN(0));
    assert!(result.is_err());
}
