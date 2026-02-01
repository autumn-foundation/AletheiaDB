use gallifreydb::core::property::{MAX_RECURSION_DEPTH, PropertyValue};

#[test]
fn test_deeply_nested_serialization_recursion_limit() {
    // Create a deeply nested array exceeding the recursion limit
    let depth = MAX_RECURSION_DEPTH + 10;
    let mut value = PropertyValue::string("leaf");

    for _ in 0..depth {
        value = PropertyValue::array(vec![value]);
    }

    // Attempt to serialize - should return an error due to recursion limit
    let result = value.serialize();

    assert!(
        result.is_err(),
        "Serialization should fail due to recursion limit"
    );

    // Verify the error message
    let err = result.unwrap_err();
    let err_str = format!("{}", err);
    assert!(
        err_str
            .to_lowercase()
            .contains("recursion depth limit exceeded"),
        "Error should mention recursion depth. Got: {}",
        err_str
    );
}
