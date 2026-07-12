//! Multi-variable, multi-pattern `MATCH` evaluation (Issue #549).
//!
//! AletheiaDB's standard query pipeline is a Volcano pull-iterator tree whose
//! rows carry exactly **one** entity ([`QueryRow::entity`]). That single-entity
//! shape cannot represent a query that binds and returns more than one variable
//! (`MATCH (a),(b) RETURN a, b`) or joins several comma-separated patterns, so
//! routing such a query through the standard converter silently drops variables
//! or concatenates un-joined scans -- a wrong answer.
//!
//! This module is a **contained** Cypher-level evaluator for the read-only
//! multi-variable subset. It reuses the database's public primitives (label
//! scans, adjacency, property access) and materializes rows carrying
//! [`QueryRow::bindings`] (named `variable -> entity`) and/or
//! [`QueryRow::columns`] (scalar projections). It never disturbs the existing
//! single-entity path: only queries the router
//! ([`super::exec::needs_multi_binding`]) classifies as multi-variable reach it.
//!
//! # House rule
//!
//! Anything this evaluator cannot answer **correctly** is rejected with a
//! structured [`CypherError::UnsupportedFeature`]; it never returns a
//! silently-wrong row. v1 rejects: `OPTIONAL MATCH`, `WITH`, temporal `AS OF`,
//! aggregates in `RETURN`, and variable-length relationships inside a bound
//! pattern.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::core::error::Result as CoreResult;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::PropertyValue;
use crate::db::AletheiaDB;
use crate::query::executor::{EntityResult, QueryResults, QueryRow, ResultIterator};

use super::ast::{
    CypherCompOp, CypherDirection, CypherExpr, CypherNodePattern, CypherPattern,
    CypherPatternElement, CypherRelPattern, CypherReturn, CypherReturnItem, CypherStatement,
    CypherValue,
};
use super::converter::CypherParameterValue;
use super::error::CypherError;

/// An ordered set of `variable -> entity` bindings for one candidate row.
type Binding = Vec<(String, EntityResult)>;

