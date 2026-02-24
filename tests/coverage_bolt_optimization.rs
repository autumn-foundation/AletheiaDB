use aletheiadb::{AletheiaDB, PropertyMapBuilder, NodeId};

#[test]
fn test_with_node_api_coverage() {
    let db = AletheiaDB::new().expect("Failed to open DB");

    // 1. Create a node
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .build();
    let node_id = db.create_node("Person", props).expect("Failed to create node");

    // 2. Test success path: AletheiaDB::with_node
    // This exercises AletheiaDB::with_node -> CurrentStorage::with_node -> CurrentIndexes::with_node
    let name_len = db.with_node(node_id, |n| {
        // Access property without cloning entire node
        n.properties.get("name").map(|v| v.as_str().unwrap_or("").len()).unwrap_or(0)
    }).expect("with_node failed");

    assert_eq!(name_len, 5); // "Alice".len()

    // 3. Test failure path: AletheiaDB::with_node (non-existent node)
    let non_existent = NodeId::new(9999).unwrap();
    let result = db.with_node(non_existent, |_| 0);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(format!("{}", err).contains("not found"));
}

#[test]
fn test_get_node_vector_failure_coverage() {
    let db = AletheiaDB::new().expect("Failed to open DB");

    // 1. Test failure: node not found (should trigger the NodeNotFound path in get_node_vector)
    // We can trigger get_node_vector via find_similar, but vector index must be enabled first
    use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
    let config = HnswConfig::new(2, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config).expect("Failed to enable index");

    let non_existent = NodeId::new(9999).unwrap();
    // This calls find_similar -> get_vector_index_internal -> get_node_vector
    let result = db.find_similar(non_existent, 1);

    assert!(result.is_err());
    let err = result.unwrap_err();
    // The exact error message depends on how StorageError is formatted
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("not found"), "Actual error: {}", err_msg);

    // 2. Test failure: property not found (triggers PropertyNotFound path in get_node_vector)
    let props = PropertyMapBuilder::new().insert("name", "Bob").build();
    let node_id = db.create_node("Person", props).expect("Created node");

    let result = db.find_similar(node_id, 1);
    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("Property") && err_msg.contains("not found"), "Actual error: {}", err_msg);

    // 3. Test failure: invalid vector (triggers InvalidVector path in get_node_vector)
    let props_invalid = PropertyMapBuilder::new()
        .insert("embedding", "not a vector")
        .build();
    let node_id_invalid = db.create_node("Person", props_invalid).expect("Created node");

    let result = db.find_similar(node_id_invalid, 1);
    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("Property is not a vector"), "Actual error: {}", err_msg);
}
