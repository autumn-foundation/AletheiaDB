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
/// # Current Limitations (TODO)
///
/// **WARNING**: This iterator currently returns CURRENT state, not historical state.
/// The temporal lookup is not yet implemented - it just annotates results with the
/// requested timestamp but returns current node data.
///
/// A complete implementation requires:
/// 1. Looking up the node's version chain in HistoricalStorage
/// 2. Finding the anchor version at or before the requested time
/// 3. Applying delta reconstructions to get the state at that point
/// 4. Returning the reconstructed historical node
///
/// For now, use `db.as_of().get_node()` directly for accurate temporal queries.
pub struct TemporalNodeIterator {
    node_ids: std::vec::IntoIter<NodeId>,
    valid_time: Timestamp,
    _transaction_time: Timestamp,
    current: Arc<CurrentStorage>,
    _historical: Arc<RwLock<HistoricalStorage>>,
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
            _transaction_time: transaction_time,
            current,
            _historical: historical,
        }
    }
}

impl ResultIterator for TemporalNodeIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.node_ids.next().map(|id| {
            // TODO: Implement proper temporal lookup using HistoricalStorage
            // For now, just get the current node and annotate with timestamp
            // A full implementation would reconstruct the node at the given time
            // using anchor+delta from historical storage.
            self.current.get_node(id).map(|node| {
                QueryRow::from_entity(EntityResult::Node(node)).at_time(self.valid_time)
            })
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

    #[test]
    fn test_empty_iterator() {
        let mut iter = EmptyIterator;
        assert!(iter.next().is_none());
        assert_eq!(iter.size_hint(), (0, Some(0)));
    }

    #[test]
    fn test_filter_predicate_eq() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::eq("name", "Alice");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
    }

    #[test]
    fn test_filter_predicate_ne() {
        let node = test_node(1, "Alice");
        let predicate = Predicate::ne("name", "Bob");

        let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
        assert!(filter.evaluate(&node));
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
}
