use aletheiadb::core::graph::{Edge, Node};
use aletheiadb::core::id::{EdgeId, NodeId, VersionId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::{PropertyMap, PropertyMapBuilder};

#[test]
fn test_node_get_property_exhaustive() {
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let props = PropertyMapBuilder::new().insert("name", "Alice").build();

    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        props,
        VersionId::new(100).unwrap(),
    );

    // Kill "replace Node::get_property -> Option<&PropertyValue> with None"
    let val = node.get_property("name").expect("Property should exist");
    assert_eq!(val.as_str(), Some("Alice"));

    // Kill "replace Node::get_property -> Option<&PropertyValue> with Some(...)"
    let missing = node.get_property("missing");
    assert_eq!(missing, None);
}

#[test]
fn test_node_has_label_exhaustive() {
    let label1 = GLOBAL_INTERNER.intern("Label1").unwrap();
    let label2 = GLOBAL_INTERNER.intern("Label2").unwrap();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label1,
        PropertyMap::new(),
        VersionId::new(1).unwrap(),
    );

    // Kill "replace Node::has_label -> bool with true / false" and "== with !="
    let has1 = node.has_label(label1);
    let has2 = node.has_label(label2);
    assert!(has1);
    assert!(!has2);
}

#[test]
fn test_edge_get_property_exhaustive() {
    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let props = PropertyMapBuilder::new().insert("since", 2024i64).build();

    let edge = Edge::new(
        EdgeId::new(1).unwrap(),
        label,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        props,
        VersionId::new(100).unwrap(),
    );

    // Kill "replace Edge::get_property -> Option<&PropertyValue> with None"
    let val = edge.get_property("since").expect("Property should exist");
    assert_eq!(val.as_int(), Some(2024));

    // Kill "replace Edge::get_property -> Option<&PropertyValue> with Some(...)"
    let missing = edge.get_property("missing");
    assert_eq!(missing, None);
}

#[test]
fn test_edge_has_label_exhaustive() {
    let label1 = GLOBAL_INTERNER.intern("EdgeLabel1").unwrap();
    let label2 = GLOBAL_INTERNER.intern("EdgeLabel2").unwrap();
    let edge = Edge::new(
        EdgeId::new(1).unwrap(),
        label1,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMap::new(),
        VersionId::new(1).unwrap(),
    );

    // Kill "replace Edge::has_label -> bool with true / false" and "== with !="
    let has1 = edge.has_label(label1);
    let has2 = edge.has_label(label2);
    assert!(has1);
    assert!(!has2);
}

#[test]
fn test_matches_label_exhaustive() {
    let label_id = GLOBAL_INTERNER.intern("TestLabel").unwrap();

    // Kill "replace matches_label -> bool with true"
    // By checking a mismatch explicitly and asserting false
    let is_match = aletheiadb::core::graph::Node::new(
        NodeId::new(1).unwrap(),
        label_id,
        PropertyMap::new(),
        VersionId::new(1).unwrap(),
    )
    .has_label_str("OtherLabel");

    assert!(!is_match);

    // Kill "replace matches_label -> bool with false"
    let is_match_true = aletheiadb::core::graph::Node::new(
        NodeId::new(1).unwrap(),
        label_id,
        PropertyMap::new(),
        VersionId::new(1).unwrap(),
    )
    .has_label_str("TestLabel");

    assert!(is_match_true);
}

#[test]
fn test_edge_connects_exhaustive() {
    let source = NodeId::new(1).unwrap();
    let target = NodeId::new(2).unwrap();
    let other1 = NodeId::new(3).unwrap();
    let other2 = NodeId::new(4).unwrap();

    let edge = Edge::new(
        EdgeId::new(1).unwrap(),
        GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        source,
        target,
        PropertyMap::new(),
        VersionId::new(1).unwrap(),
    );

    // Kill "replace Edge::connects -> bool with true / false"
    assert!(edge.connects(source, target));
    assert!(!edge.connects(other1, target));
    assert!(!edge.connects(source, other2));
    assert!(!edge.connects(other1, other2));
}

#[test]
fn test_node_with_metadata_exhaustive() {
    let label = GLOBAL_INTERNER.intern("NodeMetaLabel").unwrap();
    let props = PropertyMapBuilder::new().build();
    let metadata = aletheiadb::core::version::VersionMetadata::new(
        aletheiadb::core::id::TxId::new(42),
        aletheiadb::core::temporal::Timestamp::from(42),
    );
    let node = Node::with_metadata(
        NodeId::new(1).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
        metadata,
    );

    // Kill "replace Node::with_metadata -> Self with Default::default()"
    assert_ne!(node.id, NodeId::new(0).unwrap()); // Or something that proves it's not default
    assert_eq!(node.metadata, metadata);
}

#[test]
fn test_edge_with_metadata_exhaustive() {
    let label = GLOBAL_INTERNER.intern("EdgeMetaLabel").unwrap();
    let props = PropertyMapBuilder::new().build();
    let metadata = aletheiadb::core::version::VersionMetadata::new(
        aletheiadb::core::id::TxId::new(42),
        aletheiadb::core::temporal::Timestamp::from(42),
    );
    let edge = Edge::with_metadata(
        EdgeId::new(1).unwrap(),
        label,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        props,
        VersionId::new(1).unwrap(),
        metadata,
    );

    // Kill "replace Edge::with_metadata -> Self with Default::default()"
    assert_ne!(edge.id, EdgeId::new(0).unwrap());
    assert_eq!(edge.metadata, metadata);
}

#[test]
fn test_node_debug_format_exhaustive() {
    let label = GLOBAL_INTERNER.intern("DebugNodeLabel").unwrap();
    let node = Node::new(
        NodeId::new(42).unwrap(),
        label,
        PropertyMap::new(),
        VersionId::new(1).unwrap(),
    );
    let formatted = format!("{:?}", node);
    // Kill "replace <impl std::fmt::Debug for Node>::fmt -> std::fmt::Result with Ok(Default::default())"
    assert!(formatted.contains("DebugNodeLabel"));
    assert!(formatted.contains("42"));
    assert!(!formatted.is_empty());
}

#[test]
fn test_edge_debug_format_exhaustive() {
    let label = GLOBAL_INTERNER.intern("DebugEdgeLabel").unwrap();
    let edge = Edge::new(
        EdgeId::new(42).unwrap(),
        label,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMap::new(),
        VersionId::new(1).unwrap(),
    );
    let formatted = format!("{:?}", edge);
    // Kill "replace <impl std::fmt::Debug for Edge>::fmt -> std::fmt::Result with Ok(Default::default())"
    assert!(formatted.contains("DebugEdgeLabel"));
    assert!(formatted.contains("42"));
    assert!(!formatted.is_empty());
}
