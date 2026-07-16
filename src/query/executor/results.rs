//! Query Result Types
//!
//! This module defines the vocabulary for reading the story of a graph query.
//! Once the query planner translates an intention into physical operations,
//! and the executor pulls the records, those records materialize here.
//!
//! The result types bridge the gap between low-level database engine operations
//! (which speak in iterators and raw byte arrays) and human developers (who
//! expect meaningful structures like `QueryRow` and `QueryResult`).
//!
//! # The Journey of a Result
//!
//! Queries produce a lazy iterator of results, managed by `QueryResults`.
//! This ensures that even queries matching millions of nodes don't exhaust memory.
//! For users who want everything at once (e.g. for serialization to a client),
//! `QueryResults::collect_structured()` gathers them into a `QueryResult` object.
//!
//! ## Example
//!
//! ```rust
//! # use aletheiadb::query::executor::{QueryResults, QueryRow, EntityResult};
//! # use aletheiadb::core::id::NodeId;
//! # // Mocking a QueryResults is internal, but this shows how you consume them:
//! # fn consume_results(results: QueryResults) -> Result<(), aletheiadb::core::error::Error> {
//! // Iterating lazily is the memory-efficient way to read a story:
//! for row in results {
//!     let row = row?;
//!     match row.entity {
//!         EntityResult::Node(node) => println!("Found node: {}", node.id),
//!         EntityResult::Edge(edge) => println!("Found edge: {}", edge.id),
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
use crate::core::id::VersionId;
use crate::core::interning::InternedString;
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::temporal::Timestamp;
use crate::core::{EdgeId, NodeId};

use super::iterators::ResultIterator;

/// Score threshold above which results are highlighted green in display output.
const HIGH_SCORE_THRESHOLD: f64 = 0.8;

/// Score threshold below which results are highlighted yellow in display output.
const LOW_SCORE_THRESHOLD: f64 = 0.5;

/// Maximum number of properties shown per node in display output.
const MAX_DISPLAY_PROPERTIES: usize = 5;

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
///
/// Marked `#[non_exhaustive]`: downstream crates must include a wildcard arm
/// when matching, so future variants (like the [`EntityResult::Null`] binding
/// added for `OPTIONAL MATCH`) are not semver-breaking.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EntityResult {
    /// Full node data
    Node(Node),
    /// Full edge data
    Edge(Edge),
    /// Just a node ID (for efficiency when full data not needed)
    NodeId(NodeId),
    /// Just an edge ID
    EdgeId(EdgeId),
    /// A null binding produced by an unmatched `OPTIONAL MATCH` pattern.
    ///
    /// Preserves the row (openCypher left-outer semantics) while carrying no
    /// entity. Predicates evaluated against a null row are not-true except
    /// `IS NULL`-style checks.
    Null,
}

impl EntityResult {
    /// Get the entity ID, or `None` for a null binding.
    pub fn id(&self) -> Option<EntityId> {
        match self {
            EntityResult::Node(n) => Some(EntityId::Node(n.id)),
            EntityResult::Edge(e) => Some(EntityId::Edge(e.id)),
            EntityResult::NodeId(id) => Some(EntityId::Node(*id)),
            EntityResult::EdgeId(id) => Some(EntityId::Edge(*id)),
            EntityResult::Null => None,
        }
    }

