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
mod tests {
    use super::*;
    use crate::core::NodeId;

    // ==================== PhysicalPlan Tests ====================

    #[test]
    fn test_physical_plan_is_temporal() {
        let plan = PhysicalPlan {
            root: PhysicalOp::Empty,
            estimated_cost: Cost::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };
        assert!(!plan.is_temporal());

        let temporal_plan = PhysicalPlan {
            root: PhysicalOp::Empty,
            estimated_cost: Cost::default(),
            temporal_context: Some(TemporalContext::as_of(1000.into(), 2000.into())),
            parallel: false,
            include_provenance: false,
        };
        assert!(temporal_plan.is_temporal());
    }

    #[test]
    fn test_physical_plan_cpu_cost() {
        let plan = PhysicalPlan {
            root: PhysicalOp::Empty,
            estimated_cost: Cost {
                cpu: 42.0,
                io: 0.0,
                memory: 0,
                network: 0.0,
            },
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };
        assert_eq!(plan.cpu_cost(), 42.0);
    }

    #[test]
    fn test_physical_plan_memory_cost() {
        let plan = PhysicalPlan {
            root: PhysicalOp::Empty,
            estimated_cost: Cost {
                cpu: 0.0,
                io: 0.0,
                memory: 1024,
                network: 0.0,
            },
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };
        assert_eq!(plan.memory_cost(), 1024);
    }

    // ==================== PhysicalOp Name Tests ====================

    #[test]
    fn test_physical_op_names() {
        let lookup = PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
        };
        assert_eq!(lookup.name(), "NodeLookup");

