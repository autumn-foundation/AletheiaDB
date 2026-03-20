#[cfg(test)]
mod sentinel_id_generator_exhaustive_tests {
    use aletheiadb::core::id::{EdgeId, EntityId, NodeId, TxId, VersionId};
    use std::str::FromStr;

    #[test]
    fn test_node_id_try_from_exhaustive() {
        let id = NodeId::try_from(42).unwrap();
        assert_eq!(id.as_u64(), 42);

        // Explicit check against the default mutation logic
        let default_id = NodeId::try_from(0).unwrap();
        assert_eq!(default_id.as_u64(), 0);
        assert_ne!(id.as_u64(), default_id.as_u64());
    }

    #[test]
    fn test_node_id_from_str_exhaustive() {
        let id = NodeId::from_str("42").unwrap();
        assert_eq!(id.as_u64(), 42);

        let default_id = NodeId::from_str("0").unwrap();
        assert_eq!(default_id.as_u64(), 0);
        assert_ne!(id.as_u64(), default_id.as_u64());
    }

    #[test]
    fn test_edge_id_try_from_exhaustive() {
        let id = EdgeId::try_from(42).unwrap();
        assert_eq!(id.as_u64(), 42);

        // Explicit check against the default mutation logic
        let default_id = EdgeId::try_from(0).unwrap();
        assert_eq!(default_id.as_u64(), 0);
        assert_ne!(id.as_u64(), default_id.as_u64());
    }

    #[test]
    fn test_edge_id_from_str_exhaustive() {
        let id = EdgeId::from_str("42").unwrap();
        assert_eq!(id.as_u64(), 42);

        let default_id = EdgeId::from_str("0").unwrap();
        assert_eq!(default_id.as_u64(), 0);
        assert_ne!(id.as_u64(), default_id.as_u64());
    }

    #[test]
    fn test_version_id_try_from_exhaustive() {
        let id = VersionId::try_from(42).unwrap();
        assert_eq!(id.as_u64(), 42);

        // Explicit check against the default mutation logic
        let default_id = VersionId::try_from(0).unwrap();
        assert_eq!(default_id.as_u64(), 0);
        assert_ne!(id.as_u64(), default_id.as_u64());
    }

    #[test]
    fn test_version_id_from_str_exhaustive() {
        let id = VersionId::from_str("42").unwrap();
        assert_eq!(id.as_u64(), 42);

        let default_id = VersionId::from_str("0").unwrap();
        assert_eq!(default_id.as_u64(), 0);
        assert_ne!(id.as_u64(), default_id.as_u64());
    }

    #[test]
    fn test_tx_id_try_from_exhaustive() {
        let id = TxId::try_from(42).unwrap();
        assert_eq!(id.as_u64(), 42);

        // Explicit check against the default mutation logic
        let default_id = TxId::try_from(0).unwrap();
        assert_eq!(default_id.as_u64(), 0);
        assert_ne!(id.as_u64(), default_id.as_u64());
    }

    #[test]
    fn test_tx_id_from_str_exhaustive() {
        let id = TxId::from_str("42").unwrap();
        assert_eq!(id.as_u64(), 42);

        let default_id = TxId::from_str("0").unwrap();
        assert_eq!(default_id.as_u64(), 0);
        assert_ne!(id.as_u64(), default_id.as_u64());
    }

    #[test]
    fn test_node_id_display_exhaustive() {
        let id = NodeId::try_from(42).unwrap();
        assert_eq!(id.to_string(), "Node(42)");

        let default_id = NodeId::try_from(0).unwrap();
        assert_ne!(id.to_string(), default_id.to_string());
    }

    #[test]
    fn test_edge_id_display_exhaustive() {
        let id = EdgeId::try_from(42).unwrap();
        assert_eq!(id.to_string(), "Edge(42)");

        let default_id = EdgeId::try_from(0).unwrap();
        assert_ne!(id.to_string(), default_id.to_string());
    }

    #[test]
    fn test_version_id_display_exhaustive() {
        let id = VersionId::try_from(42).unwrap();
        assert_eq!(id.to_string(), "Version(42)");

        let default_id = VersionId::try_from(0).unwrap();
        assert_ne!(id.to_string(), default_id.to_string());
    }

    #[test]
    fn test_tx_id_display_exhaustive() {
        let id = TxId::try_from(42).unwrap();
        assert_eq!(id.to_string(), "TxId(42)");

        let default_id = TxId::try_from(0).unwrap();
        assert_ne!(id.to_string(), default_id.to_string());
    }

    #[test]
    fn test_entity_id_is_node_exhaustive() {
        let node_id = NodeId::try_from(42).unwrap();
        let entity_node: EntityId = node_id.into();

        assert!(entity_node.is_node());
        assert!(!entity_node.is_edge());
    }

    #[test]
    fn test_entity_id_is_edge_exhaustive() {
        let edge_id = EdgeId::try_from(42).unwrap();
        let entity_edge: EntityId = edge_id.into();

        assert!(!entity_edge.is_node());
        assert!(entity_edge.is_edge());
    }

    #[test]
    fn test_entity_id_as_node_exhaustive() {
        let node_id = NodeId::try_from(42).unwrap();
        let entity_node: EntityId = node_id.into();

        assert_eq!(entity_node.as_node().unwrap(), node_id);

        let edge_id = EdgeId::try_from(42).unwrap();
        let entity_edge: EntityId = edge_id.into();
        assert!(entity_edge.as_node().is_none());
    }

    #[test]
    fn test_entity_id_as_edge_exhaustive() {
        let edge_id = EdgeId::try_from(42).unwrap();
        let entity_edge: EntityId = edge_id.into();

        assert_eq!(entity_edge.as_edge().unwrap(), edge_id);

        let node_id = NodeId::try_from(42).unwrap();
        let entity_node: EntityId = node_id.into();
        assert!(entity_node.as_edge().is_none());
    }

    #[test]
    fn test_entity_id_display_exhaustive() {
        let node_id = NodeId::try_from(42).unwrap();
        let entity_node: EntityId = node_id.into();
        assert_eq!(entity_node.to_string(), "Node(42)");

        let edge_id = EdgeId::try_from(42).unwrap();
        let entity_edge: EntityId = edge_id.into();
        assert_eq!(entity_edge.to_string(), "Edge(42)");

        let default_node = NodeId::try_from(0).unwrap();
        let default_entity_node: EntityId = default_node.into();
        assert_ne!(entity_node.to_string(), default_entity_node.to_string());

        let default_edge = EdgeId::try_from(0).unwrap();
        let default_entity_edge: EntityId = default_edge.into();
        assert_ne!(entity_edge.to_string(), default_entity_edge.to_string());
    }
}
