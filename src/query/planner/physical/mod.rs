//! Physical Query Plan
//!
//! The physical query plan represents the explicit strategy the query engine will use
//! to fulfill a request. It maps directly to execution primitives.
//!
//! # Logical vs Physical
//!
//! While a *Logical Plan* describes **what** data to retrieve (e.g., "Join these two tables"),
//! the *Physical Plan* describes **how** to execute it (e.g., "Use a HashJoin, building the table on the smaller side").
//!
//! The optimizer in the query planner converts a logical plan into a physical plan by:
//! 1. Choosing the right algorithm (e.g., `NodeScan` vs `PropertyScan`).
//! 2. Evaluating costs (`EstimatedCost`).
//! 3. Utilizing indexes where available.
//!
//! Each `PhysicalOp` implementation typically defines an exact execution flow over the underlying
//! graph data and indexes.
//!
//! # Examples
//!
//! To see how physical plans are structured, you can call `.explain()` on a plan:
//!
//! ```rust,ignore
//! let plan = planner.plan(&db, query)?;
//! println!("{}", plan.explain());
//! ```

use std::sync::Arc;

use crate::core::NodeId;
use crate::core::temporal::{TimeRange, Timestamp};

use super::super::ir::{Direction, Predicate};
use super::super::plan::{SortKey, TemporalContext};
use super::cost::Cost;

/// A physical query plan ready for execution.
///
/// This structure represents a compiled, optimized query plan that maps
/// directly to execution operators. It contains the root operator tree,
/// cost estimates, and execution context.
///
/// ## Examples
///
/// ```rust
/// # use aletheiadb::core::NodeId;
/// # use aletheiadb::query::planner::physical::{PhysicalPlan, PhysicalOp};
/// # use aletheiadb::query::planner::cost::Cost;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Create a simple plan that looks up a node by ID
/// let plan = PhysicalPlan {
///     root: PhysicalOp::NodeLookup { node_ids: vec![NodeId::new(1)?] },
///     estimated_cost: Cost { cpu: 0.5, io: 1.0, memory: 1024, network: 0.0 },
///     temporal_context: None,
///     parallel: false,
///     include_provenance: false,
/// };
///
/// assert!(!plan.is_temporal());
/// assert_eq!(plan.cpu_cost(), 0.5);
/// assert_eq!(plan.memory_cost(), 1024);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct PhysicalPlan {
    /// Root physical operator
    pub root: PhysicalOp,
    /// Estimated execution cost
    pub estimated_cost: Cost,
    /// Temporal context (if any)
    pub temporal_context: Option<TemporalContext>,
    /// Enable parallel execution
    pub parallel: bool,
    /// Include provenance metadata (timestamps, paths) in results
    pub include_provenance: bool,
}

impl PhysicalPlan {
    /// Check if this plan requires temporal storage access
    #[must_use]
    pub fn is_temporal(&self) -> bool {
        self.temporal_context.is_some()
    }

    /// Get the estimated CPU cost
    #[must_use]
    pub fn cpu_cost(&self) -> f64 {
        self.estimated_cost.cpu
    }

    /// Get the estimated memory usage
    #[must_use]
    pub fn memory_cost(&self) -> usize {
        self.estimated_cost.memory
    }