        let empty = PhysicalOp::Empty;
        assert_eq!(empty.name(), "Empty");
    }

    #[test]
    fn test_physical_op_names_all_variants() {
        // Scan operators
        assert_eq!(
            PhysicalOp::NodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()]
            }
            .name(),
            "NodeLookup"
        );
        assert_eq!(
            PhysicalOp::NodeScan {
                label: None,
                estimated_rows: 100
            }
            .name(),
            "NodeScan"
        );
        assert_eq!(
            PhysicalOp::HnswSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                label_filter: None,
                property_key: None,
            }
            .name(),
            "HnswSearch"
        );
        assert_eq!(
            PhysicalOp::TemporalNodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
                valid_time: 1000.into(),
                transaction_time: 2000.into(),
                use_batch: false,
            }
            .name(),
            "TemporalNodeLookup"
        );
        assert_eq!(
            PhysicalOp::TemporalVectorSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                timestamp: 1000.into(),
                property_key: None,
            }
            .name(),
            "TemporalVectorSearch"
        );

        // Traversal operators
        assert_eq!(
            PhysicalOp::IndexedTraversal {
                input: Box::new(PhysicalOp::Empty),
                direction: Direction::Outgoing,
                label: None,
                depth: 1,
                temporal_context: None,
            }
            .name(),
            "IndexedTraversal"
        );

        // Join operators
        assert_eq!(
            PhysicalOp::HashJoin {
                left: Box::new(PhysicalOp::Empty),
                right: Box::new(PhysicalOp::Empty),
                left_key: "id".to_string(),
                right_key: "id".to_string()
            }
            .name(),
            "HashJoin"
        );
        assert_eq!(
            PhysicalOp::Union {
                left: Box::new(PhysicalOp::Empty),
                right: Box::new(PhysicalOp::Empty)
            }
            .name(),
            "Union"
        );
        assert_eq!(
            PhysicalOp::Intersect {
                left: Box::new(PhysicalOp::Empty),
                right: Box::new(PhysicalOp::Empty)
            }
            .name(),
            "Intersect"
        );
        assert_eq!(
            PhysicalOp::Except {
                left: Box::new(PhysicalOp::Empty),
                right: Box::new(PhysicalOp::Empty)
            }
            .name(),
            "Except"
        );

        // Transform operators
        assert_eq!(
            PhysicalOp::Filter {
                input: Box::new(PhysicalOp::Empty),
                predicate: Predicate::True
            }
            .name(),
            "Filter"
        );
        assert_eq!(
            PhysicalOp::VectorRerank {
                input: Box::new(PhysicalOp::Empty),
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                property_key: None,
            }
            .name(),
            "VectorRerank"
        );
        assert_eq!(
            PhysicalOp::Sort {
                input: Box::new(PhysicalOp::Empty),
                key: SortKey::Property("name".to_string()),
                descending: false
            }
            .name(),
            "Sort"
        );
        assert_eq!(
            PhysicalOp::Limit {
                input: Box::new(PhysicalOp::Empty),
                count: 10,
                offset: 0
            }
            .name(),
            "Limit"
        );
        assert_eq!(
            PhysicalOp::Project {
                input: Box::new(PhysicalOp::Empty),
                properties: vec!["name".to_string()]
            }
            .name(),
            "Project"
        );
        assert_eq!(
            PhysicalOp::Distinct {
                input: Box::new(PhysicalOp::Empty)
            }
            .name(),
            "Distinct"
        );
        assert_eq!(
            PhysicalOp::Count {
                input: Box::new(PhysicalOp::Empty)
            }
            .name(),
            "Count"
        );
        assert_eq!(
            PhysicalOp::TemporalTrack {
                input: Box::new(PhysicalOp::Empty),
                time_range: TimeRange::new(1000.into(), 2000.into()).unwrap()
            }
            .name(),
            "TemporalTrack"
        );
        assert_eq!(
            PhysicalOp::Materialize {
                input: Box::new(PhysicalOp::Empty)
            }
            .name(),
            "Materialize"
        );
        assert_eq!(PhysicalOp::Empty.name(), "Empty");
    }

    // ==================== is_leaf Tests ====================

    #[test]
    fn test_is_leaf() {
        let lookup = PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
        };
        assert!(lookup.is_leaf());

        let filter = PhysicalOp::Filter {
            input: Box::new(PhysicalOp::Empty),
            predicate: Predicate::True,
        };
        assert!(!filter.is_leaf());
    }

    #[test]
    fn test_is_leaf_all_leaf_operators() {
        assert!(
            PhysicalOp::NodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()]
            }
            .is_leaf()
        );
        assert!(
            PhysicalOp::NodeScan {
                label: None,
                estimated_rows: 100
            }
            .is_leaf()
        );
        assert!(
            PhysicalOp::HnswSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                label_filter: None,
                property_key: None,
            }
            .is_leaf()
        );
        assert!(
            PhysicalOp::TemporalNodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
                valid_time: 1000.into(),
                transaction_time: 2000.into(),
                use_batch: false,
            }
            .is_leaf()
        );
        assert!(
            PhysicalOp::TemporalVectorSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                timestamp: 1000.into(),
                property_key: None,
            }
            .is_leaf()
        );
        assert!(PhysicalOp::Empty.is_leaf());
    }

    #[test]
    fn test_is_leaf_non_leaf_operators() {
        assert!(
            !PhysicalOp::IndexedTraversal {
                input: Box::new(PhysicalOp::Empty),
                direction: Direction::Outgoing,
                label: None,
                depth: 1,
                temporal_context: None,
            }
            .is_leaf()
        );
        assert!(
            !PhysicalOp::Filter {
                input: Box::new(PhysicalOp::Empty),
                predicate: Predicate::True
            }
            .is_leaf()
        );
        assert!(
            !PhysicalOp::VectorRerank {
                input: Box::new(PhysicalOp::Empty),
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                property_key: None,
            }
            .is_leaf()
        );
        assert!(
            !PhysicalOp::Limit {
                input: Box::new(PhysicalOp::Empty),
                count: 10,
                offset: 0
            }
            .is_leaf()
        );
        assert!(
            !PhysicalOp::Union {
                left: Box::new(PhysicalOp::Empty),
                right: Box::new(PhysicalOp::Empty)
            }
            .is_leaf()
        );
    }

    // ==================== depth Tests ====================

    #[test]
    fn test_depth() {
        let lookup = PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
        };
        assert_eq!(lookup.depth(), 1);

        let filter = PhysicalOp::Filter {
            input: Box::new(lookup),
            predicate: Predicate::True,
        };
        assert_eq!(filter.depth(), 2);

        let limit = PhysicalOp::Limit {
            input: Box::new(filter),
            count: 10,
            offset: 0,
        };
        assert_eq!(limit.depth(), 3);
    }

    #[test]
    fn test_depth_all_leaf_operators() {
        assert_eq!(PhysicalOp::Empty.depth(), 1);
        assert_eq!(
            PhysicalOp::NodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()]
            }
            .depth(),
            1
        );
        assert_eq!(
            PhysicalOp::NodeScan {
                label: None,
                estimated_rows: 100
            }
            .depth(),
            1
        );
        assert_eq!(
            PhysicalOp::HnswSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                label_filter: None,
                property_key: None,
            }
            .depth(),
            1
        );
        assert_eq!(
            PhysicalOp::TemporalNodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
                valid_time: 1000.into(),
                transaction_time: 2000.into(),
                use_batch: false,
            }
            .depth(),
            1
        );
        assert_eq!(
            PhysicalOp::TemporalVectorSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                timestamp: 1000.into(),
                property_key: None,
            }
            .depth(),
            1
        );
    }

    #[test]
    fn test_depth_binary_operators() {
        // Symmetric depths
        let union = PhysicalOp::Union {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
        };
        assert_eq!(union.depth(), 2);

        // Asymmetric depths - should take max
        let union_asymmetric = PhysicalOp::Union {
            left: Box::new(PhysicalOp::Filter {
                input: Box::new(PhysicalOp::Empty),
                predicate: Predicate::True,
            }),
            right: Box::new(PhysicalOp::Empty),
        };
        assert_eq!(union_asymmetric.depth(), 3);

        // HashJoin
        let hash_join = PhysicalOp::HashJoin {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Filter {
                input: Box::new(PhysicalOp::Filter {
                    input: Box::new(PhysicalOp::Empty),
                    predicate: Predicate::True,
                }),
                predicate: Predicate::True,
            }),
            left_key: "id".to_string(),
            right_key: "id".to_string(),
        };
        assert_eq!(hash_join.depth(), 4); // 1 + max(1, 3)

        // Intersect
        let intersect = PhysicalOp::Intersect {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
        };
        assert_eq!(intersect.depth(), 2);

        // Except
        let except = PhysicalOp::Except {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
        };
        assert_eq!(except.depth(), 2);
    }

    #[test]
    fn test_depth_unary_operators() {
        let base = PhysicalOp::Empty;

        // All unary operators add 1 to depth
        assert_eq!(
            PhysicalOp::Sort {
                input: Box::new(base.clone()),
                key: SortKey::Property("name".to_string()),
                descending: false
            }
            .depth(),
            2
        );
        assert_eq!(
            PhysicalOp::Project {
                input: Box::new(base.clone()),
                properties: vec![]
            }
            .depth(),
            2
        );
        assert_eq!(
            PhysicalOp::Distinct {
                input: Box::new(base.clone())
            }
            .depth(),
            2
        );
        assert_eq!(
            PhysicalOp::Count {
                input: Box::new(base.clone())
            }
            .depth(),
            2
        );
        assert_eq!(
            PhysicalOp::TemporalTrack {
                input: Box::new(base.clone()),
                time_range: TimeRange::new(1000.into(), 2000.into()).unwrap()
            }
            .depth(),
            2
        );
        assert_eq!(
            PhysicalOp::Materialize {
                input: Box::new(base)
            }
            .depth(),
            2
        );
    }

    // ==================== explain Tests ====================

    #[test]
    fn test_explain() {
        let plan = PhysicalOp::Limit {
            input: Box::new(PhysicalOp::Filter {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![NodeId::new(1).unwrap()],
                }),
                predicate: Predicate::True,
            }),
            count: 10,
            offset: 0,
        };

        let explain = plan.explain();
        assert!(explain.contains("Limit"));
        assert!(explain.contains("Filter"));
        assert!(explain.contains("NodeLookup"));
    }

    #[test]
    fn test_explain_node_scan() {
        let plan = PhysicalOp::NodeScan {
            label: Some("Person".to_string()),
            estimated_rows: 1000,
        };

        let explain = plan.explain();
        assert!(explain.contains("NodeScan"));
        assert!(explain.contains("Person"));
        assert!(explain.contains("1000"));
    }

    #[test]
    fn test_explain_hnsw_search() {
        let plan = PhysicalOp::HnswSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            label_filter: Some("Document".to_string()),
            property_key: None,
        };

        let explain = plan.explain();
        assert!(explain.contains("HnswSearch"));
        assert!(explain.contains("k: 10"));
        assert!(explain.contains("Document"));
    }

    #[test]
    fn test_explain_temporal_node_lookup() {
        let plan = PhysicalOp::TemporalNodeLookup {
            node_ids: vec![NodeId::new(42).unwrap()],
            valid_time: 1000.into(),
            transaction_time: 2000.into(),
            use_batch: false,
        };

        let explain = plan.explain();
        assert!(explain.contains("TemporalNodeLookup"));
        assert!(explain.contains("vt: 1000"));
        assert!(explain.contains("tt: 2000"));
        assert!(explain.contains("batch: false"));
    }

    #[test]
    fn test_explain_indexed_traversal() {
        let plan = PhysicalOp::IndexedTraversal {
            input: Box::new(PhysicalOp::Empty),
            direction: Direction::Outgoing,
            label: Some("KNOWS".to_string()),
            depth: 2,
            temporal_context: None,
        };

        let explain = plan.explain();
        assert!(explain.contains("IndexedTraversal"));
        assert!(explain.contains("Outgoing"));
        assert!(explain.contains("KNOWS"));
        assert!(explain.contains("depth: 2"));
    }

    #[test]
    fn test_explain_vector_rerank() {
        let plan = PhysicalOp::VectorRerank {
            input: Box::new(PhysicalOp::Empty),
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 5,
            property_key: None,
        };

        let explain = plan.explain();
        assert!(explain.contains("VectorRerank"));
        assert!(explain.contains("k: 5"));
    }

    #[test]
    fn test_explain_hash_join() {
        let plan = PhysicalOp::HashJoin {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
            left_key: "user_id".to_string(),
            right_key: "id".to_string(),
        };

        let explain = plan.explain();
        assert!(explain.contains("HashJoin"));
        assert!(explain.contains("user_id"));
        assert!(explain.contains("id"));
    }

    #[test]
    fn test_explain_union() {
        let plan = PhysicalOp::Union {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
        };

        let explain = plan.explain();
        assert!(explain.contains("Union"));
        assert!(explain.contains("Empty"));
    }

    #[test]
    fn test_explain_simple_operators() {
        // Sort
        let sort = PhysicalOp::Sort {
            input: Box::new(PhysicalOp::Empty),
            key: SortKey::Property("name".to_string()),
            descending: false,
        };
        let explain = sort.explain();
        assert!(explain.contains("Sort"));
        assert!(explain.contains("Empty"));

        // Project
        let project = PhysicalOp::Project {
            input: Box::new(PhysicalOp::Empty),
            properties: vec!["name".to_string()],
        };
        let explain = project.explain();
        assert!(explain.contains("Project"));

        // Distinct
        let distinct = PhysicalOp::Distinct {
            input: Box::new(PhysicalOp::Empty),
        };
        let explain = distinct.explain();
        assert!(explain.contains("Distinct"));

        // Count
        let count = PhysicalOp::Count {
            input: Box::new(PhysicalOp::Empty),
        };
        let explain = count.explain();
        assert!(explain.contains("Count"));

        // Materialize
        let materialize = PhysicalOp::Materialize {
            input: Box::new(PhysicalOp::Empty),
        };
        let explain = materialize.explain();
        assert!(explain.contains("Materialize"));
    }

    // ==================== get_input Tests ====================

    #[test]
    fn test_get_input_returns_none_for_leaf() {
        let lookup = PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
        };
        assert!(lookup.get_input().is_none());
        assert!(PhysicalOp::Empty.get_input().is_none());
    }

    #[test]
    fn test_get_input_returns_some_for_unary() {
        let filter = PhysicalOp::Filter {
            input: Box::new(PhysicalOp::Empty),
            predicate: Predicate::True,
        };
        assert!(filter.get_input().is_some());
        assert!(matches!(filter.get_input(), Some(PhysicalOp::Empty)));
    }

    #[test]
    fn test_get_input_returns_none_for_binary() {
        let union = PhysicalOp::Union {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
        };
        // Binary operators don't have a single input
        assert!(union.get_input().is_none());
    }

    // ==================== Additional Explain Tests ====================

    #[test]
    fn test_explain_temporal_vector_search() {
        let plan = PhysicalOp::TemporalVectorSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            timestamp: 42000.into(),
            property_key: None,
        };

        let explain = plan.explain();
        assert!(explain.contains("TemporalVectorSearch"));
    }

    #[test]
    fn test_explain_intersect() {
        let plan = PhysicalOp::Intersect {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::NodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
            }),
        };

        let explain = plan.explain();
        assert!(explain.contains("Intersect"));
        assert!(explain.contains("Empty"));
        assert!(explain.contains("NodeLookup"));
    }

    #[test]
    fn test_explain_except() {
        let plan = PhysicalOp::Except {
            left: Box::new(PhysicalOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: 100,
            }),
            right: Box::new(PhysicalOp::Empty),
        };

        let explain = plan.explain();
        assert!(explain.contains("Except"));
        assert!(explain.contains("NodeScan"));
        assert!(explain.contains("Empty"));
    }

    #[test]
    fn test_explain_temporal_track() {
        let plan = PhysicalOp::TemporalTrack {
            input: Box::new(PhysicalOp::Empty),
            time_range: TimeRange::new(1000.into(), 2000.into()).unwrap(),
        };

        let explain = plan.explain();
        assert!(explain.contains("TemporalTrack"));
        assert!(explain.contains("Empty"));
    }

    // ==================== Additional Name Tests ====================

    #[test]
    fn test_all_operator_names() {
        // Test all operator variants have correct names
        assert_eq!(
            PhysicalOp::TemporalVectorSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                timestamp: 1000.into(),
                property_key: None,
            }
            .name(),
            "TemporalVectorSearch"
        );

        assert_eq!(
            PhysicalOp::Intersect {
                left: Box::new(PhysicalOp::Empty),
                right: Box::new(PhysicalOp::Empty),
            }
            .name(),
            "Intersect"
        );

        assert_eq!(
            PhysicalOp::Except {
                left: Box::new(PhysicalOp::Empty),
                right: Box::new(PhysicalOp::Empty),
            }
            .name(),
            "Except"
        );

        assert_eq!(
            PhysicalOp::TemporalTrack {
                input: Box::new(PhysicalOp::Empty),
                time_range: TimeRange::new(1000.into(), 2000.into()).unwrap(),
            }
            .name(),
            "TemporalTrack"
        );

        assert_eq!(
            PhysicalOp::Materialize {
                input: Box::new(PhysicalOp::Empty),
            }
            .name(),
            "Materialize"
        );
    }

    // ==================== Additional Depth Tests ====================

    #[test]
    fn test_depth_additional_binary_operators() {
        let intersect = PhysicalOp::Intersect {
            left: Box::new(PhysicalOp::Filter {
                input: Box::new(PhysicalOp::Empty),
                predicate: Predicate::True,
            }),
            right: Box::new(PhysicalOp::Empty),
        };
        assert_eq!(intersect.depth(), 3); // 1 + max(2, 1)

        let except = PhysicalOp::Except {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Limit {
                input: Box::new(PhysicalOp::Empty),
                count: 10,
                offset: 0,
            }),
        };
        assert_eq!(except.depth(), 3); // 1 + max(1, 2)
    }

    #[test]
    fn test_depth_temporal_operators() {
        let temporal_track = PhysicalOp::TemporalTrack {
            input: Box::new(PhysicalOp::NodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
            }),
            time_range: TimeRange::new(1000.into(), 2000.into()).unwrap(),
        };
        assert_eq!(temporal_track.depth(), 2); // 1 + 1
    }

    // ==================== Additional is_leaf Tests ====================

    #[test]
    fn test_is_leaf_additional_non_leaf_operators() {
        // Temporal track
        assert!(
            !PhysicalOp::TemporalTrack {
                input: Box::new(PhysicalOp::Empty),
                time_range: TimeRange::new(1000.into(), 2000.into()).unwrap(),
            }
            .is_leaf()
        );

        // Binary operators
        assert!(
            !PhysicalOp::Intersect {
                left: Box::new(PhysicalOp::Empty),
                right: Box::new(PhysicalOp::Empty),
            }
            .is_leaf()
        );

        assert!(
            !PhysicalOp::Except {
                left: Box::new(PhysicalOp::Empty),
                right: Box::new(PhysicalOp::Empty),
            }
            .is_leaf()
        );
    }

    // ==================== Explain Tests ====================

    #[test]
    fn test_explain_simple_node_lookup() {
        let plan = PhysicalPlan {
            root: PhysicalOp::NodeLookup {
                node_ids: vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()],
            },
            estimated_cost: Cost {
                cpu: 1.0,
                io: 0.0,
                memory: 100,
                network: 0.0,
            },
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let explanation = plan.explain();

        // Should include operator name
        assert!(explanation.contains("NodeLookup"));
        // Should include cost estimate
        assert!(explanation.contains("cost"));
        // Should include cardinality (2 nodes)
        assert!(explanation.contains("rows"));
    }

    #[test]
    fn test_explain_nested_operations() {
        let plan = PhysicalPlan {
            root: PhysicalOp::Filter {
                input: Box::new(PhysicalOp::IndexedTraversal {
                    input: Box::new(PhysicalOp::NodeLookup {
                        node_ids: vec![NodeId::new(1).unwrap()],
                    }),
                    direction: Direction::Outgoing,
                    label: Some("KNOWS".to_string()),
                    depth: 2,
                    temporal_context: None,
                }),
                predicate: Predicate::eq("age", 25i64),
            },
            estimated_cost: Cost {
                cpu: 10.0,
                io: 5.0,
                memory: 1000,
                network: 0.0,
            },
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let explanation = plan.explain();

        // Should show nested structure with indentation
        assert!(explanation.contains("Filter"));
        assert!(explanation.contains("IndexedTraversal"));
        assert!(explanation.contains("NodeLookup"));

        // Check that nested operators are indented correctly
        let lines: Vec<&str> = explanation.lines().collect();
        assert_eq!(lines.len(), 4, "Expected 4 lines for header + 3 nested ops");
        assert!(lines[1].starts_with("Filter"));
        assert!(lines[2].starts_with("  └─ IndexedTraversal"));
        assert!(lines[3].starts_with("    └─ NodeLookup"));
    }

    #[test]
    fn test_explain_with_temporal_context() {
        let plan = PhysicalPlan {
            root: PhysicalOp::TemporalNodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
                valid_time: 1000.into(),
                transaction_time: 2000.into(),
                use_batch: false,
            },
            estimated_cost: Cost {
                cpu: 50.0,
                io: 10.0,
                memory: 500,
                network: 0.0,
            },
            temporal_context: Some(TemporalContext::as_of(1000.into(), 2000.into())),
            parallel: false,
            include_provenance: false,
        };

        let explanation = plan.explain();

        // Should mention temporal context
        assert!(explanation.contains("Temporal") || explanation.contains("temporal"));
        assert!(explanation.contains("TemporalNodeLookup"));
    }

    #[test]
    fn test_explain_binary_operations() {
        let plan = PhysicalPlan {
            root: PhysicalOp::HashJoin {
                left: Box::new(PhysicalOp::NodeScan {
                    label: Some("Person".to_string()),
                    estimated_rows: 100,
                }),
                right: Box::new(PhysicalOp::NodeScan {
                    label: Some("Company".to_string()),
                    estimated_rows: 50,
                }),
                left_key: "id".to_string(),
                right_key: "person_id".to_string(),
            },
            estimated_cost: Cost {
                cpu: 100.0,
                io: 20.0,
                memory: 5000,
                network: 0.0,
            },
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let explanation = plan.explain();

        // Should show both sides of the join
        assert!(explanation.contains("HashJoin"));
        assert!(explanation.contains("Person"));
        assert!(explanation.contains("Company"));
    }

    #[test]
    fn test_explain_includes_cost_breakdown() {
        let plan = PhysicalPlan {
            root: PhysicalOp::HnswSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                label_filter: Some("Document".to_string()),
                property_key: None,
            },
            estimated_cost: Cost {
                cpu: 15.5,
                io: 2.0,
                memory: 1024,
                network: 0.0,
            },
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let explanation = plan.explain();

        // Check for exact formatted cost strings
        assert!(explanation.contains("cpu=15.5"));
        assert!(explanation.contains("io=2.0"));
        assert!(explanation.contains("mem=1.0KB"));
    }

    // ==================== Property Key Explain Tests (Issue #411) ====================

    #[test]
    fn test_explain_hnsw_search_with_property_key() {
        let plan = PhysicalOp::HnswSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            label_filter: None,
            property_key: Some("title_embedding".to_string()),
        };

        let explain = plan.explain();
        assert!(explain.contains("HnswSearch"));
        assert!(explain.contains("k: 10"));
        assert!(explain.contains("prop: title_embedding"));
    }

    #[test]
    fn test_explain_hnsw_search_without_property_key() {
        let plan = PhysicalOp::HnswSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            label_filter: None,
            property_key: None,
        };

        let explain = plan.explain();
        assert!(explain.contains("HnswSearch"));
        assert!(explain.contains("k: 10"));
        // Should not show property key when None
        assert!(!explain.contains("prop:"));
    }

    #[test]
    fn test_explain_temporal_vector_search_with_property_key() {
        let plan = PhysicalOp::TemporalVectorSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            timestamp: 42000.into(),
            property_key: Some("content_embedding".to_string()),
        };

        let explain = plan.explain();
        assert!(explain.contains("TemporalVectorSearch"));
        assert!(explain.contains("k: 10"));
        assert!(explain.contains("ts: 42000"));
        assert!(explain.contains("prop: content_embedding"));
    }

    #[test]
    fn test_explain_temporal_vector_search_without_property_key() {
        let plan = PhysicalOp::TemporalVectorSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            timestamp: 42000.into(),
            property_key: None,
        };

        let explain = plan.explain();
        assert!(explain.contains("TemporalVectorSearch"));
        assert!(explain.contains("k: 10"));
        assert!(explain.contains("ts: 42000"));
        // Should not show property key when None
        assert!(!explain.contains("prop:"));
    }

    #[test]
    fn test_explain_similar_to_node_with_property_key() {
        let plan = PhysicalOp::SimilarToNode {
            source_node: NodeId::new(42).unwrap(),
            property_key: "document_embedding".to_string(),
            k: 5,
            label_filter: Some("Document".to_string()),
        };

        let explain = plan.explain();
        assert!(explain.contains("SimilarToNode"));
        assert!(explain.contains("k: 5"));
        assert!(explain.contains("label: Some(\"Document\")"));
        assert!(explain.contains("prop: document_embedding"));
    }
}
