use aletheiadb::core::property::{PropertyMap, PropertyValue};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn fuzz_property_value_deserialize(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
        // Just ensure it doesn't panic
        let _ = PropertyValue::deserialize(&bytes);
    }

    #[test]
    fn fuzz_property_map_deserialize(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
        // Just ensure it doesn't panic
        let _ = PropertyMap::deserialize(&bytes);
    }
}
