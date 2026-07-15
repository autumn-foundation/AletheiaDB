//! Query Intermediate Representation (IR)
//!
//! Defines the core types for representing hybrid queries before planning.
//! These types are used by the [`QueryBuilder`](super::builder::QueryBuilder)
//! and transformed into logical plans by the planner.

use std::ops::Not;
use std::sync::Arc;

use crate::core::NodeId;
use crate::core::temporal::{TimeRange, Timestamp};
// The brute-force vector rerank path (`RankBySimilarity`) selects its metric
// from the `core::vector` family (Cosine/Euclidean/DotProduct), which exposes a
// ready `compute_similarity` used by the executor. This is distinct from the
// HNSW-index `DistanceMetric` (6 variants) used by `VectorSearch` below.
use crate::core::vector::DistanceMetric as VectorMetric;
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

    /// Scan all edges, optionally filtered by edge type
    ScanEdges {
        /// Optional edge type filter (e.g., "KNOWS", "FOLLOWS")
        edge_type: Option<String>,
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
        /// Distance/similarity metric selected by the calling surface (e.g. the
        /// Cypher `vector.cosine` / `vector.euclidean` / `vector.dot_product`
        /// function name). Determines how each candidate is scored; `Cosine`
        /// reproduces the historical default.
        metric: VectorMetric,
        /// Optional threshold that keeps only rows whose similarity score
        /// satisfies the comparison (Cypher `WHERE vector.similarity(...) > t`).
        /// `None` ranks/keeps all input rows.
        threshold: Option<ScoreThreshold>,
        /// When set, the computed similarity score is also materialized as a
        /// named output column under this alias (Cypher
        /// `vector.cosine(...) AS score`), in addition to `QueryRow::score`.
        score_alias: Option<String>,
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

    /// Left-outer application of an optional sub-pattern (Cypher `OPTIONAL MATCH`).
    ///
    /// For each input row, the sub-pipeline in `ops` is executed seeded from
    /// that row. If it produces at least one row, those rows are emitted;
    /// otherwise a single null-bound row is emitted so the input row is
    /// preserved (openCypher left-outer semantics).
    ///
    /// Allowed sub-operations are `TraverseOut`/`TraverseIn`/`TraverseBoth`
    /// and `Filter`. When `Optional` is the *first* operation of a query
    /// (a leading `OPTIONAL MATCH`), the sub-pipeline must instead begin with
    /// a `ScanNodes` source; if the whole scan pipeline yields nothing, one
    /// null row is emitted.
    ///
    /// Filters inside `ops` (inline pattern properties and the clause's
    /// `WHERE`) participate in the matched/unmatched decision -- they run
    /// *before* the null-row fallback, not after it.
    Optional {
        /// Sub-pipeline executed per input row (or once, for the leading form).
        ops: Vec<QueryOp>,
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

    /// Grouped aggregation over the input rows (openCypher implicit grouping).
    ///
    /// Partitions input rows by the values of `group_keys` (empty = a single
    /// global group) and, for each group, evaluates every entry in
    /// `aggregates`. Emits one output row per group carrying the group-key
    /// values followed by the aggregate values as named columns. A global
    /// aggregation over empty input still emits exactly one row (e.g.
    /// `count(*) = 0`); a grouped aggregation over empty input emits no rows.
    Aggregate {
        /// Grouping keys (non-aggregate `RETURN`/`WITH` items). Empty means a
        /// single global group.
        group_keys: Vec<AggregateGroupKey>,
        /// The aggregate expressions to compute per group.
        aggregates: Vec<AggregateSpec>,
    },

    /// Collect unique values
    Distinct,

    /// Project specific properties
    Project(Vec<String>),

    /// Project one or more provenance accessors (`RETURN source(x)`,
    /// `RETURN n, confidence(n) AS conf`, Issue #3354) as output columns.
    ///
    /// This op runs **last** in the pipeline (after any Sort/Skip/Limit) so it
    /// never interferes with entity-based ordering or pagination. For each input
    /// row it resolves the row entity's write-time provenance (at the query's
    /// bi-temporal coordinate, exactly as the `WHERE` provenance leaves do) and
    /// attaches one column per projection. When the `RETURN` also names a bare
    /// entity variable, the row is emitted through the bindings+columns shape so
    /// the entity survives alongside the projected columns (mirroring the
    /// multi-variable / aggregation row shape); otherwise a pure columns row is
    /// produced.
    ProjectProvenance(ProvenanceProjection),
}

/// A single provenance accessor projected as an output column
/// (`source(x) AS alias`, Issue #3354).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceProjectionItem {
    /// The output column name (the alias if one was given, else the accessor
    /// rendered as `"<accessor>(<var>)"`).
    pub output_name: String,
    /// Which provenance field to resolve.
    pub field: ProvenanceField,
}