    /// Returns `true` if this is a null binding from an unmatched optional pattern.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, EntityResult::Null)
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

/// A single, multi-dimensional row yielded from a query execution.
///
/// A `QueryRow` is more than just a matched node or edge. It carries context:
/// * **Vector Search:** It might have a `score` reflecting its cosine similarity to the query embedding.
/// * **Graph Traversal:** It might have a `path` showing exactly *how* it was reached from the start node.
/// * **Time Travel:** It might have a `timestamp` showing *when* this specific snapshot of the entity was valid.
///
/// This context is why the query engine doesn't just return `Vec<Node>`.
///
/// ## Examples
///
/// ```rust
/// use aletheiadb::query::executor::{QueryRow, EntityResult};
/// use aletheiadb::core::id::NodeId;
///
/// let node_id = NodeId::new(42).unwrap();
/// let row = QueryRow::from_entity(EntityResult::NodeId(node_id));
/// assert!(row.score().is_none());
/// ```
#[derive(Debug, Clone)]
pub struct QueryRow {
    /// The primary entity (node or edge) returned by the query operation.
    pub entity: EntityResult,
    /// A similarity score between 0.0 and 1.0 (for vector search queries).
    pub score: Option<f32>,
    /// The sequence of nodes and edges connecting the start point to this result.
    pub path: Option<Vec<EntityId>>,
    /// The specific point in bi-temporal history this result represents.
    pub timestamp: Option<Timestamp>,
    /// Computed output columns for a row that does not correspond to a single
    /// stored entity -- e.g. the grouped result of a Cypher aggregation
    /// (`RETURN n.dept, count(*)`). Each entry is an `(column_name, value)`
    /// pair, in projection order.
    ///
    /// `None` for ordinary entity rows (the overwhelming common case), so those
    /// rows are byte-identical to their pre-aggregation form. When `Some`, the
    /// row's [`entity`](Self::entity) is typically [`EntityResult::Null`]: the
    /// meaningful payload lives here.
    pub columns: Option<Vec<(String, PropertyValue)>>,
    /// Named variable→entity bindings for a multi-variable row (Issue #549).
    ///
    /// The single-entity [`entity`](Self::entity) field can only carry one
    /// node/edge per row, which cannot represent a query that binds and returns
    /// several variables (e.g. `MATCH (a),(b) RETURN a, b`). Those queries are
    /// evaluated by the dedicated multi-pattern evaluator, which populates this
    /// field with one `(variable_name, entity)` pair per bound variable, in
    /// declaration order.
    ///
    /// `None` for ordinary single-entity rows (the common case), so those rows
    /// are byte-identical to their prior form. When `Some`, the row's
    /// [`entity`](Self::entity) is [`EntityResult::Null`]; the bound entities
    /// live here and any scalar projections live in [`columns`](Self::columns).
    pub bindings: Option<Vec<(String, EntityResult)>>,
}

impl QueryRow {
    /// Create a basic row with just an entity, typically for simple lookups.
    #[must_use]
    pub fn from_entity(entity: EntityResult) -> Self {
        QueryRow {
            entity,
            score: None,
            path: None,
            timestamp: None,
            columns: None,
            bindings: None,
        }
    }

    /// Create a computed-output row carrying named columns rather than a stored
    /// entity.
    ///
    /// Used by aggregation (`RETURN count(*)`, `RETURN n.dept, count(*)`) where
    /// the output row is derived from a group of input rows and has no single
    /// backing node/edge. The row's entity is [`EntityResult::Null`]; consumers
    /// read the payload from [`QueryRow::columns`].
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::executor::QueryRow;
    /// use aletheiadb::core::property::PropertyValue;
    ///
    /// let row = QueryRow::from_columns(vec![("count(*)".to_string(), PropertyValue::Int(5))]);
    /// assert!(row.entity.is_null());
    /// assert_eq!(row.columns.as_ref().unwrap()[0].1, PropertyValue::Int(5));
    /// ```
    #[must_use]
    pub fn from_columns(columns: Vec<(String, PropertyValue)>) -> Self {
        QueryRow {
            entity: EntityResult::Null,
            score: None,
            path: None,
            timestamp: None,
            columns: Some(columns),
            bindings: None,
        }
    }

    /// Create a multi-variable row carrying named `variable → entity` bindings
    /// (Issue #549).
    ///
    /// Used by the multi-pattern evaluator for queries that bind and return
    /// more than one entity variable (e.g. `MATCH (a),(b) RETURN a, b`), which
    /// the single-entity [`entity`](Self::entity) field cannot represent. The
    /// row's entity is [`EntityResult::Null`]; consumers read the bound
    /// entities from [`bindings`](Self::bindings) and any scalar projections
    /// (property accesses, aliased expressions) from the optional `columns`.
    #[must_use]
    pub fn from_bindings(
        bindings: Vec<(String, EntityResult)>,
        columns: Option<Vec<(String, PropertyValue)>>,
    ) -> Self {
        QueryRow {
            entity: EntityResult::Null,
            score: None,
            path: None,
            timestamp: None,
            columns,
            bindings: Some(bindings),
        }
    }

