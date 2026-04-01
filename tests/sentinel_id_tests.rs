use aletheiadb::core::id::{EdgeId, EntityId, IdGenerator, NodeId, TxId, TxIdGenerator, VersionId};

#[test]
fn test_node_id_new() {
    let raw_id = 42;
    let node_id = NodeId::new(raw_id).unwrap();
    assert_eq!(node_id.as_u64(), raw_id);
}

#[test]
fn test_node_id_as_u64() {
    let node_id = NodeId::new(5).unwrap();
    assert_eq!(node_id.as_u64(), 5);
}

#[test]
fn test_node_id_display() {
    let node_id = NodeId::new(42).unwrap();
    assert_eq!(format!("{}", node_id), "Node(42)");
}

#[test]
fn test_edge_id_new() {
    let raw_id = 42;
    let edge_id = EdgeId::new(raw_id).unwrap();
    assert_eq!(edge_id.as_u64(), raw_id);
}

#[test]
fn test_edge_id_as_u64() {
    let edge_id = EdgeId::new(5).unwrap();
    assert_eq!(edge_id.as_u64(), 5);
}

#[test]
fn test_edge_id_display() {
    let edge_id = EdgeId::new(42).unwrap();
    assert_eq!(format!("{}", edge_id), "Edge(42)");
}

#[test]
fn test_version_id_new() {
    let raw_id = 42;
    let version_id = VersionId::new(raw_id).unwrap();
    assert_eq!(version_id.as_u64(), raw_id);
}

#[test]
fn test_version_id_as_u64() {
    let version_id = VersionId::new(5).unwrap();
    assert_eq!(version_id.as_u64(), 5);
}

#[test]
fn test_version_id_display() {
    let version_id = VersionId::new(42).unwrap();
    assert_eq!(format!("{}", version_id), "Version(42)");
}

#[test]
fn test_entity_id_is_node() {
    let node_id = NodeId::new(1).unwrap();
    let entity_node: EntityId = node_id.into();
    let edge_id = EdgeId::new(1).unwrap();
    let entity_edge: EntityId = edge_id.into();
    assert!(entity_node.is_node());
    assert!(!entity_edge.is_node());
    // Also use explicit boolean comparison to ensure the mutant returning true/false for all is killed
    let is_node_val = entity_node.is_node();
    let is_not_node_val = entity_edge.is_node();
    assert!(is_node_val && !is_not_node_val);
}

#[test]
fn test_entity_id_is_edge() {
    let node_id = NodeId::new(1).unwrap();
    let entity_node: EntityId = node_id.into();
    let edge_id = EdgeId::new(1).unwrap();
    let entity_edge: EntityId = edge_id.into();
    assert!(!entity_node.is_edge());
    assert!(entity_edge.is_edge());
    // Also use explicit boolean comparison to ensure the mutant returning true/false for all is killed
    let is_not_edge_val = entity_node.is_edge();
    let is_edge_val = entity_edge.is_edge();
    assert!(!is_not_edge_val && is_edge_val);
}

#[test]
fn test_entity_id_as_node() {
    let node_id = NodeId::new(42).unwrap();
    let entity_node: EntityId = node_id.into();
    let edge_id = EdgeId::new(42).unwrap();
    let entity_edge: EntityId = edge_id.into();
    assert_eq!(entity_node.as_node(), Some(node_id));
    assert_eq!(entity_edge.as_node(), None);
}

#[test]
fn test_entity_id_as_edge() {
    let node_id = NodeId::new(42).unwrap();
    let entity_node: EntityId = node_id.into();
    let edge_id = EdgeId::new(42).unwrap();
    let entity_edge: EntityId = edge_id.into();
    assert_eq!(entity_node.as_edge(), None);
    assert_eq!(entity_edge.as_edge(), Some(edge_id));
}

#[test]
fn test_entity_id_display() {
    let node_id = NodeId::new(42).unwrap();
    let entity_node: EntityId = node_id.into();
    let edge_id = EdgeId::new(42).unwrap();
    let entity_edge: EntityId = edge_id.into();
    assert_eq!(format!("{}", entity_node), "Node(42)");
    assert_eq!(format!("{}", entity_edge), "Edge(42)");
}