/// The plan for [`QueryOp::ProjectProvenance`]: the bare-entity binding to
/// preserve (if any) plus the ordered provenance columns to project
/// (Issue #3354).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceProjection {
    /// The variable name of a bare entity also returned (`RETURN n, source(n)`),
    /// preserved via the bindings channel so it stays observable. `None` when
    /// only accessors are projected (`RETURN source(n)`), yielding a pure
    /// columns row.
    pub entity_binding: Option<String>,
    /// The provenance columns to project, in `RETURN` order.
    pub items: Vec<ProvenanceProjectionItem>,
}

/// A comparison operator for a [`ScoreThreshold`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreComparison {
    /// Keep rows whose score is strictly greater than the threshold.
    Gt,
    /// Keep rows whose score is greater than or equal to the threshold.
    Ge,
    /// Keep rows whose score is strictly less than the threshold.
    Lt,
    /// Keep rows whose score is less than or equal to the threshold.
    Le,
}

/// A similarity-score threshold applied by [`QueryOp::RankBySimilarity`].
///
/// The comparison is evaluated against the metric's **similarity** score
/// (higher = more similar), i.e. the same value materialized on
/// [`crate::query::executor::QueryRow::score`]. For `Euclidean` that similarity
/// is `1 / (1 + distance)` rather than the raw distance (see the Cypher vector
/// guide); prefer `Cosine`/`DotProduct`/`similarity` for threshold filters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreThreshold {
    /// The comparison operator.
    pub op: ScoreComparison,
    /// The threshold value to compare the similarity score against.
    pub value: f32,
}

impl ScoreThreshold {
    /// Returns `true` if `score` satisfies this threshold.
    #[inline]
    pub fn passes(&self, score: f32) -> bool {
        match self.op {
            ScoreComparison::Gt => score > self.value,
            ScoreComparison::Ge => score >= self.value,
            ScoreComparison::Lt => score < self.value,
            ScoreComparison::Le => score <= self.value,
        }
    }
}

/// A grouping key in a [`QueryOp::Aggregate`] (a non-aggregate projection
/// item such as `n.dept`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateGroupKey {
    /// The node property to read for the grouping value (e.g. `"dept"` for
    /// `n.dept`).
    pub property_key: String,
    /// The output column name for this key (alias if given, else the source
    /// text such as `"n.dept"`).
    pub alias: String,
}

/// A single aggregate expression in a [`QueryOp::Aggregate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateSpec {
    /// Which aggregate function to apply.
    pub func: AggregateFunc,
    /// What the aggregate reads from each row.
    pub arg: AggregateArg,
    /// Whether the `DISTINCT` quantifier was supplied (deduplicates values
    /// before aggregating).
    pub distinct: bool,
    /// The output column name (alias if given, else generated source text such
    /// as `"count(*)"`).
    pub alias: String,
}

/// The argument an aggregate reads from each input row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregateArg {
    /// `count(*)` -- count every row unconditionally (ignores nulls/DISTINCT).
    Star,
    /// `count(n)` -- count rows whose bound entity is non-null (openCypher
    /// `count(<variable>)`). Distinct from [`AggregateArg::Star`].
    Entity,
    /// `func(n.prop)` -- read the named node property from each row.
    Property(String),
}

