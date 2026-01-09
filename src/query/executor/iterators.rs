//! Result Iterators
//!
//! Pull-based iterators for query execution. Each physical operator
//! has a corresponding iterator that lazily produces results.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, RwLock};

#[cfg(feature = "observability")]
use tracing;

use crate::core::graph::Node;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyValue;
use crate::core::vector::cosine_similarity;
use crate::core::{NodeId, Timestamp};
use crate::query::ir::{Direction, Predicate, PredicateValue};
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::utils::error::Result;

use super::results::{EntityId, EntityResult, QueryRow};

/// Trait for result iteration (pull-based).
pub trait ResultIterator: Send {
    /// Get the next result row
    fn next(&mut self) -> Option<Result<QueryRow>>;

    /// Estimate the remaining results
    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}

/// Empty iterator that produces no results.
pub struct EmptyIterator;

impl ResultIterator for EmptyIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(0))
    }
}

/// Iterator for direct node lookups.
pub struct NodeLookupIterator {
    node_ids: std::vec::IntoIter<NodeId>,
    current: Arc<CurrentStorage>,
}

impl NodeLookupIterator {
    pub fn new(node_ids: Vec<NodeId>, current: Arc<CurrentStorage>) -> Self {
        NodeLookupIterator {
            node_ids: node_ids.into_iter(),
            current,
        }
    }
}

impl ResultIterator for NodeLookupIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.node_ids.next().map(|id| {
            self.current
                .get_node(id)
                .map(|node| QueryRow::from_entity(EntityResult::Node(node)))
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.node_ids.size_hint()
    }
}

/// Iterator for node scans with optional label filter.
///
/// # Memory Considerations
///
/// **WARNING**: This iterator collects all node IDs into a `Vec` upfront during
/// initialization. For very large graphs (millions of nodes), this can cause:
///
/// - **High memory consumption**: O(n) where n = number of nodes
/// - **Initial latency**: Delay before the first result is produced
///
/// This design is a trade-off due to the `Send` bound on `ResultIterator` and
/// the fact that DashMap's iterators hold internal locks that cannot be sent
/// across threads. The current implementation prioritizes correctness and
/// simplicity over optimal memory usage for full scans.
///
/// ## Mitigation Strategies
///
/// For production workloads with large graphs:
/// 1. **Use label filters** - `scan(Some("Person"))` limits the scan scope
/// 2. **Use LIMIT** - Add `.limit(n)` to queries to enable early termination
/// 3. **Prefer targeted queries** - Use `start(node_id)` instead of full scans
///
/// ## Future Improvements (Issue #307)
///
/// Possible optimizations include:
/// - Streaming iteration using channels (`std::sync::mpsc`)
/// - Chunked iteration to limit memory per batch
/// - Index-based iteration that doesn't require holding locks
pub struct NodeScanIterator {
    label: Option<String>,
    current: Arc<CurrentStorage>,
    initialized: bool,
    node_ids: Option<std::vec::IntoIter<NodeId>>,
}

impl NodeScanIterator {
    pub fn new(label: Option<String>, current: Arc<CurrentStorage>) -> Self {
        NodeScanIterator {
            label,
            current,
            initialized: false,
            node_ids: None,
        }
    }

    fn initialize(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;

        // Collect all node IDs upfront.
        //
        // NOTE: This is a known memory concern for large graphs. See the struct
        // documentation above for details and mitigation strategies.
        //
        // The current implementation trades memory efficiency for correctness:
        // DashMap iterators cannot be sent across threads (not Send), and the
        // ResultIterator trait requires Send for parallel query execution.
        let ids: Vec<NodeId> = self.current.get_all_node_ids();
        self.node_ids = Some(ids.into_iter());
    }
}

impl ResultIterator for NodeScanIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.initialize();

        loop {
            match self.node_ids.as_mut()?.next() {
                Some(id) => {
                    match self.current.get_node(id) {
                        Ok(node) => {
                            // Check label filter by comparing InternedString IDs
                            if let Some(ref label_str) = self.label {
                                // Get the InternedString ID for the filter label
                                let label_id = GLOBAL_INTERNER.get_id(label_str);
                                if label_id != Some(node.label) {
                                    continue; // Skip this node
                                }
                            }
                            return Some(Ok(QueryRow::from_entity(EntityResult::Node(node))));
                        }
                        Err(e) => return Some(Err(e)),
                    }
                }
                None => return None,
            }
        }
    }
}

/// Iterator for vector search results.
pub struct VectorResultIterator {
    results: std::vec::IntoIter<(NodeId, f32)>,
    current: Arc<CurrentStorage>,
}

impl VectorResultIterator {
    pub fn new(results: Vec<(NodeId, f32)>, current: Arc<CurrentStorage>) -> Self {
        VectorResultIterator {
            results: results.into_iter(),
            current,
        }
    }
}

impl ResultIterator for VectorResultIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.results.next().map(|(node_id, score)| {
            self.current
                .get_node(node_id)
                .map(|node| QueryRow::with_score(EntityResult::Node(node), score))
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.results.size_hint()
    }
}