#[test]
fn test_entity_id_from_exhaustive() {
    let node_id = NodeId::new(42).unwrap();
    let entity_node: EntityId = node_id.into();
    assert_eq!(entity_node.as_node(), Some(node_id));
    assert_ne!(entity_node, EntityId::Node(NodeId::new(0).unwrap()));

    let edge_id = EdgeId::new(42).unwrap();
    let entity_edge: EntityId = edge_id.into();
    assert_eq!(entity_edge.as_edge(), Some(edge_id));
    assert_ne!(entity_edge, EntityId::Edge(EdgeId::new(0).unwrap()));
}

#[test]
fn test_id_generator_with_start() {
    let generator = IdGenerator::with_start(42);
    assert_eq!(generator.current_approximate(), 42);
    let gen2 = IdGenerator::with_start(50);
    assert_eq!(gen2.current_approximate(), 50);

    // Kill `replace IdGenerator::with_start -> Self with Default::default()`
    // which starts at 0
    assert_ne!(generator.current_approximate(), 0);
    assert_ne!(gen2.current_approximate(), 0);
}

#[test]
fn test_id_generator_next() {
    let generator = IdGenerator::with_start(42);
    let first = generator.next().unwrap();
    assert_eq!(first, 42);
    let second = generator.next().unwrap();
    assert_eq!(second, 43);
}

#[test]
fn test_id_generator_current_approximate() {
    let generator = IdGenerator::with_start(42);
    let first = generator.current_approximate();
    assert_eq!(first, 42);
    let _ = generator.next().unwrap();
    let second = generator.current_approximate();
    assert_eq!(second, 43);
}

#[test]
fn test_id_generator_current_exhaustive() {
    let generator = IdGenerator::with_start(42);
    let first = generator.current();
    assert_eq!(first, 42);
    let _ = generator.next().unwrap();
    let second = generator.current();
    assert_eq!(second, 43);
}

#[test]
fn test_tx_id_generator_next() {
    let generator = TxIdGenerator::new();
    let first = generator.next();
    assert_eq!(first, TxId::new(1));
    let second = generator.next();
    assert_eq!(second, TxId::new(2));
}

#[test]
fn test_tx_id_generator_current() {
    let generator = TxIdGenerator::new();
    assert_eq!(generator.current(), TxId::new(0));
    let _ = generator.next();
    let current = generator.current();
    assert_eq!(current, TxId::new(1));
}

#[test]
fn test_tx_id_as_u64() {
    let id = TxId::new(5);
    assert_eq!(id.as_u64(), 5);
}

#[test]
fn test_tx_id_display() {
    let id = TxId::new(5);
    assert_eq!(format!("{}", id), "TxId(5)");
}

#[test]
fn test_entity_id_as_node_exhaustive() {
    let node_id = NodeId::new(42).unwrap();
    let entity_node: EntityId = node_id.into();
    let edge_id = EdgeId::new(42).unwrap();
    let entity_edge: EntityId = edge_id.into();

    // Kill "replace EntityId::as_node -> Option<NodeId> with None"
    // Kill "replace EntityId::as_node -> Option<NodeId> with Some(Default::default())"
    let as_node = entity_node.as_node().expect("Should be Some(NodeId)");
    assert_eq!(as_node.as_u64(), 42); // 42 is not default 0

    // Check edge case returns None
    assert_eq!(entity_edge.as_node(), None);
}

#[test]
fn test_entity_id_as_edge_exhaustive() {
    let node_id = NodeId::new(42).unwrap();
    let entity_node: EntityId = node_id.into();
    let edge_id = EdgeId::new(42).unwrap();
    let entity_edge: EntityId = edge_id.into();

    // Kill "replace EntityId::as_edge -> Option<EdgeId> with None"
    // Kill "replace EntityId::as_edge -> Option<EdgeId> with Some(Default::default())"
    let as_edge = entity_edge.as_edge().expect("Should be Some(EdgeId)");
    assert_eq!(as_edge.as_u64(), 42); // 42 is not default 0

    // Check node case returns None
    assert_eq!(entity_node.as_edge(), None);
}

