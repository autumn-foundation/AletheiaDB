
// ========================================================================
// Convenience Methods Tests
// ========================================================================

#[test]
fn test_convenience_update_node() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("age", 30i64).build(),
        )
        .unwrap();

    db.update_node(
        node_id,
        PropertyMapBuilder::new().insert("age", 31i64).build(),
    )
    .unwrap();

    let node = db.get_node(node_id).unwrap();
    assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
}

#[test]
fn test_convenience_update_edge() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let edge_id = db
        .create_edge(
            n1,
            n2,
            "KNOWS",
            PropertyMapBuilder::new().insert("w", 1i64).build(),
        )
        .unwrap();

    db.update_edge(
        edge_id,
        PropertyMapBuilder::new().insert("w", 2i64).build(),
    )
    .unwrap();

    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(edge.properties.get("w").and_then(|v| v.as_int()), Some(2));
}

#[test]
fn test_convenience_delete_node() {
    let db = AletheiaDB::new().unwrap();
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    db.delete_node(node_id).unwrap();

    assert!(db.get_node(node_id).is_err());
}

#[test]
fn test_convenience_delete_node_cascade() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(n1, n2, "K", PropertyMapBuilder::new().build())
        .unwrap();

    db.delete_node_cascade(n1).unwrap();

    assert!(db.get_node(n1).is_err());
    assert_eq!(db.edge_count(), 0);
}

#[test]
fn test_convenience_delete_edge() {
    let db = AletheiaDB::new().unwrap();
    let n1 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = db
        .create_node("P", PropertyMapBuilder::new().build())
        .unwrap();
    let edge_id = db
        .create_edge(n1, n2, "K", PropertyMapBuilder::new().build())
        .unwrap();

    db.delete_edge(edge_id).unwrap();

    assert!(db.get_edge(edge_id).is_err());
}