/// The aggregate functions supported by [`QueryOp::Aggregate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateFunc {
    /// `count(...)` / `count(*)` -- number of (non-null) rows/values.
    Count,
    /// `sum(...)` -- numeric sum, skipping nulls (0 over no values).
    Sum,
    /// `avg(...)` -- numeric mean, skipping nulls (null over no values).
    Avg,
    /// `min(...)` -- minimum value in natural order, skipping nulls.
    Min,
    /// `max(...)` -- maximum value in natural order, skipping nulls.
    Max,
    /// `collect(...)` -- gather non-null values into a list, preserving order.
    Collect,
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

    /// Get the minimum depth for this specification.
    ///
    /// Variable-length traversal in this engine binds the far node to each
    /// **distinct** reachable target once, at its **shortest** hop-distance,
    /// returning it iff `min <= shortestDepth <= max`. This returns the lower
    /// bound `min`:
    /// - `Exact(n)` => `n` (a single fixed depth),
    /// - `Max(_)` => `1` (`*..n` starts at one hop),
    /// - `Range { min, .. }` => `min`,
    /// - `Variable` => `1` (`*` starts at one hop).
    ///
    /// Note this is **node-distinct / shortest-path reachability**, a deliberate
    /// v1 simplification of openCypher's trail (path-enumeration) semantics: a
    /// node whose *shortest* path is shorter than `min` is not re-emitted at a
    /// longer, in-range depth, and an anchor reachable only via an in-range
    /// cycle is not re-bound. See the `TraversalIterator` docs for details;
    /// full trail semantics is a tracked follow-up.
    #[must_use]
    pub fn min_depth(&self) -> usize {
        match self {
            TraversalDepth::Exact(n) => *n,
            TraversalDepth::Max(_) | TraversalDepth::Variable => 1,
            TraversalDepth::Range { min, .. } => *min,
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
    /// Sort by a provenance accessor (`ORDER BY source(x)`/`confidence(x)`/
    /// `reason(x)`, Issue #3354). The value is resolved per row from the write-
    /// time provenance recorded on the version the row represents (the historical
    /// version at the query's bi-temporal coordinate for an `AS OF` query),
    /// exactly as the `WHERE`-clause provenance leaves resolve it. An
    /// unattributed row (or a bundle missing the field) sorts as a null value,
    /// following the same openCypher null placement as any other key.
    Provenance(ProvenanceField),
}

/// Property predicates for filtering nodes and edges.
///
/// Predicates can be combined using logical operators (And, Or, Not).
#[derive(Debug, Clone, PartialEq)]
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

    /// Filter on a version's write-time provenance (Issue #3354a).
    ///
    /// Unlike every other [`Predicate`] variant (which reads a stored *property*
    /// of the row entity), this reads the [`Provenance`](crate::core::provenance::Provenance)
    /// bundle recorded on the version the row represents — resolved from the
    /// entity's version id at the query's bi-temporal coordinate, so an `AS OF`
    /// query filters on the provenance recorded on the version visible at that
    /// coordinate (matching Issue #3348 semantics, AC3).
    Provenance(ProvenancePredicate),

    /// Always true (identity for AND)
    True,

    /// Always false (identity for OR)
    False,
}

