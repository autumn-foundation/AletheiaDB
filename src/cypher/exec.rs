//! Standalone Cypher `UNWIND` planning and execution (Issue #559).
//!
//! `UNWIND <list> AS <var>` expands a list value into one row per element. The
//! resulting rows are *scalar* rows (a named value per element), which have no
//! representation in AletheiaDB's entity-oriented [`Query`] IR. Rather than
//! forcing list expansion through the graph executor, a **standalone** UNWIND
//! (one not preceded by `MATCH`/`WITH` and not feeding a subsequent graph
//! pattern) is evaluated directly here and materialized into [`QueryRow`]s via
//! [`QueryRow::from_columns`] -- the same computed-column row shape the runtime
//! aggregation path (Issue #558) uses.
//!
//! # Entry point
//!
//! [`plan_cypher`] / [`plan_cypher_with_params`] parse a Cypher string and
//! return a [`CypherExecution`]: either a [`Query`] to run through the standard
//! executor (every non-UNWIND statement) or a pre-computed [`QueryResults`]
//! stream (a standalone UNWIND). `AletheiaDB::execute_cypher` dispatches on it.
//!
//! # Supported semantics (executed correctly)
//!
//! - Source: a list literal (`[1, 2, 3]`, including nested lists), a parameter
//!   (`$list` bound to [`CypherParameterValue::List`]), or the `null` literal.
//! - `UNWIND []` and `UNWIND null` both expand to **zero** rows (openCypher).
//! - `RETURN` of the unwound variable (`RETURN x`, `RETURN x AS y`, `RETURN *`),
//!   optionally with `DISTINCT`, `ORDER BY <var>`, `SKIP`, and `LIMIT`.
//!
//! # Rejected (structured error, never answered silently wrong)
//!
//! Everything requiring per-row graph context or the entity executor is
//! rejected with a [`CypherError`]:
//!
//! - UNWIND combined with `MATCH`/`WITH`/traversal (rejected at parse time --
//!   the standalone grammar does not accept it).
//! - A source that is a scalar (`UNWIND 5`) or a row-dependent expression
//!   (`UNWIND n.tags`).
//! - `RETURN` items other than the unwound variable/`*` (properties, functions,
//!   **aggregates**, arithmetic).
//! - `ORDER BY` over a non-variable expression or over a heterogeneous /
//!   non-scalar list.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::core::error::Result as CoreResult;
use crate::core::property::PropertyValue;
use crate::query::Query;
use crate::query::executor::{QueryResults, QueryRow, ResultIterator};

use super::ast::{
    CypherExpr, CypherPattern, CypherPatternElement, CypherReturn, CypherReturnItem,
    CypherStatement, CypherValue,
};
use super::converter::{CypherConverter, CypherParameterValue};
use super::error::CypherError;
use super::parser::CypherParser;

/// Defensive recursion bound for evaluating a nested list source/element.
///
/// The parser already caps list-literal nesting at 128, so for any parsed AST
/// this never fires. It guards against a hand-built [`CypherParameterValue::List`]
/// (or `CypherExpr`) nested arbitrarily deep, which would otherwise overflow the
/// stack during evaluation. Kept above the parser's cap so it only ever rejects
/// pathological hand-built input.
const MAX_UNWIND_DEPTH: usize = 256;

/// One projected UNWIND output row: the unwound element value (retained for a
/// possible `ORDER BY`) paired with its `(column_name, value)` output columns.
type UnwindRow = (PropertyValue, Vec<(String, PropertyValue)>);

/// The outcome of planning a Cypher statement for execution.
///
/// A non-UNWIND statement lowers to a [`Query`] for the standard executor; a
/// standalone `UNWIND` is evaluated eagerly into a [`QueryResults`] stream.
pub enum CypherExecution {
    /// A graph query to run through the standard planner + executor.
    Query(Query),
    /// Pre-computed result rows (a standalone `UNWIND` expansion).
    Rows(QueryResults),
    /// A multi-variable, multi-pattern `MATCH` (Issue #549) to be evaluated by
    /// the dedicated multi-pattern evaluator, which needs the database handle.
    ///
    /// The single-entity `Query` IR cannot represent a row that binds several
    /// variables, so a statement the router classifies as multi-variable
    /// ([`needs_multi_binding`]) is carried here unconverted; `execute_cypher`
    /// dispatches it to `AletheiaDB::execute_multi_pattern`.
    MultiPattern {
        /// The `MATCH` statement to evaluate.
        statement: Box<CypherStatement>,
        /// Parameter bindings for `$param` references.
        params: HashMap<String, CypherParameterValue>,
    },
}