    /// Generate a human-readable explanation of the query execution plan.
    ///
    /// This method produces a tree-like visualization showing:
    /// - Physical operators and their nesting
    /// - Estimated costs (CPU, I/O, memory)
    /// - Estimated cardinalities (row counts)
    /// - Temporal context if applicable
    ///
    /// # Example Output
    ///
    /// ```text
    /// Physical Plan (cost: cpu=10.5µs, io=2, mem=1.0KB)
    /// Filter (rows: ~100, cost: cpu=5.0µs)
    ///   └─ IndexedTraversal (rows: ~1000, cost: cpu=5.0µs)
    ///      └─ NodeLookup (rows: 1, cost: cpu=0.5µs)
    /// ```
    #[must_use]
    pub fn explain(&self) -> String {
        let mut output = String::new();

        // Header with overall plan info
        output.push_str(&format!(
            "Physical Plan (cost: cpu={:.1}µs, io={:.1}, mem={})\n",
            self.estimated_cost.cpu,
            self.estimated_cost.io,
            format_memory(self.estimated_cost.memory)
        ));

        if let Some(ref ctx) = self.temporal_context {
            if let Some((valid, tx)) = ctx.as_of_tuple() {
                output.push_str(&format!(
                    "  Temporal Context: as_of(valid={}, tx={})\n",
                    valid, tx
                ));
            }
            if let Some(ref range) = ctx.valid_time_between {
                output.push_str(&format!(
                    "  Temporal Context: valid_time between({}, {})\n",
                    range.start(),
                    range.end()
                ));
            }
            if let Some(ref range) = ctx.transaction_time_between {
                output.push_str(&format!(
                    "  Temporal Context: transaction_time between({}, {})\n",
                    range.start(),
                    range.end()
                ));
            }
        }

        if self.parallel {
            output.push_str("  Parallel execution enabled\n");
        }

        // Explain the operator tree
        self.explain_op(&self.root, &mut output, 0, "");

        output
    }

    /// Recursively explain an operator with indentation.
    fn explain_op(&self, op: &PhysicalOp, output: &mut String, indent: usize, prefix: &str) {
        let indent_str = "  ".repeat(indent);
        let op_name = op.name();

        // Build the line for this operator
        let mut line = format!("{}{}{}", indent_str, prefix, op_name);

        // Add operator-specific details
        match op {
            PhysicalOp::NodeLookup { node_ids } => {
                line.push_str(&format!(" (rows: {})", node_ids.len()));
            }
            PhysicalOp::NodeScan {
                label,
                estimated_rows,
            } => {
                line.push_str(&format!(" (rows: ~{})", estimated_rows));
                if let Some(l) = label {
                    line.push_str(&format!(" [label={}]", l));
                }
            }
            PhysicalOp::EdgeScan {
                edge_type,
                estimated_rows,
            } => {
                line.push_str(&format!(" (rows: ~{})", estimated_rows));
                if let Some(t) = edge_type {
                    line.push_str(&format!(" [type={}]", t));
                }
            }
            PhysicalOp::HnswSearch {
                k,
                label_filter,
                property_key,
                ..
            } => {
                line.push_str(&format!(" (k={})", k));
                if let Some(l) = label_filter {
                    line.push_str(&format!(" [label={}]", l));
                }
                if let Some(prop) = property_key {
                    line.push_str(&format!(" [property={}]", prop));
                }
            }
            PhysicalOp::TemporalNodeLookup {
                node_ids,
                use_batch,
                ..
            } => {
                line.push_str(&format!(" (rows: {}, batch={})", node_ids.len(), use_batch));
            }
            PhysicalOp::TemporalVectorSearch {
                k,
                timestamp,
                property_key,
                ..
            } => {
                line.push_str(&format!(" (k={}, ts={})", k, timestamp));
                if let Some(prop) = property_key {
                    line.push_str(&format!(" [property={}]", prop));
                }
            }
            PhysicalOp::SimilarToNode {
                k,
                label_filter,
                property_key,
                ..
            } => {
                line.push_str(&format!(" (k={})", k));
                if let Some(l) = label_filter {
                    line.push_str(&format!(" [label={}]", l));
                }
                line.push_str(&format!(" [property={}]", property_key));
            }
            PhysicalOp::PropertyScan {
                label,
                key,
                estimated_rows,
                ..
            } => {
                line.push_str(&format!(
                    " (rows: ~{}) [label={}, key={}]",
                    estimated_rows, label, key
                ));
            }
            PhysicalOp::IndexedTraversal {
                direction,
                label,
                depth,
                ..
            } => {
                line.push_str(&format!(" (depth={}, dir={:?})", depth, direction));
                if let Some(l) = label {
                    line.push_str(&format!(" [label={}]", l));
                }
            }
            PhysicalOp::Filter { predicate, .. } => {
                line.push_str(&format!(" [{:?}]", predicate));
            }
            PhysicalOp::Limit { count, offset, .. } => {
                line.push_str(&format!(" (count={}, offset={})", count, offset));
            }
            PhysicalOp::VectorRerank { k, .. } => {
                line.push_str(&format!(" (k={})", k));
            }
            PhysicalOp::Sort {
                key, descending, ..
            } => {
                line.push_str(&format!(" (key={:?}, desc={})", key, descending));
            }
            PhysicalOp::HashJoin {
                left_key,
                right_key,
                ..
            } => {
                line.push_str(&format!(" ({}={})", left_key, right_key));
            }
            PhysicalOp::Project { properties, .. } => {
                line.push_str(&format!(" ({})", properties.join(", ")));
            }
            _ => {} // Other operators don't need extra details
        }

        output.push_str(&line);
        output.push('\n');

        // Recursively explain children
        match op {
            // Unary operators
            PhysicalOp::Filter { input, .. }
            | PhysicalOp::VectorRerank { input, .. }
            | PhysicalOp::Sort { input, .. }
            | PhysicalOp::Limit { input, .. }
            | PhysicalOp::Project { input, .. }
            | PhysicalOp::Distinct { input, .. }
            | PhysicalOp::Count { input, .. }
            | PhysicalOp::Materialize { input, .. }
            | PhysicalOp::TemporalTrack { input, .. }
            | PhysicalOp::IndexedTraversal { input, .. } => {
                self.explain_op(input, output, indent + 1, "└─ ");
            }

            // Binary operators
            PhysicalOp::HashJoin { left, right, .. }
            | PhysicalOp::Union { left, right }
            | PhysicalOp::Intersect { left, right }
            | PhysicalOp::Except { left, right } => {
                self.explain_op(left, output, indent + 1, "├─ ");
                self.explain_op(right, output, indent + 1, "└─ ");
            }

            // Leaf operators (no children)
            _ => {}
        }
    }
}