/// Entry point: evaluate a multi-variable `MATCH` statement into result rows.
///
/// Invoked from `AletheiaDB::execute_multi_pattern` after the router
/// ([`super::exec::needs_multi_binding`]) has classified the statement as
/// multi-variable. Returns a materialized [`QueryResults`] stream.
///
/// # Errors
///
/// Returns [`CypherError::UnsupportedFeature`] for any construct outside the v1
/// subset (see the module docs), and [`CypherError::SemanticError`] for a
/// structurally malformed pattern or a reference to an unbound variable.
pub fn evaluate(
    db: &AletheiaDB,
    statement: &CypherStatement,
    params: &HashMap<String, CypherParameterValue>,
) -> Result<QueryResults, CypherError> {
    let CypherStatement::Match {
        optional,
        pattern,
        where_clause,
        return_clause,
        temporal,
        with_clauses,
        optional_matches,
    } = statement
    else {
        return Err(CypherError::UnsupportedFeature(
            "multi-pattern evaluator requires a MATCH statement".to_string(),
        ));
    };

    // ---- v1 rejections (honest structured error, never a silently-wrong row).
    if *optional {
        return Err(CypherError::UnsupportedFeature(
            "OPTIONAL MATCH as the leading clause is not supported by the \
             multi-variable evaluator"
                .to_string(),
        ));
    }
    if !with_clauses.is_empty() {
        return Err(CypherError::UnsupportedFeature(
            "WITH projection is not supported alongside multi-variable pattern \
             binding"
                .to_string(),
        ));
    }
    if !optional_matches.is_empty() {
        return Err(CypherError::UnsupportedFeature(
            "OPTIONAL MATCH is not supported alongside multi-variable pattern \
             binding"
                .to_string(),
        ));
    }
    if temporal.is_some() {
        return Err(CypherError::UnsupportedFeature(
            "temporal AS OF qualifiers are not supported by the multi-variable \
             evaluator; multi-pattern binding is current-state only in v1"
                .to_string(),
        ));
    }
    for item in &return_clause.items {
        if let CypherReturnItem::Expression { expr, .. } = item
            && expr_has_aggregate(expr)
        {
            return Err(CypherError::UnsupportedFeature(
                "aggregation over a multi-variable pattern is not supported; \
                 RETURN the bound variables or their properties directly"
                    .to_string(),
            ));
        }
    }
    for p in pattern {
        for element in &p.elements {
            if let CypherPatternElement::Relationship(rel) = element
                && rel.depth.is_some()
            {
                return Err(CypherError::UnsupportedFeature(
                    "variable-length relationships inside a multi-variable \
                     bound pattern are not supported in v1"
                        .to_string(),
                ));
            }
        }
    }

    // ---- Materialize the current-state node set once (sorted by id for
    // deterministic candidate enumeration and stable output order).
    let mut nodes: Vec<Node> = db.current.all_nodes().collect();
    nodes.sort_by_key(|n| n.id);
    let mut node_by_id: HashMap<NodeId, usize> = HashMap::with_capacity(nodes.len());
    for (idx, node) in nodes.iter().enumerate() {
        node_by_id.insert(node.id, idx);
    }

    let eval = MultiEval {
        db,
        params,
        nodes,
        node_by_id,
    };

    // ---- Nested-loop binding matcher: extend the binding set by each pattern.
    let mut env: Vec<Binding> = vec![Vec::new()];
    for p in pattern {
        let mut next: Vec<Binding> = Vec::new();
        for base in &env {
            eval.match_pattern_extends(p, base, &mut next)?;
        }
        env = next;
    }

    // ---- WHERE filter (cross-variable predicates resolve from the binding).
    if let Some(predicate) = where_clause {
        let mut filtered = Vec::with_capacity(env.len());
        for binding in env {
            if eval.eval_predicate(predicate, &binding)? {
                filtered.push(binding);
            }
        }
        env = filtered;
    }

    // ---- Projection: each surviving binding becomes a QueryRow.
    let entity_order = entity_var_declaration_order(pattern);
    let mut projected: Vec<(Binding, QueryRow)> = Vec::with_capacity(env.len());
    for binding in env {
        let row = eval.project(&binding, return_clause, &entity_order)?;
        projected.push((binding, row));
    }

    // ---- DISTINCT (dedupe by projected content) -> ORDER BY -> SKIP -> LIMIT.
    if return_clause.distinct {
        projected = distinct_rows(projected);
    }
    if !return_clause.order_by.is_empty() {
        eval.order_rows(&mut projected, return_clause)?;
    }
    let skip = return_clause.skip.unwrap_or(0);
    let rows: Vec<QueryRow> = projected
        .into_iter()
        .skip(skip)
        .take(return_clause.limit.unwrap_or(usize::MAX))
        .map(|(_, row)| row)
        .collect();

    Ok(QueryResults::new(Box::new(VecResultIterator::new(rows))))
}

/// Shared evaluation context: the database handle, bound parameters, and the
/// materialized node snapshot.
struct MultiEval<'a> {
    db: &'a AletheiaDB,
    params: &'a HashMap<String, CypherParameterValue>,
    nodes: Vec<Node>,
    node_by_id: HashMap<NodeId, usize>,
}

