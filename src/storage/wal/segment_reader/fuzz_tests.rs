use super::*;
use proptest::prelude::*;

proptest! {
    // Fuzz parse_entry_at with arbitrary bytes
    #[test]
    fn fuzz_parse_entry_at(
        bytes in prop::collection::vec(any::<u8>(), 0..2048),
        offset in 0..100usize,
        version in 0..2u8
    ) {
        // Should not panic
        let _ = parse_entry_at(&bytes, offset, version);
    }
}
