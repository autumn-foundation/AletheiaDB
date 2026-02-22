use aletheiadb::core::error::StorageError;
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::TimeRange;

#[test]
fn test_regression_deserialize_invariants() {
    println!("Testing TimeRange deserialization invariants...");

    // 1. Test start > end
    // Construct bytes for TimeRange: [start: 12 bytes][end: 12 bytes]
    let mut bytes = Vec::new();

    // Start: wallclock=2000, logical=0
    bytes.extend_from_slice(&2000i64.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());

    // End: wallclock=1000, logical=0
    bytes.extend_from_slice(&1000i64.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let result = TimeRange::deserialize(&bytes);
    match result {
        Ok((range, _)) => {
            panic!(
                "CRITICAL: Deserialization should have rejected start > end, but got: {:?}",
                range
            );
        }
        Err(StorageError::CorruptedData(msg)) => {
            println!("Correctly rejected start > end: {}", msg);
            assert!(msg.contains("start 2000.0 > end 1000.0"));
        }
        Err(e) => panic!("Expected CorruptedData error, got: {:?}", e),
    }
}

#[test]
fn test_regression_timestamp_max_variant() {
    // 2. Test TIMESTAMP_MAX with logical > 0
    let mut bytes_max = Vec::new();

    // wallclock: i64::MAX (little endian)
    bytes_max.extend_from_slice(&i64::MAX.to_le_bytes());
    // logical: 1 (little endian)
    bytes_max.extend_from_slice(&1u32.to_le_bytes());

    let result_ts = HybridTimestamp::deserialize(&bytes_max);
    match result_ts {
        Ok((ts, _)) => {
            panic!(
                "CRITICAL: Deserialization should have rejected i64::MAX with logical > 0, but got: {:?}",
                ts
            );
        }
        Err(StorageError::CorruptedData(msg)) => {
            println!("Correctly rejected invalid TIMESTAMP_MAX: {}", msg);
            assert!(msg.contains("non-zero logical counter: 1"));
        }
        Err(e) => panic!("Expected CorruptedData error, got: {:?}", e),
    }
}

#[test]
fn test_regression_duration_panic() {
    // 3. Test duration_micros overflow
    // start: i64::MIN, end: 0
    let very_old = HybridTimestamp::new(i64::MIN, 0).unwrap();
    let zero = HybridTimestamp::new(0, 0).unwrap();

    let range_overflow = TimeRange::new(very_old, zero).unwrap();
    println!("Testing duration_micros panic...");

    let duration = range_overflow.duration_micros();
    match duration {
        Some(d) => {
            println!("Duration: {}", d);
            assert_eq!(d, i64::MAX, "Expected saturated duration i64::MAX");
        }
        None => panic!("Expected finite duration, got None (current)"),
    }
}