#[test]
fn test_id_generator_mutants() {
    let generator = IdGenerator::with_start(42);
    assert_ne!(generator.current(), 0); // kill return 0 from with_start Default

    let val = generator.next().unwrap();
    assert_eq!(val, 42);
    assert_ne!(val, 0); // kill Ok(0)
    assert_ne!(val, 1); // kill Ok(1)

    let curr = generator.current();
    assert_eq!(curr, 43);
    assert_ne!(curr, 0); // kill return 0
    assert_ne!(curr, 1); // kill return 1

    let approx = generator.current_approximate();
    assert_eq!(approx, 43);
    assert_ne!(approx, 0); // kill return 0
    assert_ne!(approx, 1); // kill return 1
}

#[test]
fn test_tx_id_generator_current_mutants() {
    let generator = TxIdGenerator::new();
    let val = generator.current();
    assert_eq!(val.as_u64(), 0);

    generator.next();
    let val2 = generator.current();
    assert_eq!(val2.as_u64(), 1);
    assert_ne!(val2, TxId::new(0)); // kill Default::default()
}

#[test]
fn test_id_try_from_mutants() {
    let n = NodeId::try_from(42).unwrap();
    assert_eq!(n.as_u64(), 42);
    assert_ne!(n, NodeId::new(0).unwrap()); // kill Ok(Default::default())

    let e = EdgeId::try_from(100).unwrap();
    assert_eq!(e.as_u64(), 100);
    assert_ne!(e, EdgeId::new(0).unwrap()); // kill Ok(Default::default())

    let v = VersionId::try_from(1000).unwrap();
    assert_eq!(v.as_u64(), 1000);
    assert_ne!(v, VersionId::new(0).unwrap()); // kill Ok(Default::default())

    let t = TxId::try_from(55).unwrap();
    assert_eq!(t.as_u64(), 55);
    assert_ne!(t, TxId::new(0)); // kill Ok(Default::default())
}

#[test]
fn test_id_from_str_mutants() {
    use std::str::FromStr;
    let n = NodeId::from_str("42").unwrap();
    assert_eq!(n.as_u64(), 42);
    assert_ne!(n, NodeId::new(0).unwrap()); // kill Ok(Default::default())

    let e = EdgeId::from_str("100").unwrap();
    assert_eq!(e.as_u64(), 100);
    assert_ne!(e, EdgeId::new(0).unwrap()); // kill Ok(Default::default())

    let v = VersionId::from_str("1000").unwrap();
    assert_eq!(v.as_u64(), 1000);
    assert_ne!(v, VersionId::new(0).unwrap()); // kill Ok(Default::default())

    let t = TxId::from_str("55").unwrap();
    assert_eq!(t.as_u64(), 55);
    assert_ne!(t, TxId::new(0)); // kill Ok(Default::default())
}

#[test]
fn test_id_display_defaults() {
    let n = NodeId::new(42).unwrap();
    assert_ne!(format!("{}", n), "Ok(Default::default())");
    assert_ne!(format!("{}", n), "");
    assert_eq!(format!("{}", n), "Node(42)");

    let e = EdgeId::new(100).unwrap();
    assert_ne!(format!("{}", e), "Ok(Default::default())");
    assert_ne!(format!("{}", e), "");
    assert_eq!(format!("{}", e), "Edge(100)");

    let v = VersionId::new(1000).unwrap();
    assert_ne!(format!("{}", v), "Ok(Default::default())");
    assert_ne!(format!("{}", v), "");
    assert_eq!(format!("{}", v), "Version(1000)");

    let t = TxId::new(55);
    assert_ne!(format!("{}", t), "Ok(Default::default())");
    assert_ne!(format!("{}", t), "");
    assert_eq!(format!("{}", t), "TxId(55)");
}
