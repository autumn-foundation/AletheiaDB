use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::property::{PropertyMap, PropertyValue};
use aletheiadb::core::vector::serialization::{deserialize_sparse_vector, deserialize_vector};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn test_hlc_deserialization_no_panic(bytes in prop::collection::vec(any::<u8>(), 0..32)) {
        let _ = HybridTimestamp::deserialize(&bytes);
    }

    #[test]
    fn test_property_value_deserialization_no_panic(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
        let _ = PropertyValue::deserialize(&bytes);
    }

    #[test]
    fn test_property_map_deserialization_no_panic(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
        let _ = PropertyMap::deserialize(&bytes);
    }

    #[test]
    fn test_vector_deserialization_no_panic(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
        let _ = deserialize_vector(&bytes);
    }

    #[test]
    fn test_sparse_vector_deserialization_no_panic(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
        let _ = deserialize_sparse_vector(&bytes);
    }
}
