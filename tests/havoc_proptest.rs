use proptest::prelude::*;
use aletheiadb::core::property::{deserialize_vector, deserialize_sparse_vector};

proptest! {
    #[test]
    fn fuzz_deserialize_vector(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
        // Havoc Fuzz Target: deserialize_vector
        // Input: Random byte stream
        // Expectation: Result::Ok or Result::Err, NEVER Panic
        let _ = deserialize_vector(&bytes);
    }

    #[test]
    fn fuzz_deserialize_sparse_vector(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
        // Havoc Fuzz Target: deserialize_sparse_vector
        // Input: Random byte stream
        // Expectation: Result::Ok or Result::Err, NEVER Panic
        let _ = deserialize_sparse_vector(&bytes);
    }
}
