//! Query Intermediate Representation (IR)
//!
//! Defines the core types for representing hybrid queries before planning.
//! These types are used by the [`QueryBuilder`](super::builder::QueryBuilder)
//! and transformed into logical plans by the planner.

use std::ops::Not;
use std::sync::Arc;

use crate::core::NodeId;
use crate::core::temporal::{TimeRange, Timestamp};
use crate::index::vector::DistanceMetric;

/// A query operation that can be composed into a query pipeline.
///
/// Operations are categorized into:
/// - **Source operations**: Define starting points (nodes, scans, vector search)
/// - **Graph operations**: Traverse relationships
/// - **Vector operations**: Similarity-based operations
/// - **Temporal operations**: Time-travel context
/// - **Filter operations**: Restrict results
#[derive(Debug, Clone)]
pub enum QueryOp {
    // === Source Operations ===
    /// Start from a specific node
    StartNode(NodeId),

    /// Start from multiple nodes
    StartNodes(Vec<NodeId>),

    /// Scan all nodes, optionally filtered by label
    ScanNodes {
        /// Optional label filter (e.g., "Person", "Document")
        label: Option<String>,
    },

    // === Graph Operations ===
    /// Traverse outgoing edges
    TraverseOut {
        /// Optional edge label filter
        label: Option<String>,
        /// Traversal depth specification
        depth: TraversalDepth,
    },

    /// Traverse incoming edges
    TraverseIn {
        /// Optional edge label filter
        label: Option<String>,
        /// Traversal depth specification
        depth: TraversalDepth,
    },

    /// Traverse edges in both directions
    TraverseBoth {
        /// Optional edge label filter
        label: Option<String>,
        /// Traversal depth specification
        depth: TraversalDepth,
    },

    /// Get edges connected to current nodes
    GetEdges {
        /// Direction to get edges
        direction: Direction,
    },

    // === Vector Operations ===
    /// Find k-nearest neighbors to an embedding vector
    VectorSearch {
        /// Query embedding vector
        embedding: Arc<[f32]>,
        /// Number of results to return
        k: usize,
        /// Distance metric to use
        metric: DistanceMetric,
        /// Property key containing the embedding (default: "embedding")
        property_key: Option<String>,
    },

    /// Find nodes similar to a specific node's embedding
    SimilarTo {
        /// Source node whose embedding to use
        source_node: NodeId,
        /// Number of results to return
        k: usize,
        /// Property key containing the embedding (default: "embedding")
        property_key: Option<String>,
        /// Optional label filter for results
        label_filter: Option<String>,
    },

    /// Rank current results by similarity to a target embedding
    RankBySimilarity {
        /// Target embedding for similarity comparison
        embedding: Arc<[f32]>,
        /// Optional limit on results (if None, ranks all)
        top_k: Option<usize>,
        /// Property key containing the embedding (default: "embedding")
        property_key: Option<String>,
    },

    // === Temporal Operations ===
    /// Query at a specific point in time (bi-temporal)
    AsOf {
        /// Valid time for the query
        valid_time: Timestamp,
        /// Transaction time for the query
        transaction_time: Timestamp,
    },

    /// Query across a time range
    Between {
        /// Time range for the query
        time_range: TimeRange,
    },

    /// Track changes to entities over time
    TrackChanges {
        /// Time range to track changes within
        time_range: TimeRange,
    },

    // === Filter Operations ===
    /// Filter by property predicate
    Filter(Predicate),

    /// Filter by node/edge label
    FilterLabel(String),

    /// Limit the number of results
    Limit(usize),

    /// Skip a number of results
    Skip(usize),

    /// Sort results by a key
    Sort {
        /// Sort key (property name or special key like "score")
        key: SortKey,
        /// Sort order (true = descending)
        descending: bool,
    },

    // === Aggregation Operations ===
    /// Count results
    Count,

    /// Collect unique values
    Distinct,

    /// Project specific properties
    Project(Vec<String>),
}

/// Specifies how deep to traverse in graph operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalDepth {
    /// Exactly N hops from the starting node
    Exact(usize),

    /// At most N hops (0..=N), returns all encountered nodes
    Max(usize),

    /// Range of hops (min..=max)
    Range {
        /// Minimum depth
        min: usize,
        /// Maximum depth
        max: usize,
    },

    /// Variable depth (unbounded) - use with caution as it can be expensive
    Variable,
}

