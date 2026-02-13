//! Property-based fuzzing for PropertyValue deserialization.
//!
//! # HAVOC: PROPERTY TORTURE
//! This test uses `proptest` to generate random byte sequences and valid structures
//! to fuzz the serialization/deserialization logic, looking for panics or crashes.
//!
//! Note: While `cargo-fuzz` (LibFuzzer) is more powerful for finding edge cases
//! in parsing logic, `proptest` provides a good baseline for CI without requiring
//! nightly Rust or special fuzzing infrastructure.

use proptest::prelude::*;
use aletheiadb::core::property::{PropertyValue, PropertyMap, deserialize_vector};

proptest! {
    // Fuzz PropertyValue::deserialize with random bytes
    #[test]
    fn fuzz_property_value_deserialize(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        // Just verify it doesn't panic/crash
        let _ = PropertyValue::deserialize(&bytes);
    }

    // Fuzz PropertyMap::deserialize with random bytes
    #[test]
    fn fuzz_property_map_deserialize(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        // Just verify it doesn't panic/crash
        let _ = PropertyMap::deserialize(&bytes);
    }

    // Fuzz deserialize_vector with random bytes
    #[test]
    fn fuzz_vector_deserialize(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        // Just verify it doesn't panic/crash
        let _ = deserialize_vector(&bytes);
    }
}