/// A write-time provenance predicate over a single version, lowered from an AQL
/// `WHERE` provenance accessor (Issue #3354a).
///
/// Evaluated per-row against the `Option<Provenance>` recorded on the version
/// the row represents. Absent provenance (no bundle, or a bundle missing the
/// queried field) makes every accessor evaluate to null, so any comparison is
/// false and the row is excluded — deliberately mirroring
/// [`ProvenanceFilter::matches`](crate::core::provenance::ProvenanceFilter::matches)'s
/// exclude-unattributed default. The [`ProvenancePredicate::Filter`] arm reuses
/// that exact `matches` implementation for the foldable positive shapes.
#[derive(Debug, Clone, PartialEq)]
pub enum ProvenancePredicate {
    /// The foldable positive shapes that the reusable core
    /// [`ProvenanceFilter`](crate::core::provenance::ProvenanceFilter) already
    /// expresses, evaluated per-version via its `matches`:
    /// - `source(x) = 'S'`    → `sources = [S]` (any-of exact match)
    /// - `confidence(x) >= t` → `min_confidence = t` (inclusive lower bound)
    ///
    /// Correctness rides on the shared `matches` implementation, so the AQL
    /// accessor and the #3348 structured filter return identical result sets.
    Filter(crate::core::provenance::ProvenanceFilter),

    /// A general per-row accessor comparison for the operators the reusable
    /// `ProvenanceFilter` cannot express (`<`, `<=`, `>`, `<>`, and `=` on
    /// confidence/reason). Semantics for absent provenance still match
    /// `ProvenanceFilter::matches` (excluded).
    Accessor {
        /// Which provenance field the accessor reads.
        field: ProvenanceField,
        /// The comparison operator.
        op: ProvenanceCmp,
        /// The right-hand literal to compare against.
        operand: ProvenanceOperand,
    },

    /// `provenance(x) IS [NOT] NULL` — deliberately selects (or excludes) the
    /// versions with **no** provenance bundle at all. `negated == false` keeps
    /// rows whose bundle is absent (`IS NULL`); `negated == true` keeps rows
    /// whose bundle is present (`IS NOT NULL`).
    IsNull {
        /// `true` for `IS NOT NULL`, `false` for `IS NULL`.
        negated: bool,
    },
}

/// A provenance field addressable by an AQL accessor (Issue #3354a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceField {
    /// `source(x)` → [`Provenance::source`](crate::core::provenance::Provenance::source).
    Source,
    /// `confidence(x)` → [`Provenance::confidence`](crate::core::provenance::Provenance::confidence).
    Confidence,
    /// `reason(x)` → [`Provenance::note`](crate::core::provenance::Provenance::note)
    /// (the user-facing name for the internal `note` field).
    Reason,
}

impl ProvenanceField {
    /// The user-facing accessor name (`"source"`/`"confidence"`/`"reason"`).
    #[must_use]
    pub fn accessor_name(self) -> &'static str {
        match self {
            ProvenanceField::Source => "source",
            ProvenanceField::Confidence => "confidence",
            ProvenanceField::Reason => "reason",
        }
    }
}

/// Comparison operator for a [`ProvenancePredicate::Accessor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceCmp {
    /// `=`
    Eq,
    /// `<>` / `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

/// The right-hand literal of a [`ProvenancePredicate::Accessor`] comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum ProvenanceOperand {
    /// A string operand (for `source`/`reason`).
    Str(String),
    /// A numeric operand (for `confidence`, already validated to `[0,1]`).
    Num(f64),
}

