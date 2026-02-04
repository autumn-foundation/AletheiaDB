use gallifreydb::core::property::{PropertyMapBuilder, MAX_VECTOR_DIMENSIONS, PropertyValue};
use std::sync::Arc;

#[test]
fn test_try_insert_vector_returns_error() {
    // This test verifies that try_insert_vector returns an error on overflow,
    // rather than panicking.

    let large_vector = vec![0.0; MAX_VECTOR_DIMENSIONS + 1];

    let result = PropertyMapBuilder::new()
        .try_insert_vector("test", &large_vector);

    assert!(result.is_err(), "Expected try_insert_vector to return error");
}

#[test]
fn test_serialize_returns_error_on_oversized_vector() {
    // Manually construct an oversized PropertyValue::Vector
    // This bypasses the PropertyValue::vector() checks but mimics a scenario
    // where a user might construct the enum variant directly (it's public).

    let large_vector = vec![0.0; MAX_VECTOR_DIMENSIONS + 1];
    let val = PropertyValue::Vector(Arc::from(large_vector.into_boxed_slice()));

    // Attempt to serialize
    // Before fix: This would panic inside serialize_vector_into
    // After fix: This should return Err
    let result = val.serialize();

    assert!(result.is_err(), "Expected serialize to return error for oversized vector");

    // Verify error message
    let err = result.unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("exceeds maximum allowed"), "Unexpected error message: {}", msg);
}
