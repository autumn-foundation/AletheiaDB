import sys

def main():
    with open('src/core/id.rs', 'r') as f:
        content = f.read()

    # Find the start of the root level tests
    start_idx = content.find('#[test]\nfn test_entity_id_as_node_exhaustive() {')

    if start_idx == -1:
        print("Could not find start index")
        return

    # Find the start of the sentinel_tests_id_fmt module
    mod_idx = content.find('#[cfg(test)]\nmod sentinel_tests_id_fmt {')

    if mod_idx == -1:
        print("Could not find module index")
        return

    # Find the end of the root level tests right before the module
    end_idx = mod_idx

    # The tests to move
    tests_content = content[start_idx:end_idx].strip()

    # Now let's just rewrite the end of the file starting from where we cut
    # to avoid duplication, we'll strip out the duplicate display tests from the root level

    # We want to keep these:
    # test_entity_id_as_node_exhaustive
    # test_entity_id_as_edge_exhaustive
    # test_entity_id_from_exhaustive
    # test_id_generator_with_start_exhaustive

    # And we discard these (duplicates):
    # test_entity_id_display_exhaustive
    # test_node_id_display_exhaustive
    # test_edge_id_display_exhaustive
    # test_version_id_display_exhaustive

    # Let's just create a clean version of sentinel_entity_id_exhaustive_tests

    # Find the sentinel_entity_id_exhaustive_tests block
    exhaustive_start = content.find('#[cfg(test)]\nmod sentinel_entity_id_exhaustive_tests {')
    exhaustive_end = content.find('}', content.find('test_entity_id_is_edge_exhaustive')) + 2 # closing brace of the module

    # Let's replace everything from the start of exhaustive tests to the end of the file

    new_end_content = """#[cfg(test)]
mod sentinel_entity_id_exhaustive_tests {
    use super::*;

    #[test]
    fn test_entity_id_is_node_exhaustive() {
        let node_id = NodeId::new(42).unwrap();
        let entity_node: EntityId = node_id.into();
        assert!(entity_node.is_node());

        let edge_id = EdgeId::new(42).unwrap();
        let entity_edge: EntityId = edge_id.into();
        assert!(!entity_edge.is_node());
    }

    #[test]
    fn test_entity_id_is_edge_exhaustive() {
        let node_id = NodeId::new(42).unwrap();
        let entity_node: EntityId = node_id.into();
        assert!(!entity_node.is_edge());

        let edge_id = EdgeId::new(42).unwrap();
        let entity_edge: EntityId = edge_id.into();
        assert!(entity_edge.is_edge());
    }

    #[test]
    fn test_entity_id_as_node_exhaustive() {
        let node_id = NodeId::new(42).unwrap();
        let entity_node: EntityId = node_id.into();
        assert_eq!(entity_node.as_node(), Some(node_id));

        let edge_id = EdgeId::new(42).unwrap();
        let entity_edge: EntityId = edge_id.into();
        assert_eq!(entity_edge.as_node(), None);
    }

    #[test]
    fn test_entity_id_as_edge_exhaustive() {
        let node_id = NodeId::new(42).unwrap();
        let entity_node: EntityId = node_id.into();
        assert_eq!(entity_node.as_edge(), None);

        let edge_id = EdgeId::new(42).unwrap();
        let entity_edge: EntityId = edge_id.into();
        assert_eq!(entity_edge.as_edge(), Some(edge_id));
    }

    #[test]
    fn test_entity_id_from_exhaustive() {
        let node_id = NodeId::new(42).unwrap();
        let entity_node: EntityId = node_id.into();
        assert_eq!(entity_node, EntityId::Node(node_id));

        let edge_id = EdgeId::new(42).unwrap();
        let entity_edge: EntityId = edge_id.into();
        assert_eq!(entity_edge, EntityId::Edge(edge_id));
    }

    #[test]
    fn test_id_generator_with_start_exhaustive() {
        let generator = IdGenerator::with_start(42);
        assert_eq!(generator.current(), 42);

        let generator_default: IdGenerator = Default::default();
        assert_eq!(generator_default.current(), 0);
    }
}

#[cfg(test)]
mod sentinel_tests_id_fmt {
    use super::*;

    #[test]
    fn test_node_id_fmt_display() {
        let node_id = NodeId::new(42).unwrap();
        assert_eq!(format!("{}", node_id), "Node(42)");
        assert_ne!(format!("{}", node_id), "");
    }

    #[test]
    fn test_edge_id_fmt_display() {
        let edge_id = EdgeId::new(42).unwrap();
        assert_eq!(format!("{}", edge_id), "Edge(42)");
        assert_ne!(format!("{}", edge_id), "");
    }

    #[test]
    fn test_version_id_fmt_display() {
        let version_id = VersionId::new(42).unwrap();
        assert_eq!(format!("{}", version_id), "Version(42)");
        assert_ne!(format!("{}", version_id), "");
    }

    #[test]
    fn test_tx_id_fmt_display() {
        let tx_id = TxId::new(42);
        assert_eq!(format!("{}", tx_id), "TxId(42)");
        assert_ne!(format!("{}", tx_id), "");
    }

    #[test]
    fn test_entity_id_fmt_display() {
        let node_id = NodeId::new(42).unwrap();
        let entity_node: EntityId = node_id.into();
        assert_eq!(format!("{}", entity_node), "Node(42)");
        assert_ne!(format!("{}", entity_node), "");

        let edge_id = EdgeId::new(42).unwrap();
        let entity_edge: EntityId = edge_id.into();
        assert_eq!(format!("{}", entity_edge), "Edge(42)");
        assert_ne!(format!("{}", entity_edge), "");
    }
}
"""

    new_content = content[:exhaustive_start] + new_end_content

    with open('src/core/id.rs', 'w') as f:
        f.write(new_content)

    print("Done")

if __name__ == '__main__':
    main()
