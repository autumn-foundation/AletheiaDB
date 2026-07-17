//! Cypher AST → Query IR Converter
//!
//! Converts a parsed [`CypherStatement`] AST into the internal [`Query`] IR
//! that the query planner and executor understand. This bridges the gap between
//! the user-facing Cypher syntax and AletheiaDB's native query pipeline.
//!
//! # Architecture
//!
//! ```text
//! Cypher String ─► [Lexer] ─► Tokens ─► [Parser] ─► CypherStatement
//!                                                          │
//!                                                   [CypherConverter]
//!                                                          │
//!                                                          ▼
//!                                                    Query { ops, hints }
//! ```
//!
//! # Usage
//!
//! ```rust
//! use aletheiadb::cypher::{parse_cypher, parse_cypher_with_params, CypherParameterValue};
//! use std::collections::HashMap;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Simple: no parameters
//! let query = parse_cypher("MATCH (n:Person) RETURN n")?;
//!
//! // With parameters
//! let mut params = HashMap::new();
//! params.insert("name".to_string(), CypherParameterValue::String("Alice".into()));
//! let query = parse_cypher_with_params("MATCH (n:Person {name: $name}) RETURN n", params)?;
//! # Ok(())
//! # }
//! ```

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use super::ast::*;
use super::error::CypherError;
use super::parser::CypherParser;
use crate::core::temporal::{TimeRange, Timestamp};
use crate::core::vector::DistanceMetric as VectorMetric;
use crate::query::builder::Query;
use crate::query::converter::{
    ProvenanceLoweringError, lower_provenance_comparison, provenance_field_name,
};
use crate::query::ir::{
    AggregateArg, AggregateFunc, AggregateGroupKey, AggregateSpec, Predicate, PredicateValue,
    ProvenanceCmp, ProvenanceField, ProvenanceOperand, ProvenancePredicate, ProvenanceProjection,
    ProvenanceProjectionItem, QueryOp, ScoreComparison, ScoreThreshold, SortKey, TraversalDepth,
};
use crate::query::plan::{QueryHints, TemporalContext};

/// Defensive recursion bound for expression → predicate conversion.
///
/// The parser already caps expression nesting at 128, so for any AST it
/// produces this never fires. It exists solely because `convert` is `pub`: a
/// caller constructing a `CypherExpr` by hand (bypassing the parser) could nest
/// `Not`/`Grouped` arbitrarily deep and overflow the stack during conversion.
/// Kept clearly above the parser's cap so it only ever rejects such hand-built
/// pathological input, never a legitimately parsed query.
const MAX_CONVERT_DEPTH: usize = 256;

/// Map a Cypher [`CypherCompOp`] to the IR [`ProvenanceCmp`] (Issue #3354b).
fn cypher_comp_to_provenance_cmp(op: CypherCompOp) -> ProvenanceCmp {
    match op {
        CypherCompOp::Eq => ProvenanceCmp::Eq,
        CypherCompOp::Ne => ProvenanceCmp::Ne,
        CypherCompOp::Lt => ProvenanceCmp::Lt,
        CypherCompOp::Le => ProvenanceCmp::Le,
        CypherCompOp::Gt => ProvenanceCmp::Gt,
        CypherCompOp::Ge => ProvenanceCmp::Ge,
    }
}

/// Flip a Cypher comparison operator when its operands are swapped, so an
/// accessor on the right-hand side (`0.9 <= confidence(x)`) lowers identically
/// to the accessor-on-left form (`confidence(x) >= 0.9`). Equality/inequality
/// are symmetric (Issue #3354b).
fn flip_cypher_comp_op(op: CypherCompOp) -> CypherCompOp {
    match op {
        CypherCompOp::Eq => CypherCompOp::Eq,
        CypherCompOp::Ne => CypherCompOp::Ne,
        CypherCompOp::Lt => CypherCompOp::Gt,
        CypherCompOp::Le => CypherCompOp::Ge,
        CypherCompOp::Gt => CypherCompOp::Lt,
        CypherCompOp::Ge => CypherCompOp::Le,
    }
}