/// Iterator for temporal node lookups.
///
/// This iterator reconstructs nodes at a specific point in bi-temporal time
/// by querying the historical storage for the appropriate version and
/// reconstructing properties using the anchor+delta compression strategy.
///
/// The reconstruction process:
/// 1. Find the version valid at the requested (valid_time, transaction_time)
/// 2. Reconstruct properties from the version using anchor+delta
/// 3. Return a Node with the historical label and properties
pub struct TemporalNodeIterator {
    node_ids: std::vec::IntoIter<NodeId>,
    valid_time: Timestamp,
    transaction_time: Timestamp,
    #[allow(dead_code)]
    current: Arc<CurrentStorage>,
    historical: Arc<RwLock<HistoricalStorage>>,
}

impl TemporalNodeIterator {
    pub fn new(
        node_ids: Vec<NodeId>,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
    ) -> Self {
        TemporalNodeIterator {
            node_ids: node_ids.into_iter(),
            valid_time,
            transaction_time,
            current,
            historical,
        }
    }
}

impl ResultIterator for TemporalNodeIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.node_ids.next().map(|id| {
            // Look up the version valid at the requested time
            // Acquire read lock on historical storage
            let historical = match self.historical.read() {
                Ok(guard) => guard,
                Err(_) => {
                    return Err(crate::utils::error::StorageError::LockPoisoned {
                        lock_type: "historical_storage_read",
                    }
                    .into());
                }
            };

            match historical.find_node_version_at_time(id, self.valid_time, self.transaction_time) {
                Some(version_id) => {
                    // Get the version metadata
                    let version = match historical.get_node_version(version_id) {
                        Some(v) => v,
                        None => {
                            return Err(crate::utils::error::TemporalError::VersionNotFound(
                                version_id,
                            )
                            .into());
                        }
                    };

                    // Reconstruct the properties from the version
                    let properties = match historical.reconstruct_node_properties(version_id) {
                        Ok(props) => props,
                        Err(e) => return Err(e),
                    };

                    // Construct a node with the historical data
                    let node = Node::new(id, version.label, properties, version_id);

                    Ok(QueryRow::from_entity(EntityResult::Node(node)).at_time(self.valid_time))
                }
                None => {
                    // Node did not exist at the requested time - return not found
                    Err(crate::utils::error::TemporalError::NodeNotFoundAtTime {
                        node_id: id,
                        valid_time: self.valid_time,
                        transaction_time: self.transaction_time,
                    }
                    .into())
                }
            }
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.node_ids.size_hint()
    }
}

/// Iterator for graph traversal using BFS.
///
/// # Deduplication Semantics
///
/// The `visited` set is cleared for each new input node. This means:
/// - Each input node gets independent traversal results
/// - If multiple input nodes can reach the same target, it appears multiple times
/// - This is intentional for path-based semantics (e.g., "all friends of each person")
///
/// For global deduplication across all inputs, wrap the output in a `DistinctIterator`.
///
/// # Example
///
/// ```text
/// Input: [A, B]
/// Graph: A → C, B → C
///
/// Output: [C (from A), C (from B)]  // C appears twice
/// ```
pub struct TraversalIterator {
    input: Box<dyn ResultIterator>,
    direction: Direction,
    label: Option<String>,
    depth: usize,
    current: Arc<CurrentStorage>,
    // BFS state - reset for each input node (see doc comment above)
    frontier: VecDeque<(NodeId, Vec<EntityId>, usize)>,
    visited: HashSet<NodeId>,
    input_exhausted: bool,
}

impl TraversalIterator {
    pub fn new(
        input: Box<dyn ResultIterator>,
        direction: Direction,
        label: Option<String>,
        depth: usize,
        current: Arc<CurrentStorage>,
    ) -> Self {
        TraversalIterator {
            input,
            direction,
            label,
            depth,
            current,
            frontier: VecDeque::new(),
            visited: HashSet::new(),
            input_exhausted: false,
        }
    }

    fn get_neighbors(&self, node_id: NodeId) -> Vec<(NodeId, crate::core::EdgeId)> {
        match self.direction {
            Direction::Outgoing => {
                let edges = if let Some(ref label) = self.label {
                    self.current.get_outgoing_edges_with_label(node_id, label)
                } else {
                    self.current.get_outgoing_edges(node_id)
                };

                edges
                    .into_iter()
                    .filter_map(|edge_id| {
                        self.current
                            .get_edge(edge_id)
                            .ok()
                            .map(|e| (e.target, edge_id))
                    })
                    .collect()
            }
            Direction::Incoming => {
                let edges = if let Some(ref label) = self.label {
                    self.current.get_incoming_edges_with_label(node_id, label)
                } else {
                    self.current.get_incoming_edges(node_id)
                };

                edges
                    .into_iter()
                    .filter_map(|edge_id| {
                        self.current
                            .get_edge(edge_id)
                            .ok()
                            .map(|e| (e.source, edge_id))
                    })
                    .collect()
            }
            Direction::Both => {
                let mut neighbors = Vec::new();

                let out_edges = if let Some(ref label) = self.label {
                    self.current.get_outgoing_edges_with_label(node_id, label)
                } else {
                    self.current.get_outgoing_edges(node_id)
                };

                for edge_id in out_edges {
                    if let Ok(e) = self.current.get_edge(edge_id) {
                        neighbors.push((e.target, edge_id));
                    }
                }

                let in_edges = if let Some(ref label) = self.label {
                    self.current.get_incoming_edges_with_label(node_id, label)
                } else {
                    self.current.get_incoming_edges(node_id)
                };

                for edge_id in in_edges {
                    if let Ok(e) = self.current.get_edge(edge_id) {
                        neighbors.push((e.source, edge_id));
                    }
                }

                neighbors
            }
        }
    }
}

