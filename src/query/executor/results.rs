//! Query Result Types
//!
//! Defines the result types returned by query execution.

use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
use crate::core::id::VersionId;
use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use crate::core::{EdgeId, NodeId};

use super::iterators::ResultIterator;

/// A path through the graph, represented as a sequence of entity IDs.
pub type Path = Vec<EntityId>;

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

/// Aggregated query results in a structured format.
///
/// Unlike `QueryResults` which provides streaming access through an iterator,
/// `QueryResult` is a concrete struct that holds all results in structured vectors.
/// This is useful for batch operations or when you want to inspect all results
/// with their associated metadata at once.
///
/// # Example
///
/// ```ignore
/// let result = query_results.collect_structured()?;
/// println!("Found {} nodes", result.nodes.len());
/// if let Some(scores) = &result.scores {
///     println!("Top score: {}", scores[0]);
/// }
/// ```
///
/// # Padding Behavior
///
/// When collecting structured results from hybrid queries where not all rows have all fields,
/// missing fields are padded with sensible defaults to maintain vector alignment:
/// - Missing scores are padded with `0.0`
/// - Missing paths are padded with empty vectors (`vec![]`)
/// - Missing versions are padded with `VersionId(0)`
///
/// **Important**: Users should check the source query type to distinguish padding from actual values.
/// For example, a score of `0.0` could be either a padding value or an actual similarity score of zero.
/// The padding only occurs when at least one row has the field present.
///
/// # Performance Considerations
///
/// This method collects **ALL** results into memory before returning. For large result sets
/// (>100K rows), prefer using the streaming `QueryResults` iterator directly. This method
/// is optimized for batch operations on moderate result sets where you need random access
/// to all results with their metadata.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// Node IDs from the query results
    pub nodes: Vec<NodeId>,
    /// Properties for the results (if full nodes were requested)
    pub properties: Option<Vec<PropertyMap>>,
    /// Similarity scores for ranked results (e.g., vector search)
    pub scores: Option<Vec<f32>>,
    /// Paths for traversal results
    pub paths: Option<Vec<Path>>,
    /// Version IDs for temporal results
    pub versions: Option<Vec<VersionId>>,
}

impl QueryResult {
    /// Create a new empty query result
    #[must_use]
    pub fn new() -> Self {
        QueryResult {
            nodes: Vec::new(),
            properties: None,
            scores: None,
            paths: None,
            versions: None,
        }
    }

    /// Create a query result with just nodes
    #[must_use]
    pub fn with_nodes(nodes: Vec<NodeId>) -> Self {
        QueryResult {
            nodes,
            properties: None,
            scores: None,
            paths: None,
            versions: None,
        }
    }

    /// Add properties to this result
    #[must_use]
    pub fn with_properties(mut self, properties: Vec<PropertyMap>) -> Self {
        self.properties = Some(properties);
        self
    }

    /// Add scores to this result
    #[must_use]
    pub fn with_scores(mut self, scores: Vec<f32>) -> Self {
        self.scores = Some(scores);
        self
    }

    /// Add paths to this result
    #[must_use]
    pub fn with_paths(mut self, paths: Vec<Path>) -> Self {
        self.paths = Some(paths);
        self
    }

    /// Add versions to this result
    #[must_use]
    pub fn with_versions(mut self, versions: Vec<VersionId>) -> Self {
        self.versions = Some(versions);
        self
    }

    /// Get the number of results
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if the result is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Get nodes with their scores (if available)
    #[must_use]
    pub fn nodes_with_scores(&self) -> Option<Vec<(NodeId, f32)>> {
        self.scores.as_ref().map(|scores| {
            self.nodes
                .iter()
                .zip(scores.iter())
                .map(|(node, score)| (*node, *score))
                .collect()
        })
    }

    /// Get nodes with their paths (if available)
    #[must_use]
    pub fn nodes_with_paths(&self) -> Option<Vec<(NodeId, &Path)>> {
        self.paths.as_ref().map(|paths| {
            self.nodes
                .iter()
                .zip(paths.iter())
                .map(|(node, path)| (*node, path))
                .collect()
        })
    }

    /// Get nodes with their versions (if available)
    #[must_use]
    pub fn nodes_with_versions(&self) -> Option<Vec<(NodeId, VersionId)>> {
        self.versions.as_ref().map(|versions| {
            self.nodes
                .iter()
                .zip(versions.iter())
                .map(|(node, version)| (*node, *version))
                .collect()
        })
    }
}

