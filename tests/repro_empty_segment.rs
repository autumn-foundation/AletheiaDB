use aletheiadb::storage::wal::LSN;
use aletheiadb::storage::wal::segment_reader::read_segment;
use std::fs::File;
use tempfile::tempdir;

#[test]
fn test_read_segment_zero_byte_file() {
    let dir = tempdir().unwrap();
    let segment_path = dir.path().join("zero_byte.log");

    // Create a zero-byte file
    File::create(&segment_path).unwrap();

    // Attempt to read the segment
    // This should return Ok(Vec::new()) because empty files are valid (start of segment creation)
    // but mmap usually fails on empty files with EINVAL.
    // We expect this to fail currently if unhandled.
    let result = read_segment(&segment_path, LSN(0));

    // If it fails with "Invalid argument" (EINVAL) or similar, it's a bug.
    // If it succeeds, then maybe mmap handles it on this platform, but we should ensure portability.
    match result {
        Ok(entries) => assert!(entries.is_empty(), "Should be empty"),
        Err(e) => panic!("Failed to read zero-byte segment: {}", e),
    }
}

#[test]
fn test_read_segment_header_only_file() {
    use std::io::Write;
    let dir = tempdir().unwrap();
    let segment_path = dir.path().join("header_only.log");

    let mut file = File::create(&segment_path).unwrap();
    file.write_all(b"GWAL").unwrap(); // Magic
    file.write_all(&[1]).unwrap(); // Version 1

    let result = read_segment(&segment_path, LSN(0));

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}
