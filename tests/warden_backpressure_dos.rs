use std::sync::Arc;
use aletheiadb::storage::wal::ring_buffer::{WalRingBuffer, BackpressureConfig, PendingEntry};
use aletheiadb::storage::wal::LSN;

#[test]
#[should_panic(expected = "Invalid BackpressureConfig")]
fn test_prevent_invalid_config_panic() {
    let config = BackpressureConfig {
        initial_spins: 0, // Invalid!
        max_spins: 100,
        base_sleep_us: 1,
        max_sleep_us: 10,
    };

    // This should panic now, preventing the DoS vulnerability
    let _buf = WalRingBuffer::with_config(2, config);
}

#[test]
fn test_valid_low_latency_config() {
    let config = BackpressureConfig {
        initial_spins: 1, // Valid (minimum)
        max_spins: 100,
        base_sleep_us: 1,
        max_sleep_us: 10,
    };

    let buf = Arc::new(WalRingBuffer::with_config(2, config));

    // Should work normally
    buf.try_append(PendingEntry::new_async(LSN(1), vec![])).unwrap();
    buf.try_append(PendingEntry::new_async(LSN(2), vec![])).unwrap();

    // Buffer full, should return Err immediately or after short backoff (and not hang)
    // 1 -> 2 -> 4 ... -> 100 spins. Should be very fast.
    let result = buf.try_append(PendingEntry::new_async(LSN(3), vec![]));
    assert!(result.is_err());
}
