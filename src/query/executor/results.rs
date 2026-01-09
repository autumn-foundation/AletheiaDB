//! Query Result Types
//!
//! Defines the result types returned by query execution.

use crate::core::graph::{Edge, Node};
use crate::core::temporal::Timestamp;
use crate::core::{EdgeId, NodeId};
use crate::utils::error::Result;

use super::iterators::ResultIterator;

/// Entity identifier (node or edge).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntityId {
    /// Node identifier
    Node(NodeId),
    /// Edge identifier
    Edge(EdgeId),
}

/// Query result entity (full node or edge).
#[derive(Debug, Clone)]
pub enum EntityResult {
    /// Full node data
    Node(Node),
    /// Full edge data
    Edge(Edge),
    /// Just a node ID (for efficiency when full data not needed)
    NodeId(NodeId),
    /// Just an edge ID
    EdgeId(EdgeId),
}

impl EntityResult {
    /// Get the entity ID
    #[must_use]
    pub fn id(&self) -> EntityId {
        match self {
            EntityResult::Node(n) => EntityId::Node(n.id),
            EntityResult::Edge(e) => EntityId::Edge(e.id),
            EntityResult::NodeId(id) => EntityId::Node(*id),
            EntityResult::EdgeId(id) => EntityId::Edge(*id),
        }
    }

    /// Try to get as a Node
    #[must_use]
    pub fn as_node(&self) -> Option<&Node> {
        match self {
            EntityResult::Node(n) => Some(n),
            _ => None,
        }
    }

    /// Try to get as an Edge
    #[must_use]
    pub fn as_edge(&self) -> Option<&Edge> {
        match self {
            EntityResult::Edge(e) => Some(e),
            _ => None,
        }
    }

    /// Try to get the NodeId
    #[must_use]
    pub fn node_id(&self) -> Option<NodeId> {
        match self {
            EntityResult::Node(n) => Some(n.id),
            EntityResult::NodeId(id) => Some(*id),
            _ => None,
        }
    }
}

/// A single row in query results.
#[derive(Debug, Clone)]
pub struct QueryRow {
    /// The primary entity (node or edge)
    pub entity: EntityResult,
    /// Similarity score (for vector queries)
    pub score: Option<f32>,
    /// Traversal path (for path queries)
    pub path: Option<Vec<EntityId>>,
    /// Timestamp (for temporal queries)
    pub timestamp: Option<Timestamp>,
}

impl QueryRow {
    /// Create a new row with just an entity
    #[must_use]
    pub fn from_entity(entity: EntityResult) -> Self {
        QueryRow {
            entity,
            score: None,
            path: None,
            timestamp: None,
        }
    }

    /// Create a row with entity and score
    #[must_use]
    pub fn with_score(entity: EntityResult, score: f32) -> Self {
        QueryRow {
            entity,
            score: Some(score),
            path: None,
            timestamp: None,
        }
    }

    /// Create a row with entity and path
    #[must_use]
    pub fn with_path(entity: EntityResult, path: Vec<EntityId>) -> Self {
        QueryRow {
            entity,
            score: None,
            path: Some(path),
            timestamp: None,
        }
    }

    /// Add a timestamp to this row
    #[must_use]
    pub fn at_time(mut self, timestamp: Timestamp) -> Self {
        self.timestamp = Some(timestamp);
        self
    }

    /// Get the entity as a node (if applicable)
    #[must_use]
    pub fn as_node(&self) -> Option<&Node> {
        self.entity.as_node()
    }

    /// Get the score (if applicable)
    #[must_use]
    pub fn score(&self) -> Option<f32> {
        self.score
    }
}

/// Collected query results.
///
/// This wraps a result iterator and provides convenience methods
/// for collecting and processing results.
pub struct QueryResults {
    iterator: Box<dyn ResultIterator>,
}

impl QueryResults {
    /// Create new query results from an iterator
    pub(crate) fn new(iterator: Box<dyn ResultIterator>) -> Self {
        QueryResults { iterator }
    }

    /// Collect all results into a vector, stopping on first error
    pub fn collect_all(mut self) -> Result<Vec<QueryRow>> {
        let mut results = Vec::new();
        while let Some(row) = self.iterator.next() {
            results.push(row?);
        }
        Ok(results)
    }

