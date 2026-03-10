use aletheiadb::core::property::{PropertyMap, PropertyValue};
use proptest::prelude::*;

proptest! {
    #[test]
    fn fuzz_property_value_deserialize(data in any::<Vec<u8>>()) {
        // We throw garbage at it. It should return Err, but it MUST NOT panic.
        let _ = PropertyValue::deserialize(&data);
    }

    #[test]
    fn fuzz_property_map_deserialize(data in any::<Vec<u8>>()) {
        // Same here: garbage in, Err out. No panics allowed.
        let _ = PropertyMap::deserialize(&data);
    }
}