/// Format memory size in human-readable form
fn format_memory(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * KB;
    const GB: usize = 1024 * MB;

    if bytes >= GB {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}KB", bytes as f64 / KB as f64)
    } else {
        format!("{}B", bytes)
    }
}

/// Physical operators that execute against storage.
///
/// Unlike `LogicalOp`s which describe the intent of the query, `PhysicalOp`s
/// specify exactly how the data will be accessed and processed. They map 1:1
/// with the iterators in `src/query/executor/iterators.rs`.
///
/// ## Examples
///
/// ```rust
/// use aletheiadb::core::NodeId;
/// use aletheiadb::query::planner::physical::PhysicalOp;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // A node lookup operator directly fetches specific nodes by ID.
/// let op = PhysicalOp::NodeLookup {
///     node_ids: vec![NodeId::new(42)?],
/// };
///
/// assert_eq!(op.name(), "NodeLookup");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub enum PhysicalOp {
    // === Scan Operators ===
    /// Direct node lookup by ID(s) - O(1) per node
    NodeLookup {
        /// Node IDs to look up
        node_ids: Vec<NodeId>,
    },

    /// Full node scan with optional label filter
    NodeScan {
        /// Optional label filter
        label: Option<String>,
        /// Estimated number of rows
        estimated_rows: usize,
    },

    /// Full edge scan with optional edge type filter
    EdgeScan {
        /// Optional edge type filter (e.g., "KNOWS", "FOLLOWS")
        edge_type: Option<String>,
        /// Estimated number of rows
        estimated_rows: usize,
    },

    /// HNSW vector k-NN search
    HnswSearch {
        /// Query embedding
        embedding: Arc<[f32]>,
        /// Number of results
        k: usize,
        /// Optional label filter
        label_filter: Option<String>,
        /// Property key for multi-property vector indexes.
        /// If None, uses the default/first indexed property.
        property_key: Option<String>,
    },

    /// Temporal node lookup (historical)
    TemporalNodeLookup {
        /// Node IDs to look up
        node_ids: Vec<NodeId>,
        /// Valid time for the query
        valid_time: Timestamp,
        /// Transaction time for the query
        transaction_time: Timestamp,
        /// Whether to use batch iterator (holds lock across all lookups)
        use_batch: bool,
    },

    /// Temporal vector search using historical snapshots
    TemporalVectorSearch {
        /// Query embedding
        embedding: Arc<[f32]>,
        /// Number of results
        k: usize,
        /// Timestamp for the historical query
        timestamp: Timestamp,
        /// Property key for multi-property temporal vector indexes.
        /// If None, uses the default/first indexed property.
        property_key: Option<String>,
    },

    /// Find nodes similar to a specific node by extracting its embedding
    /// and performing k-NN search. This is a compound operation that:
    /// 1. Looks up the source node
    /// 2. Extracts the embedding from the specified property
    /// 3. Performs HNSW k-NN search with that embedding
    SimilarToNode {
        /// Source node whose embedding to use
        source_node: NodeId,
        /// Property key containing the embedding vector
        property_key: String,
        /// Number of results to return
        k: usize,
        /// Optional label filter for results
        label_filter: Option<String>,
    },

    /// Property-based node scan: finds nodes with label where property == value.
    /// Produced by FilterScanFusion rule. Delegates to `CurrentStorage::find_nodes_by_property`.
    PropertyScan {
        /// Label to filter by
        label: String,
        /// Property key to match
        key: String,
        /// Expected property value
        value: crate::query::ir::PredicateValue,
        /// Estimated number of matching rows
        estimated_rows: usize,
    },

    // === Traversal Operators ===
    /// Graph traversal using adjacency index
    IndexedTraversal {
        /// Input operator providing source nodes
        input: Box<PhysicalOp>,
        /// Traversal direction
        direction: Direction,
        /// Optional edge label filter
        label: Option<String>,
        /// Maximum depth
        depth: usize,
        /// Optional temporal context (valid_time, transaction_time) for edge filtering.
        /// When present, only edges that existed at the specified point in time are traversed.
        temporal_context: Option<(Timestamp, Timestamp)>,
    },

    // === Join Operators ===
    /// Hash join for set operations
    HashJoin {
        /// Left input
        left: Box<PhysicalOp>,
        /// Right input
        right: Box<PhysicalOp>,
        /// Join key from left
        left_key: String,
        /// Join key from right
        right_key: String,
    },

    /// Set union
    Union {
        /// Left input
        left: Box<PhysicalOp>,
        /// Right input
        right: Box<PhysicalOp>,
    },

    /// Set intersection
    Intersect {
        /// Left input
        left: Box<PhysicalOp>,
        /// Right input
        right: Box<PhysicalOp>,
    },

    /// Set difference
    Except {
        /// Left input
        left: Box<PhysicalOp>,
        /// Right input
        right: Box<PhysicalOp>,
    },

    // === Transform Operators ===
    /// Filter by predicate
    Filter {
        /// Input operator
        input: Box<PhysicalOp>,
        /// Filter predicate
        predicate: Predicate,
    },

    /// Vector reranking (compute similarities and sort)
    VectorRerank {
        /// Input operator
        input: Box<PhysicalOp>,
        /// Target embedding for similarity
        embedding: Arc<[f32]>,
        /// Number of top results to keep
        k: usize,
        /// Property key for multi-property vector indexes.
        /// If None, uses the default/first indexed property.
        property_key: Option<String>,
    },

    /// Sort by key
    Sort {
        /// Input operator
        input: Box<PhysicalOp>,
        /// Sort key
        key: SortKey,
        /// Descending order
        descending: bool,
    },

    /// Limit with optional offset
    Limit {
        /// Input operator
        input: Box<PhysicalOp>,
        /// Maximum number of results
        count: usize,
        /// Number of results to skip
        offset: usize,
    },

    /// Project specific properties
    Project {
        /// Input operator
        input: Box<PhysicalOp>,
        /// Properties to project
        properties: Vec<String>,
    },

    /// Distinct/deduplicate
    Distinct {
        /// Input operator
        input: Box<PhysicalOp>,
    },

    /// Count aggregate
    Count {
        /// Input operator
        input: Box<PhysicalOp>,
    },

    /// Track temporal changes
    TemporalTrack {
        /// Input operator
        input: Box<PhysicalOp>,
        /// Time range to track
        time_range: TimeRange,
    },

    /// Materialize results into memory
    Materialize {
        /// Input operator
        input: Box<PhysicalOp>,
    },

    /// Empty result set
    Empty,
}

