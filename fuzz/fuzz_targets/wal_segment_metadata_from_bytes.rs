#![no_main]

use libfuzzer_sys::fuzz_target;

use aletheiadb::storage::wal::flush_coordinator::SegmentMetadata;

fuzz_target!(|data: &[u8]| {
    // Parsing must be panic-free for arbitrary bytes.
    let _ = SegmentMetadata::from_bytes(data);
});