impl Default for QueryResult {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for QueryResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.nodes.is_empty() {
            return write!(f, "QueryResult {{ 0 items }}");
        }

        let mut table = comfy_table::Table::new();
        table.load_preset(comfy_table::presets::UTF8_FULL);
        table.apply_modifier(comfy_table::modifiers::UTF8_ROUND_CORNERS);

        // Dynamic headers
        let mut headers = vec!["Node ID"];
        if self.scores.is_some() {
            headers.push("Score");
        }
        if self.properties.is_some() {
            headers.push("Properties");
        }
        if self.paths.is_some() {
            headers.push("Path");
        }
        if self.versions.is_some() {
            headers.push("Version");
        }
        table.set_header(headers);

        // Data rows
        for (i, node_id) in self.nodes.iter().enumerate() {
            let mut row = comfy_table::Row::new();

            // Node ID (Cyan)
            row.add_cell(comfy_table::Cell::new(node_id).fg(comfy_table::Color::Cyan));

            // Score
            if let Some(scores) = &self.scores {
                if let Some(score) = scores.get(i) {
                    let mut cell = comfy_table::Cell::new(format!("{:.4}", score));
                    if *score > 0.8 {
                        cell = cell.fg(comfy_table::Color::Green);
                    } else if *score < 0.5 {
                        cell = cell.fg(comfy_table::Color::Yellow);
                    }
                    row.add_cell(cell);
                } else {
                    row.add_cell(comfy_table::Cell::new("-"));
                }
            }

            // Properties
            if let Some(props) = &self.properties {
                if let Some(map) = props.get(i) {
                    // Format map compactly
                    let mut parts: Vec<String> = map
                        .iter()
                        .take(5) // Limit to 5 properties
                        .map(|(k, v)| {
                            // resolve key
                            let k_str = crate::core::interning::GLOBAL_INTERNER
                                .resolve_with(*k, |s| s.to_string())
                                .unwrap_or_else(|| format!("key:{}", k.as_u32()));
                            format!("{}: {}", k_str, v)
                        })
                        .collect();

                    if map.len() > 5 {
                        parts.push(format!("... (+{})", map.len() - 5));
                    }

                    let content = if parts.is_empty() {
                        "{}".to_string()
                    } else {
                        format!("{{ {} }}", parts.join(", "))
                    };
                    row.add_cell(comfy_table::Cell::new(content));
                } else {
                    row.add_cell(comfy_table::Cell::new("{}"));
                }
            }

            // Paths
            if let Some(paths) = &self.paths {
                if let Some(path) = paths.get(i) {
                    row.add_cell(comfy_table::Cell::new(format!("len: {}", path.len())));
                } else {
                    row.add_cell(comfy_table::Cell::new("-"));
                }
            }

            // Versions
            if let Some(versions) = &self.versions {
                if let Some(ver) = versions.get(i) {
                    row.add_cell(comfy_table::Cell::new(format!("{:?}", ver)));
                } else {
                    row.add_cell(comfy_table::Cell::new("-"));
                }
            }

            table.add_row(row);
        }

        write!(f, "{}", table)
    }
}

