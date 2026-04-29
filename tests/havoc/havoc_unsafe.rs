//! Unsafe memory testing
//!
//! # HAVOC: UNSAFE MEMORY
//! We need to verify that bad data passed to `from_bytes` or memory mapped paths
//! doesn't cause out-of-bounds panics, but rather safe errors.

use aletheiadb::storage::wal::flush_coordinator::*;
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_havoc_flush_coordinator_from_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let _ = SegmentMetadata::from_bytes(&bytes);
    }
}