impl ResultIterator for TraversalIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        loop {
            // Process current frontier
            if let Some((node_id, path, current_depth)) = self.frontier.pop_front() {
                if current_depth >= self.depth {
                    // Reached target depth, yield result
                    match self.current.get_node(node_id) {
                        Ok(node) => {
                            return Some(Ok(QueryRow::with_path(EntityResult::Node(node), path)));
                        }
                        Err(e) => return Some(Err(e)),
                    }
                }

                // Expand neighbors
                let neighbors = self.get_neighbors(node_id);
                for (target, edge_id) in neighbors {
                    if self.visited.insert(target) {
                        let mut new_path = path.clone();
                        new_path.push(EntityId::Edge(edge_id));
                        new_path.push(EntityId::Node(target));
                        self.frontier
                            .push_back((target, new_path, current_depth + 1));
                    }
                }
                continue;
            }

            // Frontier exhausted, get next from input
            if self.input_exhausted {
                return None;
            }

            match self.input.next() {
                Some(Ok(row)) => {
                    if let Some(node_id) = row.entity.node_id() {
                        self.visited.clear();
                        self.visited.insert(node_id);
                        self.frontier
                            .push_back((node_id, vec![EntityId::Node(node_id)], 0));
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    self.input_exhausted = true;
                    // Process any remaining frontier
                    if self.frontier.is_empty() {
                        return None;
                    }
                }
            }
        }
    }
}

/// Iterator for filtering results.
pub struct FilterIterator {
    input: Box<dyn ResultIterator>,
    predicate: Predicate,
}

impl FilterIterator {
    pub fn new(input: Box<dyn ResultIterator>, predicate: Predicate) -> Self {
        FilterIterator { input, predicate }
    }

    fn evaluate(&self, node: &Node) -> bool {
        self.evaluate_predicate(&self.predicate, node)
    }

    fn evaluate_predicate(&self, predicate: &Predicate, node: &Node) -> bool {
        match predicate {
            Predicate::True => true,
            Predicate::False => false,

            Predicate::Eq { key, value } => {
                if let Some(prop) = node.properties.get(key) {
                    self.compare_eq(prop, value)
                } else {
                    false
                }
            }

            Predicate::Ne { key, value } => {
                if let Some(prop) = node.properties.get(key) {
                    !self.compare_eq(prop, value)
                } else {
                    true // Non-existent != anything
                }
            }

            Predicate::Gt { key, value } => {
                if let Some(prop) = node.properties.get(key) {
                    self.compare_gt(prop, value)
                } else {
                    false
                }
            }

            Predicate::Lt { key, value } => {
                if let Some(prop) = node.properties.get(key) {
                    self.compare_lt(prop, value)
                } else {
                    false
                }
            }

            Predicate::Gte { key, value } => {
                if let Some(prop) = node.properties.get(key) {
                    self.compare_gte(prop, value)
                } else {
                    false
                }
            }

            Predicate::Lte { key, value } => {
                if let Some(prop) = node.properties.get(key) {
                    self.compare_lte(prop, value)
                } else {
                    false
                }
            }

            Predicate::Exists(key) => node.properties.get(key).is_some(),

            Predicate::NotExists(key) => node.properties.get(key).is_none(),

            Predicate::Contains { key, substring } => {
                if let Some(PropertyValue::String(s)) = node.properties.get(key) {
                    s.contains(substring.as_str())
                } else {
                    false
                }
            }

            Predicate::StartsWith { key, prefix } => {
                if let Some(PropertyValue::String(s)) = node.properties.get(key) {
                    s.starts_with(prefix.as_str())
                } else {
                    false
                }
            }

            Predicate::EndsWith { key, suffix } => {
                if let Some(PropertyValue::String(s)) = node.properties.get(key) {
                    s.ends_with(suffix.as_str())
                } else {
                    false
                }
            }

            Predicate::In { key, values } => {
                if let Some(prop) = node.properties.get(key) {
                    values.iter().any(|v| self.compare_eq(prop, v))
                } else {
                    false
                }
            }

            Predicate::And(preds) => preds.iter().all(|p| self.evaluate_predicate(p, node)),

            Predicate::Or(preds) => preds.iter().any(|p| self.evaluate_predicate(p, node)),

            Predicate::Not(pred) => !self.evaluate_predicate(pred, node),
            // All variants covered - no default case needed
        }
    }

    fn compare_eq(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Bool(a), PredicateValue::Bool(b)) => a == b,
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a == b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => (a - b).abs() < f64::EPSILON,
            (PropertyValue::String(a), PredicateValue::String(b)) => a.as_ref() == b.as_str(),
            (PropertyValue::Null, PredicateValue::Null) => true,
            _ => false,
        }
    }

    fn compare_gt(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a > b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a > b,
            _ => false,
        }
    }

    fn compare_lt(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a < b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a < b,
            _ => false,
        }
    }

    fn compare_gte(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a >= b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a >= b,
            _ => false,
        }
    }

    fn compare_lte(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a <= b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a <= b,
            _ => false,
        }
    }
}

