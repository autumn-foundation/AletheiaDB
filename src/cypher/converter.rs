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

use std::collections::HashMap;
use std::sync::Arc;

use super::ast::*;
use super::error::CypherError;
use super::parser::CypherParser;
use crate::core::temporal::{TimeRange, Timestamp};
use crate::query::builder::Query;
use crate::query::ir::{Predicate, PredicateValue, QueryOp, SortKey, TraversalDepth};
use crate::query::plan::{QueryHints, TemporalContext};

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
        }
    }
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
                    let predicate = self.convert_expr_to_predicate(&expr)?;
                    base_ops.push(QueryOp::Filter(predicate));
                }
                if optional {
                    ops.push(QueryOp::Optional { ops: base_ops });
                } else {
                    ops.append(&mut base_ops);
                }

                // 2. Convert temporal clause → TemporalContext + ops
                let temporal_context = if let Some(ref temporal_clause) = temporal {
                    Some(self.convert_temporal(temporal_clause, &mut ops)?)
                } else {
                    None
                };

                // 3. Interleave WITH clauses (→ Filter ops for WITH WHERE) and
                //    OPTIONAL MATCH clauses (→ Optional ops) in source order.
                let mut with_idx = 0;
                for opt_match in &optional_matches {
                    while with_idx < opt_match.preceding_withs && with_idx < with_clauses.len() {
                        self.convert_with_clause(&with_clauses[with_idx], &mut ops)?;
                        with_idx += 1;
                    }
                    let mut sub_ops = Vec::new();
                    for pat in &opt_match.pattern {
                        self.convert_optional_pattern(pat, &mut sub_ops)?;
                    }
                    if let Some(ref where_expr) = opt_match.where_clause {
                        // Per openCypher, the clause's WHERE is part of the
                        // optional pattern: it must run before the
                        // matched/unmatched decision, hence inside the op.
                        let predicate = self.convert_expr_to_predicate(where_expr)?;
                        sub_ops.push(QueryOp::Filter(predicate));
                    }
                    ops.push(QueryOp::Optional { ops: sub_ops });
                }
                while with_idx < with_clauses.len() {
                    self.convert_with_clause(&with_clauses[with_idx], &mut ops)?;
                    with_idx += 1;
                }

                // 4. Convert RETURN clause modifiers
                if return_clause.distinct {
                    ops.push(QueryOp::Distinct);
                }

                // 4b. Check for aggregation functions in RETURN items
                for item in &return_clause.items {
                    if let CypherReturnItem::Expression {
                        expr: CypherExpr::FunctionCall { name, .. },
                        ..
                    } = item
                        && name.eq_ignore_ascii_case("count")
                    {
                        ops.push(QueryOp::Count);
                    }
                }

                // Check if ORDER BY contains a vector function call that should
                // be converted to RankBySimilarity instead of Sort.
                let vector_rank_emitted = self.try_emit_vector_rank(&return_clause, &mut ops)?;

                if !vector_rank_emitted {
                    for order_item in &return_clause.order_by {
                        let sort_key = self.convert_order_item_to_sort_key(order_item)?;
                        ops.push(QueryOp::Sort {
                            key: sort_key,
                            descending: order_item.descending,
                        });
                    }
                }

                if let Some(skip) = return_clause.skip {
                    ops.push(QueryOp::Skip(skip));
                }

                if let Some(limit) = return_clause.limit {
                    // If we already emitted a RankBySimilarity (which includes top_k),
                    // still emit the Limit for the general pipeline.
                    ops.push(QueryOp::Limit(limit));
                }

                Ok(Query {
                    ops,
                    temporal_context,
                    hints: QueryHints::default(),
                })
            }
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
    /// Limitation: like the rest of the converter, this performs no variable
    /// binding analysis -- an OPTIONAL MATCH pattern starting from a variable
    /// not bound by the preceding pipeline is treated positionally as if it
    /// referred to the current row.
    fn convert_optional_pattern(
        &self,
        pattern: &CypherPattern,
        ops: &mut Vec<QueryOp>,
    ) -> Result<(), CypherError> {
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

    /// Convert a `WITH` clause into query operations (currently only its
    /// `WHERE` sub-clause produces a `Filter`; projections are not lowered).
    fn convert_with_clause(
        &self,
        with: &CypherWith,
        ops: &mut Vec<QueryOp>,
    ) -> Result<(), CypherError> {
        if let Some(ref where_expr) = with.where_clause {
            let predicate = self.convert_expr_to_predicate(where_expr)?;
            ops.push(QueryOp::Filter(predicate));
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
        match expr {
            CypherExpr::Comparison { left, op, right } => self.convert_comparison(left, *op, right),
            CypherExpr::And(left, right) => {
                let left_pred = self.convert_expr_to_predicate(left)?;
                let right_pred = self.convert_expr_to_predicate(right)?;
                Ok(Predicate::And(vec![left_pred, right_pred]))
            }
            CypherExpr::Or(left, right) => {
                let left_pred = self.convert_expr_to_predicate(left)?;
                let right_pred = self.convert_expr_to_predicate(right)?;
                Ok(Predicate::Or(vec![left_pred, right_pred]))
            }
            CypherExpr::Not(inner) => {
                let inner_pred = self.convert_expr_to_predicate(inner)?;
                Ok(Predicate::Not(Box::new(inner_pred)))
            }
            CypherExpr::IsNull(inner) => {
                let key = self.extract_property_key(inner)?;
                Ok(Predicate::Eq {
                    key,
                    value: PredicateValue::Null,
                })
            }
            CypherExpr::IsNotNull(inner) => {
                let key = self.extract_property_key(inner)?;
                Ok(Predicate::Ne {
                    key,
                    value: PredicateValue::Null,
                })
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
            CypherExpr::Grouped(inner) => self.convert_expr_to_predicate(inner),
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
    // ORDER BY helpers
    // =======================================================================

    /// Convert an `ORDER BY` item into a [`SortKey`].
    ///
    /// # Errors
    ///
    /// Returns [`CypherError::UnsupportedFeature`] if the expression is not a
    /// property access or variable reference.
    fn convert_order_item_to_sort_key(
        &self,
        item: &CypherOrderItem,
    ) -> Result<SortKey, CypherError> {
        match &item.expr {
            CypherExpr::Property { property, .. } => Ok(SortKey::Property(property.clone())),
            CypherExpr::Variable(name) => Ok(SortKey::Property(name.clone())),
            _ => Err(CypherError::UnsupportedFeature(format!(
                "unsupported expression in ORDER BY clause: {:?}",
                item.expr
            ))),
        }
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

        let order_item = &return_clause.order_by[0];
        if let CypherExpr::FunctionCall { name, args } = &order_item.expr
            && is_vector_function(name)
        {
            let (property_key, embedding) = self.extract_vector_args(args)?;
            let top_k = return_clause.limit;
            ops.push(QueryOp::RankBySimilarity {
                embedding,
                top_k,
                property_key: Some(property_key),
            });
            return Ok(true);
        }

        // Also check if the ORDER BY references an alias that was defined as a
        // vector function in the RETURN items (e.g., `vector.cosine(...) AS score`
        // then `ORDER BY score DESC`).
        if let CypherExpr::Variable(ref alias_name) = order_item.expr {
            for item in &return_clause.items {
                if let CypherReturnItem::Expression {
                    expr: CypherExpr::FunctionCall { name, args },
                    alias: Some(alias),
                } = item
                    && alias == alias_name
                    && is_vector_function(name)
                {
                    let (property_key, embedding) = self.extract_vector_args(args)?;
                    let top_k = return_clause.limit;
                    ops.push(QueryOp::RankBySimilarity {
                        embedding,
                        top_k,
                        property_key: Some(property_key),
                    });
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Extract the property key and embedding from vector function arguments.
    ///
    /// Expected args: `(entity.property, $param)` or `(entity.property, [...])`
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

        // Second arg: parameter reference or vector literal
        let embedding = match &args[1] {
            CypherExpr::Value(CypherValue::Parameter(param_name)) => {
                let param = self.params.get(param_name).ok_or_else(|| {
                    CypherError::ParameterError(format!("unbound parameter: ${param_name}"))
                })?;
                match param {
                    CypherParameterValue::Embedding(emb) => Arc::clone(emb),
                    _ => {
                        return Err(CypherError::ParameterError(format!(
                            "parameter ${param_name} must be an Embedding, got: {param:?}"
                        )));
                    }
                }
            }
            CypherExpr::Value(CypherValue::Vector(v)) => Arc::clone(v),
            _ => {
                return Err(CypherError::SemanticError(
                    "second argument to vector function must be a parameter or vector literal"
                        .to_string(),
                ));
            }
        };

        Ok((property_key, embedding))
    }
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
        "vector.similarity" | "vector.cosine" | "vector.euclidean"
    )
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
