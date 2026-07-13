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
    /// `EXPLAIN <query>` (Issue #562): plan the wrapped query and return the
    /// physical plan as a single `plan` row **without executing it**. The
    /// caller (`AletheiaDB::execute_cypher`) plans only.
    Explain(Query),
    /// `PROFILE <query>` (Issue #562): execute the wrapped query with
    /// per-operator instrumentation and return the annotated plan as a single
    /// `plan` row.
    Profile(Query),
    /// A write statement (Issue #560): `CREATE` / `SET` / `DELETE` /
    /// `DETACH DELETE`, optionally with a reading `MATCH` and a `RETURN`.
    ///
    /// Carried unconverted (writes do not lower to the read-only [`Query`] IR)
    /// and dispatched to `AletheiaDB::execute_mutation`, which needs the database
    /// handle to apply the mutation through the native write APIs.
    Mutation {
        /// The parsed write statement to execute.
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
        CypherStatement::Explain(inner) => {
            let query = lower_for_plan(*inner, params, "EXPLAIN")?;
            Ok(CypherExecution::Explain(query))
        }
        CypherStatement::Profile(inner) => {
            let query = lower_for_plan(*inner, params, "PROFILE")?;
            Ok(CypherExecution::Profile(query))
        }
        stmt @ CypherStatement::Write(_) => {
            // A write statement is executed directly against the native write
            // APIs (see `crate::cypher::mutation`); it has no read-only `Query`
            // representation, so it is carried unconverted.
            Ok(CypherExecution::Mutation {
                statement: Box::new(stmt),
                params,
            })
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

/// Lower the statement wrapped by `EXPLAIN`/`PROFILE` into a plannable [`Query`].
///
/// Only statements that convert to the single-entity graph-query IR are
/// supported. Two inner shapes have no `Query` representation and are rejected
/// with an honest [`CypherError::UnsupportedFeature`] rather than fabricating a
/// plan:
///
/// - A standalone `UNWIND` (evaluated directly into scalar rows).
/// - A multi-variable / multi-pattern `MATCH` (Issue #549): classified by
///   [`needs_multi_binding`], it routes to the dedicated multi-pattern
///   evaluator ([`CypherExecution::MultiPattern`]), which has **no** physical
///   `Query`/plan to display or profile. Explaining/profiling it would either
///   mis-lower through the single-entity converter or produce a wrong/empty
///   plan, so we reject it explicitly and cleanly.
///
/// A nested `EXPLAIN`/`PROFILE` prefix is unreachable (the parser rejects it)
/// but is handled defensively.
fn lower_for_plan(
    inner: CypherStatement,
    params: HashMap<String, CypherParameterValue>,
    verb: &str,
) -> Result<Query, CypherError> {
    match inner {
        CypherStatement::Unwind { .. } => Err(CypherError::UnsupportedFeature(format!(
            "{verb} is not supported for a standalone UNWIND statement yet; UNWIND expands a list \
             into scalar rows and does not lower to a plannable graph query"
        ))),
        CypherStatement::Explain(_) | CypherStatement::Profile(_) => Err(
            CypherError::UnsupportedFeature(format!("{verb} cannot wrap another EXPLAIN/PROFILE")),
        ),
        // A write statement has no read-only `Query` plan to display or profile,
        // and EXPLAIN/PROFILE must never execute a mutation as a side effect, so
        // reject rather than mis-lower.
        CypherStatement::Write(_) => Err(CypherError::UnsupportedFeature(format!(
            "{verb} is not supported for write statements (CREATE / SET / DELETE); \
             EXPLAIN/PROFILE describe read-only query plans"
        ))),
        // A multi-variable / multi-pattern MATCH (Issue #549) is dispatched to
        // the dedicated evaluator, which has no physical `Query` plan; there is
        // nothing to lower here, so reject rather than mis-lower.
        stmt if needs_multi_binding(&stmt) => Err(CypherError::UnsupportedFeature(format!(
            "{verb} is not supported for multi-pattern queries yet; a multi-variable / \
             multi-pattern MATCH is evaluated by the dedicated multi-pattern evaluator and has \
             no physical query plan to display"
        ))),
        stmt => CypherConverter::with_params(params).convert(stmt),
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
/// A single-terminal-variable query -- including a single-pattern `WITH` /
/// `OPTIONAL MATCH` query the existing path handles correctly -- stays `false`
/// and is untouched. But a genuinely multi-variable query IS routed to the
/// evaluator even when combined with `WITH` / `OPTIONAL MATCH` (FIX #5): those
/// clauses are out of the v1 evaluator's scope, so it rejects them with a
/// structured error rather than letting the single-entity path silently return
/// a wrong row.
pub fn needs_multi_binding(stmt: &CypherStatement) -> bool {
    let CypherStatement::Match {
        pattern,
        where_clause,
        return_clause,
        ..
    } = stmt
    else {
        return false;
    };

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

    // The multi-binding decision is made from the pattern shape and the
    // referenced variables ALONE -- the `WITH` / `OPTIONAL MATCH` clauses are
    // deliberately NOT consulted here (FIX #5). When a query genuinely binds
    // more than one entity variable (or joins comma-separated patterns), it
    // must route to the multi-pattern evaluator EVEN if `WITH`/`OPTIONAL MATCH`
    // is present: the evaluator then rejects those clauses with a structured
    // `UnsupportedFeature` error, rather than the single-entity path silently
    // discarding an earlier scan and returning a wrong row. A genuine
    // single-terminal-variable query (including single-pattern `WITH` /
    // `OPTIONAL MATCH` queries the old path handles correctly) stays `false`.
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

/// The variable of the **last node element** of a pattern, or `None` when that
/// terminal node is unnamed (FIX #4).
///
/// This must return the last node's `.variable` directly -- `None` if the last
/// node is anonymous -- rather than skipping past an unnamed terminal to an
/// earlier named node. Skipping would make `MATCH (a)-[:R]->()` report a
/// terminal of `a`, wrongly classifying it as single-terminal and routing it to
/// the single-entity path (which returns the anonymous endpoint, not `a`).
/// Mirrors `converter::last_node_variable`.
fn last_node_variable(pattern: &CypherPattern) -> Option<String> {
    pattern
        .elements
        .iter()
        .rev()
        .find_map(|element| match element {
            CypherPatternElement::Node(n) => Some(n.variable.clone()),
            CypherPatternElement::Relationship(_) => None,
        })
        .flatten()
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

#[cfg(test)]
mod tests {
    //! Standalone-UNWIND error-path coverage (Issue #3535 mutation backfill).
    //!
    //! These tests pin the exact `Err(...)` variants/messages and `Ok(...)`
    //! values of the UNWIND planning/validation/evaluation helpers so the
    //! surviving mutants on this file's error branches (surfaced by the
    //! PR #3527 Mutants run) are killed: each test fails if the branch it
    //! targets is deleted or its return value swapped.

    use super::*;
    use crate::cypher::ast::CypherOrderItem;

    // --- helpers -----------------------------------------------------------

    fn no_params() -> HashMap<String, CypherParameterValue> {
        HashMap::new()
    }

    fn ret_var(variable: &str) -> CypherReturn {
        CypherReturn {
            distinct: false,
            items: vec![CypherReturnItem::Variable(variable.to_string())],
            order_by: vec![],
            skip: None,
            limit: None,
        }
    }

    /// Build one `UnwindRow` for the `order_kind` / `order_rows` helpers.
    fn urow(value: PropertyValue) -> UnwindRow {
        (value.clone(), vec![("x".to_string(), value)])
    }

    /// Plan a standalone UNWIND and collect the first output column of each row.
    fn run_unwind_params(
        cypher: &str,
        params: HashMap<String, CypherParameterValue>,
    ) -> Vec<PropertyValue> {
        match plan_cypher_with_params(cypher, params).expect("plan should succeed") {
            CypherExecution::Rows(results) => results
                .collect_all()
                .expect("collect should succeed")
                .into_iter()
                .map(|row| {
                    row.columns.expect("standalone UNWIND rows carry columns")[0]
                        .1
                        .clone()
                })
                .collect(),
            _ => panic!("expected CypherExecution::Rows for a standalone UNWIND"),
        }
    }

    fn run_unwind(cypher: &str) -> Vec<PropertyValue> {
        run_unwind_params(cypher, no_params())
    }

    // --- unwind_source_values ---------------------------------------------

    #[test]
    fn source_null_literal_is_empty() {
        let src = CypherExpr::Value(CypherValue::Null);
        assert!(
            unwind_source_values(&src, "x", &no_params())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn source_list_literal_yields_elements() {
        let src = CypherExpr::List(vec![
            CypherExpr::Value(CypherValue::Int(1)),
            CypherExpr::Value(CypherValue::Int(2)),
        ]);
        assert_eq!(
            unwind_source_values(&src, "x", &no_params()).unwrap(),
            vec![PropertyValue::Int(1), PropertyValue::Int(2)]
        );
    }

    #[test]
    fn source_grouped_recurses() {
        let src = CypherExpr::Grouped(Box::new(CypherExpr::List(vec![CypherExpr::Value(
            CypherValue::Int(7),
        )])));
        assert_eq!(
            unwind_source_values(&src, "x", &no_params()).unwrap(),
            vec![PropertyValue::Int(7)]
        );
    }

    #[test]
    fn source_unbound_parameter_is_parameter_error() {
        let src = CypherExpr::Value(CypherValue::Parameter("missing".to_string()));
        let err = unwind_source_values(&src, "x", &no_params()).unwrap_err();
        assert!(
            matches!(err, CypherError::ParameterError(ref m) if m == "unbound parameter: $missing"),
            "got {err:?}"
        );
    }

    #[test]
    fn source_null_parameter_is_empty() {
        let mut params = no_params();
        params.insert("p".to_string(), CypherParameterValue::Null);
        let src = CypherExpr::Value(CypherValue::Parameter("p".to_string()));
        assert!(unwind_source_values(&src, "x", &params).unwrap().is_empty());
    }

    #[test]
    fn source_list_parameter_yields_elements() {
        let mut params = no_params();
        params.insert(
            "p".to_string(),
            CypherParameterValue::List(vec![
                CypherParameterValue::Int(1),
                CypherParameterValue::Int(2),
            ]),
        );
        let src = CypherExpr::Value(CypherValue::Parameter("p".to_string()));
        assert_eq!(
            unwind_source_values(&src, "x", &params).unwrap(),
            vec![PropertyValue::Int(1), PropertyValue::Int(2)]
        );
    }

    #[test]
    fn source_embedding_parameter_expands_to_floats() {
        let mut params = no_params();
        params.insert(
            "p".to_string(),
            CypherParameterValue::Embedding(Arc::from([1.0f32, 2.0].as_slice())),
        );
        let src = CypherExpr::Value(CypherValue::Parameter("p".to_string()));
        assert_eq!(
            unwind_source_values(&src, "x", &params).unwrap(),
            vec![PropertyValue::Float(1.0), PropertyValue::Float(2.0)]
        );
    }

    #[test]
    fn source_scalar_parameter_is_semantic_error() {
        let mut params = no_params();
        params.insert("p".to_string(), CypherParameterValue::Int(5));
        let src = CypherExpr::Value(CypherValue::Parameter("p".to_string()));
        let err = unwind_source_values(&src, "x", &params).unwrap_err();
        match err {
            CypherError::SemanticError(m) => {
                assert!(m.contains("scalar"), "got {m}");
            }
            other => panic!("expected SemanticError, got {other:?}"),
        }
    }

    #[test]
    fn source_row_dependent_expression_is_semantic_error() {
        // A bare property access is neither a list nor null -> the catch-all
        // `other` arm rejects it.
        let src = CypherExpr::Property {
            variable: "n".to_string(),
            property: "tags".to_string(),
        };
        let err = unwind_source_values(&src, "x", &no_params()).unwrap_err();
        match err {
            CypherError::SemanticError(m) => {
                assert!(m.contains("scalar or row-dependent"), "got {m}");
            }
            other => panic!("expected SemanticError, got {other:?}"),
        }
    }

    // --- eval_element ------------------------------------------------------

    #[test]
    fn eval_element_depth_guard_is_parse_error() {
        let expr = CypherExpr::Value(CypherValue::Int(1));
        let err = eval_element(&expr, &no_params(), MAX_UNWIND_DEPTH + 1).unwrap_err();
        match err {
            CypherError::ParseError { message, .. } => {
                assert_eq!(
                    message,
                    "UNWIND list nesting too deep for evaluation (max 256)"
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn eval_element_value_and_nested_list() {
        assert_eq!(
            eval_element(&CypherExpr::Value(CypherValue::Int(3)), &no_params(), 0).unwrap(),
            PropertyValue::Int(3)
        );
        let nested = CypherExpr::List(vec![CypherExpr::Value(CypherValue::Int(1))]);
        let out = eval_element(&nested, &no_params(), 0).unwrap();
        assert_eq!(
            out,
            PropertyValue::Array(Arc::new(vec![PropertyValue::Int(1)]))
        );
    }

    #[test]
    fn eval_element_grouped_recurses() {
        let expr = CypherExpr::Grouped(Box::new(CypherExpr::Value(CypherValue::Int(8))));
        assert_eq!(
            eval_element(&expr, &no_params(), 0).unwrap(),
            PropertyValue::Int(8)
        );
    }

    #[test]
    fn eval_element_row_dependent_is_unsupported() {
        let expr = CypherExpr::Property {
            variable: "n".to_string(),
            property: "k".to_string(),
        };
        let err = eval_element(&expr, &no_params(), 0).unwrap_err();
        match err {
            CypherError::UnsupportedFeature(m) => {
                assert!(m.contains("row-dependent expression"), "got {m}");
            }
            other => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }

    // --- value_to_property -------------------------------------------------

    #[test]
    fn value_to_property_all_literal_variants() {
        let p = no_params();
        assert_eq!(
            value_to_property(&CypherValue::Null, &p).unwrap(),
            PropertyValue::Null
        );
        assert_eq!(
            value_to_property(&CypherValue::Bool(true), &p).unwrap(),
            PropertyValue::Bool(true)
        );
        assert_eq!(
            value_to_property(&CypherValue::Int(4), &p).unwrap(),
            PropertyValue::Int(4)
        );
        assert_eq!(
            value_to_property(&CypherValue::Float(1.5), &p).unwrap(),
            PropertyValue::Float(1.5)
        );
        assert_eq!(
            value_to_property(&CypherValue::String("hi".to_string()), &p).unwrap(),
            PropertyValue::String(Arc::from("hi"))
        );
        let vec: Arc<[f32]> = Arc::from([1.0f32, 2.0, 3.0].as_slice());
        assert_eq!(
            value_to_property(&CypherValue::Vector(Arc::clone(&vec)), &p).unwrap(),
            PropertyValue::Vector(vec)
        );
    }

    #[test]
    fn value_to_property_unbound_parameter_is_parameter_error() {
        let err =
            value_to_property(&CypherValue::Parameter("q".to_string()), &no_params()).unwrap_err();
        assert!(
            matches!(err, CypherError::ParameterError(ref m) if m == "unbound parameter: $q"),
            "got {err:?}"
        );
    }

    #[test]
    fn value_to_property_bound_parameter_resolves() {
        let mut params = no_params();
        params.insert("q".to_string(), CypherParameterValue::Int(42));
        assert_eq!(
            value_to_property(&CypherValue::Parameter("q".to_string()), &params).unwrap(),
            PropertyValue::Int(42)
        );
    }

    // --- param_to_property -------------------------------------------------

    #[test]
    fn param_to_property_depth_guard_is_parse_error() {
        let err =
            param_to_property(&CypherParameterValue::Int(1), MAX_UNWIND_DEPTH + 1).unwrap_err();
        match err {
            CypherError::ParseError { message, .. } => {
                assert_eq!(
                    message,
                    "UNWIND list parameter nesting too deep for evaluation (max 256)"
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn param_to_property_variants_and_list_recursion() {
        assert_eq!(
            param_to_property(&CypherParameterValue::Null, 0).unwrap(),
            PropertyValue::Null
        );
        assert_eq!(
            param_to_property(&CypherParameterValue::Bool(true), 0).unwrap(),
            PropertyValue::Bool(true)
        );
        assert_eq!(
            param_to_property(&CypherParameterValue::Float(2.0), 0).unwrap(),
            PropertyValue::Float(2.0)
        );
        assert_eq!(
            param_to_property(&CypherParameterValue::String("s".to_string()), 0).unwrap(),
            PropertyValue::String(Arc::from("s"))
        );
        let nested = CypherParameterValue::List(vec![CypherParameterValue::Int(9)]);
        assert_eq!(
            param_to_property(&nested, 0).unwrap(),
            PropertyValue::Array(Arc::new(vec![PropertyValue::Int(9)]))
        );
        // The Embedding arm maps to a dense Vector (distinct from the
        // list-source expansion in unwind_source_values).
        let emb: Arc<[f32]> = Arc::from([1.0f32, 2.0].as_slice());
        assert_eq!(
            param_to_property(&CypherParameterValue::Embedding(Arc::clone(&emb)), 0).unwrap(),
            PropertyValue::Vector(emb)
        );
    }

    // --- validate_return_items --------------------------------------------

    #[test]
    fn validate_return_items_accepts_star_and_variable() {
        let star = CypherReturn {
            items: vec![CypherReturnItem::Star],
            ..ret_var("x")
        };
        assert!(validate_return_items(&star, "x").is_ok());
        assert!(validate_return_items(&ret_var("x"), "x").is_ok());
    }

    #[test]
    fn validate_return_items_rejects_other_variable() {
        let err = validate_return_items(&ret_var("y"), "x").unwrap_err();
        match err {
            CypherError::UnsupportedFeature(m) => {
                assert!(m.contains("RETURN references variable 'y'"), "got {m}");
            }
            other => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }

    #[test]
    fn validate_return_items_accepts_expression_of_unwound_variable() {
        let ret = CypherReturn {
            items: vec![CypherReturnItem::Expression {
                expr: CypherExpr::Variable("x".to_string()),
                alias: Some("out".to_string()),
            }],
            ..ret_var("x")
        };
        assert!(validate_return_items(&ret, "x").is_ok());
    }

    #[test]
    fn validate_return_items_rejects_expression_of_other_variable() {
        let ret = CypherReturn {
            items: vec![CypherReturnItem::Expression {
                expr: CypherExpr::Variable("y".to_string()),
                alias: None,
            }],
            ..ret_var("x")
        };
        let err = validate_return_items(&ret, "x").unwrap_err();
        assert!(
            matches!(err, CypherError::UnsupportedFeature(ref m) if m.contains("RETURN references variable 'y'")),
            "got {err:?}"
        );
    }

    #[test]
    fn validate_return_items_rejects_aggregate() {
        let ret = CypherReturn {
            items: vec![CypherReturnItem::Expression {
                expr: CypherExpr::FunctionCall {
                    name: "count".to_string(),
                    args: vec![CypherExpr::Star],
                    distinct: false,
                },
                alias: None,
            }],
            ..ret_var("x")
        };
        let err = validate_return_items(&ret, "x").unwrap_err();
        match err {
            CypherError::UnsupportedFeature(m) => {
                assert!(m.contains("aggregation (count)"), "got {m}");
            }
            other => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }

    #[test]
    fn validate_return_items_rejects_other_expression() {
        let ret = CypherReturn {
            items: vec![CypherReturnItem::Expression {
                expr: CypherExpr::Property {
                    variable: "x".to_string(),
                    property: "k".to_string(),
                },
                alias: None,
            }],
            ..ret_var("x")
        };
        let err = validate_return_items(&ret, "x").unwrap_err();
        match err {
            CypherError::UnsupportedFeature(m) => {
                assert!(m.contains("is not supported after a standalone"), "got {m}");
            }
            other => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }

    // --- validate_order_by -------------------------------------------------

    #[test]
    fn validate_order_by_empty_returns_false() {
        assert!(!validate_order_by(&ret_var("x"), "x").unwrap());
    }

    #[test]
    fn validate_order_by_by_variable_returns_true() {
        let ret = CypherReturn {
            order_by: vec![CypherOrderItem {
                expr: CypherExpr::Variable("x".to_string()),
                descending: false,
            }],
            ..ret_var("x")
        };
        assert!(validate_order_by(&ret, "x").unwrap());
    }

    #[test]
    fn validate_order_by_by_alias_returns_true() {
        let ret = CypherReturn {
            items: vec![CypherReturnItem::Expression {
                expr: CypherExpr::Variable("x".to_string()),
                alias: Some("a".to_string()),
            }],
            order_by: vec![CypherOrderItem {
                expr: CypherExpr::Variable("a".to_string()),
                descending: false,
            }],
            ..ret_var("x")
        };
        assert!(validate_order_by(&ret, "x").unwrap());
    }

    #[test]
    fn validate_order_by_non_variable_expr_is_unsupported() {
        let ret = CypherReturn {
            order_by: vec![CypherOrderItem {
                expr: CypherExpr::Property {
                    variable: "x".to_string(),
                    property: "k".to_string(),
                },
                descending: false,
            }],
            ..ret_var("x")
        };
        let err = validate_order_by(&ret, "x").unwrap_err();
        assert!(
            matches!(err, CypherError::UnsupportedFeature(ref m) if m.contains("ORDER BY")),
            "got {err:?}"
        );
    }

    #[test]
    fn validate_order_by_wrong_variable_is_unsupported() {
        let ret = CypherReturn {
            order_by: vec![CypherOrderItem {
                expr: CypherExpr::Variable("z".to_string()),
                descending: false,
            }],
            ..ret_var("x")
        };
        let err = validate_order_by(&ret, "x").unwrap_err();
        match err {
            CypherError::UnsupportedFeature(m) => {
                assert!(m.contains("ORDER BY 'z'"), "got {m}");
            }
            other => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }

    // --- order_kind --------------------------------------------------------

    #[test]
    fn order_kind_homogeneous_kinds() {
        assert!(matches!(
            order_kind(&[urow(PropertyValue::Int(1)), urow(PropertyValue::Float(2.0))]).unwrap(),
            OrderKind::Numeric
        ));
        assert!(matches!(
            order_kind(&[urow(PropertyValue::String(Arc::from("a")))]).unwrap(),
            OrderKind::Text
        ));
        assert!(matches!(
            order_kind(&[urow(PropertyValue::Bool(true))]).unwrap(),
            OrderKind::Boolean
        ));
    }

    #[test]
    fn order_kind_all_null_defaults_to_numeric() {
        assert!(matches!(
            order_kind(&[urow(PropertyValue::Null)]).unwrap(),
            OrderKind::Numeric
        ));
        assert!(matches!(order_kind(&[]).unwrap(), OrderKind::Numeric));
    }

    #[test]
    fn order_kind_non_scalar_is_unsupported() {
        let arr = PropertyValue::Array(Arc::new(vec![PropertyValue::Int(1)]));
        match order_kind(&[urow(arr)]) {
            Err(CypherError::UnsupportedFeature(m)) => {
                assert!(m.contains("only scalar values"), "got {m}");
            }
            Ok(_) => panic!("expected an error, got Ok"),
            Err(other) => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }

    #[test]
    fn order_kind_heterogeneous_is_unsupported() {
        match order_kind(&[
            urow(PropertyValue::Int(1)),
            urow(PropertyValue::String(Arc::from("a"))),
        ]) {
            Err(CypherError::UnsupportedFeature(m)) => {
                assert!(m.contains("homogeneous scalar"), "got {m}");
            }
            Ok(_) => panic!("expected an error, got Ok"),
            Err(other) => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }

    // --- order_rows / compare_scalar --------------------------------------

    fn first_values(rows: &[UnwindRow]) -> Vec<PropertyValue> {
        rows.iter().map(|(v, _)| v.clone()).collect()
    }

    #[test]
    fn order_rows_ascending_and_descending_ints() {
        let mut rows = vec![
            urow(PropertyValue::Int(3)),
            urow(PropertyValue::Int(1)),
            urow(PropertyValue::Int(2)),
        ];
        order_rows(&mut rows, false).unwrap();
        assert_eq!(
            first_values(&rows),
            vec![
                PropertyValue::Int(1),
                PropertyValue::Int(2),
                PropertyValue::Int(3)
            ]
        );
        order_rows(&mut rows, true).unwrap();
        assert_eq!(
            first_values(&rows),
            vec![
                PropertyValue::Int(3),
                PropertyValue::Int(2),
                PropertyValue::Int(1)
            ]
        );
    }

    #[test]
    fn order_rows_large_ints_keep_precision() {
        // Two distinct integers above 2^53 must not collapse to a tie.
        let a = (1_i64 << 53) + 1;
        let b = (1_i64 << 53) + 2;
        let mut rows = vec![urow(PropertyValue::Int(b)), urow(PropertyValue::Int(a))];
        order_rows(&mut rows, false).unwrap();
        assert_eq!(
            first_values(&rows),
            vec![PropertyValue::Int(a), PropertyValue::Int(b)]
        );
    }

    #[test]
    fn order_rows_strings_and_bools() {
        let mut rows = vec![
            urow(PropertyValue::String(Arc::from("b"))),
            urow(PropertyValue::String(Arc::from("a"))),
        ];
        order_rows(&mut rows, false).unwrap();
        assert_eq!(
            first_values(&rows),
            vec![
                PropertyValue::String(Arc::from("a")),
                PropertyValue::String(Arc::from("b"))
            ]
        );

        let mut bools = vec![
            urow(PropertyValue::Bool(true)),
            urow(PropertyValue::Bool(false)),
        ];
        order_rows(&mut bools, false).unwrap();
        assert_eq!(
            first_values(&bools),
            vec![PropertyValue::Bool(false), PropertyValue::Bool(true)]
        );
    }

    #[test]
    fn order_rows_null_placement() {
        // Ascending: nulls last.
        let mut asc = vec![
            urow(PropertyValue::Null),
            urow(PropertyValue::Int(2)),
            urow(PropertyValue::Int(1)),
        ];
        order_rows(&mut asc, false).unwrap();
        assert_eq!(
            first_values(&asc),
            vec![
                PropertyValue::Int(1),
                PropertyValue::Int(2),
                PropertyValue::Null
            ]
        );
        // Descending: nulls first.
        let mut desc = vec![
            urow(PropertyValue::Int(1)),
            urow(PropertyValue::Null),
            urow(PropertyValue::Int(2)),
        ];
        order_rows(&mut desc, true).unwrap();
        assert_eq!(
            first_values(&desc),
            vec![
                PropertyValue::Null,
                PropertyValue::Int(2),
                PropertyValue::Int(1)
            ]
        );
    }

    // --- is_aggregate_name -------------------------------------------------

    #[test]
    fn is_aggregate_name_recognizes_aggregates_case_insensitively() {
        for name in ["count", "SUM", "Avg", "min", "MAX", "collect"] {
            assert!(is_aggregate_name(name), "{name} should be an aggregate");
        }
        assert!(!is_aggregate_name("length"));
        assert!(!is_aggregate_name("similarity"));
    }

    // --- needs_multi_binding ----------------------------------------------

    fn parse_stmt(input: &str) -> CypherStatement {
        CypherParser::parse(input).expect("parse should succeed")
    }

    #[test]
    fn needs_multi_binding_single_terminal_variable_is_false() {
        assert!(!needs_multi_binding(&parse_stmt("MATCH (n) RETURN n")));
        assert!(!needs_multi_binding(&parse_stmt(
            "MATCH (a)-[:R]->(b) RETURN b"
        )));
    }

    #[test]
    fn needs_multi_binding_multiple_referenced_vars_is_true() {
        assert!(needs_multi_binding(&parse_stmt(
            "MATCH (a)-[:R]->(b) RETURN a, b"
        )));
    }

    #[test]
    fn needs_multi_binding_anonymous_terminal_is_true() {
        // Terminal node is anonymous, but `a` is referenced -> routed to the
        // multi-pattern evaluator.
        assert!(needs_multi_binding(&parse_stmt(
            "MATCH (a)-[:R]->() RETURN a"
        )));
    }

    #[test]
    fn needs_multi_binding_non_match_is_false() {
        assert!(!needs_multi_binding(&parse_stmt(
            "UNWIND [1] AS x RETURN x"
        )));
    }

    #[test]
    fn needs_multi_binding_empty_referenced_is_false() {
        // `RETURN *` binds no entity var into `referenced`, so with a single
        // node pattern `referenced` is empty: original returns false, but the
        // `referenced.len() > 1` -> `< 1` (== 0) mutant would flip it to true
        // (0 < 1) because the `any()` over the empty set does NOT mask at len 0.
        assert!(!needs_multi_binding(&parse_stmt("MATCH (n) RETURN *")));
    }

    #[test]
    fn needs_multi_binding_varless_pattern_is_false() {
        // All-anonymous pattern -> entity_vars empty -> early-return false.
        assert!(!needs_multi_binding(&parse_stmt("MATCH () RETURN *")));
    }

    #[test]
    fn needs_multi_binding_where_reference_counts() {
        // The second entity var `a` is referenced ONLY via WHERE; without the
        // where-clause collection path it would look single-terminal (`b`).
        assert!(needs_multi_binding(&parse_stmt(
            "MATCH (a)-[:R]->(b) WHERE a.age > 1 RETURN b"
        )));
    }

    #[test]
    fn needs_multi_binding_order_by_reference_counts() {
        // The second entity var `a` is referenced ONLY via ORDER BY; this pins
        // the order_by collection path.
        assert!(needs_multi_binding(&parse_stmt(
            "MATCH (a)-[:R]->(b) RETURN b ORDER BY a.age"
        )));
    }

    // --- end-to-end execute_unwind ----------------------------------------

    #[test]
    fn unwind_empty_and_null_expand_to_zero_rows() {
        assert!(run_unwind("UNWIND [] AS x RETURN x").is_empty());
        assert!(run_unwind("UNWIND null AS x RETURN x").is_empty());
    }

    #[test]
    fn unwind_order_by_asc_and_desc() {
        assert_eq!(
            run_unwind("UNWIND [3, 1, 2] AS x RETURN x ORDER BY x"),
            vec![
                PropertyValue::Int(1),
                PropertyValue::Int(2),
                PropertyValue::Int(3)
            ]
        );
        assert_eq!(
            run_unwind("UNWIND [3, 1, 2] AS x RETURN x ORDER BY x DESC"),
            vec![
                PropertyValue::Int(3),
                PropertyValue::Int(2),
                PropertyValue::Int(1)
            ]
        );
    }

    #[test]
    fn unwind_distinct_dedups() {
        assert_eq!(
            run_unwind("UNWIND [1, 1, 2] AS x RETURN DISTINCT x"),
            vec![PropertyValue::Int(1), PropertyValue::Int(2)]
        );
    }

    #[test]
    fn unwind_skip_and_limit_paginate() {
        assert_eq!(
            run_unwind("UNWIND [1, 2, 3, 4] AS x RETURN x SKIP 1 LIMIT 2"),
            vec![PropertyValue::Int(2), PropertyValue::Int(3)]
        );
    }

    #[test]
    fn unwind_parameter_list_end_to_end() {
        let mut params = no_params();
        params.insert(
            "p".to_string(),
            CypherParameterValue::List(vec![
                CypherParameterValue::Int(10),
                CypherParameterValue::Int(20),
            ]),
        );
        assert_eq!(
            run_unwind_params("UNWIND $p AS x RETURN x", params),
            vec![PropertyValue::Int(10), PropertyValue::Int(20)]
        );
    }

    // --- sharper boundary / discrimination tests --------------------------
    // These pin operator, guard, size-hint, and depth-arithmetic mutants that
    // a coarse "MAX+1" / message-only assertion leaves alive.

    #[test]
    fn vec_result_iterator_size_hint_is_exact() {
        let it = VecResultIterator::new(vec![
            QueryRow::from_columns(vec![("x".to_string(), PropertyValue::Int(1))]),
            QueryRow::from_columns(vec![("x".to_string(), PropertyValue::Int(2))]),
        ]);
        // Exact `(len, Some(len))` -- distinguishes every wrong-tuple mutant.
        assert_eq!(it.size_hint(), (2, Some(2)));
    }

    #[test]
    fn eval_element_depth_boundary_is_inclusive() {
        // At exactly MAX_UNWIND_DEPTH the guard (`depth > MAX`) must NOT fire;
        // this kills the `>` -> `>=` mutant that an over-limit test cannot.
        assert_eq!(
            eval_element(
                &CypherExpr::Value(CypherValue::Int(1)),
                &no_params(),
                MAX_UNWIND_DEPTH
            )
            .unwrap(),
            PropertyValue::Int(1)
        );
    }

    #[test]
    fn param_to_property_depth_boundary_is_inclusive() {
        assert_eq!(
            param_to_property(&CypherParameterValue::Int(1), MAX_UNWIND_DEPTH).unwrap(),
            PropertyValue::Int(1)
        );
    }

    #[test]
    fn eval_element_list_recursion_increments_depth() {
        // A list nested deeper than MAX must trip the guard. The `depth + 1`
        // increment is what accumulates toward the cap; a `depth * 1` mutant
        // would leave depth pinned at 0 and never error.
        let mut expr = CypherExpr::Value(CypherValue::Int(1));
        for _ in 0..(MAX_UNWIND_DEPTH + 40) {
            expr = CypherExpr::List(vec![expr]);
        }
        assert!(
            matches!(
                eval_element(&expr, &no_params(), 0),
                Err(CypherError::ParseError { .. })
            ),
            "deeply nested list should trip the depth guard"
        );
    }

    #[test]
    fn eval_element_grouped_recursion_increments_depth() {
        let mut expr = CypherExpr::Value(CypherValue::Int(1));
        for _ in 0..(MAX_UNWIND_DEPTH + 40) {
            expr = CypherExpr::Grouped(Box::new(expr));
        }
        assert!(matches!(
            eval_element(&expr, &no_params(), 0),
            Err(CypherError::ParseError { .. })
        ));
    }

    #[test]
    fn param_to_property_list_recursion_increments_depth() {
        let mut param = CypherParameterValue::Int(1);
        for _ in 0..(MAX_UNWIND_DEPTH + 40) {
            param = CypherParameterValue::List(vec![param]);
        }
        assert!(matches!(
            param_to_property(&param, 0),
            Err(CypherError::ParseError { .. })
        ));
    }

    #[test]
    fn validate_return_items_non_aggregate_function_is_generic_error() {
        // A non-aggregate function call must hit the generic `other` arm, NOT
        // the aggregate arm -- this kills the `is_aggregate_name(name)` guard
        // being replaced with `true`.
        let ret = CypherReturn {
            items: vec![CypherReturnItem::Expression {
                expr: CypherExpr::FunctionCall {
                    name: "length".to_string(),
                    args: vec![CypherExpr::Variable("x".to_string())],
                    distinct: false,
                },
                alias: None,
            }],
            ..ret_var("x")
        };
        match validate_return_items(&ret, "x") {
            Err(CypherError::UnsupportedFeature(m)) => {
                assert!(m.contains("is not supported after a standalone"), "got {m}");
                assert!(!m.contains("aggregation"), "got {m}");
            }
            other => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }

    #[test]
    fn needs_multi_binding_comma_patterns_route_on_pattern_count() {
        // Two comma-separated patterns with a single referenced terminal var:
        // the ONLY clause making this multi-binding is `pattern.len() > 1`.
        // Kills the `>` -> `<` and the `||` -> `&&` mutants on that clause.
        assert!(needs_multi_binding(&parse_stmt("MATCH (a), (b) RETURN b")));
    }

    #[test]
    fn plan_routes_multi_pattern_vs_single_entity() {
        // The `needs_multi_binding` match guard in `plan_cypher_with_params`
        // decides MultiPattern vs Query routing.
        assert!(matches!(
            plan_cypher("MATCH (a)-[:R]->(b) RETURN a, b").unwrap(),
            CypherExecution::MultiPattern { .. }
        ));
        assert!(matches!(
            plan_cypher("MATCH (n) RETURN n").unwrap(),
            CypherExecution::Query(_)
        ));
    }

    #[test]
    fn explain_rejects_multi_pattern_but_plans_single() {
        // The `needs_multi_binding` guard in `lower_for_plan`.
        assert!(matches!(
            plan_cypher("EXPLAIN MATCH (n) RETURN n").unwrap(),
            CypherExecution::Explain(_)
        ));
        match plan_cypher("EXPLAIN MATCH (a)-[:R]->(b) RETURN a, b") {
            Err(CypherError::UnsupportedFeature(m)) => {
                assert!(m.contains("multi-pattern"), "got {m}");
            }
            Ok(_) => panic!("expected an error, got a plannable execution"),
            Err(other) => panic!("expected UnsupportedFeature, got {other:?}"),
        }
    }
}
