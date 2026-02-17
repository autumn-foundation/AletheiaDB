use aletheiadb::core::property::{PropertyMap, PropertyValue, TAG_ARRAY};
use aletheiadb::utils::error::StorageError;

#[test]
fn test_array_preallocation_dos_protection() {
    // Scenario: Attacker sends a payload claiming 1,000,000 elements.
    // 1,000,000 is allowed by MAX_ARRAY_ELEMENTS (which is 1,000,000).
    // But the payload is only 5 bytes long (Tag + Count).

    let count: u32 = 1_000_000;
    let mut payload = Vec::new();
    payload.push(TAG_ARRAY);
    payload.extend_from_slice(&count.to_le_bytes());

    // We expect an error.
    let result = PropertyValue::deserialize(&payload);

    assert!(result.is_err());
    let err = result.unwrap_err();

    println!("Array Error received: {:?}", err);

    if let aletheiadb::utils::error::Error::Storage(StorageError::CorruptedData(msg)) = err {
        // Verify we hit the NEW protection check (before allocation)
        assert!(msg.contains("Insufficient buffer size for Array elements"));
    } else {
        panic!("Expected StorageError::CorruptedData, got {:?}", err);
    }
}

#[test]
fn test_propertymap_preallocation_dos_protection() {
    // Scenario: Attacker sends a PropertyMap payload claiming 10,000 entries.
    // 10,000 is allowed by MAX_PROPERTY_MAP_CAPACITY.
    // Each entry is minimum 5 bytes. 10,000 * 5 = 50,000 bytes.
    // Payload is only 4 bytes (Count).

    let count: u32 = 10_000;
    let mut payload = Vec::new();
    payload.extend_from_slice(&count.to_le_bytes());

    let result = PropertyMap::deserialize(&payload);

    assert!(result.is_err());
    let err = result.unwrap_err();

    println!("Map Error received: {:?}", err);

    if let aletheiadb::utils::error::Error::Storage(StorageError::CorruptedData(msg)) = err {
        // Verify we hit the NEW protection check
        // "Insufficient buffer size for PropertyMap entries: need 50000 bytes, have 0"
        assert!(msg.contains("Insufficient buffer size for PropertyMap entries"));
        assert!(msg.contains("need 50000 bytes"));
    } else {
        panic!("Expected StorageError::CorruptedData, got {:?}", err);
    }
}