    /// Collect all nodes from results
    pub fn collect_nodes(self) -> Result<Vec<Node>> {
        let rows = self.collect_all()?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                if let EntityResult::Node(n) = row.entity {
                    Some(n)
                } else {
                    None
                }
            })
            .collect())
    }

    /// Collect nodes with their scores
    pub fn collect_nodes_with_scores(self) -> Result<Vec<(Node, f32)>> {
        let rows = self.collect_all()?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                if let (EntityResult::Node(n), Some(score)) = (row.entity, row.score) {
                    Some((n, score))
                } else {
                    None
                }
            })
            .collect())
    }

    /// Take at most n results
    pub fn take_n(mut self, n: usize) -> Result<Vec<QueryRow>> {
        let mut results = Vec::with_capacity(n);
        for _ in 0..n {
            match self.iterator.next() {
                Some(Ok(row)) => results.push(row),
                Some(Err(e)) => return Err(e),
                None => break,
            }
        }
        Ok(results)
    }

    /// Skip n results and take the rest
    pub fn skip_n(mut self, n: usize) -> Self {
        for _ in 0..n {
            if self.iterator.next().is_none() {
                break;
            }
        }
        self
    }

    /// Count the number of results
    pub fn count_all(self) -> Result<usize> {
        Ok(self.collect_all()?.len())
    }

    /// Check if there are any results
    pub fn is_empty_check(mut self) -> Result<bool> {
        Ok(self.iterator.next().is_none())
    }

    /// Get an estimate of the result size
    #[must_use]
    pub fn estimated_size(&self) -> (usize, Option<usize>) {
        self.iterator.size_hint()
    }
}

impl Iterator for QueryResults {
    type Item = Result<QueryRow>;

    fn next(&mut self) -> Option<Self::Item> {
        self.iterator.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iterator.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::VersionId;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;

    fn test_node(id: u64) -> Node {
        Node::new(
            NodeId::new(id).unwrap(),
            GLOBAL_INTERNER.intern("Test").unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        )
    }

    fn test_edge(id: u64) -> Edge {
        Edge::new(
            EdgeId::new(id).unwrap(),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        )
    }

    // Mock iterator for testing QueryResults
    struct MockIterator {
        items: std::vec::IntoIter<Result<QueryRow>>,
    }

    impl MockIterator {
        fn new(rows: Vec<QueryRow>) -> Self {
            MockIterator {
                items: rows.into_iter().map(Ok).collect::<Vec<_>>().into_iter(),
            }
        }

        fn with_error(mut rows: Vec<QueryRow>, error_at: usize) -> Self {
            let mut results: Vec<Result<QueryRow>> = Vec::new();
            for (i, row) in rows.drain(..).enumerate() {
                if i == error_at {
                    results.push(Err(crate::utils::error::Error::Other(
                        "Test error".to_string(),
                    )));
                }
                results.push(Ok(row));
            }
            if error_at >= results.len() {
                results.push(Err(crate::utils::error::Error::Other(
                    "Test error".to_string(),
                )));
            }
            MockIterator {
                items: results.into_iter(),
            }
        }
    }

    impl ResultIterator for MockIterator {
        fn next(&mut self) -> Option<Result<QueryRow>> {
            self.items.next()
        }

        fn size_hint(&self) -> (usize, Option<usize>) {
            self.items.size_hint()
        }
    }

    #[test]
    fn test_entity_result() {
        let node = test_node(1);
        let result = EntityResult::Node(node.clone());

        assert!(result.as_node().is_some());
        assert!(result.as_edge().is_none());
        assert_eq!(result.node_id(), Some(node.id));
    }

    #[test]
    fn test_entity_result_edge() {
        let edge = test_edge(1);
        let result = EntityResult::Edge(edge.clone());

        assert!(result.as_edge().is_some());
        assert!(result.as_node().is_none());
        assert_eq!(result.node_id(), None);

        match result.id() {
            EntityId::Edge(id) => assert_eq!(id, edge.id),
            EntityId::Node(_) => panic!("Expected Edge"),
        }
    }

    #[test]
    fn test_entity_result_edge_id() {
        let edge_id = EdgeId::new(1).unwrap();
        let result = EntityResult::EdgeId(edge_id);

        assert!(result.as_edge().is_none());
        assert!(result.as_node().is_none());
        assert_eq!(result.node_id(), None);

        match result.id() {
            EntityId::Edge(id) => assert_eq!(id, edge_id),
            EntityId::Node(_) => panic!("Expected Edge"),
        }
    }

    #[test]
    fn test_query_row() {
        let node = test_node(1);
        let row = QueryRow::from_entity(EntityResult::Node(node));

        assert!(row.score.is_none());
        assert!(row.path.is_none());
        assert!(row.timestamp.is_none());
        assert!(row.as_node().is_some());
    }

    #[test]
    fn test_query_row_with_score() {
        let node = test_node(1);
        let row = QueryRow::with_score(EntityResult::Node(node), 0.95);

        assert_eq!(row.score(), Some(0.95));
    }

    #[test]
    fn test_query_row_with_path() {
        let node = test_node(1);
        let path = vec![
            EntityId::Node(NodeId::new(1).unwrap()),
            EntityId::Node(NodeId::new(2).unwrap()),
        ];
        let row = QueryRow::with_path(EntityResult::Node(node), path.clone());

        assert!(row.score.is_none());
        assert_eq!(row.path, Some(path));
    }

    #[test]
    fn test_query_row_at_time() {
        let node = test_node(1);
        let row = QueryRow::from_entity(EntityResult::Node(node)).at_time(12345);

        assert_eq!(row.timestamp, Some(12345));
    }

    #[test]
    fn test_entity_id() {
        let node_id = NodeId::new(1).unwrap();
        let entity = EntityResult::NodeId(node_id);

        match entity.id() {
            EntityId::Node(id) => assert_eq!(id, node_id),
            EntityId::Edge(_) => panic!("Expected Node"),
        }
    }

    #[test]
    fn test_query_results_collect_all() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Node(test_node(2))),
            QueryRow::from_entity(EntityResult::Node(test_node(3))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let collected = results.collect_all().unwrap();
        assert_eq!(collected.len(), 3);
    }

