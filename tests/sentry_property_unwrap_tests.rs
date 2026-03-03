use aletheiadb::core::property::{PropertyMap, PropertyValue, TAG_FLOAT, TAG_INT};

#[test]
fn test_deserialize_too_short_bytes() {
    let bytes = vec![TAG_INT, 0, 0, 0];
    let res = PropertyValue::deserialize(&bytes);
    assert!(res.is_err());

    let bytes = vec![TAG_FLOAT, 0, 0, 0];
    let res = PropertyValue::deserialize(&bytes);
    assert!(res.is_err());
}

#[test]
fn test_property_map_too_short() {
    let bytes = vec![1, 0, 0, 0];
    let res = PropertyMap::deserialize(&bytes);
    assert!(res.is_err());
}
