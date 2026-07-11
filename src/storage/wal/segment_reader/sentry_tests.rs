use super::*;
use std::fs::File;
use std::io::Write;
use tempfile::TempDir;

#[test]
fn test_read_segment_exactly_max_size_allowed() {
    // 🛡️ Sentry Test: Verify read_segment allows file of exactly MAX_SEGMENT_SIZE.
    // This targets mutants that change `>` to `>=`.
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("max_size.log");

    let mut file = File::create(&segment_path).unwrap();

    // Write header
    file.write_all(&WAL_MAGIC).unwrap();
    file.write_all(&[WAL_VERSION]).unwrap();

    // Seek to exact MAX_SEGMENT_SIZE (sparse file)
    file.set_len(MAX_SEGMENT_SIZE).unwrap();

    drop(file);

    // Read segment - should succeed (return empty entries or corruption error, but NOT "too large").
    let result = read_segment(&segment_path, LSN(1));

    match result {
        Ok(_) => {
            // Success is fine (e.g. if sparse zeros are skipped or interpreted as empty)
        }
        Err(e) => {
            let msg = e.to_string();
            assert!(
                !msg.contains("too large"),
                "Should not reject max size file. Error was: {}",
                msg
            );
        }
    }
}

#[test]
fn test_read_segment_header_only() {
    // 🛡️ Sentry Test: Verify read_segment handles file with ONLY header (5 bytes).
    // This targets mutants that change `>=` to `>` in header size check.
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("header_only.log");

    let mut file = File::create(&segment_path).unwrap();
    file.write_all(&WAL_MAGIC).unwrap();
    file.write_all(&[WAL_VERSION]).unwrap();
    // Total size = 5 bytes.
    drop(file);

    let result = read_segment(&segment_path, LSN(1));

    assert!(result.is_ok(), "Should accept header-only segment");
    assert!(result.unwrap().is_empty());
}

#[test]
fn test_parse_entry_at_exact_header_size() {
    // 🛡️ Sentry Test: Verify parse_entry_at behavior with exactly 24 bytes (header size).
    // Targets `>` vs `>=` in `if current_offset.checked_add(24)? > buffer.len()`.

    // 24 bytes buffer
    let buffer = vec![0u8; 24];

    let result = parse_entry_at(&buffer, 0, WAL_VERSION);

    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    // We expect it to pass the first check (24 !> 24) and fail the op-type check.
    assert!(
        msg.contains("operation type"),
        "Should fail at op type check, not header check. Got: {}",
        msg
    );
}

#[test]
fn test_parse_entry_at_exact_header_and_op_type() {
    // 🛡️ Sentry Test: Verify parse_entry_at behavior with exactly 25 bytes (header + op type).
    // Targets `>` vs `>=` in `if current_offset >= buffer.len()`.

    let mut buffer = vec![0u8; 25];
    // LSN=0, TS=0, Checksum=0.
    // OpType = 255 (Unknown) at index 24.
    buffer[24] = 255;

    let result = parse_entry_at(&buffer, 0, WAL_VERSION);

    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("Unknown WAL operation type"),
        "Should read op type and fail validation. Got: {}",
        msg
    );
}

#[test]
fn test_bolt_pre_allocate_segment_capacity() {
    use std::io::Write;
    let dir = tempfile::TempDir::new().unwrap();
    let file_path = dir.path().join("1.log");
    let mut file = std::fs::File::create(&file_path).unwrap();

    // Write magic header and some dummy data to make buffer large enough
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&super::WAL_MAGIC);
    buffer.push(super::WAL_VERSION); // Version

    // Add padding to make the file size larger (e.g., 1024 bytes)
    buffer.extend(vec![0; 1024 - buffer.len()]);
    file.write_all(&buffer).unwrap();
    file.sync_all().unwrap();

    let entries = read_segment(&file_path, crate::storage::LSN(1)).unwrap();

    // 1024 / 128 = 8. Since we expect capacity_hint = buffer.len() / 128
    assert!(
        entries.capacity() >= 8,
        "⚡ Bolt: Vector should be pre-allocated with capacity based on file size. Capacity was {}",
        entries.capacity()
    );
}