    #[test]
    fn test_query_results_collect_nodes() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(2).unwrap())), // Not a full node
            QueryRow::from_entity(EntityResult::Node(test_node(3))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let nodes = results.collect_nodes().unwrap();
        assert_eq!(nodes.len(), 2); // Only full nodes
    }

    #[test]
    fn test_query_results_collect_nodes_with_scores() {
        let rows = vec![
            QueryRow::with_score(EntityResult::Node(test_node(1)), 0.9),
            QueryRow::from_entity(EntityResult::Node(test_node(2))), // No score
            QueryRow::with_score(EntityResult::Node(test_node(3)), 0.8),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let nodes_with_scores = results.collect_nodes_with_scores().unwrap();
        assert_eq!(nodes_with_scores.len(), 2); // Only nodes with scores
        assert_eq!(nodes_with_scores[0].1, 0.9);
        assert_eq!(nodes_with_scores[1].1, 0.8);
    }

    #[test]
    fn test_query_results_take_n() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Node(test_node(2))),
            QueryRow::from_entity(EntityResult::Node(test_node(3))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let taken = results.take_n(2).unwrap();
        assert_eq!(taken.len(), 2);
    }

    #[test]
    fn test_query_results_take_n_more_than_available() {
        let rows = vec![QueryRow::from_entity(EntityResult::Node(test_node(1)))];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let taken = results.take_n(10).unwrap();
        assert_eq!(taken.len(), 1);
    }

    #[test]
    fn test_query_results_skip_n() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Node(test_node(2))),
            QueryRow::from_entity(EntityResult::Node(test_node(3))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let remaining = results.skip_n(1).collect_all().unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn test_query_results_skip_n_all() {
        let rows = vec![QueryRow::from_entity(EntityResult::Node(test_node(1)))];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let remaining = results.skip_n(10).collect_all().unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_query_results_count_all() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Node(test_node(2))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let count = results.count_all().unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_query_results_is_empty_check_false() {
        let rows = vec![QueryRow::from_entity(EntityResult::Node(test_node(1)))];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        assert!(!results.is_empty_check().unwrap());
    }

    #[test]
    fn test_query_results_is_empty_check_true() {
        let rows: Vec<QueryRow> = vec![];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        assert!(results.is_empty_check().unwrap());
    }

    #[test]
    fn test_query_results_estimated_size() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Node(test_node(2))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let (min, max) = results.estimated_size();
        assert!(min <= 2);
        assert!(max.is_some());
    }

    #[test]
    fn test_query_results_iterator() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Node(test_node(2))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let mut count = 0;
        for row in results {
            assert!(row.is_ok());
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn test_query_results_size_hint() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Node(test_node(2))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let (min, max) = results.size_hint();
        assert!(min <= 2);
        assert!(max.is_some());
    }

    #[test]
    fn test_query_results_collect_all_with_error() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Node(test_node(2))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::with_error(rows, 1)));

        let result = results.collect_all();
        assert!(result.is_err());
    }

    #[test]
    fn test_query_results_take_n_with_error() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Node(test_node(2))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::with_error(rows, 0)));

        let result = results.take_n(5);
        assert!(result.is_err());
    }
}
