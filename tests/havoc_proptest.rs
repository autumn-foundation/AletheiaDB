use aletheiadb::core::property::{deserialize_vector, serialize_vector};
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_deserialize_vector_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        // This should return Ok or Err, but NOT panic
        let _ = deserialize_vector(&bytes);
    }

    #[test]
    fn test_vector_roundtrip(vec in prop::collection::vec(any::<f32>(), 0..100)) {
        // 💣 Risk: NaNs might break equality checks or serialization
        let bytes = serialize_vector(&vec);
        let (deserialized, _) = deserialize_vector(&bytes).unwrap();

        // Check length
        prop_assert_eq!(vec.len(), deserialized.len());

        // Check values (handling NaN)
        for (a, b) in vec.iter().zip(deserialized.iter()) {
            if a.is_nan() {
                prop_assert!(b.is_nan());
            } else {
                prop_assert_eq!(a, b);
            }
        }
    }
}