impl MultiEval<'_> {
    /// Borrow a materialized node by id, if present in the snapshot.
    fn node_ref(&self, id: NodeId) -> Option<&Node> {
        self.node_by_id.get(&id).map(|&idx| &self.nodes[idx])
    }

    /// Extend `base` by every assignment that satisfies `pattern`, appending
    /// each resulting binding to `out`.
    fn match_pattern_extends(
        &self,
        pattern: &CypherPattern,
        base: &Binding,
        out: &mut Vec<Binding>,
    ) -> Result<(), CypherError> {
        let Some(CypherPatternElement::Node(first)) = pattern.elements.first() else {
            return Err(CypherError::SemanticError(
                "a graph pattern must start with a node".to_string(),
            ));
        };
        for node in self.node_candidates(first, base)? {
            let mut binding = base.clone();
            if let Some(var) = &first.variable
                && lookup(&binding, var).is_none()
            {
                binding.push((var.clone(), EntityResult::Node(node.clone())));
            }
            self.extend_from(&pattern.elements, 1, binding, node.id, out)?;
        }
        Ok(())
    }

    /// Walk the `(relationship, node)` pairs of a pattern from element `idx`,
    /// starting at `current` node, emitting a completed binding per full
    /// consistent traversal.
    fn extend_from(
        &self,
        elements: &[CypherPatternElement],
        idx: usize,
        binding: Binding,
        current: NodeId,
        out: &mut Vec<Binding>,
    ) -> Result<(), CypherError> {
        if idx >= elements.len() {
            out.push(binding);
            return Ok(());
        }
        let CypherPatternElement::Relationship(rel) = &elements[idx] else {
            return Err(CypherError::SemanticError(
                "expected a relationship element in the pattern".to_string(),
            ));
        };
        let Some(CypherPatternElement::Node(next_patt)) = elements.get(idx + 1) else {
            return Err(CypherError::SemanticError(
                "a relationship must be followed by a node in the pattern".to_string(),
            ));
        };

        for (edge, far_id) in self.adjacent(current, rel)? {
            let Some(far_node) = self.node_ref(far_id).cloned() else {
                // Orphaned edge whose endpoint is missing from current state.
                continue;
            };
            if !self.node_element_matches(&far_node, next_patt)? {
                continue;
            }
            let mut binding = binding.clone();
            // Unify the far-node variable (a repeated variable must resolve to
            // the same node id).
            if let Some(nv) = &next_patt.variable {
                match lookup(&binding, nv) {
                    Some(existing) => {
                        if existing.node_id() != Some(far_node.id) {
                            continue;
                        }
                    }
                    None => binding.push((nv.clone(), EntityResult::Node(far_node.clone()))),
                }
            }
            // Bind (and unify) the relationship variable.
            if let Some(rv) = &rel.variable {
                match lookup(&binding, rv) {
                    Some(existing) => {
                        if entity_edge_id(existing) != Some(edge.id) {
                            continue;
                        }
                    }
                    None => binding.push((rv.clone(), EntityResult::Edge(edge.clone()))),
                }
            }
            self.extend_from(elements, idx + 2, binding, far_node.id, out)?;
        }
        Ok(())
    }

    /// Candidate nodes for a node element: the single already-bound node (if the
    /// element's variable is bound and still satisfies the element), otherwise
    /// every node satisfying the element's labels and inline properties.
    fn node_candidates(
        &self,
        patt: &CypherNodePattern,
        base: &Binding,
    ) -> Result<Vec<Node>, CypherError> {
        if let Some(var) = &patt.variable
            && let Some(existing) = lookup(base, var)
        {
            if let EntityResult::Node(n) = existing {
                return Ok(if self.node_element_matches(n, patt)? {
                    vec![n.clone()]
                } else {
                    Vec::new()
                });
            }
            // A variable bound to an edge cannot also be a node.
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for node in &self.nodes {
            if self.node_element_matches(node, patt)? {
                out.push(node.clone());
            }
        }
        Ok(out)
    }

    /// Whether a node satisfies a node element's labels and inline properties.
    fn node_element_matches(
        &self,
        node: &Node,
        patt: &CypherNodePattern,
    ) -> Result<bool, CypherError> {
        for label in &patt.labels {
            if !node.has_label_str(label) {
                return Ok(false);
            }
        }
        self.props_match(&patt.properties, |k| node.get_property(k))
    }

    /// Enumerate edges incident to `node_id` honoring the relationship's
    /// direction, types, and inline properties, paired with the far endpoint.
    /// For `Both`, a self-loop is emitted once (deduped by edge id).
    fn adjacent(
        &self,
        node_id: NodeId,
        rel: &CypherRelPattern,
    ) -> Result<Vec<(Edge, NodeId)>, CypherError> {
        let want_out = matches!(
            rel.direction,
            CypherDirection::Outgoing | CypherDirection::Both
        );
        let want_in = matches!(
            rel.direction,
            CypherDirection::Incoming | CypherDirection::Both
        );
        let mut result = Vec::new();
        let mut seen: HashSet<u64> = HashSet::new();

        if want_out {
            for edge_id in self.db.get_outgoing_edges(node_id) {
                let Ok(edge) = self.db.get_edge(edge_id) else {
                    continue;
                };
                if !self.edge_matches(&edge, rel)? || !seen.insert(edge.id.as_u64()) {
                    continue;
                }
                let far = edge.target;
                result.push((edge, far));
            }
        }
        if want_in {
            for edge_id in self.db.get_incoming_edges(node_id) {
                let Ok(edge) = self.db.get_edge(edge_id) else {
                    continue;
                };
                if !self.edge_matches(&edge, rel)? || !seen.insert(edge.id.as_u64()) {
                    continue;
                }
                let far = edge.source;
                result.push((edge, far));
            }
        }
        Ok(result)
    }

    /// Whether an edge satisfies a relationship element's types and properties.
    fn edge_matches(&self, edge: &Edge, rel: &CypherRelPattern) -> Result<bool, CypherError> {
        if !rel.rel_types.is_empty() && !rel.rel_types.iter().any(|t| edge.has_label_str(t)) {
            return Ok(false);
        }
        self.props_match(&rel.properties, |k| edge.get_property(k))
    }

    /// Whether every inline `(key, value)` constraint holds against `get`.
    fn props_match<'p>(
        &self,
        props: &[(String, CypherValue)],
        get: impl Fn(&str) -> Option<&'p PropertyValue>,
    ) -> Result<bool, CypherError> {
        for (key, want) in props {
            match get(key) {
                Some(actual) if self.value_equals(actual, want)? => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    /// Compare a stored [`PropertyValue`] against a pattern [`CypherValue`]
    /// (resolving parameters), with numeric int/float coercion.
    fn value_equals(
        &self,
        actual: &PropertyValue,
        want: &CypherValue,
    ) -> Result<bool, CypherError> {
        let want = self.cypher_value_to_property(want)?;
        Ok(loosely_equal(actual, &want))
    }

    /// Convert a Cypher literal (resolving a `$param`) into a [`PropertyValue`].
    fn cypher_value_to_property(&self, value: &CypherValue) -> Result<PropertyValue, CypherError> {
        Ok(match value {
            CypherValue::Null => PropertyValue::Null,
            CypherValue::Bool(b) => PropertyValue::Bool(*b),
            CypherValue::Int(n) => PropertyValue::Int(*n),
            CypherValue::Float(f) => PropertyValue::Float(*f),
            CypherValue::String(s) => PropertyValue::String(std::sync::Arc::from(s.as_str())),
            CypherValue::Vector(v) => PropertyValue::Vector(std::sync::Arc::clone(v)),
            CypherValue::Parameter(name) => {
                let param = self.params.get(name).ok_or_else(|| {
                    CypherError::ParameterError(format!("unbound parameter: ${name}"))
                })?;
                param_to_property(param)
            }
        })
    }

    // -- Predicate / scalar evaluation ------------------------------------

    /// Evaluate a `WHERE` predicate against a binding (null-yields-false).
    fn eval_predicate(&self, expr: &CypherExpr, binding: &Binding) -> Result<bool, CypherError> {
        match expr {
            CypherExpr::Comparison { left, op, right } => {
                let l = self.eval_value(left, binding)?;
                let r = self.eval_value(right, binding)?;
                match (l, r) {
                    (Some(a), Some(b)) => Ok(compare(&a, &b, *op)),
                    _ => Ok(false),
                }
            }
            CypherExpr::And(a, b) => {
                Ok(self.eval_predicate(a, binding)? && self.eval_predicate(b, binding)?)
            }
            CypherExpr::Or(a, b) => {
                Ok(self.eval_predicate(a, binding)? || self.eval_predicate(b, binding)?)
            }
            CypherExpr::Not(inner) => Ok(!self.eval_predicate(inner, binding)?),
            CypherExpr::IsNull(inner) => Ok(self.eval_value(inner, binding)?.is_none()),
            CypherExpr::IsNotNull(inner) => Ok(self.eval_value(inner, binding)?.is_some()),
            CypherExpr::In { expr, values } => {
                let Some(needle) = self.eval_value(expr, binding)? else {
                    return Ok(false);
                };
                for candidate in values {
                    if let Some(v) = self.eval_value(candidate, binding)?
                        && loosely_equal(&needle, &v)
                    {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            CypherExpr::Contains { expr, substring } => Ok(self
                .eval_string(expr, binding)?
                .is_some_and(|s| s.contains(substring))),
            CypherExpr::StartsWith { expr, prefix } => Ok(self
                .eval_string(expr, binding)?
                .is_some_and(|s| s.starts_with(prefix))),
            CypherExpr::EndsWith { expr, suffix } => Ok(self
                .eval_string(expr, binding)?
                .is_some_and(|s| s.ends_with(suffix))),
            CypherExpr::Grouped(inner) => self.eval_predicate(inner, binding),
            CypherExpr::Value(CypherValue::Bool(b)) => Ok(*b),
            other => match self.eval_value(other, binding)? {
                Some(PropertyValue::Bool(b)) => Ok(b),
                None => Ok(false),
                Some(_) => Err(CypherError::UnsupportedFeature(format!(
                    "WHERE expression is not a boolean predicate: {other:?}"
                ))),
            },
        }
    }

    /// Evaluate an expression to an optional scalar value (`None` == null).
    fn eval_value(
        &self,
        expr: &CypherExpr,
        binding: &Binding,
    ) -> Result<Option<PropertyValue>, CypherError> {
        match expr {
            CypherExpr::Value(value) => {
                let pv = self.cypher_value_to_property(value)?;
                Ok(if matches!(pv, PropertyValue::Null) {
                    None
                } else {
                    Some(pv)
                })
            }
            CypherExpr::Property { variable, property } => {
                let entity = lookup(binding, variable).ok_or_else(|| {
                    CypherError::SemanticError(format!(
                        "property access references unbound variable '{variable}'"
                    ))
                })?;
                Ok(entity_property(entity, property))
            }
            CypherExpr::Grouped(inner) => self.eval_value(inner, binding),
            CypherExpr::Variable(name) => Err(CypherError::UnsupportedFeature(format!(
                "a bare entity variable ('{name}') cannot be used as a scalar \
                 value; use a property access like {name}.<key>"
            ))),
            other => Err(CypherError::UnsupportedFeature(format!(
                "expression not supported in a scalar position by the \
                 multi-variable evaluator: {other:?}"
            ))),
        }
    }

    /// Evaluate an expression expecting a string value (else `None`).
    fn eval_string(
        &self,
        expr: &CypherExpr,
        binding: &Binding,
    ) -> Result<Option<String>, CypherError> {
        // `PropertyValue` implements `Drop`, so its `String` payload cannot be
        // moved out of the owned value by pattern; borrow and clone instead.
        Ok(match self.eval_value(expr, binding)? {
            Some(PropertyValue::String(ref s)) => Some(s.to_string()),
            _ => None,
        })
    }

    // -- Projection --------------------------------------------------------

    /// Project one binding into a [`QueryRow`] according to `RETURN`.
    fn project(
        &self,
        binding: &Binding,
        ret: &CypherReturn,
        entity_order: &[String],
    ) -> Result<QueryRow, CypherError> {
        let mut bindings_out: Binding = Vec::new();
        let mut columns_out: Vec<(String, PropertyValue)> = Vec::new();

        for item in &ret.items {
            match item {
                CypherReturnItem::Star => {
                    for var in entity_order {
                        if let Some(entity) = lookup(binding, var) {
                            bindings_out.push((var.clone(), entity.clone()));
                        }
                    }
                }
                CypherReturnItem::Variable(v) => {
                    self.project_variable(binding, v, None, &mut bindings_out)?;
                }
                CypherReturnItem::Expression { expr, alias } => match expr {
                    CypherExpr::Variable(v) => {
                        self.project_variable(binding, v, alias.clone(), &mut bindings_out)?;
                    }
                    _ => {
                        let value = self
                            .eval_value(expr, binding)?
                            .unwrap_or(PropertyValue::Null);
                        let name = alias.clone().unwrap_or_else(|| default_column_name(expr));
                        columns_out.push((name, value));
                    }
                },
            }
        }

        let columns = if columns_out.is_empty() {
            None
        } else {
            Some(columns_out)
        };
        if bindings_out.is_empty() {
            // Pure scalar projection row (no entity variables returned).
            Ok(QueryRow::from_columns(columns.unwrap_or_default()))
        } else {
            Ok(QueryRow::from_bindings(bindings_out, columns))
        }
    }

    /// Project a returned variable: an entity variable becomes a binding entry
    /// (renamed by `alias` when present); anything else is a scope error.
    fn project_variable(
        &self,
        binding: &Binding,
        var: &str,
        alias: Option<String>,
        bindings_out: &mut Binding,
    ) -> Result<(), CypherError> {
        match lookup(binding, var) {
            Some(entity) => {
                let name = alias.unwrap_or_else(|| var.to_string());
                bindings_out.push((name, entity.clone()));
                Ok(())
            }
            None => Err(CypherError::SemanticError(format!(
                "RETURN references variable '{var}', which is not bound by the \
                 pattern"
            ))),
        }
    }

    // -- ORDER BY ----------------------------------------------------------

    /// Sort projected rows by the `ORDER BY` keys (openCypher null placement:
    /// nulls last for ascending, first for descending).
    fn order_rows(
        &self,
        rows: &mut [(Binding, QueryRow)],
        ret: &CypherReturn,
    ) -> Result<(), CypherError> {
        // Pre-compute each row's ordered sort keys so the comparator is
        // infallible (evaluation errors surface here, before sorting).
        let mut keys: Vec<Vec<Option<PropertyValue>>> = Vec::with_capacity(rows.len());
        for (binding, _) in rows.iter() {
            let mut key = Vec::with_capacity(ret.order_by.len());
            for item in &ret.order_by {
                key.push(self.eval_value(&item.expr, binding)?);
            }
            keys.push(key);
        }

        let mut index: Vec<usize> = (0..rows.len()).collect();
        index.sort_by(|&a, &b| compare_order_keys(&keys[a], &keys[b], ret));

        // Apply the permutation.
        let mut reordered: Vec<Option<(Binding, QueryRow)>> = rows
            .iter_mut()
            .map(|slot| Some(std::mem::replace(slot, placeholder_slot())))
            .collect();
        for (dst, &src) in index.iter().enumerate() {
            rows[dst] = reordered[src].take().expect("each source used once");
        }
        Ok(())
    }
}

/// A throwaway `(Binding, QueryRow)` used only to vacate a slot during the
/// in-place permutation in [`MultiEval::order_rows`]; every placeholder is
/// overwritten before the function returns.
fn placeholder_slot() -> (Binding, QueryRow) {
    (Vec::new(), QueryRow::from_columns(Vec::new()))
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Look up the entity bound to `var` in a binding.
fn lookup<'b>(binding: &'b Binding, var: &str) -> Option<&'b EntityResult> {
    binding.iter().find(|(name, _)| name == var).map(|(_, e)| e)
}

/// The edge id of an entity, if it is an edge.
fn entity_edge_id(entity: &EntityResult) -> Option<EdgeId> {
    match entity {
        EntityResult::Edge(e) => Some(e.id),
        EntityResult::EdgeId(id) => Some(*id),
        _ => None,
    }
}

/// A property value of a bound entity (node or edge), cloned.
fn entity_property(entity: &EntityResult, property: &str) -> Option<PropertyValue> {
    match entity {
        EntityResult::Node(n) => n.get_property(property).cloned(),
        EntityResult::Edge(e) => e.get_property(property).cloned(),
        _ => None,
    }
}

/// Convert a runtime parameter value into a [`PropertyValue`].
fn param_to_property(param: &CypherParameterValue) -> PropertyValue {
    match param {
        CypherParameterValue::Null => PropertyValue::Null,
        CypherParameterValue::Bool(b) => PropertyValue::Bool(*b),
        CypherParameterValue::Int(n) => PropertyValue::Int(*n),
        CypherParameterValue::Float(f) => PropertyValue::Float(*f),
        CypherParameterValue::String(s) => PropertyValue::String(std::sync::Arc::from(s.as_str())),
        CypherParameterValue::Embedding(e) => PropertyValue::Vector(std::sync::Arc::clone(e)),
        CypherParameterValue::List(items) => PropertyValue::Array(std::sync::Arc::new(
            items.iter().map(param_to_property).collect(),
        )),
    }
}

/// Entity variable names in pattern declaration order (nodes and relationships,
/// first occurrence wins), used for `RETURN *`.
fn entity_var_declaration_order(patterns: &[CypherPattern]) -> Vec<String> {
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    for pattern in patterns {
        for element in &pattern.elements {
            let var = match element {
                CypherPatternElement::Node(n) => n.variable.as_ref(),
                CypherPatternElement::Relationship(r) => r.variable.as_ref(),
            };
            if let Some(v) = var
                && seen.insert(v.clone())
            {
                order.push(v.clone());
            }
        }
    }
    order
}

/// Default output column name for a non-aliased scalar expression.
fn default_column_name(expr: &CypherExpr) -> String {
    match expr {
        CypherExpr::Property { variable, property } => format!("{variable}.{property}"),
        CypherExpr::Variable(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

/// Whether an expression contains an aggregate function call.
fn expr_has_aggregate(expr: &CypherExpr) -> bool {
    match expr {
        CypherExpr::FunctionCall { name, args, .. } => {
            is_aggregate_name(name) || args.iter().any(expr_has_aggregate)
        }
        CypherExpr::Comparison { left, right, .. } => {
            expr_has_aggregate(left) || expr_has_aggregate(right)
        }
        CypherExpr::And(a, b) | CypherExpr::Or(a, b) => {
            expr_has_aggregate(a) || expr_has_aggregate(b)
        }
        CypherExpr::Not(inner)
        | CypherExpr::IsNull(inner)
        | CypherExpr::IsNotNull(inner)
        | CypherExpr::Grouped(inner) => expr_has_aggregate(inner),
        CypherExpr::In { expr, values } => {
            expr_has_aggregate(expr) || values.iter().any(expr_has_aggregate)
        }
        CypherExpr::Contains { expr, .. }
        | CypherExpr::StartsWith { expr, .. }
        | CypherExpr::EndsWith { expr, .. } => expr_has_aggregate(expr),
        CypherExpr::List(items) => items.iter().any(expr_has_aggregate),
        CypherExpr::Value(_)
        | CypherExpr::Variable(_)
        | CypherExpr::Property { .. }
        | CypherExpr::Star => false,
    }
}

/// Whether `name` is a case-insensitive aggregate function name.
fn is_aggregate_name(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "COLLECT"
    )
}

/// Equality with int/float numeric coercion; other types use `PropertyValue`'s
/// own equality.
fn loosely_equal(a: &PropertyValue, b: &PropertyValue) -> bool {
    if is_numeric(a) && is_numeric(b) {
        return partial_cmp(a, b) == Some(Ordering::Equal);
    }
    a == b
}

/// Whether a value is an integer or float.
fn is_numeric(v: &PropertyValue) -> bool {
    matches!(v, PropertyValue::Int(_) | PropertyValue::Float(_))
}

/// Partial ordering over comparable scalar values (numeric, string, bool).
fn partial_cmp(a: &PropertyValue, b: &PropertyValue) -> Option<Ordering> {
    match (a, b) {
        (PropertyValue::Int(x), PropertyValue::Int(y)) => Some(x.cmp(y)),
        (PropertyValue::Int(x), PropertyValue::Float(y)) => (*x as f64).partial_cmp(y),
        (PropertyValue::Float(x), PropertyValue::Int(y)) => x.partial_cmp(&(*y as f64)),
        (PropertyValue::Float(x), PropertyValue::Float(y)) => x.partial_cmp(y),
        (PropertyValue::String(x), PropertyValue::String(y)) => Some(x.as_ref().cmp(y.as_ref())),
        (PropertyValue::Bool(x), PropertyValue::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

/// Evaluate a comparison operator over two scalar values.
fn compare(a: &PropertyValue, b: &PropertyValue, op: CypherCompOp) -> bool {
    match op {
        CypherCompOp::Eq => loosely_equal(a, b),
        CypherCompOp::Ne => !loosely_equal(a, b),
        CypherCompOp::Lt => partial_cmp(a, b) == Some(Ordering::Less),
        CypherCompOp::Le => matches!(partial_cmp(a, b), Some(Ordering::Less | Ordering::Equal)),
        CypherCompOp::Gt => partial_cmp(a, b) == Some(Ordering::Greater),
        CypherCompOp::Ge => matches!(partial_cmp(a, b), Some(Ordering::Greater | Ordering::Equal)),
    }
}

/// Compare two rows' ordered sort keys per the `ORDER BY` directions.
fn compare_order_keys(
    a: &[Option<PropertyValue>],
    b: &[Option<PropertyValue>],
    ret: &CypherReturn,
) -> Ordering {
    for (idx, item) in ret.order_by.iter().enumerate() {
        let descending = item.descending;
        let ord = match (&a[idx], &b[idx]) {
            (None, None) => Ordering::Equal,
            // Null placement: last for ascending, first for descending.
            (None, Some(_)) => {
                if descending {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (Some(_), None) => {
                if descending {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (Some(x), Some(y)) => {
                let base = partial_cmp(x, y).unwrap_or(Ordering::Equal);
                if descending { base.reverse() } else { base }
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// A comparable identity for a projected row, used for `DISTINCT` dedup:
/// bound entity ids plus scalar column values.
type RowKey = (Vec<(String, u8, u64)>, Vec<(String, PropertyValue)>);

/// Compute a row's `DISTINCT` identity key.
fn row_key(row: &QueryRow) -> RowKey {
    let binds = row
        .bindings
        .as_ref()
        .map(|b| {
            b.iter()
                .map(|(name, entity)| {
                    let (kind, id) = entity_kind_id(entity);
                    (name.clone(), kind, id)
                })
                .collect()
        })
        .unwrap_or_default();
    let cols = row.columns.clone().unwrap_or_default();
    (binds, cols)
}

/// A `(kind_tag, id)` identity for an entity result.
fn entity_kind_id(entity: &EntityResult) -> (u8, u64) {
    match entity {
        EntityResult::Node(n) => (0, n.id.as_u64()),
        EntityResult::NodeId(id) => (0, id.as_u64()),
        EntityResult::Edge(e) => (1, e.id.as_u64()),
        EntityResult::EdgeId(id) => (1, id.as_u64()),
        EntityResult::Null => (2, 0),
    }
}

/// Deduplicate projected rows by their [`RowKey`] (first occurrence wins).
fn distinct_rows(rows: Vec<(Binding, QueryRow)>) -> Vec<(Binding, QueryRow)> {
    let mut seen: Vec<RowKey> = Vec::with_capacity(rows.len());
    let mut out = Vec::with_capacity(rows.len());
    for (binding, row) in rows {
        let key = row_key(&row);
        if seen.iter().any(|k| k == &key) {
            continue;
        }
        seen.push(key);
        out.push((binding, row));
    }
    out
}

// ---------------------------------------------------------------------------
// Result iterator
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