impl ResultIterator for FilterIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        loop {
            match self.input.next() {
                Some(Ok(row)) => {
                    if let Some(node) = row.entity.as_node() {
                        if self.evaluate(node) {
                            return Some(Ok(row));
                        }
                        // Filter didn't pass, continue to next
                    } else {
                        // Non-node entities pass through
                        return Some(Ok(row));
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
    }
}

/// Iterator for vector reranking.
pub struct VectorRerankIterator {
    sorted: Option<std::vec::IntoIter<(QueryRow, f32)>>,
    input: Option<Box<dyn ResultIterator>>,
    embedding: Arc<[f32]>,
    k: usize,
    _current: Arc<CurrentStorage>,
    /// Vector property name, or None if no vector index is configured
    vector_property: Option<String>,
}

impl VectorRerankIterator {
    pub fn new(
        input: Box<dyn ResultIterator>,
        embedding: Arc<[f32]>,
        k: usize,
        current: Arc<CurrentStorage>,
    ) -> Self {
        // Get the vector property name from the current storage
        // If no vector index is configured, we'll return an error on first next() call
        let vector_property = current.get_vector_property_name();

        VectorRerankIterator {
            sorted: None,
            input: Some(input),
            embedding,
            k,
            _current: current,
            vector_property,
        }
    }

    /// Compute similarity score for a query row if it has a vector property.
    /// Returns None if the node has no vector, or if the similarity is invalid (NaN/Inf).
    fn compute_similarity(&self, row: &QueryRow, vector_property: &str) -> Option<f32> {
        let node = row.entity.as_node()?;
        let PropertyValue::Vector(vec) = node.properties.get(vector_property)? else {
            return None;
        };
        let similarity = cosine_similarity(&self.embedding, vec).ok()?;
        // Reject NaN/Inf values - these indicate invalid input (e.g., zero-length vectors)
        if similarity.is_finite() {
            Some(similarity)
        } else {
            #[cfg(feature = "observability")]
            tracing::debug!(
                "Skipping node {:?} with non-finite similarity score: {}",
                node.id,
                similarity
            );
            None
        }
    }
}

impl ResultIterator for VectorRerankIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        // Lazy initialization: collect and sort on first call
        if self.sorted.is_none() && self.input.is_some() {
            // Check if vector index is configured
            let vector_property = match &self.vector_property {
                Some(prop) => prop.clone(),
                None => {
                    return Some(Err(crate::utils::error::Error::Vector(
                        crate::utils::error::VectorError::IndexError(
                            "VectorRerank requires a vector index to be enabled. \
                             Call db.enable_vector_index() first."
                                .to_string(),
                        ),
                    )));
                }
            };

            let mut input = self.input.take().unwrap();
            let mut scored: Vec<(QueryRow, f32)> = Vec::new();

            while let Some(result) = input.next() {
                match result {
                    Ok(row) => {
                        // Get vector from node and compute similarity
                        if let Some(similarity) = self.compute_similarity(&row, &vector_property) {
                            scored.push((row, similarity));
                        }
                    }
                    Err(e) => return Some(Err(e)),
                }
            }

            // Sort by similarity descending
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(self.k);

            self.sorted = Some(scored.into_iter());
        }

        self.sorted.as_mut()?.next().map(|(mut row, score)| {
            row.score = Some(score);
            Ok(row)
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if let Some(ref sorted) = self.sorted {
            sorted.size_hint()
        } else {
            (0, Some(self.k))
        }
    }
}

/// Iterator for limiting results.
pub struct LimitIterator {
    input: Box<dyn ResultIterator>,
    offset: usize,
    count: usize,
    skipped: usize,
    returned: usize,
}

impl LimitIterator {
    pub fn new(input: Box<dyn ResultIterator>, offset: usize, count: usize) -> Self {
        LimitIterator {
            input,
            offset,
            count,
            skipped: 0,
            returned: 0,
        }
    }
}

impl ResultIterator for LimitIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        // Skip offset
        while self.skipped < self.offset {
            match self.input.next() {
                Some(Ok(_)) => self.skipped += 1,
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }

        // Check count limit
        if self.returned >= self.count {
            return None;
        }

        match self.input.next() {
            Some(result) => {
                self.returned += 1;
                Some(result)
            }
            None => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.count.saturating_sub(self.returned);
        let (lower, upper) = self.input.size_hint();
        (lower.min(remaining), upper.map(|u| u.min(remaining)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::VersionId;
    use crate::core::interning::InternedString;
    use crate::core::property::PropertyMapBuilder;

    fn test_node(id: u64, name: &str) -> Node {
        let props = PropertyMapBuilder::new().insert("name", name).build();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        Node::new(
            NodeId::new(id).unwrap(),
            label,
            props,
            VersionId::new(1).unwrap(),
        )
    }

    fn test_node_with_age(id: u64, name: &str, age: i64) -> Node {
        let props = PropertyMapBuilder::new()
            .insert("name", name)
            .insert("age", age)
            .build();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        Node::new(
            NodeId::new(id).unwrap(),
            label,
            props,
            VersionId::new(1).unwrap(),
        )
    }

    fn test_node_with_vector(id: u64, name: &str, embedding: Vec<f32>) -> Node {
        let props = PropertyMapBuilder::new()
            .insert("name", name)
            .insert_vector("embedding", &embedding)
            .build();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        Node::new(
            NodeId::new(id).unwrap(),
            label,
            props,
            VersionId::new(1).unwrap(),
        )
    }

    /// Mock iterator for testing
    struct MockIterator {
        items: std::vec::IntoIter<Result<QueryRow>>,
    }

    impl MockIterator {
        fn from_nodes(nodes: Vec<Node>) -> Self {
            let items: Vec<Result<QueryRow>> = nodes
                .into_iter()
                .map(|n| Ok(QueryRow::from_entity(EntityResult::Node(n))))
                .collect();
            MockIterator {
                items: items.into_iter(),
            }
        }

        fn from_results(results: Vec<Result<QueryRow>>) -> Self {
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

    // ==================== EmptyIterator Tests ====================

    #[test]
    fn test_empty_iterator() {
        let mut iter = EmptyIterator;
        assert!(iter.next().is_none());
        assert_eq!(iter.size_hint(), (0, Some(0)));
    }

    #[test]
    fn test_empty_iterator_multiple_calls() {
        let mut iter = EmptyIterator;
        assert!(iter.next().is_none());
        assert!(iter.next().is_none());
        assert!(iter.next().is_none());
    }

    // ==================== FilterIterator Predicate Tests ====================

    #[test]
    fn test_filter_predicate_eq() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::eq("name", "Alice");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_eq_false() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::eq("name", "Bob");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_eq_missing_property() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::eq("missing", "value");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_ne() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::ne("name", "Bob");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_ne_same_value() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::ne("name", "Alice");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_ne_missing_property() {
        let node = test_node(1, "Alice");
        // Missing property != anything is true
        let predicate = Predicate::ne("missing", "value");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_gt() {
        let node = test_node_with_age(1, "Alice", 30);
        let predicate = Predicate::gt("age", 18i64);

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_gt_equal_value() {
        let node = test_node_with_age(1, "Alice", 18);
        let predicate = Predicate::gt("age", 18i64);

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_gt_less_value() {
        let node = test_node_with_age(1, "Alice", 15);
        let predicate = Predicate::gt("age", 18i64);

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_lt() {
        let node = test_node_with_age(1, "Alice", 15);
        let predicate = Predicate::lt("age", 18i64);

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_lt_equal_value() {
        let node = test_node_with_age(1, "Alice", 18);
        let predicate = Predicate::lt("age", 18i64);

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_gte() {
        let node = test_node_with_age(1, "Alice", 18);
        let predicate = Predicate::Gte {
            key: "age".to_string(),
            value: PredicateValue::Int(18),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_gte_greater() {
        let node = test_node_with_age(1, "Alice", 20);
        let predicate = Predicate::Gte {
            key: "age".to_string(),
            value: PredicateValue::Int(18),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_gte_less() {
        let node = test_node_with_age(1, "Alice", 15);
        let predicate = Predicate::Gte {
            key: "age".to_string(),
            value: PredicateValue::Int(18),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_lte() {
        let node = test_node_with_age(1, "Alice", 18);
        let predicate = Predicate::Lte {
            key: "age".to_string(),
            value: PredicateValue::Int(18),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_lte_less() {
        let node = test_node_with_age(1, "Alice", 15);
        let predicate = Predicate::Lte {
            key: "age".to_string(),
            value: PredicateValue::Int(18),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_lte_greater() {
        let node = test_node_with_age(1, "Alice", 20);
        let predicate = Predicate::Lte {
            key: "age".to_string(),
            value: PredicateValue::Int(18),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_exists() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::exists("name");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_exists_missing() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::exists("missing");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_not_exists() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::NotExists("missing".to_string());

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_not_exists_present() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::NotExists("name".to_string());

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_contains() {
        let node = test_node(1, "Alice Johnson");
        let predicate = Predicate::contains("name", "John");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_contains_not_found() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::contains("name", "Bob");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_starts_with() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::StartsWith {
            key: "name".to_string(),
            prefix: "Ali".to_string(),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_starts_with_not_match() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::StartsWith {
            key: "name".to_string(),
            prefix: "Bob".to_string(),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_ends_with() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::EndsWith {
            key: "name".to_string(),
            suffix: "ice".to_string(),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_ends_with_not_match() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::EndsWith {
            key: "name".to_string(),
            suffix: "Bob".to_string(),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_in() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::In {
            key: "name".to_string(),
            values: vec![
                PredicateValue::String("Alice".to_string()),
                PredicateValue::String("Bob".to_string()),
                PredicateValue::String("Charlie".to_string()),
            ],
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_in_not_found() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::In {
            key: "name".to_string(),
            values: vec![
                PredicateValue::String("Bob".to_string()),
                PredicateValue::String("Charlie".to_string()),
            ],
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_and() {
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            props,
            VersionId::new(1).unwrap(),
        );

        let predicate = Predicate::eq("name", "Alice").and(Predicate::gt("age", 18i64));

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_and_one_false() {
        let node = test_node_with_age(1, "Alice", 15);
        let predicate = Predicate::eq("name", "Alice").and(Predicate::gt("age", 18i64));

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_or() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::eq("name", "Alice").or(Predicate::eq("name", "Bob"));

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_or_second_true() {
        let node = test_node(1, "Bob");
        let predicate = Predicate::eq("name", "Alice").or(Predicate::eq("name", "Bob"));

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_or_both_false() {
        let node = test_node(1, "Charlie");
        let predicate = Predicate::eq("name", "Alice").or(Predicate::eq("name", "Bob"));

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_not() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::Not(Box::new(Predicate::eq("name", "Bob")));

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_not_negates_true() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::Not(Box::new(Predicate::eq("name", "Alice")));

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_true() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::True;

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_false() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::False;

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_float_comparison() {
        let props = PropertyMapBuilder::new().insert("score", 3.5f64).build();
        let label = GLOBAL_INTERNER.intern("Score").unwrap();
        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            props,
            VersionId::new(1).unwrap(),
        );

        let predicate = Predicate::gt("score", 3.0f64);
        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));

        let predicate = Predicate::lt("score", 4.0f64);
        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_bool_comparison() {
        let props = PropertyMapBuilder::new().insert("active", true).build();
        let label = GLOBAL_INTERNER.intern("Status").unwrap();
        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            props,
            VersionId::new(1).unwrap(),
        );

        let predicate = Predicate::eq("active", true);
        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));

        let predicate = Predicate::eq("active", false);
        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    // ==================== FilterIterator Integration Tests ====================

    #[test]
    fn test_filter_iterator_passes_matching_nodes() {
        let nodes = vec![
            test_node_with_age(1, "Alice", 30),
            test_node_with_age(2, "Bob", 25),
            test_node_with_age(3, "Charlie", 35),
        ];

        let input = MockIterator::from_nodes(nodes);
        let predicate = Predicate::gt("age", 28i64);
        let mut filter = FilterIterator::new(Box::new(input), predicate);

        let mut results = Vec::new();
        while let Some(Ok(row)) = filter.next() {
            results.push(row);
        }

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entity.node_id(), Some(NodeId::new(1).unwrap())); // Alice (30)
        assert_eq!(results[1].entity.node_id(), Some(NodeId::new(3).unwrap())); // Charlie (35)
    }

    #[test]
    fn test_filter_iterator_no_matches() {
        let nodes = vec![
            test_node_with_age(1, "Alice", 20),
            test_node_with_age(2, "Bob", 25),
        ];

        let input = MockIterator::from_nodes(nodes);
        let predicate = Predicate::gt("age", 100i64);
        let mut filter = FilterIterator::new(Box::new(input), predicate);

        assert!(filter.next().is_none());
    }

    #[test]
    fn test_filter_iterator_propagates_errors() {
        let results = vec![
            Ok(QueryRow::from_entity(EntityResult::Node(test_node(
                1, "Alice",
            )))),
            Err(crate::utils::error::Error::other("test error")),
        ];

        let input = MockIterator::from_results(results);
        let predicate = Predicate::True;
        let mut filter = FilterIterator::new(Box::new(input), predicate);

        // First result succeeds
        assert!(filter.next().unwrap().is_ok());
        // Second result is error
        assert!(filter.next().unwrap().is_err());
    }

    // ==================== LimitIterator Tests ====================

    #[test]
    fn test_limit_iterator() {
        let test_label = GLOBAL_INTERNER.intern("Test").unwrap();

        struct CountingIterator {
            count: usize,
            max: usize,
            label: InternedString,
        }

        impl ResultIterator for CountingIterator {
            fn next(&mut self) -> Option<Result<QueryRow>> {
                if self.count < self.max {
                    self.count += 1;
                    let node = Node::new(
                        NodeId::new(self.count as u64).unwrap(),
                        self.label,
                        PropertyMapBuilder::new().build(),
                        VersionId::new(1).unwrap(),
                    );
                    Some(Ok(QueryRow::from_entity(EntityResult::Node(node))))
                } else {
                    None
                }
            }
        }

        let input = Box::new(CountingIterator {
            count: 0,
            max: 10,
            label: test_label,
        });
        let mut limit = LimitIterator::new(input, 2, 3);

        // Should skip 2, return 3
        let mut results = Vec::new();
        while let Some(Ok(row)) = limit.next() {
            results.push(row);
        }

        assert_eq!(results.len(), 3);
        // First result should be node 3 (after skipping 2)
        assert_eq!(results[0].entity.node_id(), Some(NodeId::new(3).unwrap()));
    }

    #[test]
    fn test_limit_iterator_no_offset() {
        let nodes = vec![
            test_node(1, "Alice"),
            test_node(2, "Bob"),
            test_node(3, "Charlie"),
            test_node(4, "Dave"),
        ];

        let input = MockIterator::from_nodes(nodes);
        let mut limit = LimitIterator::new(Box::new(input), 0, 2);

        let mut results = Vec::new();
        while let Some(Ok(row)) = limit.next() {
            results.push(row);
        }

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entity.node_id(), Some(NodeId::new(1).unwrap()));
        assert_eq!(results[1].entity.node_id(), Some(NodeId::new(2).unwrap()));
    }

    #[test]
    fn test_limit_iterator_offset_exceeds_input() {
        let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

        let input = MockIterator::from_nodes(nodes);
        let mut limit = LimitIterator::new(Box::new(input), 5, 10);

        // Offset exceeds input, should return nothing
        assert!(limit.next().is_none());
    }

    #[test]
    fn test_limit_iterator_count_zero() {
        let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

        let input = MockIterator::from_nodes(nodes);
        let mut limit = LimitIterator::new(Box::new(input), 0, 0);

        // Count is 0, should return nothing
        assert!(limit.next().is_none());
    }

    #[test]
    fn test_limit_iterator_count_exceeds_remaining() {
        let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

        let input = MockIterator::from_nodes(nodes);
        let mut limit = LimitIterator::new(Box::new(input), 1, 10);

        let mut results = Vec::new();
        while let Some(Ok(row)) = limit.next() {
            results.push(row);
        }

        // Skipped 1, only 1 remaining
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity.node_id(), Some(NodeId::new(2).unwrap()));
    }

    #[test]
    fn test_limit_iterator_propagates_errors_during_skip() {
        let results = vec![
            Err(crate::utils::error::Error::other("test error")),
            Ok(QueryRow::from_entity(EntityResult::Node(test_node(
                1, "Alice",
            )))),
        ];

        let input = MockIterator::from_results(results);
        let mut limit = LimitIterator::new(Box::new(input), 1, 5);

        // Should get error during skip phase
        let result = limit.next();
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn test_limit_iterator_size_hint() {
        let nodes = vec![
            test_node(1, "Alice"),
            test_node(2, "Bob"),
            test_node(3, "Charlie"),
        ];

        let input = MockIterator::from_nodes(nodes);
        let limit = LimitIterator::new(Box::new(input), 0, 2);

        // Size hint should respect the limit
        let (lower, upper) = limit.size_hint();
        assert!(lower <= 2);
        assert!(upper.map(|u| u <= 2).unwrap_or(true));
    }

    // ==================== VectorRerankIterator Tests ====================

    #[test]
    fn test_vector_rerank_no_vector_index_error() {
        let nodes = vec![test_node_with_vector(1, "Alice", vec![1.0, 0.0, 0.0, 0.0])];

        // Create CurrentStorage without vector index
        let current = Arc::new(CurrentStorage::new());

        let input = MockIterator::from_nodes(nodes);
        let query = Arc::from(vec![1.0f32, 0.0, 0.0, 0.0]);

        let mut rerank = VectorRerankIterator::new(Box::new(input), query, 10, current);

        // Should return error because no vector index is configured
        let result = rerank.next();
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn test_vector_rerank_size_hint_before_init() {
        let nodes = vec![test_node_with_vector(1, "Alice", vec![1.0, 0.0, 0.0, 0.0])];

        let current = Arc::new(CurrentStorage::new());
        let input = MockIterator::from_nodes(nodes);
        let query = Arc::from(vec![1.0f32, 0.0, 0.0, 0.0]);

        let rerank = VectorRerankIterator::new(Box::new(input), query, 5, current);

        // Before initialization, size_hint upper bound is k
        let (lower, upper) = rerank.size_hint();
        assert_eq!(lower, 0);
        assert_eq!(upper, Some(5));
    }

    // ==================== MockIterator Tests ====================

    #[test]
    fn test_mock_iterator_from_nodes() {
        let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

        let mut iter = MockIterator::from_nodes(nodes);

        let row1 = iter.next().unwrap().unwrap();
        assert_eq!(row1.entity.node_id(), Some(NodeId::new(1).unwrap()));

        let row2 = iter.next().unwrap().unwrap();
        assert_eq!(row2.entity.node_id(), Some(NodeId::new(2).unwrap()));

        assert!(iter.next().is_none());
    }

    #[test]
    fn test_mock_iterator_size_hint() {
        let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

        let iter = MockIterator::from_nodes(nodes);

        let (lower, upper) = iter.size_hint();
        assert_eq!(lower, 2);
        assert_eq!(upper, Some(2));
    }

    // ==================== Type comparison edge cases ====================

    #[test]
    fn test_filter_type_mismatch_returns_false() {
        // String property compared to Int predicate
        let node = test_node(1, "Alice"); // name is String
        let predicate = Predicate::gt("name", 10i64); // Comparing String to Int

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node)); // Type mismatch returns false
    }

    #[test]
    fn test_filter_contains_on_non_string_returns_false() {
        let node = test_node_with_age(1, "Alice", 30);
        let predicate = Predicate::contains("age", "30"); // age is Int, not String

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_starts_with_on_non_string_returns_false() {
        let node = test_node_with_age(1, "Alice", 30);
        let predicate = Predicate::StartsWith {
            key: "age".to_string(),
            prefix: "3".to_string(),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    #[test]
    fn test_filter_ends_with_on_non_string_returns_false() {
        let node = test_node_with_age(1, "Alice", 30);
        let predicate = Predicate::EndsWith {
            key: "age".to_string(),
            suffix: "0".to_string(),
        };

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    // ==================== Null handling ====================

    #[test]
    fn test_filter_null_equality() {
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("optional", PropertyValue::Null)
            .build();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let node = Node::new(
            NodeId::new(1).unwrap(),
            label,
            props,
            VersionId::new(1).unwrap(),
        );

        // Null == Null should be true
        let predicate = Predicate::Eq {
            key: "optional".to_string(),
            value: PredicateValue::Null,
        };
        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    // ==================== Complex nested predicates ====================

    #[test]
    fn test_filter_deeply_nested_predicate() {
        let node = test_node_with_age(1, "Alice", 30);

        // (name == "Alice" AND age > 20) OR (name == "Bob")
        let predicate = Predicate::Or(vec![
            Predicate::And(vec![
                Predicate::eq("name", "Alice"),
                Predicate::gt("age", 20i64),
            ]),
            Predicate::eq("name", "Bob"),
        ]);

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_empty_and_is_true() {
        let node = test_node(1, "Alice");
        // Empty AND is vacuously true
        let predicate = Predicate::And(vec![]);

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_empty_or_is_false() {
        let node = test_node(1, "Alice");
        // Empty OR is vacuously false
        let predicate = Predicate::Or(vec![]);

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(!filter.evaluate(&node));
    }

    // ==================== NodeLookupIterator Tests ====================

    #[test]
    fn test_node_lookup_iterator_success() {
        let current = Arc::new(CurrentStorage::new());

        // Create test nodes
        let node1 = current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let node2 = current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();

        let node_ids = vec![node1, node2];
        let mut iter = NodeLookupIterator::new(node_ids, current);

        // Should get both nodes
        let row1 = iter.next().unwrap().unwrap();
        assert_eq!(row1.entity.node_id(), Some(node1));

        let row2 = iter.next().unwrap().unwrap();
        assert_eq!(row2.entity.node_id(), Some(node2));

        assert!(iter.next().is_none());
    }

    #[test]
    fn test_node_lookup_iterator_missing_node() {
        let current = Arc::new(CurrentStorage::new());

        // Don't add the node
        let node_ids = vec![NodeId::new(999).unwrap()];
        let mut iter = NodeLookupIterator::new(node_ids, current);

        // Should return error for missing node
        let result = iter.next().unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn test_node_lookup_iterator_size_hint() {
        let current = Arc::new(CurrentStorage::new());
        let node_ids = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
        let iter = NodeLookupIterator::new(node_ids, current);

        let (lower, upper) = iter.size_hint();
        assert_eq!(lower, 2);
        assert_eq!(upper, Some(2));
    }

    // ==================== NodeScanIterator Tests ====================

    #[test]
    fn test_node_scan_iterator_all_nodes() {
        let current = Arc::new(CurrentStorage::new());

        current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();

        let mut iter = NodeScanIterator::new(None, current);

        let mut results = Vec::new();
        while let Some(Ok(row)) = iter.next() {
            results.push(row);
        }

        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_node_scan_iterator_with_label_filter() {
        let current = Arc::new(CurrentStorage::new());

        let person = current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        current
            .create_node(
                "Company",
                PropertyMapBuilder::new().insert("name", "Acme").build(),
            )
            .unwrap();

        let mut iter = NodeScanIterator::new(Some("Person".to_string()), current);

        let mut results = Vec::new();
        while let Some(Ok(row)) = iter.next() {
            results.push(row);
        }

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity.node_id(), Some(person));
    }

    #[test]
    fn test_node_scan_iterator_empty_storage() {
        let current = Arc::new(CurrentStorage::new());
        let mut iter = NodeScanIterator::new(None, current);

        assert!(iter.next().is_none());
    }

    // ==================== VectorResultIterator Tests ====================

    #[test]
    fn test_vector_result_iterator_with_scores() {
        let current = Arc::new(CurrentStorage::new());

        let node1 = current
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let node2 = current
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert_vector("embedding", &[0.0f32, 1.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let results = vec![(node1, 0.95), (node2, 0.85)];

        let mut iter = VectorResultIterator::new(results, current);

        let row1 = iter.next().unwrap().unwrap();
        assert_eq!(row1.entity.node_id(), Some(node1));
        assert_eq!(row1.score, Some(0.95));

        let row2 = iter.next().unwrap().unwrap();
        assert_eq!(row2.entity.node_id(), Some(node2));
        assert_eq!(row2.score, Some(0.85));

        assert!(iter.next().is_none());
    }

    #[test]
    fn test_vector_result_iterator_missing_node() {
        let current = Arc::new(CurrentStorage::new());

        // Node doesn't exist
        let results = vec![(NodeId::new(999).unwrap(), 0.95)];
        let mut iter = VectorResultIterator::new(results, current);

        let result = iter.next().unwrap();
        assert!(result.is_err());
    }

    // ==================== TemporalNodeIterator Tests ====================

    #[test]
    fn test_temporal_node_iterator_returns_current_state() {
        use crate::storage::historical::HistoricalStorage;
        use crate::storage::version::AnchorConfig;

        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
            AnchorConfig::default(),
        )));

        let node = current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();

        let node_ids = vec![node];
        let now = crate::core::temporal::time::now();

        let mut iter = TemporalNodeIterator::new(node_ids, now, now, current, historical);

        let row = iter.next().unwrap().unwrap();
        assert_eq!(row.entity.node_id(), Some(node));
        assert_eq!(row.timestamp, Some(now));
    }

    #[test]
    fn test_temporal_node_iterator_empty() {
        use crate::storage::historical::HistoricalStorage;
        use crate::storage::version::AnchorConfig;

        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
            AnchorConfig::default(),
        )));

        let node_ids = vec![];
        let now = crate::core::temporal::time::now();

        let mut iter = TemporalNodeIterator::new(node_ids, now, now, current, historical);

        assert!(iter.next().is_none());
    }
}