impl ProvenancePredicate {
    /// Evaluate this predicate against the provenance bundle recorded on the
    /// version a row represents.
    ///
    /// `prov` is the `Option<Provenance>` for the exact version being
    /// considered (the historical version at the query's bi-temporal
    /// coordinate for an `AS OF` query), so the decision reflects provenance as
    /// recorded at that coordinate (Issue #3348 AC3).
    ///
    /// Absent provenance handling deliberately mirrors
    /// [`ProvenanceFilter::matches`](crate::core::provenance::ProvenanceFilter::matches):
    /// a missing bundle (or a bundle missing the queried field) fails every
    /// accessor comparison; only [`ProvenancePredicate::IsNull`] can select the
    /// no-bundle rows.
    #[must_use]
    pub fn evaluate(&self, prov: Option<&crate::core::provenance::Provenance>) -> bool {
        match self {
            // Reuse the shared core semantics for the foldable positive shapes.
            ProvenancePredicate::Filter(filter) => filter.matches(prov),
            ProvenancePredicate::IsNull { negated } => {
                if *negated {
                    prov.is_some()
                } else {
                    prov.is_none()
                }
            }
            ProvenancePredicate::Accessor { field, op, operand } => {
                match (field, operand) {
                    (ProvenanceField::Confidence, ProvenanceOperand::Num(rhs)) => {
                        match prov.and_then(crate::core::provenance::Provenance::confidence) {
                            Some(lhs) => op.compare_num(lhs, *rhs),
                            None => false, // absent field => excluded (matches semantics)
                        }
                    }
                    (ProvenanceField::Source, ProvenanceOperand::Str(rhs)) => {
                        match prov.and_then(crate::core::provenance::Provenance::source) {
                            Some(lhs) => op.compare_str(lhs, rhs),
                            None => false,
                        }
                    }
                    (ProvenanceField::Reason, ProvenanceOperand::Str(rhs)) => {
                        match prov.and_then(crate::core::provenance::Provenance::note) {
                            Some(lhs) => op.compare_str(lhs, rhs),
                            None => false,
                        }
                    }
                    // Field/operand type mismatches are rejected at convert time,
                    // so this arm is unreachable in practice; fail closed.
                    _ => false,
                }
            }
        }
    }
}

impl ProvenanceCmp {
    /// Apply this comparator to two numbers (for `confidence`).
    #[must_use]
    pub fn compare_num(self, lhs: f64, rhs: f64) -> bool {
        match self {
            ProvenanceCmp::Eq => lhs == rhs,
            ProvenanceCmp::Ne => lhs != rhs,
            ProvenanceCmp::Lt => lhs < rhs,
            ProvenanceCmp::Le => lhs <= rhs,
            ProvenanceCmp::Gt => lhs > rhs,
            ProvenanceCmp::Ge => lhs >= rhs,
        }
    }

    /// Apply this comparator to two strings (for `source`/`reason`).
    ///
    /// Only equality/inequality are meaningful for strings. The ordering
    /// comparators (`Lt`/`Le`/`Gt`/`Ge`) are **rejected at convert time** for
    /// the string accessors (see `build_provenance_comparison` in
    /// `src/query/converter.rs`), so they are unreachable through the AQL
    /// surface; the lexicographic arms below exist only for total-match
    /// completeness and are never exercised by a converted query.
    #[must_use]
    pub fn compare_str(self, lhs: &str, rhs: &str) -> bool {
        match self {
            ProvenanceCmp::Eq => lhs == rhs,
            ProvenanceCmp::Ne => lhs != rhs,
            ProvenanceCmp::Lt => lhs < rhs,
            ProvenanceCmp::Le => lhs <= rhs,
            ProvenanceCmp::Gt => lhs > rhs,
            ProvenanceCmp::Ge => lhs >= rhs,
        }
    }
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
mod provenance_predicate_tests {
    use super::*;
    use crate::core::provenance::{Provenance, ProvenanceFilter};