impl Default for TraversalDepth {
    fn default() -> Self {
        TraversalDepth::Exact(1)
    }
}

impl TraversalDepth {
    /// Create a single-hop traversal
    #[must_use]
    pub fn one() -> Self {
        TraversalDepth::Exact(1)
    }

    /// Create a multi-hop traversal of exactly N hops
    #[must_use]
    pub fn hops(n: usize) -> Self {
        TraversalDepth::Exact(n)
    }

    /// Create a traversal up to N hops
    #[must_use]
    pub fn up_to(n: usize) -> Self {
        TraversalDepth::Max(n)
    }

    /// Create a traversal within a range of hops
    #[must_use]
    pub fn between(min: usize, max: usize) -> Self {
        TraversalDepth::Range { min, max }
    }

    /// Get the maximum depth for this specification (None if unbounded)
    #[must_use]
    pub fn max_depth(&self) -> Option<usize> {
        match self {
            TraversalDepth::Exact(n) | TraversalDepth::Max(n) => Some(*n),
            TraversalDepth::Range { max, .. } => Some(*max),
            TraversalDepth::Variable => None,
        }
    }
}

/// Edge traversal direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    /// Outgoing edges (source → target)
    #[default]
    Outgoing,
    /// Incoming edges (target ← source)
    Incoming,
    /// Both directions
    Both,
}

/// Key for sorting results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortKey {
    /// Sort by a property value
    Property(String),
    /// Sort by similarity score (for vector search results)
    Score,
    /// Sort by timestamp (for temporal queries)
    Timestamp,
}

/// Property predicates for filtering nodes and edges.
///
/// Predicates can be combined using logical operators (And, Or, Not).
#[derive(Debug, Clone)]
pub enum Predicate {
    /// Property equals a value
    Eq {
        /// Property key
        key: String,
        /// Expected value
        value: PredicateValue,
    },

    /// Property does not equal a value
    Ne {
        /// Property key
        key: String,
        /// Value to exclude
        value: PredicateValue,
    },

    /// Property is greater than a value
    Gt {
        /// Property key
        key: String,
        /// Threshold value
        value: PredicateValue,
    },

    /// Property is greater than or equal to a value
    Gte {
        /// Property key
        key: String,
        /// Threshold value
        value: PredicateValue,
    },

    /// Property is less than a value
    Lt {
        /// Property key
        key: String,
        /// Threshold value
        value: PredicateValue,
    },

    /// Property is less than or equal to a value
    Lte {
        /// Property key
        key: String,
        /// Threshold value
        value: PredicateValue,
    },

    /// Property value is in a list
    In {
        /// Property key
        key: String,
        /// List of acceptable values
        values: Vec<PredicateValue>,
    },

    /// String property contains a substring
    Contains {
        /// Property key
        key: String,
        /// Substring to search for
        substring: String,
    },

    /// String property starts with a prefix
    StartsWith {
        /// Property key
        key: String,
        /// Prefix to match
        prefix: String,
    },

    /// String property ends with a suffix
    EndsWith {
        /// Property key
        key: String,
        /// Suffix to match
        suffix: String,
    },

    /// Property exists on the entity
    Exists(String),

    /// Property does not exist on the entity
    NotExists(String),

    /// Logical AND of multiple predicates
    And(Vec<Predicate>),

    /// Logical OR of multiple predicates
    Or(Vec<Predicate>),

    /// Logical NOT of a predicate
    Not(Box<Predicate>),

    /// Always true (identity for AND)
    True,

    /// Always false (identity for OR)
    False,
}

