#[cfg(test)]
mod traversal_tests {
    use super::super::iterators::*;
    use crate::core::id::NodeId;
    use crate::core::property::PropertyMapBuilder;
    use crate::query::ir::Direction;
    use crate::storage::current::CurrentStorage;
    use crate::storage::historical::HistoricalStorage;
    use parking_lot::RwLock;
    use std::sync::Arc;

    fn create_test_node(current: &Arc<CurrentStorage>, label: &str) -> NodeId {
        current
            .create_node(label, PropertyMapBuilder::new().build())
            .unwrap()
    }

    fn create_test_edge(
        current: &Arc<CurrentStorage>,
        source: NodeId,
        target: NodeId,
        label: &str,
    ) {
        current
            .create_edge(source, target, label, PropertyMapBuilder::new().build())
            .unwrap();
    }

    // Helper for setup
    fn setup_storage() -> (Arc<CurrentStorage>, Arc<RwLock<HistoricalStorage>>) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        (current, historical)
    }

    #[test]
    fn test_traversal_outgoing() {
        let (current, historical) = setup_storage();
        let n1 = create_test_node(&current, "Person");
        let n2 = create_test_node(&current, "Person");
        let n3 = create_test_node(&current, "Person");

        create_test_edge(&current, n1, n2, "KNOWS");
        create_test_edge(&current, n1, n3, "LIKES");

        let input = Box::new(NodeLookupIterator::new(vec![n1], current.clone()));
        let mut iter = TraversalIterator::new(
            input,
            Direction::Outgoing,
            None,
            1,
            current.clone(),
            historical.clone(),
            None,
        );

        let mut results = Vec::new();
        while let Some(Ok(row)) = iter.next() {
            results.push(row.entity.node_id().unwrap());
        }
        results.sort();
        assert_eq!(results, vec![n2, n3]);
    }

    #[test]
    fn test_traversal_outgoing_with_label() {
        let (current, historical) = setup_storage();
        let n1 = create_test_node(&current, "Person");
        let n2 = create_test_node(&current, "Person");
        let n3 = create_test_node(&current, "Person");

        create_test_edge(&current, n1, n2, "KNOWS");
        create_test_edge(&current, n1, n3, "LIKES");

        let input = Box::new(NodeLookupIterator::new(vec![n1], current.clone()));
        let mut iter = TraversalIterator::new(
            input,
            Direction::Outgoing,
            Some("KNOWS".to_string()),
            1,
            current.clone(),
            historical.clone(),
            None,
        );

        let mut results = Vec::new();
        while let Some(Ok(row)) = iter.next() {
            results.push(row.entity.node_id().unwrap());
        }
        assert_eq!(results, vec![n2]);
    }

    #[test]
    fn test_traversal_incoming() {
        let (current, historical) = setup_storage();
        let n1 = create_test_node(&current, "Person");
        let n2 = create_test_node(&current, "Person");
        let n3 = create_test_node(&current, "Person");

        create_test_edge(&current, n2, n1, "KNOWS");
        create_test_edge(&current, n3, n1, "LIKES");

        let input = Box::new(NodeLookupIterator::new(vec![n1], current.clone()));
        let mut iter = TraversalIterator::new(
            input,
            Direction::Incoming,
            None,
            1,
            current.clone(),
            historical.clone(),
            None,
        );

        let mut results = Vec::new();
        while let Some(Ok(row)) = iter.next() {
            results.push(row.entity.node_id().unwrap());
        }
        results.sort();
        assert_eq!(results, vec![n2, n3]);
    }

    #[test]
    fn test_traversal_incoming_with_label() {
        let (current, historical) = setup_storage();
        let n1 = create_test_node(&current, "Person");
        let n2 = create_test_node(&current, "Person");
        let n3 = create_test_node(&current, "Person");

        create_test_edge(&current, n2, n1, "KNOWS");
        create_test_edge(&current, n3, n1, "LIKES");

        let input = Box::new(NodeLookupIterator::new(vec![n1], current.clone()));
        let mut iter = TraversalIterator::new(
            input,
            Direction::Incoming,
            Some("KNOWS".to_string()),
            1,
            current.clone(),
            historical.clone(),
            None,
        );

        let mut results = Vec::new();
        while let Some(Ok(row)) = iter.next() {
            results.push(row.entity.node_id().unwrap());
        }
        assert_eq!(results, vec![n2]);
    }

    #[test]
    fn test_traversal_both() {
        let (current, historical) = setup_storage();
        let n1 = create_test_node(&current, "Person");
        let n2 = create_test_node(&current, "Person");
        let n3 = create_test_node(&current, "Person");

        create_test_edge(&current, n1, n2, "KNOWS");
        create_test_edge(&current, n3, n1, "LIKES");

        let input = Box::new(NodeLookupIterator::new(vec![n1], current.clone()));
        let mut iter = TraversalIterator::new(
            input,
            Direction::Both,
            None,
            1,
            current.clone(),
            historical.clone(),
            None,
        );

        let mut results = Vec::new();
        while let Some(Ok(row)) = iter.next() {
            results.push(row.entity.node_id().unwrap());
        }
        results.sort();
        assert_eq!(results, vec![n2, n3]);
    }

    #[test]
    fn test_traversal_both_with_label() {
        let (current, historical) = setup_storage();
        let n1 = create_test_node(&current, "Person");
        let n2 = create_test_node(&current, "Person");
        let n3 = create_test_node(&current, "Person");
        let n4 = create_test_node(&current, "Person");

        create_test_edge(&current, n1, n2, "KNOWS");
        create_test_edge(&current, n3, n1, "KNOWS");
        create_test_edge(&current, n1, n4, "LIKES"); // Should be filtered out

        let input = Box::new(NodeLookupIterator::new(vec![n1], current.clone()));
        let mut iter = TraversalIterator::new(
            input,
            Direction::Both,
            Some("KNOWS".to_string()),
            1,
            current.clone(),
            historical.clone(),
            None,
        );

        let mut results = Vec::new();
        while let Some(Ok(row)) = iter.next() {
            results.push(row.entity.node_id().unwrap());
        }
        results.sort();
        assert_eq!(results, vec![n2, n3]);
    }
}