/// Parse and plan a Cypher string with no bound parameters.
///
/// # Errors
///
/// Returns [`CypherError`] on lex, parse, conversion, or UNWIND-evaluation
/// errors.
pub fn plan_cypher(input: &str) -> Result<CypherExecution, CypherError> {
    plan_cypher_with_params(input, HashMap::new())
}

/// Parse and plan a Cypher string with parameter bindings.
///
/// A standalone `UNWIND` statement is evaluated here and returned as
/// [`CypherExecution::Rows`]; every other statement is converted to a
/// [`Query`] and returned as [`CypherExecution::Query`].
///
/// # Errors
///
/// Returns [`CypherError`] on lex, parse, conversion, missing-parameter, or
/// UNWIND-evaluation errors.
pub fn plan_cypher_with_params(
    input: &str,
    params: HashMap<String, CypherParameterValue>,
) -> Result<CypherExecution, CypherError> {
    let stmt = CypherParser::parse(input)?;
    match stmt {
        CypherStatement::Unwind {
            source,
            variable,
            return_clause,
        } => {
            let results = execute_unwind(&source, &variable, &return_clause, &params)?;
            Ok(CypherExecution::Rows(results))
        }
        other if needs_multi_binding(&other) => {
            // A multi-variable / multi-pattern MATCH has no faithful
            // single-entity `Query` representation; route it to the dedicated
            // evaluator (which needs the DB handle) instead of converting.
            Ok(CypherExecution::MultiPattern {
                statement: Box::new(other),
                params,
            })
        }
        other => {
            let query = CypherConverter::with_params(params).convert(other)?;
            Ok(CypherExecution::Query(query))
        }
    }
}

/// Classify whether a statement requires multi-variable binding and must be
/// routed to the multi-pattern evaluator (Issue #549) rather than the
/// single-entity `Query` pipeline.
///
/// Returns `true` only for a base `MATCH` (no `OPTIONAL MATCH`, no `WITH`) that
/// either joins more than one comma-separated pattern, returns more than one
/// entity variable, or references a non-terminal entity variable -- exactly the
/// cases the single-entity row model answers incorrectly. Every query that the
/// old path already answers correctly (single terminal variable, `RETURN
/// n.name`, `count(*)`, a vector-ranked `RETURN d, score`) stays `false` and is
/// untouched.
///
/// `OPTIONAL MATCH` / `WITH` statements stay on the existing path (which has its
/// own handling for them); a multi-variable query combined with those clauses is
/// out of the v1 evaluator's scope and the evaluator rejects it if it is ever
/// reached.
pub fn needs_multi_binding(stmt: &CypherStatement) -> bool {
    let CypherStatement::Match {
        optional,
        pattern,
        where_clause,
        return_clause,
        with_clauses,
        optional_matches,
        ..
    } = stmt
    else {
        return false;
    };
    if *optional || !with_clauses.is_empty() || !optional_matches.is_empty() {
        return false;
    }

    // Entity (node + relationship) variables declared across all patterns.
    let mut entity_vars: HashSet<String> = HashSet::new();
    for pattern in pattern {
        collect_pattern_entity_vars(pattern, &mut entity_vars);
    }
    if entity_vars.is_empty() {
        return false;
    }

    // Entity variables referenced by RETURN / WHERE / ORDER BY.
    let mut referenced: HashSet<String> = HashSet::new();
    for item in &return_clause.items {
        collect_return_item_refs(item, &mut referenced);
    }
    if let Some(where_expr) = where_clause {
        collect_expr_refs(where_expr, &mut referenced);
    }
    for order_item in &return_clause.order_by {
        collect_expr_refs(&order_item.expr, &mut referenced);
    }
    referenced.retain(|v| entity_vars.contains(v));

    let terminal = pattern.last().and_then(last_node_variable);

    pattern.len() > 1
        || referenced.len() > 1
        || referenced
            .iter()
            .any(|v| Some(v.as_str()) != terminal.as_deref())
}