impl Predicate {
    /// Create an equality predicate
    #[must_use]
    pub fn eq(key: impl Into<String>, value: impl Into<PredicateValue>) -> Self {
        Predicate::Eq {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Create a not-equal predicate
    #[must_use]
    pub fn ne(key: impl Into<String>, value: impl Into<PredicateValue>) -> Self {
        Predicate::Ne {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Create a greater-than predicate
    #[must_use]
    pub fn gt(key: impl Into<String>, value: impl Into<PredicateValue>) -> Self {
        Predicate::Gt {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Create a less-than predicate
    #[must_use]
    pub fn lt(key: impl Into<String>, value: impl Into<PredicateValue>) -> Self {
        Predicate::Lt {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Create an exists predicate
    #[must_use]
    pub fn exists(key: impl Into<String>) -> Self {
        Predicate::Exists(key.into())
    }

    /// Create a contains predicate for string properties
    #[must_use]
    pub fn contains(key: impl Into<String>, substring: impl Into<String>) -> Self {
        Predicate::Contains {
            key: key.into(),
            substring: substring.into(),
        }
    }

    /// Combine with another predicate using AND
    #[must_use]
    pub fn and(self, other: Predicate) -> Self {
        match (self, other) {
            (Predicate::True, p) | (p, Predicate::True) => p,
            (Predicate::False, _) | (_, Predicate::False) => Predicate::False,
            (Predicate::And(mut preds), Predicate::And(others)) => {
                preds.extend(others);
                Predicate::And(preds)
            }
            (Predicate::And(mut preds), other) => {
                preds.push(other);
                Predicate::And(preds)
            }
            (this, Predicate::And(mut preds)) => {
                preds.insert(0, this);
                Predicate::And(preds)
            }
            (a, b) => Predicate::And(vec![a, b]),
        }
    }

    /// Combine with another predicate using OR
    #[must_use]
    pub fn or(self, other: Predicate) -> Self {
        match (self, other) {
            (Predicate::False, p) | (p, Predicate::False) => p,
            (Predicate::True, _) | (_, Predicate::True) => Predicate::True,
            (Predicate::Or(mut preds), Predicate::Or(others)) => {
                preds.extend(others);
                Predicate::Or(preds)
            }
            (Predicate::Or(mut preds), other) => {
                preds.push(other);
                Predicate::Or(preds)
            }
            (this, Predicate::Or(mut preds)) => {
                preds.insert(0, this);
                Predicate::Or(preds)
            }
            (a, b) => Predicate::Or(vec![a, b]),
        }
    }

    /// Negate this predicate (use `!predicate` instead)
    #[must_use]
    pub fn negate(self) -> Self {
        !self
    }
}

impl Not for Predicate {
    type Output = Self;

    fn not(self) -> Self::Output {
        match self {
            Predicate::True => Predicate::False,
            Predicate::False => Predicate::True,
            Predicate::Not(inner) => *inner,
            other => Predicate::Not(Box::new(other)),
        }
    }
}

/// Values that can be used in predicates.
///
/// This is a simplified version of PropertyValue for query purposes.
#[derive(Debug, Clone, PartialEq)]
pub enum PredicateValue {
    /// Null value
    Null,
    /// Boolean value
    Bool(bool),
    /// Integer value (i64)
    Int(i64),
    /// Floating-point value (f64)
    Float(f64),
    /// String value
    String(String),
}

impl From<bool> for PredicateValue {
    fn from(v: bool) -> Self {
        PredicateValue::Bool(v)
    }
}

impl From<i64> for PredicateValue {
    fn from(v: i64) -> Self {
        PredicateValue::Int(v)
    }
}

impl From<i32> for PredicateValue {
    fn from(v: i32) -> Self {
        PredicateValue::Int(v as i64)
    }
}

impl From<f64> for PredicateValue {
    fn from(v: f64) -> Self {
        PredicateValue::Float(v)
    }
}

impl From<f32> for PredicateValue {
    fn from(v: f32) -> Self {
        PredicateValue::Float(v as f64)
    }
}

impl From<String> for PredicateValue {
    fn from(v: String) -> Self {
        PredicateValue::String(v)
    }
}

impl From<&str> for PredicateValue {
    fn from(v: &str) -> Self {
        PredicateValue::String(v.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_traversal_depth_constructors() {
        assert_eq!(TraversalDepth::one(), TraversalDepth::Exact(1));
        assert_eq!(TraversalDepth::hops(3), TraversalDepth::Exact(3));
        assert_eq!(TraversalDepth::up_to(5), TraversalDepth::Max(5));
        assert_eq!(
            TraversalDepth::between(2, 4),
            TraversalDepth::Range { min: 2, max: 4 }
        );
    }

    #[test]
    fn test_traversal_depth_max_depth() {
        assert_eq!(TraversalDepth::Exact(3).max_depth(), Some(3));
        assert_eq!(TraversalDepth::Max(5).max_depth(), Some(5));
        assert_eq!(
            TraversalDepth::Range { min: 1, max: 10 }.max_depth(),
            Some(10)
        );
        assert_eq!(TraversalDepth::Variable.max_depth(), None);
    }

    #[test]
    fn test_predicate_constructors() {
        let eq = Predicate::eq("name", "Alice");
        assert!(matches!(eq, Predicate::Eq { .. }));

        let gt = Predicate::gt("age", 18);
        assert!(matches!(gt, Predicate::Gt { .. }));

        let exists = Predicate::exists("email");
        assert!(matches!(exists, Predicate::Exists(_)));
    }

    #[test]
    fn test_predicate_and() {
        let p1 = Predicate::eq("a", 1);
        let p2 = Predicate::eq("b", 2);
        let combined = p1.and(p2);

        assert!(matches!(combined, Predicate::And(_)));
    }

    #[test]
    fn test_predicate_and_identity() {
        let p = Predicate::eq("a", 1);

        // True is identity for AND
        let result = p.clone().and(Predicate::True);
        assert!(matches!(result, Predicate::Eq { .. }));

        // False absorbs AND
        let result = p.and(Predicate::False);
        assert!(matches!(result, Predicate::False));
    }

    #[test]
    fn test_predicate_or_identity() {
        let p = Predicate::eq("a", 1);

        // False is identity for OR
        let result = p.clone().or(Predicate::False);
        assert!(matches!(result, Predicate::Eq { .. }));

        // True absorbs OR
        let result = p.or(Predicate::True);
        assert!(matches!(result, Predicate::True));
    }

    #[test]
    fn test_predicate_not() {
        let p = Predicate::eq("a", 1);
        let negated = !p;
        assert!(matches!(negated, Predicate::Not(_)));

        // Double negation
        let double = !negated;
        assert!(matches!(double, Predicate::Eq { .. }));
    }

    #[test]
    fn test_predicate_value_conversions() {
        let v: PredicateValue = true.into();
        assert_eq!(v, PredicateValue::Bool(true));

        let v: PredicateValue = 42i64.into();
        assert_eq!(v, PredicateValue::Int(42));

        let v: PredicateValue = 1.23f64.into();
        assert_eq!(v, PredicateValue::Float(1.23));

        let v: PredicateValue = "hello".into();
        assert_eq!(v, PredicateValue::String("hello".to_string()));
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;

    #[test]
    fn test_predicate_not_identities() {
        // 🛡️ Sentry Test: Verify boolean identities for negation.
        // !True = False, !False = True.
        // This targets mutants that might break the boolean logic in `not()`.

        let not_true = !Predicate::True;
        assert!(
            matches!(not_true, Predicate::False),
            "!True should be False, got {:?}",
            not_true
        );

        let not_false = !Predicate::False;
        assert!(
            matches!(not_false, Predicate::True),
            "!False should be True, got {:?}",
            not_false
        );
    }

    #[test]
    fn test_predicate_double_negation_complex() {
        // 🛡️ Sentry Test: Verify double negation simplification for nested predicates.
        // !!(A AND B) -> (A AND B).
        // The implementation recursively unwraps Not(Not(X)) -> X.

        let inner = Predicate::And(vec![Predicate::eq("a", 1), Predicate::eq("b", 2)]);

        let not_inner = !inner.clone();
        assert!(matches!(not_inner, Predicate::Not(_)));

        let double_not = !not_inner;

        // Should be structurally equal to inner
        // We can't use assert_eq! directly because Predicate doesn't derive PartialEq
        // So we match the structure.
        if let Predicate::And(preds) = double_not {
            assert_eq!(preds.len(), 2);
            match &preds[0] {
                Predicate::Eq { key, .. } => assert_eq!(key, "a"),
                _ => panic!("Expected Eq"),
            }
            match &preds[1] {
                Predicate::Eq { key, .. } => assert_eq!(key, "b"),
                _ => panic!("Expected Eq"),
            }
        } else {
            panic!("Expected And predicate after double negation");
        }
    }
}