impl QueryResults {
    /// Collect all results into a structured `QueryResult`.
    ///
    /// This method aggregates all query rows into separate vectors for nodes, scores,
    /// paths, and versions. Only fields that are present in at least one row will be
    /// included in the result.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Any row fails to be read from the iterator
    /// - A timestamp cannot be converted to a valid `VersionId` (exceeds `MAX_VALID_ID`)
    ///
    /// # Performance
    ///
    /// This method performs two passes over the data:
    /// 1. Collects all rows into memory to determine which fields are present
    /// 2. Extracts and pads data into separate vectors
    ///
    /// For very large result sets, this may consume significant memory. Consider using
    /// the streaming iterator directly for result sets exceeding 100K rows.
    pub fn collect_structured(mut self) -> Result<QueryResult> {
        // First pass: collect all rows
        let mut rows = Vec::new();
        while let Some(row) = self.iterator.next() {
            rows.push(row?);
        }

        // Determine which fields we have
        let has_any_scores = rows.iter().any(|r| r.score.is_some());
        let has_any_paths = rows.iter().any(|r| r.path.is_some());
        let has_any_versions = rows.iter().any(|r| r.timestamp.is_some());
        let has_any_nodes = rows.iter().any(|r| r.entity.as_node().is_some());

        // Second pass: extract data with padding
        let mut nodes = Vec::new();
        let mut properties = if has_any_nodes {
            Some(Vec::new())
        } else {
            None
        };
        let mut scores = if has_any_scores {
            Some(Vec::new())
        } else {
            None
        };
        let mut paths = if has_any_paths {
            Some(Vec::new())
        } else {
            None
        };
        let mut versions = if has_any_versions {
            Some(Vec::new())
        } else {
            None
        };

        for row in rows {
            // Extract node ID
            if let Some(node_id) = row.entity.node_id() {
                nodes.push(node_id);
                // Extract properties if we are collecting them
                if let Some(ref mut props) = properties {
                    let map = row
                        .entity
                        .as_node()
                        .map(|n| n.properties.clone())
                        .unwrap_or_default();
                    props.push(map);
                }
            }

            // Extract or pad scores
            if let Some(ref mut s) = scores {
                s.push(row.score.unwrap_or(0.0));
            }

            // Extract or pad paths
            if let Some(ref mut p) = paths {
                p.push(row.path.unwrap_or_default());
            }

            // Extract or pad versions
            if let Some(ref mut v) = versions {
                if let Some(timestamp) = row.timestamp {
                    // Safely convert timestamp (i64) to VersionId (u64)
                    // Negative timestamps are clamped to 0
                    // Phase 2: Use wallclock component for version ID
                    let wallclock = timestamp.wallclock();
                    let ts_u64 = if wallclock < 0 {
                        0_u64
                    } else {
                        wallclock as u64
                    };

                    // VersionId::new validates against MAX_VALID_ID
                    let version_id = VersionId::new(ts_u64).map_err(|e| {
                        crate::core::error::Error::Query(
                            crate::core::error::QueryError::InvalidParameter {
                                parameter: "timestamp".to_string(),
                                reason: format!(
                                    "Timestamp {} exceeds MAX_VALID_ID: {}",
                                    timestamp, e
                                ),
                            },
                        )
                    })?;
                    v.push(version_id);
                } else {
                    // Pad with version 0 for missing timestamps
                    // SAFETY: VersionId(0) is always valid as 0 < MAX_VALID_ID
                    v.push(VersionId::new(0).expect("VersionId(0) should always be valid"));
                }
            }
        }

        Ok(QueryResult {
            nodes,
            properties,
            scores,
            paths,
            versions,
        })
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
                    results.push(Err(crate::core::error::Error::Other(
                        "Test error".to_string(),
                    )));
                }
                results.push(Ok(row));
            }
            if error_at >= results.len() {
                results.push(Err(crate::core::error::Error::Other(
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
        let row = QueryRow::from_entity(EntityResult::Node(node)).at_time(12345.into());

        assert_eq!(row.timestamp, Some(12345.into()));
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

    // ==================== QueryResult Tests ====================

    #[test]
    fn test_query_result_new() {
        let result = QueryResult::new();
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(result.scores.is_none());
        assert!(result.paths.is_none());
        assert!(result.versions.is_none());
    }

    #[test]
    fn test_query_result_default() {
        let result = QueryResult::default();
        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_query_result_with_nodes() {
        let nodes = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
        let result = QueryResult::with_nodes(nodes.clone());

        assert_eq!(result.len(), 2);
        assert!(!result.is_empty());
        assert_eq!(result.nodes, nodes);
        assert!(result.scores.is_none());
        assert!(result.paths.is_none());
        assert!(result.versions.is_none());
    }

    #[test]
    fn test_query_result_with_scores() {
        let nodes = vec![NodeId::new(1).unwrap()];
        let scores = vec![0.95];
        let result = QueryResult::with_nodes(nodes).with_scores(scores.clone());

        assert!(result.scores.is_some());
        assert_eq!(result.scores.unwrap(), scores);
    }

    #[test]
    fn test_query_result_with_paths() {
        let nodes = vec![NodeId::new(1).unwrap()];
        let path = vec![EntityId::Node(NodeId::new(1).unwrap())];
        let paths = vec![path.clone()];
        let result = QueryResult::with_nodes(nodes).with_paths(paths.clone());

        assert!(result.paths.is_some());
        assert_eq!(result.paths.as_ref().unwrap()[0], path);
    }

    #[test]
    fn test_query_result_with_versions() {
        let nodes = vec![NodeId::new(1).unwrap()];
        let versions = vec![VersionId::new(100).unwrap()];
        let result = QueryResult::with_nodes(nodes).with_versions(versions.clone());

        assert!(result.versions.is_some());
        assert_eq!(result.versions.unwrap(), versions);
    }

    #[test]
    fn test_query_result_builder_pattern() {
        let nodes = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
        let scores = vec![0.9, 0.8];
        let path1 = vec![EntityId::Node(NodeId::new(1).unwrap())];
        let path2 = vec![EntityId::Node(NodeId::new(2).unwrap())];
        let paths = vec![path1, path2];
        let versions = vec![VersionId::new(100).unwrap(), VersionId::new(101).unwrap()];

        let result = QueryResult::with_nodes(nodes.clone())
            .with_scores(scores.clone())
            .with_paths(paths.clone())
            .with_versions(versions.clone());

        assert_eq!(result.len(), 2);
        assert_eq!(result.nodes, nodes);
        assert_eq!(result.scores, Some(scores));
        assert_eq!(result.paths, Some(paths));
        assert_eq!(result.versions, Some(versions));
    }

    #[test]
    fn test_query_result_nodes_with_scores() {
        let nodes = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
        let scores = vec![0.9, 0.8];
        let result = QueryResult::with_nodes(nodes.clone()).with_scores(scores.clone());

        let pairs = result.nodes_with_scores();
        assert!(pairs.is_some());

        let pairs = pairs.unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], (nodes[0], scores[0]));
        assert_eq!(pairs[1], (nodes[1], scores[1]));
    }

    #[test]
    fn test_query_result_nodes_with_scores_none() {
        let nodes = vec![NodeId::new(1).unwrap()];
        let result = QueryResult::with_nodes(nodes);

        assert!(result.nodes_with_scores().is_none());
    }

    #[test]
    fn test_query_result_nodes_with_paths() {
        let nodes = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
        let path1 = vec![EntityId::Node(NodeId::new(1).unwrap())];
        let path2 = vec![EntityId::Node(NodeId::new(2).unwrap())];
        let paths = vec![path1.clone(), path2.clone()];
        let result = QueryResult::with_nodes(nodes.clone()).with_paths(paths);

        let pairs = result.nodes_with_paths();
        assert!(pairs.is_some());

        let pairs = pairs.unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, nodes[0]);
        assert_eq!(pairs[0].1, &path1);
        assert_eq!(pairs[1].0, nodes[1]);
        assert_eq!(pairs[1].1, &path2);
    }

    #[test]
    fn test_query_result_nodes_with_paths_none() {
        let nodes = vec![NodeId::new(1).unwrap()];
        let result = QueryResult::with_nodes(nodes);

        assert!(result.nodes_with_paths().is_none());
    }

    #[test]
    fn test_query_result_nodes_with_versions() {
        let nodes = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
        let versions = vec![VersionId::new(100).unwrap(), VersionId::new(101).unwrap()];
        let result = QueryResult::with_nodes(nodes.clone()).with_versions(versions.clone());

        let pairs = result.nodes_with_versions();
        assert!(pairs.is_some());

        let pairs = pairs.unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], (nodes[0], versions[0]));
        assert_eq!(pairs[1], (nodes[1], versions[1]));
    }

    #[test]
    fn test_query_result_nodes_with_versions_none() {
        let nodes = vec![NodeId::new(1).unwrap()];
        let result = QueryResult::with_nodes(nodes);

        assert!(result.nodes_with_versions().is_none());
    }

    #[test]
    fn test_query_result_display() {
        let nodes = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
        let scores = vec![0.9, 0.8];
        let result = QueryResult::with_nodes(nodes).with_scores(scores);

        let display = format!("{}", result);
        // Check for headers
        assert!(display.contains("Node ID"));
        assert!(display.contains("Score"));

        // Check for data
        assert!(display.contains("0.9000"));
        assert!(display.contains("0.8000"));
    }

    #[test]
    fn test_query_result_display_empty() {
        let result = QueryResult::new();
        let display = format!("{}", result);

        assert!(display.contains("QueryResult"));
        assert!(display.contains("0 items"));
    }

    #[test]
    fn test_query_result_display_with_paths() {
        let nodes = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
        let path1 = vec![
            EntityId::Node(NodeId::new(1).unwrap()),
            EntityId::Node(NodeId::new(2).unwrap()),
        ];
        let path2 = vec![
            EntityId::Node(NodeId::new(3).unwrap()),
            EntityId::Node(NodeId::new(4).unwrap()),
            EntityId::Node(NodeId::new(5).unwrap()),
        ];
        let paths = vec![path1, path2];
        let result = QueryResult::with_nodes(nodes).with_paths(paths);

        let display = format!("{}", result);
        assert!(display.contains("Path")); // Header
        assert!(display.contains("len: 2")); // Path 1 length
        assert!(display.contains("len: 3")); // Path 2 length
    }

    #[test]
    fn test_collect_structured_nodes_only() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(1).unwrap())),
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(2).unwrap())),
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(3).unwrap())),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured().unwrap();
        assert_eq!(structured.len(), 3);
        assert_eq!(structured.nodes[0], NodeId::new(1).unwrap());
        assert_eq!(structured.nodes[1], NodeId::new(2).unwrap());
        assert_eq!(structured.nodes[2], NodeId::new(3).unwrap());
        assert!(structured.scores.is_none());
        assert!(structured.paths.is_none());
        assert!(structured.versions.is_none());
    }

    #[test]
    fn test_collect_structured_with_scores() {
        let rows = vec![
            QueryRow::with_score(EntityResult::NodeId(NodeId::new(1).unwrap()), 0.9),
            QueryRow::with_score(EntityResult::NodeId(NodeId::new(2).unwrap()), 0.8),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured().unwrap();
        assert_eq!(structured.len(), 2);
        assert!(structured.scores.is_some());

        let scores = structured.scores.unwrap();
        assert_eq!(scores.len(), 2);
        assert_eq!(scores[0], 0.9);
        assert_eq!(scores[1], 0.8);
    }

    #[test]
    fn test_collect_structured_with_paths() {
        let path1 = vec![
            EntityId::Node(NodeId::new(1).unwrap()),
            EntityId::Node(NodeId::new(2).unwrap()),
        ];
        let path2 = vec![EntityId::Node(NodeId::new(3).unwrap())];
        let rows = vec![
            QueryRow::with_path(EntityResult::NodeId(NodeId::new(1).unwrap()), path1.clone()),
            QueryRow::with_path(EntityResult::NodeId(NodeId::new(2).unwrap()), path2.clone()),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured().unwrap();
        assert_eq!(structured.len(), 2);
        assert!(structured.paths.is_some());

        let paths = structured.paths.unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], path1);
        assert_eq!(paths[1], path2);
    }

    #[test]
    fn test_collect_structured_with_timestamps() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(1).unwrap()))
                .at_time(100.into()),
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(2).unwrap()))
                .at_time(200.into()),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured().unwrap();
        assert_eq!(structured.len(), 2);
        assert!(structured.versions.is_some());

        let versions = structured.versions.unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0], VersionId::new(100).unwrap());
        assert_eq!(versions[1], VersionId::new(200).unwrap());
    }

    #[test]
    fn test_collect_structured_hybrid() {
        let path = vec![EntityId::Node(NodeId::new(1).unwrap())];
        let rows = vec![
            QueryRow::with_score(EntityResult::NodeId(NodeId::new(1).unwrap()), 0.95)
                .at_time(100.into()),
            QueryRow::with_path(EntityResult::NodeId(NodeId::new(2).unwrap()), path.clone()),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured().unwrap();
        assert_eq!(structured.len(), 2);

        // Should have scores (with padding for second row)
        assert!(structured.scores.is_some());
        let scores = structured.scores.as_ref().unwrap();
        assert_eq!(scores[0], 0.95);
        assert_eq!(scores[1], 0.0); // Padded

        // Should have paths (with padding for first row)
        assert!(structured.paths.is_some());
        let paths = structured.paths.as_ref().unwrap();
        assert!(paths[0].is_empty()); // Padded
        assert_eq!(paths[1], path);

        // Should have versions (with padding for second row)
        assert!(structured.versions.is_some());
        let versions = structured.versions.as_ref().unwrap();
        assert_eq!(versions[0], VersionId::new(100).unwrap());
        assert_eq!(versions[1], VersionId::new(0).unwrap()); // Padded
    }

    #[test]
    fn test_collect_structured_empty() {
        let rows: Vec<QueryRow> = vec![];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured().unwrap();
        assert!(structured.is_empty());
        assert_eq!(structured.len(), 0);
        assert!(structured.scores.is_none());
        assert!(structured.paths.is_none());
        assert!(structured.versions.is_none());
    }

    #[test]
    fn test_path_type_alias() {
        let path: Path = vec![
            EntityId::Node(NodeId::new(1).unwrap()),
            EntityId::Node(NodeId::new(2).unwrap()),
        ];
        assert_eq!(path.len(), 2);
    }

    // ==================== Error Handling Tests ====================

    #[test]
    fn test_collect_structured_with_negative_timestamp() {
        // Negative timestamps should be clamped to 0
        let rows = vec![
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(1).unwrap()))
                .at_time((-100i64).into()),
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(2).unwrap()))
                .at_time((-50i64).into()),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured().unwrap();
        assert_eq!(structured.len(), 2);
        assert!(structured.versions.is_some());

        // Both negative timestamps should be clamped to VersionId(0)
        let versions = structured.versions.unwrap();
        assert_eq!(versions[0], VersionId::new(0).unwrap());
        assert_eq!(versions[1], VersionId::new(0).unwrap());
    }

    #[test]
    fn test_collect_structured_with_large_valid_timestamp() {
        // Test with large timestamps that are valid
        // Use values well below i64::MAX to ensure they're valid
        let large_ts = i64::MAX / 2;
        let rows = vec![
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(1).unwrap()))
                .at_time(large_ts.into()),
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(2).unwrap()))
                .at_time((large_ts - 1000).into()),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured().unwrap();
        assert_eq!(structured.len(), 2);
        assert!(structured.versions.is_some());

        let versions = structured.versions.unwrap();
        assert_eq!(versions[0], VersionId::new(large_ts as u64).unwrap());
        assert_eq!(
            versions[1],
            VersionId::new((large_ts - 1000) as u64).unwrap()
        );
    }

    #[test]
    fn test_collect_structured_with_invalid_timestamp() {
        // Create a timestamp that will exceed MAX_VALID_ID when cast to u64
        // Since MAX_VALID_ID = u64::MAX - 1000, we need i64::MAX which becomes
        // 9223372036854775807 as u64, which is less than MAX_VALID_ID
        // So we create a custom row with an artificially large timestamp

        // Note: Since Timestamp is i64, the maximum positive value is i64::MAX
        // which as u64 is 9223372036854775807, which is actually < MAX_VALID_ID
        // This means we can't actually create an invalid timestamp through normal means
        // The invalid case would only occur with corrupted data or future changes

        // For now, let's test that very large timestamps still work
        let rows = vec![
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(1).unwrap()))
                .at_time((i64::MAX - 1000).into()),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let result = results.collect_structured();
        // Should succeed since i64::MAX as u64 < MAX_VALID_ID
        assert!(result.is_ok());
    }

    #[test]
    fn test_query_result_display_with_nan_scores() {
        let nodes = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
        let scores = vec![f32::NAN, 0.8];
        let result = QueryResult::with_nodes(nodes).with_scores(scores);

        let display = format!("{}", result);
        assert!(display.contains("Score"));
        assert!(display.contains("NaN"));
        assert!(display.contains("0.8000"));
    }

    #[test]
    fn test_query_result_display_with_infinity_scores() {
        let nodes = vec![NodeId::new(1).unwrap()];
        let scores = vec![f32::INFINITY];
        let result = QueryResult::with_nodes(nodes).with_scores(scores);

        let display = format!("{}", result);
        assert!(display.contains("Score"));
        assert!(display.contains("inf"));
    }

    #[test]
    fn test_collect_structured_mixed_timestamps() {
        // Test mixed positive, negative, and zero timestamps
        let rows = vec![
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(1).unwrap()))
                .at_time(100.into()),
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(2).unwrap()))
                .at_time((-50i64).into()),
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(3).unwrap())).at_time(0.into()),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured().unwrap();
        assert_eq!(structured.len(), 3);
        assert!(structured.versions.is_some());

        let versions = structured.versions.unwrap();
        assert_eq!(versions[0], VersionId::new(100).unwrap());
        assert_eq!(versions[1], VersionId::new(0).unwrap()); // Clamped
        assert_eq!(versions[2], VersionId::new(0).unwrap());
    }
}
