//! Physical Query Plan
//!
//! Physical operators that directly map to execution primitives.
//! These are the "instructions" that the query executor runs.

use std::sync::Arc;

use crate::core::NodeId;
use crate::core::temporal::{TimeRange, Timestamp};

use super::super::ir::{Direction, Predicate};
use super::super::plan::{SortKey, TemporalContext};
use super::cost::Cost;

/// A physical query plan ready for execution.
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
    },

    /// Temporal node lookup (historical)
    TemporalNodeLookup {
        /// Node IDs to look up
        node_ids: Vec<NodeId>,
        /// Valid time for the query
        valid_time: Timestamp,
        /// Transaction time for the query
        transaction_time: Timestamp,
    },

    /// Temporal vector search using historical snapshots
    TemporalVectorSearch {
        /// Query embedding
        embedding: Arc<[f32]>,
        /// Number of results
        k: usize,
        /// Timestamp for the historical query
        timestamp: Timestamp,
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
                k, label_filter, ..
            } => {
                format!("{prefix}{name} (k: {}, label: {:?})", k, label_filter)
            }
            PhysicalOp::TemporalNodeLookup {
                node_ids,
                valid_time,
                transaction_time,
            } => {
                format!(
                    "{prefix}{name} (ids: {:?}, vt: {}, tt: {})",
                    node_ids, valid_time, transaction_time
                )
            }
            PhysicalOp::IndexedTraversal {
                input,
                direction,
                label,
                depth,
            } => {
                format!(
                    "{prefix}{name} (dir: {:?}, label: {:?}, depth: {})\n{}",
                    direction,
                    label,
                    depth,
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
}
