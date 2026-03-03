use aletheiadb::core::property::{PropertyMap, PropertyValue};
use aletheiadb::core::vector::constants::{TAG_SPARSE_VECTOR, TAG_VECTOR};
use aletheiadb::core::vector::serialization::{deserialize_sparse_vector, deserialize_vector};

#[test]
fn should_return_error_on_invalid_int_deserialization() {
    let bytes = vec![aletheiadb::core::property::TAG_INT, 0, 0, 0, 0, 0, 0, 0];
    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
}

#[test]
fn should_return_error_on_invalid_float_deserialization() {
    let bytes = vec![aletheiadb::core::property::TAG_FLOAT, 0, 0, 0, 0, 0, 0, 0];
    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
}

#[test]
fn should_return_error_on_invalid_string_deserialization() {
    let bytes = vec![aletheiadb::core::property::TAG_STRING, 0, 0, 0];
    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
}

#[test]
fn should_return_error_on_invalid_bytes_deserialization() {
    let bytes = vec![aletheiadb::core::property::TAG_BYTES, 0, 0, 0];
    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
}

#[test]
fn should_return_error_on_invalid_array_deserialization() {
    let bytes = vec![aletheiadb::core::property::TAG_ARRAY, 0, 0, 0];
    let result = PropertyValue::deserialize(&bytes);
    assert!(result.is_err());
}

#[test]
fn should_return_error_on_invalid_map_deserialization() {
    let bytes = vec![0, 0, 0, 0];
    let result = PropertyMap::deserialize(&bytes);
    assert!(result.is_ok()); // A valid map of size 0

    let bad_bytes = vec![1, 0, 0]; // count = 1, but no key ID
    let result = PropertyMap::deserialize(&bad_bytes);
    assert!(result.is_err());
}

#[test]
fn should_return_error_on_invalid_vector_deserialization() {
    let bytes = vec![TAG_VECTOR, 0, 0, 0];
    let result = deserialize_vector(&bytes);
    assert!(result.is_err());
}

#[test]
fn should_return_error_on_invalid_sparse_vector_deserialization() {
    let bytes = vec![TAG_SPARSE_VECTOR, 0, 0, 0];
    let result = deserialize_sparse_vector(&bytes);
    assert!(result.is_err());

    let bytes_nnz = vec![TAG_SPARSE_VECTOR, 0, 0, 0, 0, 0, 0, 0];
    let result = deserialize_sparse_vector(&bytes_nnz);
    assert!(result.is_err());
}