    fn prov(source: Option<&str>, confidence: Option<f64>, reason: Option<&str>) -> Provenance {
        Provenance::from_parts(
            source.map(String::from),
            confidence,
            reason.map(String::from),
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn is_null_selects_no_bundle_only() {
        let is_null = ProvenancePredicate::IsNull { negated: false };
        let is_not_null = ProvenancePredicate::IsNull { negated: true };
        assert!(is_null.evaluate(None));
        assert!(!is_null.evaluate(Some(&prov(Some("crm"), None, None))));
        assert!(!is_not_null.evaluate(None));
        assert!(is_not_null.evaluate(Some(&prov(Some("crm"), None, None))));
    }

    #[test]
    fn absent_provenance_fails_every_accessor() {
        let cases = [
            ProvenancePredicate::Accessor {
                field: ProvenanceField::Confidence,
                op: ProvenanceCmp::Ge,
                operand: ProvenanceOperand::Num(0.5),
            },
            ProvenancePredicate::Accessor {
                field: ProvenanceField::Confidence,
                op: ProvenanceCmp::Lt,
                operand: ProvenanceOperand::Num(0.5),
            },
            ProvenancePredicate::Accessor {
                field: ProvenanceField::Source,
                op: ProvenanceCmp::Ne,
                operand: ProvenanceOperand::Str("x".into()),
            },
        ];
        for c in cases {
            assert!(!c.evaluate(None), "absent bundle must fail {c:?}");
        }
    }

    #[test]
    fn confidence_operators() {
        let p = prov(None, Some(0.8), None);
        let mk = |op| ProvenancePredicate::Accessor {
            field: ProvenanceField::Confidence,
            op,
            operand: ProvenanceOperand::Num(0.8),
        };
        assert!(mk(ProvenanceCmp::Ge).evaluate(Some(&p)));
        assert!(mk(ProvenanceCmp::Le).evaluate(Some(&p)));
        assert!(mk(ProvenanceCmp::Eq).evaluate(Some(&p)));
        assert!(!mk(ProvenanceCmp::Gt).evaluate(Some(&p)));
        assert!(!mk(ProvenanceCmp::Lt).evaluate(Some(&p)));
        assert!(!mk(ProvenanceCmp::Ne).evaluate(Some(&p)));
    }

    /// The foldable `Filter` arm and the shared `ProvenanceFilter::matches` must
    /// agree exactly on every bundle shape (AC3 parity).
    #[test]
    fn filter_arm_matches_core_provenance_filter() {
        let filter = ProvenanceFilter::validated(Some("hr".into()), None, Some(0.9), false)
            .unwrap()
            .unwrap();
        let pred = ProvenancePredicate::Filter(filter.clone());
        let bundles = [
            None,
            Some(prov(Some("hr"), Some(0.95), None)),
            Some(prov(Some("hr"), Some(0.5), None)),
            Some(prov(Some("crm"), Some(0.95), None)),
            Some(prov(None, Some(0.95), None)),
            Some(prov(Some("hr"), None, None)),
        ];
        for b in &bundles {
            assert_eq!(
                pred.evaluate(b.as_ref()),
                filter.matches(b.as_ref()),
                "Filter arm must equal ProvenanceFilter::matches for {b:?}"
            );
        }
    }

    /// The general `Accessor` arm for the foldable positive shapes must equal
    /// the equivalent `ProvenanceFilter::matches` result (source exact match,
    /// inclusive min-confidence).
    #[test]
    fn accessor_arm_parity_with_filter_on_foldable_shapes() {
        // confidence(x) >= 0.9  ==  min_confidence 0.9
        let acc = ProvenancePredicate::Accessor {
            field: ProvenanceField::Confidence,
            op: ProvenanceCmp::Ge,
            operand: ProvenanceOperand::Num(0.9),
        };
        let filter = ProvenanceFilter::validated(None, None, Some(0.9), false)
            .unwrap()
            .unwrap();
        for b in [
            None,
            Some(prov(None, Some(0.95), None)),
            Some(prov(None, Some(0.9), None)),
            Some(prov(None, Some(0.5), None)),
            Some(prov(Some("crm"), None, None)),
        ] {
            assert_eq!(
                acc.evaluate(b.as_ref()),
                filter.matches(b.as_ref()),
                "{b:?}"
            );
        }

        // source(x) = 'crm'  ==  sources [crm]
        let acc = ProvenancePredicate::Accessor {
            field: ProvenanceField::Source,
            op: ProvenanceCmp::Eq,
            operand: ProvenanceOperand::Str("crm".into()),
        };
        let filter = ProvenanceFilter::validated(Some("crm".into()), None, None, false)
            .unwrap()
            .unwrap();
        for b in [
            None,
            Some(prov(Some("crm"), None, None)),
            Some(prov(Some("hr"), None, None)),
        ] {
            assert_eq!(
                acc.evaluate(b.as_ref()),
                filter.matches(b.as_ref()),
                "{b:?}"
            );
        }
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

        // Predicate derives PartialEq, so direct comparison works
        assert_eq!(double_not, inner);
    }
}