/// Collect the node and relationship variable names of one pattern.
fn collect_pattern_entity_vars(pattern: &CypherPattern, out: &mut HashSet<String>) {
    for element in &pattern.elements {
        let var = match element {
            CypherPatternElement::Node(n) => n.variable.as_ref(),
            CypherPatternElement::Relationship(r) => r.variable.as_ref(),
        };
        if let Some(v) = var {
            out.insert(v.clone());
        }
    }
}

/// The variable of the last node element of a pattern, if named.
fn last_node_variable(pattern: &CypherPattern) -> Option<String> {
    pattern.elements.iter().rev().find_map(|element| match element {
        CypherPatternElement::Node(n) => n.variable.clone(),
        CypherPatternElement::Relationship(_) => None,
    })
}

/// Collect variable references from a `RETURN` item.
fn collect_return_item_refs(item: &CypherReturnItem, out: &mut HashSet<String>) {
    match item {
        CypherReturnItem::Star => {}
        CypherReturnItem::Variable(name) => {
            out.insert(name.clone());
        }
        CypherReturnItem::Expression { expr, .. } => collect_expr_refs(expr, out),
    }
}

/// Collect the variable names an expression references (bare variables and the
/// base of a property access), recursing through sub-expressions. Parser-bounded
/// nesting (Issue #3404) makes unbounded recursion safe here.
fn collect_expr_refs(expr: &CypherExpr, out: &mut HashSet<String>) {
    match expr {
        CypherExpr::Variable(name) => {
            out.insert(name.clone());
        }
        CypherExpr::Property { variable, .. } => {
            out.insert(variable.clone());
        }
        CypherExpr::Value(_) | CypherExpr::Star => {}
        CypherExpr::Comparison { left, right, .. } => {
            collect_expr_refs(left, out);
            collect_expr_refs(right, out);
        }
        CypherExpr::And(a, b) | CypherExpr::Or(a, b) => {
            collect_expr_refs(a, out);
            collect_expr_refs(b, out);
        }
        CypherExpr::Not(inner)
        | CypherExpr::IsNull(inner)
        | CypherExpr::IsNotNull(inner)
        | CypherExpr::Grouped(inner) => collect_expr_refs(inner, out),
        CypherExpr::In { expr, values } => {
            collect_expr_refs(expr, out);
            for value in values {
                collect_expr_refs(value, out);
            }
        }
        CypherExpr::Contains { expr, .. }
        | CypherExpr::StartsWith { expr, .. }
        | CypherExpr::EndsWith { expr, .. } => collect_expr_refs(expr, out),
        CypherExpr::FunctionCall { args, .. } | CypherExpr::List(args) => {
            for arg in args {
                collect_expr_refs(arg, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// UNWIND runtime
// ---------------------------------------------------------------------------

/// A vector-backed [`ResultIterator`] wrapping pre-computed rows.
struct VecResultIterator {
    rows: std::vec::IntoIter<QueryRow>,
}

impl VecResultIterator {
    fn new(rows: Vec<QueryRow>) -> Self {
        Self {
            rows: rows.into_iter(),
        }
    }
}

impl ResultIterator for VecResultIterator {
    fn next(&mut self) -> Option<CoreResult<QueryRow>> {
        self.rows.next().map(Ok)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.rows.len();
        (n, Some(n))
    }
}

/// Execute a standalone `UNWIND <source> AS <variable> <return_clause>`.
fn execute_unwind(
    source: &CypherExpr,
    variable: &str,
    ret: &CypherReturn,
    params: &HashMap<String, CypherParameterValue>,
) -> Result<QueryResults, CypherError> {
    // Validate the projection and ordering statically (independent of the list
    // contents) so an unsupported RETURN/ORDER BY is rejected, not mis-answered.
    validate_return_items(ret, variable)?;
    let needs_order = validate_order_by(ret, variable)?;

    // Evaluate the source into its list of element values (empty for `[]` /
    // `null`).
    let elements = unwind_source_values(source, variable, params)?;

    // Project each element into its output columns, keeping the element value
    // alongside for a possible ORDER BY.
    let mut projected: Vec<UnwindRow> = Vec::with_capacity(elements.len());
    for value in elements {
        let columns = project_element(ret, variable, &value);
        projected.push((value, columns));
    }

    // RETURN DISTINCT: deduplicate by the output column tuple, first occurrence
    // wins.
    if ret.distinct {
        projected = distinct_rows(projected);
    }

    // ORDER BY <var>: sort by the unwound value.
    if needs_order {
        let descending = ret.order_by[0].descending;
        order_rows(&mut projected, descending)?;
    }

    // SKIP / LIMIT paginate the final rows. An absent LIMIT is an unbounded
    // `take`, collapsing the Some/None arms into one pipeline.
    let skip = ret.skip.unwrap_or(0);
    let rows: Vec<QueryRow> = projected
        .into_iter()
        .skip(skip)
        .take(ret.limit.unwrap_or(usize::MAX))
        .map(|(_, columns)| QueryRow::from_columns(columns))
        .collect();

    Ok(QueryResults::new(Box::new(VecResultIterator::new(rows))))
}

/// Resolve the `UNWIND` source expression into its list of element values.
///
/// `[]` and `null` (literal or parameter) yield an empty list. A scalar or
/// row-dependent source is rejected.
fn unwind_source_values(
    source: &CypherExpr,
    variable: &str,
    params: &HashMap<String, CypherParameterValue>,
) -> Result<Vec<PropertyValue>, CypherError> {
    match source {
        // `UNWIND null AS x` -> zero rows (openCypher).
        CypherExpr::Value(CypherValue::Null) => Ok(Vec::new()),
        // `UNWIND [ ... ] AS x`
        CypherExpr::List(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for element in elements {
                out.push(eval_element(element, params, 0)?);
            }
            Ok(out)
        }
        // `UNWIND $list AS x`
        CypherExpr::Value(CypherValue::Parameter(name)) => {
            let param = params.get(name).ok_or_else(|| {
                CypherError::ParameterError(format!("unbound parameter: ${name}"))
            })?;
            match param {
                CypherParameterValue::Null => Ok(Vec::new()),
                CypherParameterValue::List(items) => {
                    let mut out = Vec::with_capacity(items.len());
                    for item in items {
                        out.push(param_to_property(item, 0)?);
                    }
                    Ok(out)
                }
                // A dense vector / embedding parameter is a valid list source:
                // the MCP layer coerces a bare numeric array (`$arr = [1,2,3]`)
                // to `Embedding`, so `UNWIND $arr AS x` must behave like the
                // identical list literal rather than being rejected. Each
                // component becomes one `Float` row.
                CypherParameterValue::Embedding(e) => {
                    Ok(e.iter().map(|c| PropertyValue::Float(*c as f64)).collect())
                }
                other => Err(CypherError::SemanticError(format!(
                    "UNWIND requires a list or null, but parameter ${name} is a \
                     scalar ({other:?}); bind a list value or wrap it, e.g. \
                     UNWIND [$val] AS {variable}"
                ))),
            }
        }
        CypherExpr::Grouped(inner) => unwind_source_values(inner, variable, params),
        // A scalar literal (`UNWIND 5`) or a row-dependent expression
        // (`UNWIND n.tags`) cannot be expanded by a standalone UNWIND.
        other => Err(CypherError::SemanticError(format!(
            "UNWIND requires a list literal, a list parameter, or null; got a \
             scalar or row-dependent expression ({other:?}). Wrap scalars in a \
             list (e.g. UNWIND [5] AS {variable}); list-valued node/edge \
             properties require an UNWIND fed by a preceding MATCH, which is not \
             yet supported"
        ))),
    }
}

/// Evaluate a single list-element expression into a [`PropertyValue`].
fn eval_element(
    expr: &CypherExpr,
    params: &HashMap<String, CypherParameterValue>,
    depth: usize,
) -> Result<PropertyValue, CypherError> {
    if depth > MAX_UNWIND_DEPTH {
        return Err(CypherError::ParseError {
            position: 0,
            message: "UNWIND list nesting too deep for evaluation (max 256)".to_string(),
        });
    }
    match expr {
        CypherExpr::Value(value) => value_to_property(value, params),
        CypherExpr::List(elements) => {
            let mut out = Vec::with_capacity(elements.len());
            for element in elements {
                out.push(eval_element(element, params, depth + 1)?);
            }
            Ok(PropertyValue::Array(Arc::new(out)))
        }
        CypherExpr::Grouped(inner) => eval_element(inner, params, depth + 1),
        other => Err(CypherError::UnsupportedFeature(format!(
            "UNWIND list element must be a literal, parameter, or nested list; \
             got a row-dependent expression ({other:?}) that a standalone UNWIND \
             cannot evaluate"
        ))),
    }
}

/// Convert a Cypher literal value (resolving parameters) into a [`PropertyValue`].
fn value_to_property(
    value: &CypherValue,
    params: &HashMap<String, CypherParameterValue>,
) -> Result<PropertyValue, CypherError> {
    match value {
        CypherValue::Null => Ok(PropertyValue::Null),
        CypherValue::Bool(b) => Ok(PropertyValue::Bool(*b)),
        CypherValue::Int(n) => Ok(PropertyValue::Int(*n)),
        CypherValue::Float(f) => Ok(PropertyValue::Float(*f)),
        CypherValue::String(s) => Ok(PropertyValue::String(Arc::from(s.as_str()))),
        CypherValue::Vector(v) => Ok(PropertyValue::Vector(Arc::clone(v))),
        CypherValue::Parameter(name) => {
            let param = params.get(name).ok_or_else(|| {
                CypherError::ParameterError(format!("unbound parameter: ${name}"))
            })?;
            param_to_property(param, 0)
        }
    }
}

/// Convert a runtime parameter value into a [`PropertyValue`].
fn param_to_property(
    param: &CypherParameterValue,
    depth: usize,
) -> Result<PropertyValue, CypherError> {
    if depth > MAX_UNWIND_DEPTH {
        return Err(CypherError::ParseError {
            position: 0,
            message: "UNWIND list parameter nesting too deep for evaluation (max 256)".to_string(),
        });
    }
    Ok(match param {
        CypherParameterValue::Null => PropertyValue::Null,
        CypherParameterValue::Bool(b) => PropertyValue::Bool(*b),
        CypherParameterValue::Int(n) => PropertyValue::Int(*n),
        CypherParameterValue::Float(f) => PropertyValue::Float(*f),
        CypherParameterValue::String(s) => PropertyValue::String(Arc::from(s.as_str())),
        CypherParameterValue::Embedding(e) => PropertyValue::Vector(Arc::clone(e)),
        CypherParameterValue::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(param_to_property(item, depth + 1)?);
            }
            PropertyValue::Array(Arc::new(out))
        }
    })
}

// ---------------------------------------------------------------------------
// RETURN projection + validation
// ---------------------------------------------------------------------------

/// Validate that every `RETURN` item is supported by the standalone UNWIND
/// runtime: the unwound variable (bare or aliased) or `*`. Anything else --
/// aggregates, functions, properties, other variables -- is rejected.
fn validate_return_items(ret: &CypherReturn, variable: &str) -> Result<(), CypherError> {
    for item in &ret.items {
        match item {
            CypherReturnItem::Star => {}
            CypherReturnItem::Variable(name) => {
                if name != variable {
                    return Err(unsupported_return_variable(name, variable));
                }
            }
            CypherReturnItem::Expression { expr, .. } => match expr {
                CypherExpr::Variable(name) if name == variable => {}
                CypherExpr::Variable(name) => {
                    return Err(unsupported_return_variable(name, variable));
                }
                CypherExpr::FunctionCall { name, .. } if is_aggregate_name(name) => {
                    return Err(CypherError::UnsupportedFeature(format!(
                        "aggregation ({name}) over an UNWIND result is not \
                         supported by the standalone UNWIND runtime; RETURN the \
                         unwound variable '{variable}' directly"
                    )));
                }
                other => {
                    return Err(CypherError::UnsupportedFeature(format!(
                        "RETURN item {other:?} is not supported after a standalone \
                         UNWIND; only the unwound variable '{variable}' (optionally \
                         aliased) or '*' may be returned"
                    )));
                }
            },
        }
    }
    Ok(())
}

/// Build the error for a RETURN item naming a variable other than the unwound
/// one (a standalone UNWIND binds only its own variable).
fn unsupported_return_variable(name: &str, variable: &str) -> CypherError {
    CypherError::UnsupportedFeature(format!(
        "RETURN references variable '{name}', but a standalone UNWIND only binds \
         the unwound variable '{variable}'"
    ))
}

/// Validate the `ORDER BY` clause (if any) references the unwound value, either
/// by the variable name or by an alias projecting it. Returns whether an
/// ordering pass is needed.
fn validate_order_by(ret: &CypherReturn, variable: &str) -> Result<bool, CypherError> {
    if ret.order_by.is_empty() {
        return Ok(false);
    }
    for order_item in &ret.order_by {
        let CypherExpr::Variable(name) = &order_item.expr else {
            return Err(CypherError::UnsupportedFeature(format!(
                "ORDER BY {:?} is not supported after a standalone UNWIND; order \
                 by the unwound variable '{variable}'",
                order_item.expr
            )));
        };
        if name != variable && !alias_projects_variable(ret, name, variable) {
            return Err(CypherError::UnsupportedFeature(format!(
                "ORDER BY '{name}' is not supported after a standalone UNWIND; \
                 order by the unwound variable '{variable}' (or an alias of it)"
            )));
        }
    }
    Ok(true)
}

/// Returns `true` if some `RETURN` item projects the unwound variable under the
/// alias `alias`.
fn alias_projects_variable(ret: &CypherReturn, alias: &str, variable: &str) -> bool {
    ret.items.iter().any(|item| {
        matches!(
            item,
            CypherReturnItem::Expression {
                expr: CypherExpr::Variable(v),
                alias: Some(a),
            } if v == variable && a == alias
        )
    })
}

/// Project one unwound element value into its `RETURN` output columns.
///
/// Preconditions (guaranteed by [`validate_return_items`]): every item is `*`,
/// the bare unwound variable, or an expression naming the unwound variable.
fn project_element(
    ret: &CypherReturn,
    variable: &str,
    value: &PropertyValue,
) -> Vec<(String, PropertyValue)> {
    let mut columns = Vec::with_capacity(ret.items.len());
    for item in &ret.items {
        match item {
            CypherReturnItem::Star => {
                columns.push((variable.to_string(), value.clone()));
            }
            CypherReturnItem::Variable(name) => {
                columns.push((name.clone(), value.clone()));
            }
            CypherReturnItem::Expression { alias, .. } => {
                let column_name = alias.clone().unwrap_or_else(|| variable.to_string());
                columns.push((column_name, value.clone()));
            }
        }
    }
    columns
}

// ---------------------------------------------------------------------------
// DISTINCT + ORDER BY helpers
// ---------------------------------------------------------------------------

/// Deduplicate projected rows by their output column tuple (first wins).
///
/// Equality is `PropertyValue`'s, so `Int(1)` and `Float(1.0)` are treated as
/// *distinct* rows -- an intentional divergence from openCypher numeric
/// coercion, consistent with AletheiaDB's DB-wide `PropertyValue` equality.
fn distinct_rows(rows: Vec<UnwindRow>) -> Vec<UnwindRow> {
    let mut out: Vec<UnwindRow> = Vec::with_capacity(rows.len());
    for (value, columns) in rows {
        if out.iter().any(|(_, existing)| existing == &columns) {
            continue;
        }
        out.push((value, columns));
    }
    out
}

/// Sort projected rows by their unwound value.
///
/// Only homogeneous scalar lists (all numeric, all string, or all boolean --
/// `null`s allowed) are orderable; mixed or non-scalar element types are
/// rejected. openCypher null placement is honored: nulls last for ascending,
/// first for descending.
fn order_rows(rows: &mut Vec<UnwindRow>, descending: bool) -> Result<(), CypherError> {
    let kind = order_kind(rows)?;

    let (mut nulls, mut non_nulls): (Vec<UnwindRow>, Vec<UnwindRow>) = std::mem::take(rows)
        .into_iter()
        .partition(|(value, _)| matches!(value, PropertyValue::Null));

    // Sort directly in the requested direction (a stable sort followed by
    // `reverse()` would invert the order of equal-key rows).
    non_nulls.sort_by(|(a, _), (b, _)| {
        if descending {
            compare_scalar(b, a, kind)
        } else {
            compare_scalar(a, b, kind)
        }
    });

    if descending {
        // nulls first, then descending values.
        nulls.append(&mut non_nulls);
        *rows = nulls;
    } else {
        // ascending values, then nulls last.
        non_nulls.append(&mut nulls);
        *rows = non_nulls;
    }
    Ok(())
}

/// The scalar kind an `ORDER BY` operates over.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OrderKind {
    Numeric,
    Text,
    Boolean,
}

/// Determine (and validate homogeneity of) the orderable scalar kind of a set
/// of unwound values, ignoring `null`s.
fn order_kind(rows: &[UnwindRow]) -> Result<OrderKind, CypherError> {
    let mut kind: Option<OrderKind> = None;
    for (value, _) in rows {
        let this = match value {
            PropertyValue::Null => continue,
            PropertyValue::Int(_) | PropertyValue::Float(_) => OrderKind::Numeric,
            PropertyValue::String(_) => OrderKind::Text,
            PropertyValue::Bool(_) => OrderKind::Boolean,
            other => {
                return Err(CypherError::UnsupportedFeature(format!(
                    "ORDER BY over an UNWIND list supports only scalar values \
                     (int/float/string/bool); got {other:?}"
                )));
            }
        };
        match kind {
            None => kind = Some(this),
            Some(existing) if existing == this => {}
            Some(_) => {
                return Err(CypherError::UnsupportedFeature(
                    "ORDER BY over an UNWIND list requires homogeneous scalar \
                     element types (all numeric, all string, or all boolean)"
                        .to_string(),
                ));
            }
        }
    }
    // An all-null (or empty) set has no meaningful kind; Numeric is a harmless
    // placeholder since `compare_scalar` is never invoked on nulls.
    Ok(kind.unwrap_or(OrderKind::Numeric))
}

/// Compare two non-null scalar [`PropertyValue`]s of the given kind.
fn compare_scalar(a: &PropertyValue, b: &PropertyValue, kind: OrderKind) -> Ordering {
    match kind {
        // Compare same-typed numerics natively: `i64 -> f64` loses precision
        // above 2^53, collapsing distinct large integers (nanosecond epoch
        // timestamps, Snowflake IDs) to spurious ties and mis-ordering them.
        // Only genuinely mixed Int/Float pairs fall back to `f64` coercion.
        OrderKind::Numeric => match (a, b) {
            (PropertyValue::Int(x), PropertyValue::Int(y)) => x.cmp(y),
            (PropertyValue::Float(x), PropertyValue::Float(y)) => x.total_cmp(y),
            _ => scalar_f64(a).total_cmp(&scalar_f64(b)),
        },
        OrderKind::Text => match (a, b) {
            (PropertyValue::String(x), PropertyValue::String(y)) => x.as_ref().cmp(y.as_ref()),
            _ => Ordering::Equal,
        },
        OrderKind::Boolean => match (a, b) {
            (PropertyValue::Bool(x), PropertyValue::Bool(y)) => x.cmp(y),
            _ => Ordering::Equal,
        },
    }
}

/// Interpret a numeric [`PropertyValue`] as `f64` for ordering.
fn scalar_f64(value: &PropertyValue) -> f64 {
    match value {
        PropertyValue::Int(n) => *n as f64,
        PropertyValue::Float(f) => *f,
        _ => 0.0,
    }
}

/// Returns `true` if `name` is a case-insensitive aggregate function name.
fn is_aggregate_name(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "COLLECT"
    )
}