/// Map the surface-agnostic [`ProvenanceLoweringError`] into the Cypher error
/// vocabulary (Issue #3354b).
///
/// Chosen so the MCP `query` tool surfaces the same structured kinds as the AQL
/// side (via [`CypherError`] → `crate::core::error::Error` → `map_query_error`):
/// type mismatches and out-of-range confidence become
/// [`CypherError::ParameterError`] (→ `invalid_params`); the ordering-op-on-
/// string rejection becomes [`CypherError::SemanticError`] (→ `parse_error`).
fn map_provenance_lowering_error(e: ProvenanceLoweringError) -> CypherError {
    match e {
        ProvenanceLoweringError::ConfidenceOutOfRange { value, .. } => CypherError::ParameterError(
            format!("confidence(x) must be between 0.0 and 1.0, got {value}"),
        ),
        ProvenanceLoweringError::ConfidenceNotNumeric => CypherError::ParameterError(
            "type mismatch: confidence(x) compares only to a numeric value".to_string(),
        ),
        ProvenanceLoweringError::OrderingOpOnStringField { field } => {
            CypherError::SemanticError(format!(
                "ordering operators (<, <=, >, >=) are not supported for {}(x); \
                 only = and <> are supported for source/reason",
                field.accessor_name()
            ))
        }
        ProvenanceLoweringError::StringFieldNotString { field } => {
            CypherError::ParameterError(format!(
                "type mismatch: {}(x) compares only to a string value",
                field.accessor_name()
            ))
        }
        ProvenanceLoweringError::FilterValidation { message, .. } => {
            CypherError::ParameterError(message)
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter values
// ---------------------------------------------------------------------------

/// A runtime parameter value that can be bound to `$param` references in Cypher
/// queries.
///
/// Parameters allow safe, injection-free query execution with dynamic values.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherParameterValue {
    /// SQL/Cypher NULL.
    Null,
    /// A boolean value.
    Bool(bool),
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit floating-point number.
    Float(f64),
    /// A UTF-8 string.
    String(String),
    /// A dense vector embedding for similarity search.
    Embedding(Arc<[f32]>),
    /// A homogeneous or heterogeneous list of parameter values.
    ///
    /// Bound to the `$list` position of a standalone `UNWIND $list AS x`
    /// (Issue #559). Lists are not valid as scalar predicate values.
    List(Vec<CypherParameterValue>),
}

impl CypherParameterValue {
    /// Convert this parameter value into a [`PredicateValue`] for use in
    /// query filters.
    ///
    /// # Errors
    ///
    /// Returns [`CypherError::ParameterError`] if the value type cannot be
    /// converted (e.g., embeddings are not valid predicate values).
    fn to_predicate_value(&self) -> Result<PredicateValue, CypherError> {
        match self {
            CypherParameterValue::Null => Ok(PredicateValue::Null),
            CypherParameterValue::Bool(b) => Ok(PredicateValue::Bool(*b)),
            CypherParameterValue::Int(n) => Ok(PredicateValue::Int(*n)),
            CypherParameterValue::Float(f) => Ok(PredicateValue::Float(*f)),
            CypherParameterValue::String(s) => Ok(PredicateValue::String(s.clone())),
            CypherParameterValue::Embedding(_) => Err(CypherError::ParameterError(
                "embedding parameters cannot be used as predicate values".to_string(),
            )),
            CypherParameterValue::List(_) => Err(CypherError::ParameterError(
                "list parameters cannot be used as predicate values".to_string(),
            )),
        }
    }
}

/// The projection shape of a `WITH` clause, as resolved for the positional
/// pipeline (Issue #556). The pipeline carries a single entity per row, so a
/// `WITH` either carries everything (`*`) or carries the current binding under
/// its original or an aliased name.
enum WithProjection {
    /// `WITH *` -- carry every visible variable forward unchanged.
    All,
    /// `WITH <source>` or `WITH <source> AS <output>` -- carry the current
    /// positional binding forward, renaming it to `output`.
    Single {
        /// The source variable being projected (must be the current binding).
        source: String,
        /// The output name it is bound to downstream (equal to `source` when
        /// no `AS` alias is given).
        output: String,
    },
}

// ---------------------------------------------------------------------------
// Converter
// ---------------------------------------------------------------------------

/// Converts a Cypher AST ([`CypherStatement`]) into a [`Query`] suitable for
/// the AletheiaDB query planner.
///
/// The converter walks the AST and emits a flat sequence of [`QueryOp`]s that
/// describe the query pipeline:
///
/// 1. A `ScanNodes` op for the first node in each pattern.
/// 2. A `TraverseOut`/`TraverseIn`/`TraverseBoth` op for each relationship.
/// 3. `Filter` ops for inline property constraints and `WHERE` clauses.
/// 4. `Distinct`, `Sort`, `Skip`, `Limit` ops from the `RETURN` clause.
pub struct CypherConverter {
    /// Bound parameters for `$param` references.
    params: HashMap<String, CypherParameterValue>,
}

impl Default for CypherConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl CypherConverter {
    /// Create a new converter with no bound parameters.
    #[must_use]
    pub fn new() -> Self {
        Self {
            params: HashMap::new(),
        }
    }

    /// Create a new converter with the given parameter bindings.
    #[must_use]
    pub fn with_params(params: HashMap<String, CypherParameterValue>) -> Self {
        Self { params }
    }

    /// Convert a [`CypherStatement`] into a [`Query`].
    ///
    /// # Errors
    ///
    /// Returns [`CypherError`] if:
    /// - A `$param` reference has no binding in the parameter map.
    /// - An unsupported AST construct is encountered.
    pub fn convert(&self, stmt: CypherStatement) -> Result<Query, CypherError> {
        match stmt {
            CypherStatement::Match {
                optional,
                pattern,
                where_clause,
                return_clause,
                temporal,
                with_clauses,
                optional_matches,
            } => {
                let mut ops = Vec::new();

                // 1. Convert graph patterns → ScanNodes + Traverse + Filter ops,
                //    followed by the base WHERE clause → Filter op.
                //
                //    For a leading OPTIONAL MATCH the whole base segment
                //    (scan + filters + WHERE) is wrapped in a single
                //    QueryOp::Optional so an empty result yields one null row.
                let mut base_ops = Vec::new();
                for pat in &pattern {
                    self.convert_pattern(pat, &mut base_ops)?;
                }
                if let Some(expr) = where_clause {
                    // Lifts any vector similarity threshold (Issue #553) into a
                    // RankBySimilarity; otherwise emits a single Filter exactly
                    // as before.
                    self.convert_where_clause(&expr, &mut base_ops)?;
                }
                if optional {
                    ops.push(QueryOp::Optional { ops: base_ops });
                } else {
                    ops.append(&mut base_ops);
                }

                // Track the variable the pipeline is positionally "on" after
                // each clause: the last node of the base pattern, then the
                // last node of each OPTIONAL MATCH pattern (or a WITH
                // projection). Subsequent OPTIONAL MATCH clauses must
                // continue from this binding (see convert_optional_pattern).
                let mut current_var: Option<String> = pattern
                    .last()
                    .and_then(Self::last_node_variable)
                    .map(str::to_string);

                // Variable scoping (Issue #556). `visible` holds the names in
                // scope at the current point; `dropped` accumulates names a
                // `WITH` has projected away. A downstream reference to a dropped
                // name is a scope violation (rejected, never answered against a
                // stale binding). The base pattern's variables seed the scope.
                let mut visible: HashSet<String> = HashSet::new();
                for pat in &pattern {
                    Self::collect_pattern_variables(pat, &mut visible);
                }
                let mut dropped: HashSet<String> = HashSet::new();

                // 2. Convert temporal clause → TemporalContext + ops
                let temporal_context = if let Some(ref temporal_clause) = temporal {
                    Some(self.convert_temporal(temporal_clause, &mut ops)?)
                } else {
                    None
                };

                // 3. Interleave WITH clauses (→ projection + Distinct/Sort/Skip/
                //    Limit/Filter ops) and OPTIONAL MATCH clauses (→ Optional
                //    ops) in source order.
                let mut with_idx = 0;
                for opt_match in &optional_matches {
                    while with_idx < opt_match.preceding_withs && with_idx < with_clauses.len() {
                        let order_by_overridden = Self::order_by_overridden_without_window(
                            &with_clauses,
                            with_idx,
                            &return_clause,
                        );
                        self.convert_with_clause(
                            &with_clauses[with_idx],
                            &mut ops,
                            &mut visible,
                            &mut dropped,
                            &mut current_var,
                            order_by_overridden,
                        )?;
                        with_idx += 1;
                    }
                    // Without variable-binding analysis, comma-separated
                    // patterns in one OPTIONAL MATCH clause would silently
                    // chain positionally (the second pattern would traverse
                    // the first pattern's result instead of re-seeding from
                    // the bound variable), returning wrong entities. Reject
                    // until binding analysis exists.
                    if opt_match.pattern.len() > 1 {
                        return Err(CypherError::UnsupportedFeature(
                            "comma-separated patterns in an OPTIONAL MATCH clause; \
                             use one pattern per OPTIONAL MATCH clause instead"
                                .to_string(),
                        ));
                    }
                    let mut sub_ops = Vec::new();
                    for pat in &opt_match.pattern {
                        self.convert_optional_pattern(pat, &mut sub_ops, current_var.as_deref())?;
                        // The optional pattern's variables (re-)enter scope: add
                        // them to `visible` and clear any earlier `dropped` mark.
                        let mut pat_vars = HashSet::new();
                        Self::collect_pattern_variables(pat, &mut pat_vars);
                        for v in &pat_vars {
                            dropped.remove(v);
                        }
                        visible.extend(pat_vars);
                    }
                    // The next clause continues from this pattern's last node.
                    current_var = opt_match
                        .pattern
                        .last()
                        .and_then(Self::last_node_variable)
                        .map(str::to_string);
                    if let Some(ref where_expr) = opt_match.where_clause {
                        // Per openCypher, the clause's WHERE is part of the
                        // optional pattern: it must run before the
                        // matched/unmatched decision, hence inside the op.
                        self.reject_dropped_refs(where_expr, &dropped, "OPTIONAL MATCH ... WHERE")?;
                        let predicate = self.convert_expr_to_predicate(where_expr)?;
                        sub_ops.push(QueryOp::Filter(predicate));
                    }
                    ops.push(QueryOp::Optional { ops: sub_ops });
                }
                while with_idx < with_clauses.len() {
                    let order_by_overridden = Self::order_by_overridden_without_window(
                        &with_clauses,
                        with_idx,
                        &return_clause,
                    );
                    self.convert_with_clause(
                        &with_clauses[with_idx],
                        &mut ops,
                        &mut visible,
                        &mut dropped,
                        &mut current_var,
                        order_by_overridden,
                    )?;
                    with_idx += 1;
                }

                // Enforce scoping on the final RETURN: a RETURN item (or its
                // ORDER BY) that references a variable a `WITH` dropped is a
                // scope violation. (The positional pipeline returns the current
                // entity regardless of which in-scope variable is named -- a
                // pre-existing limitation -- but a reference to an
                // out-of-scope, dropped variable is unambiguously wrong and is
                // rejected here rather than answered against the wrong entity.)
                for item in &return_clause.items {
                    match item {
                        CypherReturnItem::Expression { expr, .. } => {
                            self.reject_dropped_refs(expr, &dropped, "RETURN")?;
                        }
                        // Defense-in-depth for a manually-constructed AST: the
                        // parser currently emits `Expression` for a bare
                        // variable, but a hand-built `Variable` item must be
                        // scope-checked too. `Star` binds nothing to reject.
                        CypherReturnItem::Variable(name) => {
                            if dropped.contains(name) {
                                return Err(CypherError::SemanticError(format!(
                                    "RETURN references variable '{name}', which is not in \
                                     scope after a WITH projection dropped it; project it \
                                     through the WITH (e.g. `WITH ..., {name}`) to keep it \
                                     visible"
                                )));
                            }
                        }
                        CypherReturnItem::Star => {}
                    }
                }
                for order_item in &return_clause.order_by {
                    self.reject_dropped_refs(&order_item.expr, &dropped, "RETURN ... ORDER BY")?;
                }

                // 4. Aggregation (openCypher implicit grouping). If any RETURN
                //    item is an aggregate function call, the RETURN lowers to a
                //    single Aggregate op grouping on the non-aggregate items.
                //    Aggregation subsumes DISTINCT and changes what the pipeline
                //    yields (computed column rows), so RETURN DISTINCT and the
                //    ORDER BY / vector-rank projection are only applied on the
                //    non-aggregate path.
                let aggregate_op = self.build_aggregate(&return_clause)?;
                let aggregated = aggregate_op.is_some();
                if let Some(aggregate_op) = aggregate_op {
                    ops.push(aggregate_op);
                    // ORDER BY over aggregate output: sort by the output column
                    // name / aggregate alias (RETURN DISTINCT is subsumed by
                    // grouping; vector rank does not apply to computed rows).
                    for order_item in &return_clause.order_by {
                        let key = self.aggregate_order_key(&return_clause, order_item)?;
                        ops.push(QueryOp::Sort {
                            key: SortKey::Property(key),
                            descending: order_item.descending,
                        });
                    }
                } else {
                    // 4a. RETURN DISTINCT
                    if return_clause.distinct {
                        ops.push(QueryOp::Distinct);
                    }

                    // 4b. Check if ORDER BY contains a vector function call that
                    // should be converted to RankBySimilarity instead of Sort.
                    let vector_rank_emitted =
                        self.try_emit_vector_rank(&return_clause, &mut ops)?;

                    if !vector_rank_emitted {
                        for order_item in &return_clause.order_by {
                            let sort_key = self.convert_order_item_to_sort_key(order_item)?;
                            ops.push(QueryOp::Sort {
                                key: sort_key,
                                descending: order_item.descending,
                            });
                        }
                    }
                }

                // SKIP / LIMIT page the final rows in both the aggregate and the
                // ordinary case.
                if let Some(skip) = return_clause.skip {
                    ops.push(QueryOp::Skip(skip));
                }

                if let Some(limit) = return_clause.limit {
                    // If we already emitted a RankBySimilarity (which includes top_k),
                    // still emit the Limit for the general pipeline.
                    ops.push(QueryOp::Limit(limit));
                }

                // Provenance accessor projection (Issue #3354) runs LAST so it
                // never perturbs ordering/pagination. `build_provenance_projection`
                // is always consulted (even when aggregated) so an accessor mixed
                // with aggregation is REJECTED rather than silently dropped; a
                // normal aggregate query with no accessor still yields `None`.
                if let Some(op) =
                    self.build_provenance_projection(&return_clause, &pattern, aggregated)?
                {
                    ops.push(op);
                }

                Ok(Query {
                    ops,
                    temporal_context,
                    hints: QueryHints::default(),
                    // Cypher namespace scoping is a follow-up (PR2b).
                    scope: None,
                })
            }
            // A standalone `UNWIND` produces scalar rows, not stored entities,
            // so it has no representation in the entity-oriented `Query` IR. It
            // is executed by the dedicated Cypher UNWIND runtime instead (see
            // [`crate::cypher::plan_cypher`] / `AletheiaDB::execute_cypher`).
            // Reject conversion explicitly rather than answer silently wrong.
            CypherStatement::Unwind { .. } => Err(CypherError::UnsupportedFeature(
                "UNWIND does not lower into the graph query pipeline; execute it \
                 via AletheiaDB::execute_cypher (or cypher::plan_cypher), which \
                 evaluates the list-expansion runtime directly"
                    .to_string(),
            )),
            // `EXPLAIN`/`PROFILE` are planning/execution *directives*, not
            // queries: they are unwrapped and dispatched by
            // [`crate::cypher::plan_cypher`] (which lowers the inner statement
            // and returns a dedicated `CypherExecution::Explain`/`Profile`), so
            // the converter never sees them directly. Reject rather than answer
            // silently wrong if one reaches here.
            CypherStatement::Explain(_) | CypherStatement::Profile(_) => {
                Err(CypherError::UnsupportedFeature(
                    "EXPLAIN/PROFILE are query directives handled by \
                     cypher::plan_cypher, not lowered by the converter"
                        .to_string(),
                ))
            }
            // Write statements (Issues #560, #3548) are executed directly
            // against the native write APIs by `crate::cypher::mutation`, not
            // lowered into the read-only `Query` IR. Reject conversion
            // explicitly rather than fabricate a read plan for a mutation.
            CypherStatement::Write(_) => Err(CypherError::UnsupportedFeature(
                "write statements (CREATE / MERGE / SET / DELETE) do not lower into the \
                 read-only query pipeline; execute them via \
                 AletheiaDB::execute_cypher, which applies the mutation through \
                 the native write APIs"
                    .to_string(),
            )),
        }
    }

    // =======================================================================
    // Pattern conversion
    // =======================================================================

    /// Convert a single graph pattern into query operations.
    ///
    /// A pattern like `(a:Person)-[:KNOWS]->(b:Person {name: 'Bob'})` produces:
    /// 1. `ScanNodes { label: Some("Person") }` for node `a`
    /// 2. Filter ops for node `a`'s inline properties (if any)
    /// 3. `TraverseOut { label: Some("KNOWS"), depth: Exact(1) }` for the rel
    /// 4. Filter ops for node `b`'s inline properties
    fn convert_pattern(
        &self,
        pattern: &CypherPattern,
        ops: &mut Vec<QueryOp>,
    ) -> Result<(), CypherError> {
        let mut first_node = true;

        for element in &pattern.elements {
            match element {
                CypherPatternElement::Node(node) => {
                    if first_node {
                        // The first node in the pattern produces a ScanNodes op.
                        let label = node.labels.first().cloned();
                        ops.push(QueryOp::ScanNodes { label });
                        first_node = false;
                    }
                    // Emit filters for inline property constraints on any node.
                    self.convert_node_properties(node, ops)?;
                }
                CypherPatternElement::Relationship(rel) => {
                    self.convert_relationship(rel, ops)?;
                }
            }
        }

        Ok(())
    }

    /// Convert a graph pattern from a *subsequent* `OPTIONAL MATCH` clause
    /// into query operations for the optional sub-pipeline.
    ///
    /// Unlike [`Self::convert_pattern`], the first node does **not** emit a
    /// `ScanNodes` op: it is assumed to refer to the current row produced by
    /// the preceding pipeline (e.g. `a` in
    /// `MATCH (a) OPTIONAL MATCH (a)-[:KNOWS]->(x)`). Its inline property
    /// constraints still become `Filter` ops, which run against the seed row
    /// inside the optional segment and therefore participate in the
    /// matched/unmatched decision.
    ///
    /// Limitation and design choice: the converter performs no variable
    /// binding analysis, so the first node is bound purely positionally to
    /// the current row. Two constructs that would silently produce wrong
    /// rows are therefore **rejected** with
    /// [`CypherError::UnsupportedFeature`] rather than answered wrongly:
    ///
    /// - A **label** on the first node cannot be honored (there is no label
    ///   predicate to lower it to), and silently dropping it would turn
    ///   `MATCH (a:Person) OPTIONAL MATCH (b:City) RETURN b` into a query
    ///   that returns `a`.
    /// - A first-node **variable that does not name the positionally-current
    ///   binding** (`current_var`, the last node of the previous
    ///   MATCH/OPTIONAL MATCH or a `WITH` projection) would silently bind to
    ///   whatever the pipeline is positioned on: in `MATCH (a:Person)
    ///   OPTIONAL MATCH (a)-[:KNOWS]->(x) OPTIONAL MATCH (a)-[:OWNS]->(y)`
    ///   the second `(a)` would bind to `x` (or null) and traverse `OWNS`
    ///   from the wrong node. Re-anchoring on an earlier variable requires
    ///   real binding analysis and is not yet supported.
    ///
    /// An unnamed first node `()` (explicit positional reference) and a
    /// first node naming `current_var` are accepted; inline properties on
    /// the first node remain supported because they lower cheaply and
    /// correctly to seed-row filters.
    fn convert_optional_pattern(
        &self,
        pattern: &CypherPattern,
        ops: &mut Vec<QueryOp>,
        current_var: Option<&str>,
    ) -> Result<(), CypherError> {
        if let Some(CypherPatternElement::Node(first)) = pattern.elements.first() {
            // Reject a labeled first node: it is bound positionally to the
            // seed row and its label would be silently ignored (see rustdoc
            // above).
            if !first.labels.is_empty() {
                return Err(CypherError::UnsupportedFeature(format!(
                    "label ':{}' on the first node of a subsequent OPTIONAL MATCH; \
                     the first node refers to the current row and must be an \
                     unlabeled bound variable (e.g. `(a)`) -- move the constraint \
                     into the preceding MATCH or a WHERE clause",
                    first.labels.join(":"),
                )));
            }
            // Reject a first-node variable that re-anchors on something other
            // than the positionally-current binding (see rustdoc above).
            if let Some(ref first_var) = first.variable
                && current_var != Some(first_var.as_str())
            {
                return Err(CypherError::UnsupportedFeature(match current_var {
                    Some(cur) => format!(
                        "OPTIONAL MATCH must continue from the previous clause's \
                         binding '{cur}'; re-anchoring on '{first_var}' is not \
                         yet supported -- start the pattern with `({cur})` or \
                         restructure the query"
                    ),
                    None => format!(
                        "OPTIONAL MATCH must continue from the previous clause's \
                         binding, but its last node is unnamed; re-anchoring on \
                         '{first_var}' is not yet supported -- name the previous \
                         pattern's last node or restructure the query"
                    ),
                }));
            }
        }
        for element in &pattern.elements {
            match element {
                CypherPatternElement::Node(node) => {
                    // No ScanNodes: the seed row is the pattern's entry point.
                    self.convert_node_properties(node, ops)?;
                }
                CypherPatternElement::Relationship(rel) => {
                    self.convert_relationship(rel, ops)?;
                }
            }
        }
        Ok(())
    }

    /// The variable bound to a pattern's last node element, if named.
    ///
    /// This is the binding the pipeline is positionally "on" after the
    /// pattern converts: each relationship traversal moves the current row
    /// to its target node, so the last node of the pattern is where a
    /// subsequent `OPTIONAL MATCH` continues from.
    fn last_node_variable(pattern: &CypherPattern) -> Option<&str> {
        pattern
            .elements
            .iter()
            .rev()
            .find_map(|element| match element {
                CypherPatternElement::Node(node) => Some(node.variable.as_deref()),
                CypherPatternElement::Relationship(_) => None,
            })
            .flatten()
    }

    /// Convert a `WITH` clause into query operations, threading variable scope
    /// through the pipeline (Issue #556).
    ///
    /// openCypher evaluates a `WITH` body as: project the items, apply
    /// `DISTINCT`, then `ORDER BY`, `SKIP`, `LIMIT`, and finally the trailing
    /// `WHERE` (which filters the *projected* rows). This method emits that
    /// pipeline as ops and updates `visible` / `dropped` / `current_var`.
    ///
    /// # Projection boundary (positional pipeline)
    ///
    /// AletheiaDB's query pipeline carries exactly one entity per row, so a
    /// `WITH` can only *carry forward* the current positional binding: a bare
    /// or aliased reference to `current_var`, or `*` (carry everything). Any
    /// other projection -- multiple items, a property or computed expression,
    /// an aggregate, or a re-projection of a non-current variable -- has no
    /// faithful representation and is **rejected** with a structured
    /// [`CypherError::UnsupportedFeature`] rather than answered against the
    /// wrong entity.
    fn convert_with_clause(
        &self,
        with: &CypherWith,
        ops: &mut Vec<QueryOp>,
        visible: &mut HashSet<String>,
        dropped: &mut HashSet<String>,
        current_var: &mut Option<String>,
        order_by_overridden: bool,
    ) -> Result<(), CypherError> {
        // 1. Resolve the projection shape and update the scope.
        match Self::with_projection(with)? {
            WithProjection::All => {
                // `WITH *` carries every visible variable forward, but the
                // positional pipeline carries only ONE entity per row. When
                // more than one variable is in scope, `*` would silently
                // forward the wrong entity (e.g. `MATCH (a)-[:KNOWS]->(b)
                // WITH * RETURN a` returns `b`'s data as `a`). Reject rather
                // than answer wrongly; `*` with a single visible variable is a
                // faithful no-op (scope and positional binding unchanged).
                if visible.len() > 1 {
                    return Err(CypherError::UnsupportedFeature(format!(
                        "WITH * with more than one variable in scope ({} visible) is \
                         not supported: the positional execution pipeline carries a \
                         single entity per row, so '*' would forward the wrong \
                         entity. Project the single variable you need (optionally \
                         aliased) instead.",
                        visible.len()
                    )));
                }
            }
            WithProjection::Single { source, output } => {
                // The pipeline is positioned on `current_var`; it cannot
                // reposition to a different variable, so only the current
                // binding (optionally aliased) can be carried forward.
                if current_var.as_deref() != Some(source.as_str()) {
                    return Err(CypherError::UnsupportedFeature(match current_var {
                        Some(cur) => format!(
                            "WITH can only carry forward the current pattern variable \
                             '{cur}' (optionally aliased) or '*'; re-projecting \
                             '{source}' would require repositioning the positional \
                             pipeline, which is not supported"
                        ),
                        None => format!(
                            "WITH can only carry forward the current pattern variable \
                             (optionally aliased) or '*', but the preceding clause's \
                             last node is unnamed; projecting '{source}' is not \
                             supported"
                        ),
                    }));
                }
                // Everything except the projected output leaves scope. Drain
                // `visible` to move the owned names straight into `dropped`
                // (no clone), then re-seed the scope with the output.
                for name in visible.drain() {
                    if name != output {
                        dropped.insert(name);
                    }
                }
                visible.insert(output.clone());
                dropped.remove(&output);
                *current_var = Some(output);
            }
        }

        // 2. DISTINCT deduplicates the projected rows.
        if with.distinct {
            ops.push(QueryOp::Distinct);
        }

        // 3. ORDER BY / SKIP / LIMIT on the projected rows.
        //
        //    A `WITH` ORDER BY is meaningful when it selects a SKIP/LIMIT
        //    window (in this clause OR any clause before the next re-sort), or
        //    when it is the last ordering applied before output. It may be
        //    dropped ONLY when the next ordering-affecting event is a later
        //    re-sort with NO intervening SKIP/LIMIT window
        //    (`order_by_overridden`): that later ordering fully replaces it and
        //    openCypher leaves the overridden tie order unspecified. Dropping
        //    the pure-consecutive-sort case also avoids an incorrect fold in
        //    the physical planner, which merges *consecutive* Sort ops into one
        //    multi-key Sort; emitting a Sort when a window intervenes is
        //    fold-safe because the intervening Skip/Limit op breaks that fold.
        if !with.order_by.is_empty() {
            for order_item in &with.order_by {
                self.reject_dropped_refs(&order_item.expr, dropped, "WITH ... ORDER BY")?;
            }
            let has_window = with.skip.is_some() || with.limit.is_some();
            if has_window || !order_by_overridden {
                for order_item in &with.order_by {
                    let sort_key = self.convert_order_item_to_sort_key(order_item)?;
                    ops.push(QueryOp::Sort {
                        key: sort_key,
                        descending: order_item.descending,
                    });
                }
            }
        }
        if let Some(skip) = with.skip {
            ops.push(QueryOp::Skip(skip));
        }
        if let Some(limit) = with.limit {
            ops.push(QueryOp::Limit(limit));
        }

        // 4. The trailing WHERE filters the projected rows.
        if let Some(ref where_expr) = with.where_clause {
            self.reject_dropped_refs(where_expr, dropped, "WITH ... WHERE")?;
            let predicate = self.convert_expr_to_predicate(where_expr)?;
            ops.push(QueryOp::Filter(predicate));
        }
        Ok(())
    }

    /// Resolve the projection shape of a `WITH` clause, rejecting any shape the
    /// positional pipeline cannot carry (see [`Self::convert_with_clause`]).
    fn with_projection(with: &CypherWith) -> Result<WithProjection, CypherError> {
        match with.items.as_slice() {
            [CypherReturnItem::Star] => Ok(WithProjection::All),
            [single] => Self::single_projection(single),
            _ => Err(CypherError::UnsupportedFeature(
                "WITH projecting multiple items is not supported: the positional \
                 execution pipeline carries a single entity per row. Project one \
                 variable (optionally aliased) or '*'."
                    .to_string(),
            )),
        }
    }

    /// Resolve a single `WITH` item into its `(source, output)` variable names.
    ///
    /// Only a bare variable (`WITH a`) or an aliased variable (`WITH a AS b`)
    /// is representable; a computed expression, property access, aggregate, or
    /// `*` in this position is rejected.
    fn single_projection(item: &CypherReturnItem) -> Result<WithProjection, CypherError> {
        match item {
            CypherReturnItem::Variable(v) => Ok(WithProjection::Single {
                source: v.clone(),
                output: v.clone(),
            }),
            CypherReturnItem::Expression {
                expr: CypherExpr::Variable(v),
                alias,
            } => Ok(WithProjection::Single {
                source: v.clone(),
                output: alias.clone().unwrap_or_else(|| v.clone()),
            }),
            CypherReturnItem::Star => Err(CypherError::UnsupportedFeature(
                "WITH '*' cannot be mixed with other projected items".to_string(),
            )),
            CypherReturnItem::Expression { expr, .. } => {
                Err(CypherError::UnsupportedFeature(format!(
                    "WITH can only project a bound variable (optionally aliased) or \
                     '*'; projecting the expression {expr:?} is not supported -- \
                     computed columns and aggregates in WITH are a follow-up"
                )))
            }
        }
    }

    /// Whether a bare intermediate `WITH ... ORDER BY` (one with no SKIP/LIMIT
    /// window of its own) at `with_idx` is fully overridden by a later re-sort
    /// with **no intervening SKIP/LIMIT window**, and may therefore be dropped.
    ///
    /// A `WITH`/`RETURN` clause applies its own `ORDER BY` *before* its own
    /// `SKIP`/`LIMIT`, so scanning forward from the next clause:
    ///
    /// - the first later clause carrying its own `ORDER BY` is a **re-sort**
    ///   that replaces this ordering (droppable) -- its order runs before any
    ///   paging that clause also has, so it counts as a re-sort, not a window;
    /// - a `SKIP`/`LIMIT` in a clause *before* that re-sort **consumes** this
    ///   ordering (it selects a window against these ordered rows), making the
    ///   sort observable -- do not drop;
    /// - reaching the `RETURN` with no earlier re-sort: the `RETURN`'s own
    ///   `ORDER BY` (if any) is the next re-sort (droppable); otherwise this
    ///   ordering is **terminal** (the final result order) and is observable.
    ///
    /// The caller additionally guards on this `WITH`'s *own* SKIP/LIMIT (a
    /// same-clause window makes its ORDER BY observable); this look-ahead only
    /// considers the clauses that follow it.
    fn order_by_overridden_without_window(
        with_clauses: &[CypherWith],
        with_idx: usize,
        return_clause: &CypherReturn,
    ) -> bool {
        for later in with_clauses.iter().skip(with_idx + 1) {
            if !later.order_by.is_empty() {
                // Next re-sort reached with no intervening window: overridden.
                return true;
            }
            if later.skip.is_some() || later.limit.is_some() {
                // A window consumes this ordering before any later re-sort.
                return false;
            }
        }
        // No later `WITH` re-sorts before the `RETURN`: the `RETURN`'s own
        // `ORDER BY` (applied before its paging) is the next re-sort; absent
        // that, this ordering is terminal and therefore observable.
        !return_clause.order_by.is_empty()
    }

    /// Collect the variable names bound by a graph pattern (node and
    /// relationship variables) into `out`.
    fn collect_pattern_variables(pattern: &CypherPattern, out: &mut HashSet<String>) {
        for element in &pattern.elements {
            match element {
                CypherPatternElement::Node(node) => {
                    if let Some(v) = &node.variable {
                        out.insert(v.clone());
                    }
                }
                CypherPatternElement::Relationship(rel) => {
                    if let Some(v) = &rel.variable {
                        out.insert(v.clone());
                    }
                }
            }
        }
    }

    /// Reject an expression that references any variable a `WITH` has dropped
    /// from scope (Issue #556 scoping enforcement). `context` names the clause
    /// for the error message.
    fn reject_dropped_refs(
        &self,
        expr: &CypherExpr,
        dropped: &HashSet<String>,
        context: &str,
    ) -> Result<(), CypherError> {
        let mut refs = HashSet::new();
        Self::collect_expr_variables(expr, &mut refs, 0)?;
        for name in &refs {
            if dropped.contains(name) {
                return Err(CypherError::SemanticError(format!(
                    "{context} references variable '{name}', which is not in scope \
                     after a WITH projection dropped it; project it through the \
                     WITH (e.g. `WITH ..., {name}`) to keep it visible"
                )));
            }
        }
        Ok(())
    }

    /// Collect the variable names referenced by an expression (both bare
    /// variables and the base of a property access) into `out`.
    ///
    /// Depth-guarded like the predicate converter: `convert` is `pub`, so a
    /// hand-built expression could nest past the parser's own cap. The bound
    /// mirrors [`MAX_CONVERT_DEPTH`] so it never fires for parsed input.
    fn collect_expr_variables(
        expr: &CypherExpr,
        out: &mut HashSet<String>,
        depth: usize,
    ) -> Result<(), CypherError> {
        if depth > MAX_CONVERT_DEPTH {
            return Err(CypherError::ParseError {
                position: 0,
                message: "expression nesting too deep for scope analysis (max 256)".to_string(),
            });
        }
        match expr {
            CypherExpr::Variable(name) => {
                out.insert(name.clone());
            }
            CypherExpr::Property { variable, .. } => {
                out.insert(variable.clone());
            }
            CypherExpr::Value(_) | CypherExpr::Star => {}
            CypherExpr::Comparison { left, right, .. } => {
                Self::collect_expr_variables(left, out, depth + 1)?;
                Self::collect_expr_variables(right, out, depth + 1)?;
            }
            CypherExpr::And(left, right) | CypherExpr::Or(left, right) => {
                Self::collect_expr_variables(left, out, depth + 1)?;
                Self::collect_expr_variables(right, out, depth + 1)?;
            }
            CypherExpr::Not(inner)
            | CypherExpr::IsNull(inner)
            | CypherExpr::IsNotNull(inner)
            | CypherExpr::Grouped(inner) => {
                Self::collect_expr_variables(inner, out, depth + 1)?;
            }
            CypherExpr::In { expr, values } => {
                Self::collect_expr_variables(expr, out, depth + 1)?;
                for value in values {
                    Self::collect_expr_variables(value, out, depth + 1)?;
                }
            }
            CypherExpr::Contains { expr, .. }
            | CypherExpr::StartsWith { expr, .. }
            | CypherExpr::EndsWith { expr, .. } => {
                Self::collect_expr_variables(expr, out, depth + 1)?;
            }
            CypherExpr::FunctionCall { args, .. } | CypherExpr::List(args) => {
                for arg in args {
                    Self::collect_expr_variables(arg, out, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// Emit `Filter` ops for a node pattern's inline property constraints.
    fn convert_node_properties(
        &self,
        node: &CypherNodePattern,
        ops: &mut Vec<QueryOp>,
    ) -> Result<(), CypherError> {
        for (key, value) in &node.properties {
            let pred_value = self.convert_value_to_predicate_value(value)?;
            ops.push(QueryOp::Filter(Predicate::Eq {
                key: key.clone(),
                value: pred_value,
            }));
        }
        Ok(())
    }

    /// Convert a relationship pattern into a traversal op.
    ///
    /// Also emits `Filter` ops for any inline property constraints on the
    /// relationship (e.g., `-[:TYPE {since: 2020}]->`).
    fn convert_relationship(
        &self,
        rel: &CypherRelPattern,
        ops: &mut Vec<QueryOp>,
    ) -> Result<(), CypherError> {
        let label = rel.rel_types.first().cloned();
        let depth = self.convert_depth(&rel.depth);

        let op = match rel.direction {
            CypherDirection::Outgoing => QueryOp::TraverseOut { label, depth },
            CypherDirection::Incoming => QueryOp::TraverseIn { label, depth },
            CypherDirection::Both => QueryOp::TraverseBoth { label, depth },
        };

        ops.push(op);

        // Emit filters for inline relationship properties: -[:TYPE {key: val}]->
        for (key, value) in &rel.properties {
            let pred_value = self.convert_value_to_predicate_value(value)?;
            ops.push(QueryOp::Filter(Predicate::Eq {
                key: key.clone(),
                value: pred_value,
            }));
        }

        Ok(())
    }

    /// Map a Cypher depth specifier to a [`TraversalDepth`].
    fn convert_depth(&self, depth: &Option<CypherDepth>) -> TraversalDepth {
        match depth {
            None => TraversalDepth::Exact(1),
            Some(CypherDepth::Unbounded) => TraversalDepth::Variable,
            Some(CypherDepth::Exact(n)) => TraversalDepth::Exact(*n),
            Some(CypherDepth::Max(n)) => TraversalDepth::Max(*n),
            Some(CypherDepth::Min(n)) => TraversalDepth::Range {
                min: *n,
                max: usize::MAX,
            },
            Some(CypherDepth::Range { min, max }) => TraversalDepth::Range {
                min: *min,
                max: *max,
            },
        }
    }

    // =======================================================================
    // Expression → Predicate conversion
    // =======================================================================

    /// Convert a Cypher expression into a [`Predicate`].
    ///
    /// This handles comparisons, logical operators, string predicates, etc.
    fn convert_expr_to_predicate(&self, expr: &CypherExpr) -> Result<Predicate, CypherError> {
        self.convert_expr_to_predicate_at(expr, 0)
    }

    /// Depth-guarded worker for [`Self::convert_expr_to_predicate`].
    ///
    /// The `Not` / `Grouped` arms recurse structurally; `convert` is `pub`, so
    /// this guards against a caller handing us an AST nested past the parser's
    /// own [`MAX_EXPRESSION_DEPTH`]. The bound is set above the parser's cap so
    /// it never fires for parser-produced ASTs, only for hand-built ones.
    fn convert_expr_to_predicate_at(
        &self,
        expr: &CypherExpr,
        depth: usize,
    ) -> Result<Predicate, CypherError> {
        if depth > MAX_CONVERT_DEPTH {
            return Err(CypherError::ParseError {
                position: 0,
                message: "expression nesting too deep for conversion (max 256)".to_string(),
            });
        }
        match expr {
            CypherExpr::Comparison { left, op, right } => self.convert_comparison(left, *op, right),
            CypherExpr::And(_, _) => {
                // Iteratively flatten the (possibly deeply left-nested) AND
                // spine so that a long `a AND b AND c ...` chain lowers to a
                // single n-ary predicate without recursing per operator.
                let operands = flatten_logical_operands(expr, |e| match e {
                    CypherExpr::And(left, right) => Some((left, right)),
                    _ => None,
                });
                let preds = operands
                    .into_iter()
                    .map(|operand| self.convert_expr_to_predicate_at(operand, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Predicate::And(preds))
            }
            CypherExpr::Or(_, _) => {
                // Same iterative flattening for the OR spine.
                let operands = flatten_logical_operands(expr, |e| match e {
                    CypherExpr::Or(left, right) => Some((left, right)),
                    _ => None,
                });
                let preds = operands
                    .into_iter()
                    .map(|operand| self.convert_expr_to_predicate_at(operand, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Predicate::Or(preds))
            }
            CypherExpr::Not(inner) => {
                let inner_pred = self.convert_expr_to_predicate_at(inner, depth + 1)?;
                Ok(Predicate::Not(Box::new(inner_pred)))
            }
            CypherExpr::IsNull(inner) => {
                // `provenance(x) IS NULL` (Issue #3354b): selects the versions
                // with no provenance bundle at all (unattributed rows).
                if self.is_provenance_bundle_accessor(inner)? {
                    return Ok(Predicate::Provenance(ProvenancePredicate::IsNull {
                        negated: false,
                    }));
                }
                match inner.as_ref() {
                    // Property access (`n.email IS NULL`): openCypher treats a
                    // property as null when it is ABSENT *or* present with an
                    // explicit null value. The data model represents both (a
                    // missing key and a stored `PropertyValue::Null`), so lower to
                    // `NotExists(key) OR Eq(key, Null)`. The old `Eq(key, Null)`-only
                    // lowering matched present-null but silently missed absence.
                    CypherExpr::Property { property, .. } => Ok(Predicate::Or(vec![
                        Predicate::NotExists(property.clone()),
                        Predicate::Eq {
                            key: property.clone(),
                            value: PredicateValue::Null,
                        },
                    ])),
                    // Bare variable (`x IS NULL`): a node/edge-level null check
                    // (only an OPTIONAL MATCH unmatched binding is ever null). Keep
                    // the `Eq(var, Null)` lowering: a matched entity has no property
                    // literally named after the variable (so it is not-null), and a
                    // null binding is handled by the executor's null-row path.
                    _ => {
                        let key = self.extract_property_key(inner)?;
                        Ok(Predicate::Eq {
                            key,
                            value: PredicateValue::Null,
                        })
                    }
                }
            }
            CypherExpr::IsNotNull(inner) => {
                // `provenance(x) IS NOT NULL` (Issue #3354b): selects the
                // versions that carry a provenance bundle (attributed rows).
                if self.is_provenance_bundle_accessor(inner)? {
                    return Ok(Predicate::Provenance(ProvenancePredicate::IsNull {
                        negated: true,
                    }));
                }
                match inner.as_ref() {
                    // Property access (`n.email IS NOT NULL`): openCypher requires
                    // the property to be PRESENT with a non-null value. Lower to
                    // `Exists(key) AND Ne(key, Null)`; the old `Ne(key, Null)`-only
                    // lowering wrongly matched absent properties (the executor treats
                    // a missing property as `!= null`).
                    CypherExpr::Property { property, .. } => Ok(Predicate::And(vec![
                        Predicate::Exists(property.clone()),
                        Predicate::Ne {
                            key: property.clone(),
                            value: PredicateValue::Null,
                        },
                    ])),
                    // Bare variable (`x IS NOT NULL`): node/edge-level null check
                    // (see IsNull above). Keep the `Ne(var, Null)` lowering.
                    _ => {
                        let key = self.extract_property_key(inner)?;
                        Ok(Predicate::Ne {
                            key,
                            value: PredicateValue::Null,
                        })
                    }
                }
            }
            CypherExpr::In { expr, values } => {
                let key = self.extract_property_key(expr)?;
                let pred_values = values
                    .iter()
                    .map(|v| self.convert_expr_to_predicate_value(v))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Predicate::In {
                    key,
                    values: pred_values,
                })
            }
            CypherExpr::Contains { expr, substring } => {
                let key = self.extract_property_key(expr)?;
                Ok(Predicate::Contains {
                    key,
                    substring: substring.clone(),
                })
            }
            CypherExpr::StartsWith { expr, prefix } => {
                let key = self.extract_property_key(expr)?;
                Ok(Predicate::StartsWith {
                    key,
                    prefix: prefix.clone(),
                })
            }
            CypherExpr::EndsWith { expr, suffix } => {
                let key = self.extract_property_key(expr)?;
                Ok(Predicate::EndsWith {
                    key,
                    suffix: suffix.clone(),
                })
            }
            CypherExpr::Grouped(inner) => self.convert_expr_to_predicate_at(inner, depth + 1),
            CypherExpr::Value(CypherValue::Bool(true)) => Ok(Predicate::True),
            CypherExpr::Value(CypherValue::Bool(false)) => Ok(Predicate::False),
            _ => Err(CypherError::UnsupportedFeature(format!(
                "expression cannot be converted to predicate: {expr:?}"
            ))),
        }
    }

    /// Convert a comparison expression into a [`Predicate`].
    fn convert_comparison(
        &self,
        left: &CypherExpr,
        op: CypherCompOp,
        right: &CypherExpr,
    ) -> Result<Predicate, CypherError> {
        // Provenance accessors (Issue #3354b): `source(x)`/`confidence(x)`/
        // `reason(x)` appear as function calls, never property accesses, so a
        // property literally named `source` (`n.source`) is unaffected.
        // Recognize an accessor on either side and lower to a provenance
        // predicate before the ordinary property-access handling below.
        let left_accessor = self.as_provenance_accessor(left)?;
        let right_accessor = self.as_provenance_accessor(right)?;
        match (left_accessor, right_accessor) {
            (Some(_), Some(_)) => {
                return Err(CypherError::SemanticError(
                    "comparing two provenance accessors is not supported".to_string(),
                ));
            }
            (Some(field), None) => {
                return self.build_provenance_comparison(field, op, right);
            }
            (None, Some(field)) => {
                // Accessor on the right (`0.9 <= confidence(n)`): flip the
                // comparison so the accessor is the logical left-hand side.
                return self.build_provenance_comparison(field, flip_cypher_comp_op(op), left);
            }
            (None, None) => {}
        }

        let key = self.extract_property_key(left)?;
        let value = self.convert_expr_to_predicate_value(right)?;

        Ok(match op {
            CypherCompOp::Eq => Predicate::Eq { key, value },
            CypherCompOp::Ne => Predicate::Ne { key, value },
            CypherCompOp::Gt => Predicate::Gt { key, value },
            CypherCompOp::Ge => Predicate::Gte { key, value },
            CypherCompOp::Lt => Predicate::Lt { key, value },
            CypherCompOp::Le => Predicate::Lte { key, value },
        })
    }

    /// Recognize a provenance accessor expression `source(x)`/`confidence(x)`/
    /// `reason(x)` (Issue #3354b).
    ///
    /// Returns:
    /// - `Ok(Some(field))` for a well-formed accessor over a single bound
    ///   variable,
    /// - `Ok(None)` for any other expression (including unknown function calls,
    ///   which fall through to the ordinary comparison path),
    /// - `Err(..)` for a provenance-named accessor with a malformed argument
    ///   (e.g. `source(n.foo)`, `confidence('x')`, `confidence(r, s)`,
    ///   `source()`), so the mistake surfaces as a structured error rather than
    ///   silently degrading.
    fn as_provenance_accessor(
        &self,
        expr: &CypherExpr,
    ) -> Result<Option<ProvenanceField>, CypherError> {
        let CypherExpr::FunctionCall {
            name,
            args,
            distinct,
        } = expr
        else {
            return Ok(None);
        };
        let Some(field) = provenance_field_name(name) else {
            return Ok(None);
        };
        // `DISTINCT` is only meaningful for aggregates; reject it here rather
        // than silently ignoring it.
        if *distinct {
            return Err(CypherError::SemanticError(format!(
                "{}(x) provenance accessor does not accept DISTINCT",
                field.accessor_name()
            )));
        }
        match args.as_slice() {
            [CypherExpr::Variable(_)] => Ok(Some(field)),
            _ => Err(CypherError::SemanticError(format!(
                "{}(x) provenance accessor requires exactly one bound variable argument",
                field.accessor_name()
            ))),
        }
    }

    /// Recognize a `provenance(x)` bundle accessor for the `IS [NOT] NULL` form
    /// (Issue #3354b).
    ///
    /// Returns `Ok(true)` for a well-formed `provenance(<bound variable>)` call,
    /// `Ok(false)` for any other expression, and `Err(..)` for a
    /// `provenance(...)` call with a malformed argument. Unlike the field
    /// accessors, `provenance` addresses the whole bundle and is only valid in
    /// the `IS [NOT] NULL` position.
    fn is_provenance_bundle_accessor(&self, expr: &CypherExpr) -> Result<bool, CypherError> {
        let CypherExpr::FunctionCall {
            name,
            args,
            distinct,
        } = expr
        else {
            return Ok(false);
        };
        if !name.eq_ignore_ascii_case("provenance") {
            return Ok(false);
        }
        if *distinct {
            return Err(CypherError::SemanticError(
                "provenance(x) does not accept DISTINCT".to_string(),
            ));
        }
        match args.as_slice() {
            [CypherExpr::Variable(_)] => Ok(true),
            _ => Err(CypherError::SemanticError(
                "provenance(x) IS NULL requires a single bound variable argument".to_string(),
            )),
        }
    }

    /// Lower a provenance accessor comparison `field(x) <op> operand` (accessor
    /// already normalized to the left) into a [`Predicate::Provenance`], reusing
    /// the shared surface-agnostic lowering (Issue #3354b).
    ///
    /// The type-check / folding / ordering-op rejection / `[0,1]`-NaN validation
    /// semantics live once in [`lower_provenance_comparison`]; this method only
    /// adapts the Cypher operand/operator into the IR triple and maps the
    /// neutral error into the Cypher error vocabulary.
    fn build_provenance_comparison(
        &self,
        field: ProvenanceField,
        op: CypherCompOp,
        operand_expr: &CypherExpr,
    ) -> Result<Predicate, CypherError> {
        let operand = self.expr_to_provenance_operand(operand_expr)?;
        let cmp = cypher_comp_to_provenance_cmp(op);
        lower_provenance_comparison(field, cmp, operand).map_err(map_provenance_lowering_error)
    }

    /// Resolve the right-hand literal/parameter of a provenance comparison to a
    /// typed operand (Issue #3354b).
    fn expr_to_provenance_operand(
        &self,
        expr: &CypherExpr,
    ) -> Result<ProvenanceOperand, CypherError> {
        let value = match expr {
            CypherExpr::Value(v) => self.convert_value_to_predicate_value(v)?,
            _ => {
                return Err(CypherError::SemanticError(
                    "a provenance accessor must be compared to a literal or parameter".to_string(),
                ));
            }
        };
        match value {
            PredicateValue::String(s) => Ok(ProvenanceOperand::Str(s)),
            PredicateValue::Int(i) => Ok(ProvenanceOperand::Num(i as f64)),
            PredicateValue::Float(f) => Ok(ProvenanceOperand::Num(f)),
            PredicateValue::Bool(_) | PredicateValue::Null => Err(CypherError::ParameterError(
                "type mismatch: a provenance accessor operand must be a string or number"
                    .to_string(),
            )),
        }
    }

    /// Extract a property key from an expression.
    ///
    /// Valid forms: `n.prop` → `"prop"`, `n` → `"n"`.
    fn extract_property_key(&self, expr: &CypherExpr) -> Result<String, CypherError> {
        match expr {
            CypherExpr::Property { property, .. } => Ok(property.clone()),
            CypherExpr::Variable(name) => Ok(name.clone()),
            _ => Err(CypherError::SemanticError(format!(
                "expected property access or variable, got: {expr:?}"
            ))),
        }
    }

    /// Convert an expression to a [`PredicateValue`].
    ///
    /// Handles literal values and parameter references.
    fn convert_expr_to_predicate_value(
        &self,
        expr: &CypherExpr,
    ) -> Result<PredicateValue, CypherError> {
        match expr {
            CypherExpr::Value(val) => self.convert_value_to_predicate_value(val),
            _ => Err(CypherError::SemanticError(format!(
                "expected literal value, got: {expr:?}"
            ))),
        }
    }

    /// Convert a Cypher literal value to a [`PredicateValue`].
    ///
    /// Parameter references (`$name`) are resolved from the bound parameter map.
    fn convert_value_to_predicate_value(
        &self,
        value: &CypherValue,
    ) -> Result<PredicateValue, CypherError> {
        match value {
            CypherValue::Null => Ok(PredicateValue::Null),
            CypherValue::Bool(b) => Ok(PredicateValue::Bool(*b)),
            CypherValue::Int(n) => Ok(PredicateValue::Int(*n)),
            CypherValue::Float(f) => Ok(PredicateValue::Float(*f)),
            CypherValue::String(s) => Ok(PredicateValue::String(s.clone())),
            CypherValue::Parameter(name) => {
                let param = self.params.get(name).ok_or_else(|| {
                    CypherError::ParameterError(format!("unbound parameter: ${name}"))
                })?;
                param.to_predicate_value()
            }
            CypherValue::Vector(_) => Err(CypherError::UnsupportedFeature(
                "vector literals in predicate position".to_string(),
            )),
        }
    }

    // =======================================================================
    // Aggregation (implicit grouping)
    // =======================================================================

    /// Build a [`QueryOp::Aggregate`] from the `RETURN` clause if it contains
    /// any aggregate function call, applying openCypher implicit grouping:
    /// every non-aggregate item becomes a grouping key and every aggregate
    /// item becomes an [`AggregateSpec`]. Returns `Ok(None)` when the clause
    /// has no aggregates (the ordinary projection path).
    fn build_aggregate(&self, ret: &CypherReturn) -> Result<Option<QueryOp>, CypherError> {
        let has_aggregate = ret.items.iter().any(|item| {
            matches!(
                item,
                CypherReturnItem::Expression { expr, .. }
                    if Self::aggregate_call(expr).is_some()
            )
        });
        if !has_aggregate {
            return Ok(None);
        }

        let mut group_keys = Vec::new();
        let mut aggregates = Vec::new();

        for item in &ret.items {
            match item {
                CypherReturnItem::Star => {
                    return Err(CypherError::UnsupportedFeature(
                        "RETURN * combined with aggregation; project explicit \
                         grouping keys instead"
                            .to_string(),
                    ));
                }
                CypherReturnItem::Variable(name) => {
                    // Grouping by a whole node/edge cannot be expressed by the
                    // single-entity row model (it would collapse into one
                    // Null-keyed group). Reject rather than silently mis-group.
                    return Err(CypherError::UnsupportedFeature(format!(
                        "grouping by a whole node/edge is not supported \
                         (RETURN {name}, <aggregate>); group by a property such \
                         as {name}.<key> instead"
                    )));
                }
                CypherReturnItem::Expression { expr, alias } => {
                    if let Some((func, args, distinct)) = Self::aggregate_call(expr) {
                        let arg = Self::aggregate_arg(func, args)?;
                        let default_alias = Self::aggregate_default_alias(func, args, distinct);
                        aggregates.push(AggregateSpec {
                            func,
                            arg,
                            distinct,
                            alias: alias.clone().unwrap_or(default_alias),
                        });
                    } else {
                        let (property_key, default_alias) = Self::group_key_of(expr)?;
                        group_keys.push(AggregateGroupKey {
                            property_key,
                            alias: alias.clone().unwrap_or(default_alias),
                        });
                    }
                }
            }
        }

        Ok(Some(QueryOp::Aggregate {
            group_keys,
            aggregates,
        }))
    }

    /// If `expr` is an aggregate function call, return its function, argument
    /// list, and `DISTINCT` flag. Non-aggregate calls (e.g. `vector.cosine`)
    /// and non-calls return `None`.
    fn aggregate_call(expr: &CypherExpr) -> Option<(AggregateFunc, &[CypherExpr], bool)> {
        if let CypherExpr::FunctionCall {
            name,
            args,
            distinct,
        } = expr
            && let Some(func) = Self::aggregate_func_name(name)
        {
            return Some((func, args.as_slice(), *distinct));
        }
        None
    }

    /// Map a function name (case-insensitive) to an [`AggregateFunc`].
    fn aggregate_func_name(name: &str) -> Option<AggregateFunc> {
        match name.to_ascii_uppercase().as_str() {
            "COUNT" => Some(AggregateFunc::Count),
            "SUM" => Some(AggregateFunc::Sum),
            "AVG" => Some(AggregateFunc::Avg),
            "MIN" => Some(AggregateFunc::Min),
            "MAX" => Some(AggregateFunc::Max),
            "COLLECT" => Some(AggregateFunc::Collect),
            _ => None,
        }
    }

    /// Resolve what an aggregate reads from each row into an [`AggregateArg`].
    ///
    /// `count(*)` -> [`AggregateArg::Star`] (count every row); a bare-variable
    /// `count(n)` -> [`AggregateArg::Entity`] (count rows with a non-null bound
    /// entity, openCypher semantics); `func(v.prop)` ->
    /// [`AggregateArg::Property`]. Non-property arguments to value aggregates
    /// are rejected in v1.
    fn aggregate_arg(
        func: AggregateFunc,
        args: &[CypherExpr],
    ) -> Result<AggregateArg, CypherError> {
        match args {
            // `*` (or the degenerate empty arg list) is only valid for count.
            [CypherExpr::Star] | [] if func == AggregateFunc::Count => Ok(AggregateArg::Star),
            [CypherExpr::Property { property, .. }] => Ok(AggregateArg::Property(property.clone())),
            // A bare variable argument (`count(n)`) references the bound entity,
            // not a property: count non-null bindings (distinct from count(*)).
            [CypherExpr::Variable(_)] if func == AggregateFunc::Count => Ok(AggregateArg::Entity),
            _ => Err(CypherError::UnsupportedFeature(format!(
                "aggregate argument must be `*` (count only) or a property access \
                 (e.g. n.age); got {args:?}"
            ))),
        }
    }

    /// Generate the default output column name for an aggregate (used when no
    /// `AS` alias is supplied), e.g. `count(*)`, `sum(n.age)`,
    /// `count(DISTINCT n.dept)`.
    fn aggregate_default_alias(func: AggregateFunc, args: &[CypherExpr], distinct: bool) -> String {
        let func_name = match func {
            AggregateFunc::Count => "count",
            AggregateFunc::Sum => "sum",
            AggregateFunc::Avg => "avg",
            AggregateFunc::Min => "min",
            AggregateFunc::Max => "max",
            AggregateFunc::Collect => "collect",
        };
        let arg_text = match args.first() {
            None => String::new(),
            Some(CypherExpr::Star) => "*".to_string(),
            Some(CypherExpr::Property { variable, property }) => format!("{variable}.{property}"),
            Some(CypherExpr::Variable(v)) => v.clone(),
            Some(_) => "expr".to_string(),
        };
        let distinct_prefix = if distinct { "DISTINCT " } else { "" };
        format!("{func_name}({distinct_prefix}{arg_text})")
    }

    /// Resolve a non-aggregate `RETURN` item into a grouping key:
    /// `(property_to_read, default_column_name)`.
    ///
    /// Only property access (`n.dept`) is supported. A bare variable
    /// (whole node/edge) is rejected -- see the `Variable` arm in
    /// [`Self::build_aggregate`].
    fn group_key_of(expr: &CypherExpr) -> Result<(String, String), CypherError> {
        match expr {
            CypherExpr::Property { variable, property } => {
                Ok((property.clone(), format!("{variable}.{property}")))
            }
            CypherExpr::Variable(name) => Err(CypherError::UnsupportedFeature(format!(
                "grouping by a whole node/edge is not supported \
                 (RETURN {name}, <aggregate>); group by a property such as \
                 {name}.<key> instead"
            ))),
            _ => Err(CypherError::UnsupportedFeature(format!(
                "grouping key must be a property access (e.g. n.dept); got {expr:?}"
            ))),
        }
    }

    /// Resolve an `ORDER BY` item, in an aggregating query, to the output
    /// column name it should sort by (the aggregate/group alias or generated
    /// column name).
    fn aggregate_order_key(
        &self,
        ret: &CypherReturn,
        order_item: &CypherOrderItem,
    ) -> Result<String, CypherError> {
        let expr = &order_item.expr;

        // A bare variable directly names an output column / AS alias.
        if let CypherExpr::Variable(name) = expr {
            return Ok(name.clone());
        }

        // Otherwise compute the column name the same way build_aggregate does.
        let default_name = if let Some((func, args, distinct)) = Self::aggregate_call(expr) {
            Self::aggregate_default_alias(func, args, distinct)
        } else if let CypherExpr::Property { variable, property } = expr {
            format!("{variable}.{property}")
        } else {
            return Err(CypherError::UnsupportedFeature(format!(
                "unsupported ORDER BY expression over an aggregating query: {expr:?}"
            )));
        };

        // If a RETURN item projects this exact expression under an alias, order
        // by that alias (its actual output column name).
        for item in &ret.items {
            if let CypherReturnItem::Expression {
                expr: item_expr,
                alias: Some(alias),
            } = item
                && item_expr == expr
            {
                return Ok(alias.clone());
            }
        }
        Ok(default_name)
    }

    // =======================================================================
    // ORDER BY helpers
    // =======================================================================

    /// Convert an `ORDER BY` item into a [`SortKey`].
    ///
    /// # Errors
    ///
    /// Returns [`CypherError::UnsupportedFeature`] if the expression is not a
    /// property access, variable reference, or provenance accessor.
    fn convert_order_item_to_sort_key(
        &self,
        item: &CypherOrderItem,
    ) -> Result<SortKey, CypherError> {
        // ORDER BY a provenance accessor (Issue #3354): resolved per row from
        // the row entity's write-time provenance. `as_provenance_accessor`
        // returns `Err` for a malformed accessor argument (never silent).
        if let Some(field) = self.as_provenance_accessor(&item.expr)? {
            return Ok(SortKey::Provenance(field));
        }
        match &item.expr {
            CypherExpr::Property { property, .. } => Ok(SortKey::Property(property.clone())),
            CypherExpr::Variable(name) => Ok(SortKey::Property(name.clone())),
            _ => Err(CypherError::UnsupportedFeature(format!(
                "unsupported expression in ORDER BY clause: {:?}",
                item.expr
            ))),
        }
    }

    /// Detect provenance accessors in a `RETURN` clause and build a
    /// [`QueryOp::ProjectProvenance`] op (Issue #3354), or `None` when the
    /// clause projects none. A provenance-named accessor with a malformed
    /// argument is a structured error (never silently dropped), reusing the same
    /// [`as_provenance_accessor`](Self::as_provenance_accessor) recognition the
    /// `WHERE` half uses.
    ///
    /// v1 supports only the clean single-entity forms
    /// (`RETURN <entity>, <accessor(s)>` and `RETURN <accessor(s)>` over the
    /// single bound node). Building general property-projection-into-columns is
    /// out of scope, so an accessor mixed with a property projection
    /// (`RETURN n.name, source(n)`), an aggregate
    /// (`RETURN n.dept, count(*), source(n)`), or a variable other than the
    /// single bound entity (`RETURN a, source(b)`) is **rejected** with a
    /// structured [`CypherError::UnsupportedFeature`] rather than silently
    /// dropping a column or resolving the wrong entity. (Edge/traversal-variable
    /// accessors never reach this path: a multi-variable MATCH routes to the
    /// multi-pattern evaluator, so provenance projection is node-entity-scoped in
    /// v1.)
    fn build_provenance_projection(
        &self,
        return_clause: &CypherReturn,
        patterns: &[CypherPattern],
        aggregated: bool,
    ) -> Result<Option<QueryOp>, CypherError> {
        let mut items = Vec::new();
        let mut entity_binding: Option<String> = None;
        // Distinct entity variables referenced across the bare entity and every
        // accessor argument; the supported form references exactly one.
        let mut referenced_vars: BTreeSet<String> = BTreeSet::new();
        let mut has_property_projection = false;
        for item in &return_clause.items {
            let (expr, alias) = match item {
                CypherReturnItem::Expression { expr, alias } => (expr, alias.clone()),
                // A bare `RETURN n` variable item -- preserve it as the entity
                // binding so it stays observable alongside projected columns.
                CypherReturnItem::Variable(name) => {
                    if entity_binding.is_none() {
                        entity_binding = Some(name.clone());
                    }
                    referenced_vars.insert(name.clone());
                    continue;
                }
                CypherReturnItem::Star => continue,
            };
            match expr {
                // The parser emits a bare variable as an `Expression` item.
                CypherExpr::Variable(name) => {
                    if entity_binding.is_none() {
                        entity_binding = Some(name.clone());
                    }
                    referenced_vars.insert(name.clone());
                }
                // A property projection (`n.name`) alongside an accessor is the
                // unsupported mix (rejected below once an accessor is present).
                CypherExpr::Property { .. } => {
                    has_property_projection = true;
                }
                CypherExpr::FunctionCall { args, .. } => {
                    if let Some(field) = self.as_provenance_accessor(expr)? {
                        // `as_provenance_accessor` guarantees exactly one bound
                        // variable argument.
                        let var = match args.as_slice() {
                            [CypherExpr::Variable(v)] => v.clone(),
                            _ => String::new(),
                        };
                        referenced_vars.insert(var.clone());
                        let output_name =
                            alias.unwrap_or_else(|| format!("{}({})", field.accessor_name(), var));
                        items.push(ProvenanceProjectionItem { output_name, field });
                    }
                }
                _ => {}
            }
        }
        if items.is_empty() {
            return Ok(None);
        }

        // An accessor is projected: enforce the v1 single-entity contract.
        if aggregated {
            return Err(CypherError::UnsupportedFeature(
                "combining a provenance accessor with an aggregate in RETURN is not supported \
                 (v1); an aggregating RETURN produces its own computed columns"
                    .to_string(),
            ));
        }
        if has_property_projection {
            return Err(CypherError::UnsupportedFeature(
                "combining a property projection with a provenance accessor in RETURN is not \
                 supported (v1); return the entity itself (RETURN n, source(n)) or the accessors \
                 alone (RETURN source(n))"
                    .to_string(),
            ));
        }
        // The supported form binds exactly one entity: a single MATCH node with a
        // variable. Every referenced variable (bare entity + accessor args) must
        // be that node. This rejects the multi-variable mismatch
        // (`RETURN a, source(b)`, including an accessor over an unbound variable),
        // which the single-entity positional pipeline would otherwise resolve
        // against the wrong (or a nonexistent) entity.
        let sole_entity = Self::sole_pattern_node_var(patterns);
        let matches_sole = match &sole_entity {
            Some(v) => referenced_vars.len() == 1 && referenced_vars.contains(v),
            None => false,
        };
        if !matches_sole {
            return Err(CypherError::UnsupportedFeature(
                "provenance projection in RETURN is supported only for a single bound entity \
                 whose accessor variable matches the returned node (RETURN n, source(n)); \
                 projecting an accessor over a different variable or a multi-entity match is not \
                 supported (v1)"
                    .to_string(),
            ));
        }

        Ok(Some(QueryOp::ProjectProvenance(ProvenanceProjection {
            entity_binding,
            items,
        })))
    }

    /// The variable of the single bound node when the MATCH is exactly one
    /// pattern consisting of one node element with a variable and no
    /// relationships; `None` otherwise. This is the one entity the single-entity
    /// positional pipeline can correctly attribute a provenance accessor to.
    fn sole_pattern_node_var(patterns: &[CypherPattern]) -> Option<String> {
        let [pattern] = patterns else {
            return None;
        };
        let [CypherPatternElement::Node(node)] = pattern.elements.as_slice() else {
            return None;
        };
        node.variable.clone()
    }

    // =======================================================================
    // Temporal conversion
    // =======================================================================

    /// Convert a temporal AST clause into a [`TemporalContext`] and optionally
    /// emit temporal query operations.
    fn convert_temporal(
        &self,
        temporal: &CypherTemporal,
        ops: &mut Vec<QueryOp>,
    ) -> Result<TemporalContext, CypherError> {
        match temporal {
            CypherTemporal::AsOfTimestamp(ts_str) => {
                let ts = parse_timestamp_string(ts_str)?;
                // AS OF TIMESTAMP sets both valid time and transaction time
                Ok(TemporalContext::as_of(ts, ts))
            }
            CypherTemporal::AsOfValidTime(ts_str) => {
                let ts = parse_timestamp_string(ts_str)?;
                Ok(TemporalContext::as_of_valid_time(ts))
            }
            CypherTemporal::AsOfSystemTime(ts_str) => {
                let ts = parse_timestamp_string(ts_str)?;
                Ok(TemporalContext::as_of_transaction_time(ts))
            }
            CypherTemporal::BiTemporal {
                valid_time,
                system_time,
            } => {
                let vt = parse_timestamp_string(valid_time)?;
                let st = parse_timestamp_string(system_time)?;
                Ok(TemporalContext::as_of(vt, st))
            }
            CypherTemporal::Between { start, end } => {
                let start_ts = parse_timestamp_string(start)?;
                let end_ts = parse_timestamp_string(end)?;
                let time_range = TimeRange::new(start_ts, end_ts).map_err(|e| {
                    CypherError::InvalidTemporalClause(format!("invalid time range: {e}"))
                })?;
                ops.push(QueryOp::Between { time_range });
                Ok(TemporalContext {
                    valid_time_between: Some(time_range),
                    include_history: true,
                    ..Default::default()
                })
            }
        }
    }

    // =======================================================================
    // Vector / similarity conversion
    // =======================================================================

    /// Check if the ORDER BY clause contains a vector similarity function call
    /// and, if so, emit a [`QueryOp::RankBySimilarity`] instead of a regular Sort.
    ///
    /// Returns `true` if a vector rank was emitted (meaning the caller should
    /// skip normal sort emission for this ORDER BY).
    fn try_emit_vector_rank(
        &self,
        return_clause: &CypherReturn,
        ops: &mut Vec<QueryOp>,
    ) -> Result<bool, CypherError> {
        if return_clause.order_by.len() != 1 {
            return Ok(false);
        }

        // Every `vector.fn(...) AS <alias>` item in the RETURN list (Issue #555).
        // v1 supports at most ONE such projection, and it must be the SAME
        // function (name + args) as the one driving the rank -- otherwise the
        // materialized score would come from a different metric/embedding than
        // the ordering, a silent wrong answer (adversarial finding #2).
        let aliased = Self::aliased_vector_return_items(return_clause);
        if aliased.len() > 1 {
            return Err(CypherError::UnsupportedFeature(
                "multiple vector score projections in a single RETURN are not \
                 supported in v1 (project a single vector.fn(...) AS <alias>)"
                    .to_string(),
            ));
        }

        let order_item = &return_clause.order_by[0];

        // Arm 1: `ORDER BY vector.fn(entity.prop, <emb>) <DIR>` directly.
        if let CypherExpr::FunctionCall { name, args, .. } = &order_item.expr
            && is_vector_function(name)
        {
            let metric = vector_metric_for(name);
            // Reject an ordering direction that would request farthest-first
            // (silent wrong answer #1): the rank always yields nearest-first, so
            // the direction must match the metric's natural nearest-first sense.
            Self::check_vector_order_direction(name, metric, order_item.descending)?;
            // Attach the projected alias only when it is the SAME function+args.
            let score_alias = match aliased.first() {
                None => None,
                Some(&(a_name, a_args, alias)) => {
                    if a_name == name && a_args == args.as_slice() {
                        Some(alias.clone())
                    } else {
                        return Err(CypherError::UnsupportedFeature(
                            "the RETURN projects a vector function that differs \
                             from the ORDER BY vector function; independent \
                             multi-score projection is not supported in v1 \
                             (project and order by the same vector.fn(...))"
                                .to_string(),
                        ));
                    }
                }
            };
            let (property_key, embedding) = self.extract_vector_args(args)?;
            ops.push(QueryOp::RankBySimilarity {
                embedding,
                top_k: return_clause.limit,
                property_key: Some(property_key),
                metric,
                threshold: None,
                score_alias,
            });
            return Ok(true);
        }

        // Arm 2: `ORDER BY <alias>` where `<alias>` was defined as a vector
        // function in the RETURN items (e.g. `vector.cosine(...) AS score` then
        // `ORDER BY score DESC`). The score is materialized under `<alias>`.
        if let CypherExpr::Variable(ref alias_name) = order_item.expr
            && let Some(&(name, args, alias)) = aliased
                .iter()
                .find(|entry| entry.2.as_str() == alias_name.as_str())
        {
            let metric = vector_metric_for(name);
            Self::check_vector_order_direction(name, metric, order_item.descending)?;
            let (property_key, embedding) = self.extract_vector_args(args)?;
            ops.push(QueryOp::RankBySimilarity {
                embedding,
                top_k: return_clause.limit,
                property_key: Some(property_key),
                metric,
                threshold: None,
                score_alias: Some(alias.clone()),
            });
            return Ok(true);
        }

        Ok(false)
    }

    /// Every `RETURN` item of the form `vector.fn(...) AS <alias>`, as
    /// `(name, args, alias)`. Used to materialize the similarity score under its
    /// alias (Issue #555) and to reject unsupported multi-/mismatched-projection
    /// forms (adversarial finding #2).
    fn aliased_vector_return_items(
        return_clause: &CypherReturn,
    ) -> Vec<(&String, &[CypherExpr], &String)> {
        return_clause
            .items
            .iter()
            .filter_map(|item| match item {
                CypherReturnItem::Expression {
                    expr: CypherExpr::FunctionCall { name, args, .. },
                    alias: Some(alias),
                } if is_vector_function(name) => Some((name, args.as_slice(), alias)),
                _ => None,
            })
            .collect()
    }

    /// Reject a vector `ORDER BY` direction that would request farthest-first
    /// results (adversarial finding #1). The rank always yields **nearest-first**,
    /// so the ordering direction must match the metric's natural nearest-first
    /// sense: `DESC` for similarity metrics (cosine / similarity / dot_product,
    /// higher = nearer) and `ASC` for the distance-flavored `vector.euclidean`
    /// (lower = nearer, matching the Issue #553 example). Any other direction
    /// asks for farthest-first, which v1 does not support (rejected clearly
    /// rather than silently returning nearest-first).
    fn check_vector_order_direction(
        func_name: &str,
        metric: VectorMetric,
        descending: bool,
    ) -> Result<(), CypherError> {
        let natural_descending = !matches!(metric, VectorMetric::Euclidean);
        if descending != natural_descending {
            let (wanted, other) = if natural_descending {
                ("DESC", "ASC")
            } else {
                ("ASC", "DESC")
            };
            return Err(CypherError::UnsupportedFeature(format!(
                "vector ranking over {func_name} supports only {wanted} ordering \
                 (nearest-first); {other} (farthest-first) vector ordering is not \
                 supported in v1"
            )));
        }
        Ok(())
    }

    /// Extract the property key and embedding from vector function arguments.
    ///
    /// Expected args: `(entity.property, <embedding>)` where `<embedding>` is a
    /// parameter reference (`$q`) or an inline numeric vector literal
    /// (`[0.1, 0.2, 0.3]`, Issue #554).
    fn extract_vector_args(
        &self,
        args: &[CypherExpr],
    ) -> Result<(String, Arc<[f32]>), CypherError> {
        if args.len() != 2 {
            return Err(CypherError::SemanticError(
                "vector similarity function expects exactly 2 arguments".to_string(),
            ));
        }

        // First arg: property access (e.g., d.embedding)
        let property_key = match &args[0] {
            CypherExpr::Property { property, .. } => property.clone(),
            _ => return Err(CypherError::SemanticError(
                "first argument to vector function must be a property access (e.g., d.embedding)"
                    .to_string(),
            )),
        };

        let embedding = self.extract_embedding_arg(&args[1])?;
        Ok((property_key, embedding))
    }

    /// Resolve an expression used as an embedding input to a vector function.
    ///
    /// Accepts a bound `Embedding` parameter, a pre-built `Vector` literal, or
    /// an inline all-numeric list literal (`[0.1, 0.2, 0.3]` -- Int and Float
    /// elements are coerced to `f32`, Issue #554). Any other form -- a
    /// non-embedding parameter, a non-numeric list element, or an empty list --
    /// is a clear semantic/parameter error.
    fn extract_embedding_arg(&self, expr: &CypherExpr) -> Result<Arc<[f32]>, CypherError> {
        match expr {
            CypherExpr::Value(CypherValue::Parameter(param_name)) => {
                let param = self.params.get(param_name).ok_or_else(|| {
                    CypherError::ParameterError(format!("unbound parameter: ${param_name}"))
                })?;
                match param {
                    CypherParameterValue::Embedding(emb) => Ok(Arc::clone(emb)),
                    _ => Err(CypherError::ParameterError(format!(
                        "parameter ${param_name} must be an Embedding, got: {param:?}"
                    ))),
                }
            }
            CypherExpr::Value(CypherValue::Vector(v)) => Ok(Arc::clone(v)),
            // Inline numeric vector literal: `[0.1, 0.2, 0.3]` (Issue #554).
            CypherExpr::List(elements) => Self::coerce_numeric_list_to_vector(elements),
            _ => Err(CypherError::SemanticError(
                "the embedding argument to a vector function must be a parameter \
                 or a numeric vector literal (e.g. [0.1, 0.2, 0.3])"
                    .to_string(),
            )),
        }
    }

    /// Coerce an all-numeric Cypher list literal into an embedding `Arc<[f32]>`.
    ///
    /// Both integer and float elements are accepted (mirroring JSON numeric
    /// arrays). A non-numeric element or an empty list is rejected with a clear
    /// error so a mistyped embedding never silently degrades a similarity query.
    fn coerce_numeric_list_to_vector(elements: &[CypherExpr]) -> Result<Arc<[f32]>, CypherError> {
        if elements.is_empty() {
            return Err(CypherError::SemanticError(
                "vector literal must have at least one element (empty embedding)".to_string(),
            ));
        }
        let mut out: Vec<f32> = Vec::with_capacity(elements.len());
        for (idx, el) in elements.iter().enumerate() {
            let v = match el {
                CypherExpr::Value(CypherValue::Float(f)) => *f as f32,
                CypherExpr::Value(CypherValue::Int(i)) => *i as f32,
                other => {
                    return Err(CypherError::SemanticError(format!(
                        "vector literal element {idx} is not numeric: {other:?}; \
                         a vector literal must contain only numbers \
                         (e.g. [0.1, 0.2, 0.3])"
                    )));
                }
            };
            out.push(v);
        }
        Ok(Arc::from(out))
    }

    /// Convert a base-pattern `WHERE` clause into ops, lifting any vector
    /// similarity **threshold** comparison (`vector.similarity(d.emb, $q) > 0.8`,
    /// Issue #553) out of the predicate into a [`QueryOp::RankBySimilarity`]
    /// carrying a [`ScoreThreshold`], and emitting a `Filter` for the residual
    /// (non-vector) part of the predicate.
    ///
    /// A threshold conjunct may appear alone or as one term of a top-level
    /// `AND` chain (`vector.similarity(...) > 0.8 AND d.lang = 'en'`); the
    /// residual conjuncts become an ordinary `Filter` evaluated **before** the
    /// rank so the score is only computed over surviving rows.
    fn convert_where_clause(
        &self,
        expr: &CypherExpr,
        ops: &mut Vec<QueryOp>,
    ) -> Result<(), CypherError> {
        // Split the top-level AND spine so a vector threshold can be lifted out
        // of `A AND B AND ...` while the rest becomes an ordinary Filter.
        let conjuncts = flatten_logical_operands(expr, |e| match e {
            CypherExpr::And(left, right) => Some((left.as_ref(), right.as_ref())),
            _ => None,
        });

        let mut ranks: Vec<QueryOp> = Vec::new();
        let mut residual: Vec<&CypherExpr> = Vec::new();
        for conj in conjuncts {
            if let Some(rank) = self.try_vector_threshold(conj)? {
                ranks.push(rank);
            } else {
                residual.push(conj);
            }
        }

        // No vector threshold: preserve the original single-Filter behavior.
        if ranks.is_empty() {
            let predicate = self.convert_expr_to_predicate(expr)?;
            ops.push(QueryOp::Filter(predicate));
            return Ok(());
        }

        // Residual property predicate first (filter, then score survivors)...
        if !residual.is_empty() {
            let predicate = if residual.len() == 1 {
                self.convert_expr_to_predicate(residual[0])?
            } else {
                let preds = residual
                    .iter()
                    .map(|e| self.convert_expr_to_predicate(e))
                    .collect::<Result<Vec<_>, _>>()?;
                Predicate::And(preds)
            };
            ops.push(QueryOp::Filter(predicate));
        }
        // ...then the vector similarity threshold rank(s).
        //
        // v1 BEHAVIOR (adversarial finding #6): the threshold is implemented as a
        // RankBySimilarity with `top_k: None`, which computes the score for every
        // surviving row and returns them **sorted by similarity descending** (the
        // rank's inherent output order). So a bare
        // `WHERE vector.similarity(...) > t RETURN d` (no ORDER BY) returns the
        // passing rows nearest-first rather than in scan order, and a trailing
        // `LIMIT n` therefore yields the top-n *nearest* passing rows rather than
        // the first-n in scan order. openCypher leaves row order unspecified
        // without ORDER BY, so this is a superset-correct (not wrong) result; it
        // is pinned by a test and documented in the compatibility guide. A
        // score-unaware Filter-only lowering is a possible follow-up but would
        // still need the score computed to evaluate the predicate.
        ops.append(&mut ranks);
        Ok(())
    }

    /// If `expr` is a vector-similarity threshold comparison
    /// (`vector.fn(entity.prop, <emb>) <cmp> <number>`, or the mirrored
    /// `<number> <cmp> vector.fn(...)`), build the [`QueryOp::RankBySimilarity`]
    /// that computes the score and keeps only rows satisfying the threshold.
    /// Returns `Ok(None)` for any non-vector-threshold expression.
    fn try_vector_threshold(&self, expr: &CypherExpr) -> Result<Option<QueryOp>, CypherError> {
        let CypherExpr::Comparison { left, op, right } = expr else {
            return Ok(None);
        };

        // Identify which side is the vector function and orient the comparison
        // so it reads `score <cmp> value`.
        let (func_name, args, cmp) = match (left.as_ref(), right.as_ref()) {
            (CypherExpr::FunctionCall { name, args, .. }, _) if is_vector_function(name) => {
                (name, args, *op)
            }
            (_, CypherExpr::FunctionCall { name, args, .. }) if is_vector_function(name) => {
                // `value <cmp> score` == `score <flipped-cmp> value`.
                (name, args, flip_comparison(*op))
            }
            _ => return Ok(None),
        };

        let Some(threshold_op) = score_comparison_for(cmp) else {
            // `=`/`<>` against a similarity score is not a threshold filter.
            return Err(CypherError::UnsupportedFeature(
                "vector similarity comparisons support only >, >=, <, <= \
                 thresholds (not = or <>)"
                    .to_string(),
            ));
        };

        // The other operand must be a numeric literal threshold value.
        let value_expr = if matches!(left.as_ref(), CypherExpr::FunctionCall { name, .. } if is_vector_function(name))
        {
            right.as_ref()
        } else {
            left.as_ref()
        };
        let value = match value_expr {
            CypherExpr::Value(CypherValue::Float(f)) => *f as f32,
            CypherExpr::Value(CypherValue::Int(i)) => *i as f32,
            other => {
                return Err(CypherError::SemanticError(format!(
                    "vector similarity threshold must compare against a numeric \
                     literal, got: {other:?}"
                )));
            }
        };

        let (property_key, embedding) = self.extract_vector_args(args)?;
        Ok(Some(QueryOp::RankBySimilarity {
            embedding,
            top_k: None,
            property_key: Some(property_key),
            metric: vector_metric_for(func_name),
            threshold: Some(ScoreThreshold {
                op: threshold_op,
                value,
            }),
            score_alias: None,
        }))
    }
}

// ---------------------------------------------------------------------------
// Logical-operator flattening
// ---------------------------------------------------------------------------

/// Flatten a left-nested chain of a single logical operator (`AND` or `OR`, as
/// built by the parser's `parse_and_expr` / `parse_or_expr`) into its ordered
/// operand list, iteratively — without recursing down the operator spine.
///
/// `split` returns `Some((left, right))` for a node of the operator being
/// flattened and `None` for any other node. Nodes for which `split` returns
/// `None` (including a sub-tree of the *other* logical operator) are returned
/// as opaque operands, to be converted individually by the caller. Operand
/// order is preserved left-to-right.
///
/// This keeps converting an arbitrarily long `a AND b AND c ...` (or `OR`)
/// chain from overflowing the stack (issue #3404): the spine is walked with an
/// explicit heap stack instead of one call frame per operator.
fn flatten_logical_operands<'a>(
    root: &'a CypherExpr,
    split: impl Fn(&'a CypherExpr) -> Option<(&'a CypherExpr, &'a CypherExpr)>,
) -> Vec<&'a CypherExpr> {
    let mut operands = Vec::new();
    let mut stack = vec![root];
    // Pushing right-then-left means the left operand is popped first, so leaves
    // are collected in source (left-to-right) order.
    while let Some(node) = stack.pop() {
        if let Some((left, right)) = split(node) {
            stack.push(right);
            stack.push(left);
        } else {
            operands.push(node);
        }
    }
    operands
}

// ---------------------------------------------------------------------------
// Timestamp parsing
// ---------------------------------------------------------------------------

/// Parse a timestamp string into a [`Timestamp`] (HybridTimestamp).
///
/// Supports:
/// - Unix microseconds: `"1705312800000000"`
/// - ISO 8601 with timezone: `"2024-01-15T10:00:00Z"`
/// - Date only: `"2024-01-15"` (midnight UTC)
fn parse_timestamp_string(s: &str) -> Result<Timestamp, CypherError> {
    let trimmed = s.trim().trim_matches('\'').trim_matches('"');

    // Try unix microseconds first
    if let Ok(micros) = trimmed.parse::<i64>() {
        return Ok(Timestamp::from(micros));
    }

    // ISO 8601 with timezone: 2024-01-15T10:00:00Z
    if let Ok(dt) = trimmed.parse::<chrono::DateTime<chrono::Utc>>() {
        return Ok(Timestamp::from(dt.timestamp_micros()));
    }

    // Date only: 2024-01-15
    if let Ok(date) = trimmed.parse::<chrono::NaiveDate>()
        && let Some(dt) = date.and_hms_opt(0, 0, 0)
    {
        return Ok(Timestamp::from(dt.and_utc().timestamp_micros()));
    }

    Err(CypherError::InvalidTimestamp(s.to_string()))
}

/// Returns `true` if the function name is a vector similarity function.
fn is_vector_function(name: &str) -> bool {
    matches!(
        name,
        "vector.similarity" | "vector.cosine" | "vector.euclidean" | "vector.dot_product"
    )
}

/// Maps a vector function name to the distance/similarity metric it selects.
///
/// `vector.similarity` and `vector.cosine` both select cosine similarity;
/// `vector.euclidean` selects Euclidean distance (scored as `1/(1+distance)`,
/// higher = nearer); `vector.dot_product` selects the inner product. Any name
/// for which [`is_vector_function`] is `true` is covered; unknown names default
/// to cosine (never reached for a validated call).
fn vector_metric_for(name: &str) -> VectorMetric {
    match name {
        "vector.euclidean" => VectorMetric::Euclidean,
        "vector.dot_product" => VectorMetric::DotProduct,
        // "vector.similarity" | "vector.cosine" and any fallback
        _ => VectorMetric::Cosine,
    }
}

/// Map an ordering comparison operator to a [`ScoreThreshold`] operator.
///
/// Only the four ordering operators define a similarity threshold; `=`/`<>`
/// return `None` (rejected by the caller).
fn score_comparison_for(op: CypherCompOp) -> Option<ScoreComparison> {
    match op {
        CypherCompOp::Gt => Some(ScoreComparison::Gt),
        CypherCompOp::Ge => Some(ScoreComparison::Ge),
        CypherCompOp::Lt => Some(ScoreComparison::Lt),
        CypherCompOp::Le => Some(ScoreComparison::Le),
        CypherCompOp::Eq | CypherCompOp::Ne => None,
    }
}

/// Flip a comparison operator so `value <op> score` becomes `score <flipped> value`.
fn flip_comparison(op: CypherCompOp) -> CypherCompOp {
    match op {
        CypherCompOp::Gt => CypherCompOp::Lt,
        CypherCompOp::Ge => CypherCompOp::Le,
        CypherCompOp::Lt => CypherCompOp::Gt,
        CypherCompOp::Le => CypherCompOp::Ge,
        CypherCompOp::Eq => CypherCompOp::Eq,
        CypherCompOp::Ne => CypherCompOp::Ne,
    }
}

// ---------------------------------------------------------------------------
// Convenience functions
// ---------------------------------------------------------------------------

/// Parse a Cypher query string and convert it to a [`Query`].
///
/// This is the simplest way to go from a Cypher string to an executable query.
/// No parameter bindings are provided.
///
/// # Errors
///
/// Returns [`CypherError`] on lex, parse, or conversion errors.
///
/// # Example
///
/// ```rust
/// use aletheiadb::cypher::parse_cypher;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let query = parse_cypher("MATCH (n:Person) RETURN n")?;
/// # Ok(())
/// # }
/// ```
pub fn parse_cypher(input: &str) -> Result<Query, CypherError> {
    let stmt = CypherParser::parse(input)?;
    CypherConverter::new().convert(stmt)
}

/// Parse a Cypher query string with parameter bindings and convert to a [`Query`].
///
/// Parameters are referenced in the query as `$name` and resolved from the
/// provided map during conversion.
///
/// # Errors
///
/// Returns [`CypherError`] on lex, parse, conversion, or missing-parameter errors.
///
/// # Example
///
/// ```rust
/// use aletheiadb::cypher::{parse_cypher_with_params, CypherParameterValue};
/// use std::collections::HashMap;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut params = HashMap::new();
/// params.insert("name".to_string(), CypherParameterValue::String("Alice".into()));
/// let query = parse_cypher_with_params(
///     "MATCH (n:Person {name: $name}) RETURN n",
///     params,
/// )?;
/// # Ok(())
/// # }
/// ```
pub fn parse_cypher_with_params(
    input: &str,
    params: HashMap<String, CypherParameterValue>,
) -> Result<Query, CypherError> {
    let stmt = CypherParser::parse(input)?;
    CypherConverter::with_params(params).convert(stmt)
}
