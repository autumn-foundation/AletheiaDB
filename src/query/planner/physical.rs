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

    // ==================== PhysicalPlan Tests ====================

    #[test]
    fn test_physical_plan_is_temporal() {
        let plan = PhysicalPlan {
            root: PhysicalOp::Empty,
            estimated_cost: Cost::default(),
            temporal_context: None,
            parallel: false,
        };
        assert!(!plan.is_temporal());

        let temporal_plan = PhysicalPlan {
            root: PhysicalOp::Empty,
            estimated_cost: Cost::default(),
            temporal_context: Some(TemporalContext {
                as_of: Some((1000, 2000)),
                between: None,
            }),
            parallel: false,
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
                label_filter: None
            }
            .name(),
            "HnswSearch"
        );
        assert_eq!(
            PhysicalOp::TemporalNodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
                valid_time: 1000,
                transaction_time: 2000
            }
            .name(),
            "TemporalNodeLookup"
        );
        assert_eq!(
            PhysicalOp::TemporalVectorSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                timestamp: 1000
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
                depth: 1
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
                k: 10
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
                time_range: TimeRange::new(1000, 2000)
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
                label_filter: None
            }
            .is_leaf()
        );
        assert!(
            PhysicalOp::TemporalNodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
                valid_time: 1000,
                transaction_time: 2000
            }
            .is_leaf()
        );
        assert!(
            PhysicalOp::TemporalVectorSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                timestamp: 1000
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
                depth: 1
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
                k: 10
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
                label_filter: None
            }
            .depth(),
            1
        );
        assert_eq!(
            PhysicalOp::TemporalNodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
                valid_time: 1000,
                transaction_time: 2000
            }
            .depth(),
            1
        );
        assert_eq!(
            PhysicalOp::TemporalVectorSearch {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                k: 10,
                timestamp: 1000
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
                time_range: TimeRange::new(1000, 2000)
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
            valid_time: 1000,
            transaction_time: 2000,
        };

        let explain = plan.explain();
        assert!(explain.contains("TemporalNodeLookup"));
        assert!(explain.contains("vt: 1000"));
        assert!(explain.contains("tt: 2000"));
    }

    #[test]
    fn test_explain_indexed_traversal() {
        let plan = PhysicalOp::IndexedTraversal {
            input: Box::new(PhysicalOp::Empty),
            direction: Direction::Outgoing,
            label: Some("KNOWS".to_string()),
            depth: 2,
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
            timestamp: 42000,
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
            time_range: TimeRange::new(1000, 2000),
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
                timestamp: 1000,
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
                time_range: TimeRange::new(1000, 2000),
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
            time_range: TimeRange::new(1000, 2000),
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
                time_range: TimeRange::new(1000, 2000),
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
}