impl PhysicalOp {
    /// Get a descriptive name for this operator
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            PhysicalOp::NodeLookup { .. } => "NodeLookup",
            PhysicalOp::NodeScan { .. } => "NodeScan",
            PhysicalOp::EdgeScan { .. } => "EdgeScan",
            PhysicalOp::HnswSearch { .. } => "HnswSearch",
            PhysicalOp::TemporalNodeLookup { .. } => "TemporalNodeLookup",
            PhysicalOp::TemporalVectorSearch { .. } => "TemporalVectorSearch",
            PhysicalOp::SimilarToNode { .. } => "SimilarToNode",
            PhysicalOp::PropertyScan { .. } => "PropertyScan",
            PhysicalOp::IndexedTraversal { .. } => "IndexedTraversal",
            PhysicalOp::HashJoin { .. } => "HashJoin",
            PhysicalOp::Union { .. } => "Union",
            PhysicalOp::Intersect { .. } => "Intersect",
            PhysicalOp::Except { .. } => "Except",
            PhysicalOp::Filter { .. } => "Filter",
            PhysicalOp::VectorRerank { .. } => "VectorRerank",
            PhysicalOp::Sort { .. } => "Sort",
            PhysicalOp::Limit { .. } => "Limit",
            PhysicalOp::Project { .. } => "Project",
            PhysicalOp::Distinct { .. } => "Distinct",
            PhysicalOp::Count { .. } => "Count",
            PhysicalOp::TemporalTrack { .. } => "TemporalTrack",
            PhysicalOp::Materialize { .. } => "Materialize",
            PhysicalOp::Empty => "Empty",
        }
    }

    /// Check if this is a leaf operator (no input)
    #[must_use]
    pub fn is_leaf(&self) -> bool {
        matches!(
            self,
            PhysicalOp::NodeLookup { .. }
                | PhysicalOp::NodeScan { .. }
                | PhysicalOp::EdgeScan { .. }
                | PhysicalOp::HnswSearch { .. }
                | PhysicalOp::TemporalNodeLookup { .. }
                | PhysicalOp::TemporalVectorSearch { .. }
                | PhysicalOp::SimilarToNode { .. }
                | PhysicalOp::PropertyScan { .. }
                | PhysicalOp::Empty
        )
    }

    /// Get the depth of this operator tree
    #[must_use]
    pub fn depth(&self) -> usize {
        match self {
            PhysicalOp::NodeLookup { .. }
            | PhysicalOp::NodeScan { .. }
            | PhysicalOp::EdgeScan { .. }
            | PhysicalOp::HnswSearch { .. }
            | PhysicalOp::TemporalNodeLookup { .. }
            | PhysicalOp::TemporalVectorSearch { .. }
            | PhysicalOp::SimilarToNode { .. }
            | PhysicalOp::PropertyScan { .. }
            | PhysicalOp::Empty => 1,

            PhysicalOp::IndexedTraversal { input, .. }
            | PhysicalOp::Filter { input, .. }
            | PhysicalOp::VectorRerank { input, .. }
            | PhysicalOp::Sort { input, .. }
            | PhysicalOp::Limit { input, .. }
            | PhysicalOp::Project { input, .. }
            | PhysicalOp::Distinct { input, .. }
            | PhysicalOp::Count { input, .. }
            | PhysicalOp::TemporalTrack { input, .. }
            | PhysicalOp::Materialize { input, .. } => 1 + input.depth(),

            PhysicalOp::HashJoin { left, right, .. }
            | PhysicalOp::Union { left, right }
            | PhysicalOp::Intersect { left, right }
            | PhysicalOp::Except { left, right } => 1 + left.depth().max(right.depth()),
        }
    }

    /// Format the plan tree as a string for debugging
    #[must_use]
    pub fn explain(&self) -> String {
        self.explain_indent(0)
    }

    fn explain_indent(&self, indent: usize) -> String {
        let prefix = "  ".repeat(indent);
        let name = self.name();

        match self {
            PhysicalOp::NodeLookup { node_ids } => {
                format!("{prefix}{name} (ids: {:?})", node_ids)
            }
            PhysicalOp::NodeScan {
                label,
                estimated_rows,
            } => {
                format!(
                    "{prefix}{name} (label: {:?}, est_rows: {})",
                    label, estimated_rows
                )
            }
            PhysicalOp::EdgeScan {
                edge_type,
                estimated_rows,
            } => {
                format!(
                    "{prefix}{name} (edge_type: {:?}, est_rows: {})",
                    edge_type, estimated_rows
                )
            }
            PhysicalOp::HnswSearch {
                k,
                label_filter,
                property_key,
                ..
            } => {
                let prop_str = property_key
                    .as_ref()
                    .map(|p| format!(", prop: {}", p))
                    .unwrap_or_default();
                format!(
                    "{prefix}{name} (k: {}, label: {:?}{})",
                    k, label_filter, prop_str
                )
            }
            PhysicalOp::TemporalNodeLookup {
                node_ids,
                valid_time,
                transaction_time,
                use_batch,
            } => {
                format!(
                    "{prefix}{name} (ids: {:?}, vt: {}, tt: {}, batch: {})",
                    node_ids, valid_time, transaction_time, use_batch
                )
            }
            PhysicalOp::TemporalVectorSearch {
                k,
                timestamp,
                property_key,
                ..
            } => {
                let prop_str = property_key
                    .as_ref()
                    .map(|p| format!(", prop: {}", p))
                    .unwrap_or_default();
                format!("{prefix}{name} (k: {}, ts: {}{})", k, timestamp, prop_str)
            }
            PhysicalOp::SimilarToNode {
                source_node,
                property_key,
                k,
                label_filter,
            } => {
                format!(
                    "{prefix}{name} (source: {:?}, prop: {}, k: {}, label: {:?})",
                    source_node, property_key, k, label_filter
                )
            }
            PhysicalOp::IndexedTraversal {
                input,
                direction,
                label,
                depth,
                temporal_context,
            } => {
                let temporal_str = if let Some((vt, tt)) = temporal_context {
                    format!(", as_of: ({}, {})", vt, tt)
                } else {
                    String::new()
                };
                format!(
                    "{prefix}{name} (dir: {:?}, label: {:?}, depth: {}{})\n{}",
                    direction,
                    label,
                    depth,
                    temporal_str,
                    input.explain_indent(indent + 1)
                )
            }
            PhysicalOp::Filter { input, predicate } => {
                format!(
                    "{prefix}{name} ({:?})\n{}",
                    predicate,
                    input.explain_indent(indent + 1)
                )
            }
            PhysicalOp::VectorRerank { input, k, .. } => {
                format!(
                    "{prefix}{name} (k: {})\n{}",
                    k,
                    input.explain_indent(indent + 1)
                )
            }
            PhysicalOp::Limit {
                input,
                count,
                offset,
            } => {
                format!(
                    "{prefix}{name} (count: {}, offset: {})\n{}",
                    count,
                    offset,
                    input.explain_indent(indent + 1)
                )
            }
            PhysicalOp::HashJoin {
                left,
                right,
                left_key,
                right_key,
            } => {
                format!(
                    "{prefix}{name} ({} = {})\n{}\n{}",
                    left_key,
                    right_key,
                    left.explain_indent(indent + 1),
                    right.explain_indent(indent + 1)
                )
            }
            PhysicalOp::Union { left, right }
            | PhysicalOp::Intersect { left, right }
            | PhysicalOp::Except { left, right } => {
                format!(
                    "{prefix}{name}\n{}\n{}",
                    left.explain_indent(indent + 1),
                    right.explain_indent(indent + 1)
                )
            }
            _ => {
                // Simple case for other operators
                if let Some(input) = self.get_input() {
                    format!("{prefix}{name}\n{}", input.explain_indent(indent + 1))
                } else {
                    format!("{prefix}{name}")
                }
            }
        }
    }

    /// Get the input operator if this is a unary operator
    fn get_input(&self) -> Option<&PhysicalOp> {
        match self {
            PhysicalOp::IndexedTraversal { input, .. }
            | PhysicalOp::Filter { input, .. }
            | PhysicalOp::VectorRerank { input, .. }
            | PhysicalOp::Sort { input, .. }
            | PhysicalOp::Limit { input, .. }
            | PhysicalOp::Project { input, .. }
            | PhysicalOp::Distinct { input, .. }
            | PhysicalOp::Count { input, .. }
            | PhysicalOp::TemporalTrack { input, .. }
            | PhysicalOp::Materialize { input, .. } => Some(input),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