    /// Create a row with an associated vector similarity score.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::executor::{QueryRow, EntityResult};
    /// use aletheiadb::core::id::NodeId;
    ///
    /// let node_id = NodeId::new(42).unwrap();
    /// let row = QueryRow::with_score(EntityResult::NodeId(node_id), 0.95);
    /// assert_eq!(row.score(), Some(0.95));
    /// ```
    #[must_use]
    pub fn with_score(entity: EntityResult, score: f32) -> Self {
        QueryRow {
            entity,
            score: Some(score),
            path: None,
            timestamp: None,
            columns: None,
            bindings: None,
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
            columns: None,
            bindings: None,
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

/// A lazy stream of results from a query execution.
///
/// The engine evaluates physical operations as a pipeline of iterators.
/// `QueryResults` wraps this pipeline, giving users full control over how
/// memory is allocated when consuming the matched items.
///
/// You can consume the stream item by item using the `Iterator` trait,
/// or use convenience methods like `collect_all()` to exhaust it immediately.
pub struct QueryResults {
    iterator: Box<dyn ResultIterator>,
}

impl QueryResults {
    /// Create a new stream of results from a boxed iterator.
    pub(crate) fn new(iterator: Box<dyn ResultIterator>) -> Self {
        QueryResults { iterator }
    }

    /// Build a result stream from an in-memory vector of rows.
    ///
    /// Used by the Cypher `EXPLAIN`/`PROFILE` entry points (Issue #562), which
    /// synthesize a single computed `plan` row rather than streaming stored
    /// entities.
    #[cfg(feature = "cypher")]
    pub(crate) fn from_rows(rows: Vec<QueryRow>) -> Self {
        QueryResults::new(Box::new(MaterializedRows {
            rows: rows.into_iter(),
        }))
    }

    /// Eagerly collect every row in the result stream into a memory-backed vector.
    ///
    /// **Why?** Use this when the expected result set is small and you need random access.
    /// **Panics:** It does not panic, but it stops and returns the first `Err` if the iterator fails.
    ///
    /// ⚡ Bolt Optimization: Pre-allocates vector based on iterator's lower size bound
    /// to reduce heap allocations during collection.
    pub fn collect_all(mut self) -> Result<Vec<QueryRow>> {
        let (lower, _) = self.iterator.size_hint();
        let mut results = Vec::with_capacity(lower);
        while let Some(row) = self.iterator.next() {
            results.push(row?);
        }
        Ok(results)
    }

    /// Collect only the full nodes from the result stream, discarding scores, edges, and paths.
    ///
    /// **Why?** Useful when your application only cares about the entity payload and doesn't
    /// need provenance or similarity scoring context.
    ///
    /// ⚡ Bolt Optimization: Consumes the iterator directly to avoid allocating
    /// a large intermediate `Vec` of all rows before extracting the nodes.
    /// Pre-allocates vector based on iterator's lower size bound.
    pub fn collect_nodes(mut self) -> Result<Vec<Node>> {
        let (lower, _) = self.iterator.size_hint();
        let mut nodes = Vec::with_capacity(lower);
        while let Some(row) = self.iterator.next() {
            if let EntityResult::Node(n) = row?.entity {
                nodes.push(n);
            }
        }
        Ok(nodes)
    }

    /// Collect only the full edges from the result stream, discarding scores, nodes, and paths.
    ///
    /// **Why?** The edge counterpart to [`collect_nodes`](Self::collect_nodes): useful for
    /// `SELECT * FROM edges` (Issue #3607) and other edge-yielding queries where the caller
    /// wants the [`Edge`] payloads directly. Node rows in the stream are dropped, mirroring the
    /// way `collect_nodes` drops edge rows.
    ///
    /// ⚡ Bolt Optimization: Consumes the iterator directly to avoid allocating
    /// a large intermediate `Vec` of all rows before extracting the edges.
    /// Pre-allocates vector based on iterator's lower size bound.
    pub fn collect_edges(mut self) -> Result<Vec<Edge>> {
        let (lower, _) = self.iterator.size_hint();
        let mut edges = Vec::with_capacity(lower);
        while let Some(row) = self.iterator.next() {
            if let EntityResult::Edge(e) = row?.entity {
                edges.push(e);
            }
        }
        Ok(edges)
    }

    /// Collect nodes with their scores
    ///
    /// ⚡ Bolt Optimization: Consumes the iterator directly to avoid allocating
    /// a large intermediate `Vec` of all rows before extracting the nodes and scores.
    /// Pre-allocates vector based on iterator's lower size bound.
    pub fn collect_nodes_with_scores(mut self) -> Result<Vec<(Node, f32)>> {
        let (lower, _) = self.iterator.size_hint();
        let mut results = Vec::with_capacity(lower);
        while let Some(row) = self.iterator.next() {
            let row = row?;
            if let (EntityResult::Node(n), Some(score)) = (row.entity, row.score) {
                results.push((n, score));
            }
        }
        Ok(results)
    }

    /// Pull up to `n` results from the stream, leaving the rest un-evaluated.
    ///
    /// **Why?** When you only need a handful of answers from an unbounded traversal
    /// or massive dataset, this stops execution early.
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

    /// Fast-forward past the first `n` items without collecting them.
    ///
    /// **Why?** Useful for implementing pagination (`LIMIT` and `OFFSET`).
    pub fn skip_n(mut self, n: usize) -> Self {
        for _ in 0..n {
            if self.iterator.next().is_none() {
                break;
            }
        }
        self
    }

    /// Count the number of results
    ///
    /// ⚡ Bolt Optimization: Consumes the iterator and counts the items directly.
    /// This avoids allocating a large `Vec` just to check its length, significantly
    /// reducing memory overhead for large result sets.
    pub fn count_all(mut self) -> Result<usize> {
        let mut count = 0;
        while let Some(row) = self.iterator.next() {
            row?; // ensure no error
            count += 1;
        }
        Ok(count)
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
    /// Computed column rows for aggregation results (`RETURN count(*)`,
    /// `RETURN n.dept, count(*)`).
    ///
    /// `Some` when at least one collected row carried [`QueryRow::columns`];
    /// each inner vector is one output row's `(column_name, value)` pairs in
    /// projection order. `None` for ordinary entity result sets, so existing
    /// consumers are unaffected.
    pub columns: Option<Vec<Vec<(String, PropertyValue)>>>,
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
            columns: None,
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
            columns: None,
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
                    if f64::from(*score) > HIGH_SCORE_THRESHOLD {
                        cell = cell.fg(comfy_table::Color::Green);
                    } else if f64::from(*score) < LOW_SCORE_THRESHOLD {
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
                        .take(MAX_DISPLAY_PROPERTIES)
                        .map(|(k, v)| {
                            // resolve key
                            let k_str = crate::core::interning::GLOBAL_INTERNER
                                .resolve_with(*k, |s| s.to_string())
                                .unwrap_or_else(|| format!("key:{}", k.as_u32()));
                            format!("{}: {}", k_str, v)
                        })
                        .collect();

                    if map.len() > MAX_DISPLAY_PROPERTIES {
                        parts.push(format!("... (+{})", map.len() - MAX_DISPLAY_PROPERTIES));
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
    /// ⚡ Bolt Optimization: Pre-allocates vector based on iterator's lower size bound
    /// to reduce heap allocations during collection of structured results.
    pub fn collect_structured(mut self) -> Result<QueryResult> {
        // First pass: collect all rows
        let (lower, _) = self.iterator.size_hint();
        let mut rows = Vec::with_capacity(lower);
        while let Some(row) = self.iterator.next() {
            rows.push(row?);
        }

        // Determine which fields we have (single pass)
        let (mut has_any_scores, mut has_any_paths, mut has_any_versions, mut has_any_nodes) =
            (false, false, false, false);
        let mut has_any_columns = false;
        let mut node_count = 0usize;
        for row in &rows {
            has_any_scores = has_any_scores || row.score.is_some();
            has_any_paths = has_any_paths || row.path.is_some();
            has_any_versions = has_any_versions || row.timestamp.is_some();
            has_any_columns = has_any_columns || row.columns.is_some();
            if row.entity.as_node().is_some() {
                has_any_nodes = true;
                node_count += 1;
            }
        }

        // Second pass: extract data with padding
        let capacity = rows.len();
        let mut nodes = Vec::with_capacity(node_count);
        let mut properties = if has_any_nodes {
            Some(Vec::with_capacity(node_count))
        } else {
            None
        };
        let mut scores = if has_any_scores {
            Some(Vec::with_capacity(capacity))
        } else {
            None
        };
        let mut paths = if has_any_paths {
            Some(Vec::with_capacity(capacity))
        } else {
            None
        };
        let mut versions = if has_any_versions {
            Some(Vec::with_capacity(capacity))
        } else {
            None
        };
        let mut columns = if has_any_columns {
            Some(Vec::with_capacity(capacity))
        } else {
            None
        };

        for row in rows {
            let QueryRow {
                entity,
                score,
                path,
                timestamp,
                columns: row_columns,
                bindings: _,
            } = row;

            // Extract computed aggregation columns (padding rows without any
            // with an empty column list to preserve positional alignment).
            if let Some(ref mut cols) = columns {
                cols.push(row_columns.unwrap_or_default());
            }

            // ⚡ Bolt Optimization: Consumes the entity directly by value instead of cloning
            // properties map via `as_node().map(|n| n.properties.clone())`. This eliminates
            // one large heap allocation per row during result structurization.
            match entity {
                EntityResult::Node(n) => {
                    nodes.push(n.id);
                    if let Some(ref mut props) = properties {
                        props.push(n.properties);
                    }
                }
                EntityResult::NodeId(id) => {
                    nodes.push(id);
                    if let Some(ref mut props) = properties {
                        props.push(Default::default());
                    }
                }
                _ => {} // Ignore edges, etc.
            }

            // Extract or pad scores
            if let Some(ref mut s) = scores {
                s.push(score.unwrap_or(0.0));
            }

            // Extract or pad paths
            if let Some(ref mut p) = paths {
                p.push(path.unwrap_or_default());
            }

            // Extract or pad versions
            if let Some(ref mut v) = versions {
                if let Some(timestamp) = timestamp {
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
            columns,
        })
    }

    /// Collect all results into a structured [`EdgeQueryResult`].
    ///
    /// The edge counterpart to [`collect_structured`](Self::collect_structured) (Issue
    /// #3626). Where `collect_structured` projects node rows into a node-shaped
    /// [`QueryResult`] (and silently drops edges), this projects **edge** rows
    /// (`SELECT * FROM edges`, Issue #3607) into an edge-shaped [`EdgeQueryResult`]:
    /// parallel vectors of ids, endpoints, labels, properties, scores, and versions.
    /// `collect_structured` itself is unchanged and remains node-only.
    ///
    /// Node rows in the stream are dropped, symmetric to the way the node-centric helpers
    /// drop edge rows. The endpoint/label/property vectors are `Some` iff at least one full
    /// [`EntityResult::Edge`] row was collected; a stream of bare [`EntityResult::EdgeId`]
    /// rows populates only [`edges`](EdgeQueryResult::edges).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Any row fails to be read from the iterator
    /// - A timestamp cannot be converted to a valid `VersionId` (exceeds `MAX_VALID_ID`)
    ///
    /// # Performance
    ///
    /// Like `collect_structured`, this collects **all** rows into memory (two passes). For
    /// result sets exceeding 100K rows, prefer streaming the iterator directly.
    pub fn collect_structured_edges(mut self) -> Result<EdgeQueryResult> {
        // First pass: collect all rows.
        let (lower, _) = self.iterator.size_hint();
        let mut rows = Vec::with_capacity(lower);
        while let Some(row) = self.iterator.next() {
            rows.push(row?);
        }

        // Determine which fields we have (single pass). `has_any_full_edges` gates the
        // endpoint/label/property vectors: they only make sense once a full `Edge` (not a
        // bare `EdgeId`) has been seen.
        let (mut has_any_full_edges, mut has_any_scores, mut has_any_versions) =
            (false, false, false);
        let mut edge_count = 0usize;
        for row in &rows {
            has_any_scores = has_any_scores || row.score.is_some();
            has_any_versions = has_any_versions || row.timestamp.is_some();
            match row.entity {
                EntityResult::Edge(_) => {
                    has_any_full_edges = true;
                    edge_count += 1;
                }
                EntityResult::EdgeId(_) => edge_count += 1,
                _ => {}
            }
        }

        // Second pass: extract data with padding, keeping every vector parallel to `edges`.
        let mut edges = Vec::with_capacity(edge_count);
        let mut sources = has_any_full_edges.then(|| Vec::with_capacity(edge_count));
        let mut targets = has_any_full_edges.then(|| Vec::with_capacity(edge_count));
        let mut labels = has_any_full_edges.then(|| Vec::with_capacity(edge_count));
        let mut properties = has_any_full_edges.then(|| Vec::with_capacity(edge_count));
        let mut scores = has_any_scores.then(|| Vec::with_capacity(edge_count));
        let mut versions = has_any_versions.then(|| Vec::with_capacity(edge_count));

        for row in rows {
            let QueryRow {
                entity,
                score,
                path: _,
                timestamp,
                columns: _,
                bindings: _,
            } = row;

            // Only edge-bearing rows contribute; node/null rows are dropped (edge-only,
            // symmetric to `collect_structured`'s edge drop). We must keep the score/version
            // pushes inside these arms so every parallel vector stays aligned to `edges`.
            match entity {
                EntityResult::Edge(e) => {
                    edges.push(e.id);
                    if let Some(ref mut s) = sources {
                        s.push(e.source);
                    }
                    if let Some(ref mut t) = targets {
                        t.push(e.target);
                    }
                    if let Some(ref mut l) = labels {
                        l.push(e.label);
                    }
                    if let Some(ref mut p) = properties {
                        p.push(e.properties);
                    }
                }
                EntityResult::EdgeId(id) => {
                    edges.push(id);
                    // Pad the intrinsic-edge vectors: a bare `EdgeId` carries no endpoints,
                    // label, or properties (mirrors how a bare `NodeId` pads empty properties
                    // in `collect_structured`). The edge-scan path never produces bare
                    // `EdgeId` rows, so this only guards a hypothetical mixed stream.
                    if let Some(ref mut s) = sources {
                        s.push(NodeId::new_unchecked(0));
                    }
                    if let Some(ref mut t) = targets {
                        t.push(NodeId::new_unchecked(0));
                    }
                    if let Some(ref mut l) = labels {
                        l.push(InternedString::from_raw(0));
                    }
                    if let Some(ref mut p) = properties {
                        p.push(PropertyMap::default());
                    }
                }
                // Nodes / null bindings are not edges: drop them, and do not advance any
                // parallel vector so alignment with `edges` is preserved.
                _ => continue,
            }

            // Extract or pad scores (parallel to `edges`).
            if let Some(ref mut s) = scores {
                s.push(score.unwrap_or(0.0));
            }

            // Extract or pad versions (parallel to `edges`), reusing the node path's
            // timestamp→VersionId conversion and MAX_VALID_ID validation.
            if let Some(ref mut v) = versions {
                if let Some(timestamp) = timestamp {
                    let wallclock = timestamp.wallclock();
                    let ts_u64 = if wallclock < 0 {
                        0_u64
                    } else {
                        wallclock as u64
                    };
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
                    // SAFETY: VersionId(0) is always valid as 0 < MAX_VALID_ID.
                    v.push(VersionId::new(0).expect("VersionId(0) should always be valid"));
                }
            }
        }

        Ok(EdgeQueryResult {
            edges,
            sources,
            targets,
            labels,
            properties,
            scores,
            versions,
        })
    }
}

/// Aggregated edge query results in a structured format.
///
/// The edge-shaped counterpart to [`QueryResult`] (Issue #3626), produced by
/// [`QueryResults::collect_structured_edges`]. Where `QueryResult` holds node ids plus
/// parallel node metadata, `EdgeQueryResult` holds edge ids plus the data intrinsic to an
/// edge — source/target endpoints and the interned label — alongside properties, scores,
/// and versions.
///
/// # Alignment & Padding
///
/// Every `Option<Vec<_>>` field, when `Some`, is parallel to [`edges`](Self::edges) (same
/// length, same order). The endpoint/label/property vectors are `Some` iff at least one
/// full [`EntityResult::Edge`] row was collected; a stream of bare [`EntityResult::EdgeId`]
/// rows leaves them `None`. In the (practically unreachable) mixed case, `EdgeId` rows pad
/// endpoints with `NodeId(0)`, label with `InternedString(0)`, and empty properties —
/// mirroring how [`QueryResult`] pads a bare `NodeId`'s properties.
#[derive(Debug, Clone)]
pub struct EdgeQueryResult {
    /// Edge IDs from the query results, one per edge row, in stream order.
    pub edges: Vec<EdgeId>,
    /// Source node of each edge (parallel to `edges`). `Some` iff at least one full edge
    /// was collected.
    pub sources: Option<Vec<NodeId>>,
    /// Target node of each edge (parallel to `edges`). `Some` iff at least one full edge
    /// was collected.
    pub targets: Option<Vec<NodeId>>,
    /// Interned edge type/label of each edge (parallel to `edges`). `Some` iff at least one
    /// full edge was collected.
    pub labels: Option<Vec<InternedString>>,
    /// Properties of each edge (parallel to `edges`). `Some` iff at least one full edge was
    /// collected.
    pub properties: Option<Vec<PropertyMap>>,
    /// Similarity scores for ranked edge results (parallel to `edges`).
    pub scores: Option<Vec<f32>>,
    /// Version IDs for temporal edge results (parallel to `edges`).
    pub versions: Option<Vec<VersionId>>,
}

impl EdgeQueryResult {
    /// Create a new empty edge query result.
    #[must_use]
    pub fn new() -> Self {
        EdgeQueryResult {
            edges: Vec::new(),
            sources: None,
            targets: None,
            labels: None,
            properties: None,
            scores: None,
            versions: None,
        }
    }

    /// Get the number of edge results.
    #[must_use]
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Check if the result is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Get edges zipped with their `(source, target)` endpoints, when endpoints are
    /// available (i.e. full edges were collected).
    #[must_use]
    pub fn edges_with_endpoints(&self) -> Option<Vec<(EdgeId, NodeId, NodeId)>> {
        match (&self.sources, &self.targets) {
            (Some(sources), Some(targets)) => Some(
                self.edges
                    .iter()
                    .zip(sources.iter())
                    .zip(targets.iter())
                    .map(|((edge, source), target)| (*edge, *source, *target))
                    .collect(),
            ),
            _ => None,
        }
    }
}

impl Default for EdgeQueryResult {
    fn default() -> Self {
        Self::new()
    }
}

/// A [`ResultIterator`] backed by a pre-computed vector of rows.
///
/// Backs [`QueryResults::from_rows`] -- the one-row `plan` stream returned by
/// the Cypher `EXPLAIN`/`PROFILE` entry points (Issue #562).
#[cfg(feature = "cypher")]
struct MaterializedRows {
    rows: std::vec::IntoIter<QueryRow>,
}

#[cfg(feature = "cypher")]
impl ResultIterator for MaterializedRows {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.rows.next().map(Ok)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.rows.len();
        (n, Some(n))
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
            Some(EntityId::Edge(id)) => assert_eq!(id, edge.id),
            other => panic!("Expected Edge, got {:?}", other),
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
            Some(EntityId::Edge(id)) => assert_eq!(id, edge_id),
            other => panic!("Expected Edge, got {:?}", other),
        }
    }

    #[test]
    fn test_entity_result_null() {
        let result = EntityResult::Null;

        assert!(result.is_null());
        assert!(result.as_node().is_none());
        assert!(result.as_edge().is_none());
        assert_eq!(result.node_id(), None);
        assert_eq!(result.id(), None);
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
            Some(EntityId::Node(id)) => assert_eq!(id, node_id),
            other => panic!("Expected Node, got {:?}", other),
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

    // -- Edge structured projection (Issue #3626) --------------------------------------

    /// Build an edge with explicit id / endpoints / label / one property so tests can
    /// assert each field round-trips independently.
    fn edge_with(id: u64, source: u64, target: u64, label: &str, since: i64) -> Edge {
        Edge::new(
            EdgeId::new(id).unwrap(),
            GLOBAL_INTERNER.intern(label).unwrap(),
            NodeId::new(source).unwrap(),
            NodeId::new(target).unwrap(),
            PropertyMapBuilder::new().insert("since", since).build(),
            VersionId::new(1).unwrap(),
        )
    }

    #[test]
    fn test_collect_structured_edges_basic() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Edge(edge_with(10, 1, 2, "KNOWS", 2020))),
            QueryRow::from_entity(EntityResult::Edge(edge_with(11, 2, 3, "LIKES", 2021))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured_edges().unwrap();
        assert_eq!(structured.len(), 2);
        assert!(!structured.is_empty());

        assert_eq!(structured.edges[0], EdgeId::new(10).unwrap());
        assert_eq!(structured.edges[1], EdgeId::new(11).unwrap());

        let sources = structured
            .sources
            .as_ref()
            .expect("full edges carry sources");
        let targets = structured
            .targets
            .as_ref()
            .expect("full edges carry targets");
        assert_eq!(sources[0], NodeId::new(1).unwrap());
        assert_eq!(targets[0], NodeId::new(2).unwrap());
        assert_eq!(sources[1], NodeId::new(2).unwrap());
        assert_eq!(targets[1], NodeId::new(3).unwrap());

        let labels = structured.labels.as_ref().expect("full edges carry labels");
        assert_eq!(labels[0], GLOBAL_INTERNER.intern("KNOWS").unwrap());
        assert_eq!(labels[1], GLOBAL_INTERNER.intern("LIKES").unwrap());

        let props = structured
            .properties
            .as_ref()
            .expect("full edges carry properties");
        assert_eq!(props[0].get("since"), Some(&PropertyValue::Int(2020)));
        assert_eq!(props[1].get("since"), Some(&PropertyValue::Int(2021)));

        // No scores / versions on these rows.
        assert!(structured.scores.is_none());
        assert!(structured.versions.is_none());
    }

    #[test]
    fn test_collect_structured_edges_preserves_order() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Edge(edge_with(30, 1, 2, "KNOWS", 1))),
            QueryRow::from_entity(EntityResult::Edge(edge_with(10, 3, 4, "KNOWS", 2))),
            QueryRow::from_entity(EntityResult::Edge(edge_with(20, 5, 6, "KNOWS", 3))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured_edges().unwrap();
        let ids: Vec<u64> = structured.edges.iter().map(|e| e.as_u64()).collect();
        assert_eq!(
            ids,
            vec![30, 10, 20],
            "edges preserve stream order, not sorted"
        );
    }

    #[test]
    fn test_collect_structured_edges_with_scores() {
        let rows = vec![
            QueryRow::with_score(EntityResult::Edge(edge_with(10, 1, 2, "KNOWS", 1)), 0.9),
            QueryRow::with_score(EntityResult::Edge(edge_with(11, 2, 3, "KNOWS", 2)), 0.8),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured_edges().unwrap();
        let scores = structured.scores.expect("scores present");
        assert_eq!(scores, vec![0.9, 0.8]);
    }

    #[test]
    fn test_collect_structured_edges_with_timestamps() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Edge(edge_with(10, 1, 2, "KNOWS", 1)))
                .at_time(100.into()),
            QueryRow::from_entity(EntityResult::Edge(edge_with(11, 2, 3, "KNOWS", 2)))
                .at_time(200.into()),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured_edges().unwrap();
        let versions = structured.versions.expect("versions present");
        assert_eq!(versions[0], VersionId::new(100).unwrap());
        assert_eq!(versions[1], VersionId::new(200).unwrap());
    }

    #[test]
    fn test_collect_structured_edges_drops_nodes() {
        // A mixed stream: only the edge rows survive; node rows are dropped and do NOT
        // advance any parallel vector (edge-only, symmetric to collect_structured's edge drop).
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::with_score(EntityResult::Edge(edge_with(10, 1, 2, "KNOWS", 7)), 0.5),
            QueryRow::from_entity(EntityResult::NodeId(NodeId::new(9).unwrap())),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured_edges().unwrap();
        assert_eq!(structured.len(), 1, "only the single edge row survives");
        assert_eq!(structured.edges[0], EdgeId::new(10).unwrap());
        // Parallel vectors stayed aligned to the surviving edge only.
        assert_eq!(structured.sources.as_ref().unwrap().len(), 1);
        assert_eq!(structured.scores.as_ref().unwrap(), &vec![0.5]);
    }

    #[test]
    fn test_collect_structured_edges_empty() {
        let rows: Vec<QueryRow> = vec![];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured_edges().unwrap();
        assert!(structured.is_empty());
        assert_eq!(structured.len(), 0);
        assert!(structured.sources.is_none());
        assert!(structured.targets.is_none());
        assert!(structured.labels.is_none());
        assert!(structured.properties.is_none());
        assert!(structured.scores.is_none());
        assert!(structured.versions.is_none());
        assert!(structured.edges_with_endpoints().is_none());
    }

    #[test]
    fn test_collect_structured_edges_id_only_stream() {
        // A stream of bare EdgeId rows populates only `edges`; the intrinsic-edge vectors
        // stay None because no full Edge was seen.
        let rows = vec![
            QueryRow::from_entity(EntityResult::EdgeId(EdgeId::new(10).unwrap())),
            QueryRow::from_entity(EntityResult::EdgeId(EdgeId::new(11).unwrap())),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured_edges().unwrap();
        assert_eq!(structured.len(), 2);
        assert_eq!(structured.edges[0], EdgeId::new(10).unwrap());
        assert!(structured.sources.is_none());
        assert!(structured.targets.is_none());
        assert!(structured.labels.is_none());
        assert!(structured.properties.is_none());
        assert!(structured.edges_with_endpoints().is_none());
    }

    #[test]
    fn test_collect_structured_edges_mixed_full_and_id_pads() {
        // Mixed full-edge + bare-id: parallel vectors exist (a full edge was seen) and the
        // bare-id row is padded so every vector stays aligned to `edges`.
        let rows = vec![
            QueryRow::from_entity(EntityResult::Edge(edge_with(10, 1, 2, "KNOWS", 5))),
            QueryRow::from_entity(EntityResult::EdgeId(EdgeId::new(11).unwrap())),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured_edges().unwrap();
        assert_eq!(structured.len(), 2);
        let sources = structured.sources.as_ref().unwrap();
        let targets = structured.targets.as_ref().unwrap();
        let props = structured.properties.as_ref().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(targets.len(), 2);
        assert_eq!(props.len(), 2);
        // Full edge keeps real data.
        assert_eq!(sources[0], NodeId::new(1).unwrap());
        assert_eq!(props[0].get("since"), Some(&PropertyValue::Int(5)));
        // Bare-id row is padded (endpoints NodeId(0), empty properties).
        assert_eq!(sources[1], NodeId::new(0).unwrap());
        assert_eq!(targets[1], NodeId::new(0).unwrap());
        assert!(props[1].get("since").is_none());
    }

    #[test]
    fn test_collect_structured_edges_edges_with_endpoints() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Edge(edge_with(10, 1, 2, "KNOWS", 1))),
            QueryRow::from_entity(EntityResult::Edge(edge_with(11, 3, 4, "KNOWS", 2))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let structured = results.collect_structured_edges().unwrap();
        let triples = structured
            .edges_with_endpoints()
            .expect("endpoints available");
        assert_eq!(
            triples,
            vec![
                (
                    EdgeId::new(10).unwrap(),
                    NodeId::new(1).unwrap(),
                    NodeId::new(2).unwrap()
                ),
                (
                    EdgeId::new(11).unwrap(),
                    NodeId::new(3).unwrap(),
                    NodeId::new(4).unwrap()
                ),
            ]
        );
    }

    #[test]
    fn test_collect_edges_returns_full_edges_and_drops_nodes() {
        let rows = vec![
            QueryRow::from_entity(EntityResult::Node(test_node(1))),
            QueryRow::from_entity(EntityResult::Edge(edge_with(10, 1, 2, "KNOWS", 1))),
            QueryRow::from_entity(EntityResult::Edge(edge_with(11, 2, 3, "KNOWS", 2))),
        ];
        let results = QueryResults::new(Box::new(MockIterator::new(rows)));

        let edges = results.collect_edges().unwrap();
        assert_eq!(edges.len(), 2, "node row dropped, both edges kept");
        assert_eq!(edges[0].id, EdgeId::new(10).unwrap());
        assert_eq!(edges[1].source, NodeId::new(2).unwrap());
    }

    #[test]
    fn test_edge_query_result_default_is_empty() {
        let r = EdgeQueryResult::default();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
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
