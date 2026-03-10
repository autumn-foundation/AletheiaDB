use aletheiadb::core::id::{EdgeId, EntityId, IdGenerator, NodeId, TxId, TxIdGenerator, VersionId};

#[test]
fn test_node_id_new() {
    let raw_id = 42;
    let node_id = NodeId::new(raw_id).unwrap();
    assert_eq!(node_id.as_u64(), raw_id);
    assert_ne!(node_id.as_u64(), 0);
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
    assert_ne!(edge_id.as_u64(), 0);
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
    assert_ne!(version_id.as_u64(), 0);
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
}

#[test]
fn test_entity_id_is_edge() {
    let node_id = NodeId::new(1).unwrap();
    let entity_node: EntityId = node_id.into();
    let edge_id = EdgeId::new(1).unwrap();
    let entity_edge: EntityId = edge_id.into();
    assert!(!entity_node.is_edge());
    assert!(entity_edge.is_edge());
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
fn test_id_generator_with_start() {
    let generator = IdGenerator::with_start(42);
    assert_eq!(generator.current_approximate(), 42);
}

#[test]
fn test_id_generator_next() {
    let generator = IdGenerator::with_start(42);
    assert_eq!(generator.next().unwrap(), 42);
    assert_eq!(generator.next().unwrap(), 43);
}

#[test]
fn test_id_generator_current_approximate() {
    let generator = IdGenerator::with_start(42);
    assert_eq!(generator.current_approximate(), 42);
    let _ = generator.next().unwrap();
    assert_eq!(generator.current_approximate(), 43);
}

#[test]
fn test_tx_id_generator_next() {
    let generator = TxIdGenerator::new();
    assert_eq!(generator.next(), TxId::new(1));
    assert_eq!(generator.next(), TxId::new(2));
}

#[test]
fn test_tx_id_generator_current() {
    let generator = TxIdGenerator::new();
    assert_eq!(generator.current(), TxId::new(0));
    let _ = generator.next();
    assert_eq!(generator.current(), TxId::new(1));
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
