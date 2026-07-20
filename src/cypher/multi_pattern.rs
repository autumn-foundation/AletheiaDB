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
//! # v1 semantics
//!
//! * **Relationship-uniqueness (openCypher isomorphism).** Each relationship is
//!   traversed **at most once** within a single `MATCH` clause -- both inside one
//!   pattern and across the comma-separated patterns of the clause. The set of
//!   already-traversed edge ids is threaded through every candidate binding
//!   branch (see [`PartialBinding`]); reusing an edge yields no match, exactly
//!   as openCypher requires (`MATCH (x)-[:R]-(y)-[:R]-(z)` over a single edge
//!   returns zero rows).
//! * **Three-valued `WHERE`.** Predicate evaluation is Kleene three-valued
//!   ([`Tri`]): a comparison with a null/absent operand is `Null`, `NOT Null` is
//!   `Null`, and a row is kept **iff** its predicate evaluates to `True` (both
//!   `Null` and `False` drop it). This makes `WHERE NOT (a.x = 5)` drop a row
//!   whose `a.x` is absent, per openCypher.
//! * **Bounded materialization.** The intermediate binding set is capped at the
//!   configured `max_schema_as_of_entities` limit (see FIX #6) so a pathological
//!   Cartesian product (`MATCH (a),(b),(c)`) cannot exhaust memory; exceeding
//!   the cap is a structured [`CypherError`], never an OOM.
//!
//! # House rule
//!
//! Anything this evaluator cannot answer **correctly** is rejected with a
//! structured [`CypherError::UnsupportedFeature`]; it never returns a
//! silently-wrong row. v1 rejects: `OPTIONAL MATCH`, `WITH`, temporal `AS OF`,
//! aggregates in `RETURN`, and variable-length relationships inside a bound
//! pattern.
//!
//! # Remaining v1 limitations
//!
//! Bindings carry cloned entities (not ids) through the Cartesian product, so a
//! wide product clones each entity per branch -- a deliberate simplicity/perf
//! tradeoff. Carrying ids and resolving lazily is tracked as a follow-up
//! (`TODO(perf #549-followup)`).

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
///
// TODO(perf #549-followup): carry entity *ids* instead of cloned entities
// through the Cartesian product; a wide product currently clones each entity
// once per surviving branch.
pub(crate) type Binding = Vec<(String, EntityResult)>;

/// A projected row paired with its still-full pattern binding (needed to
/// evaluate `ORDER BY` expressions and `DISTINCT` keys after projection).
type ProjectedRow = (Binding, QueryRow);

/// A row's evaluated `ORDER BY` sort key: one optional scalar per key
/// expression (`None` == null).
type SortKey = Vec<Option<PropertyValue>>;

/// A projected row paired with its pre-computed sort key, used while sorting.
type KeyedRow = (SortKey, ProjectedRow);

/// A candidate binding branch during matching: the `variable -> entity`
/// assignments accumulated so far plus the set of relationship edge ids already
/// traversed by this branch.
///
/// The `used_edges` set enforces openCypher **relationship-uniqueness**: an edge
/// may be traversed at most once per `MATCH` clause. It is threaded through the
/// whole clause -- across every pattern element AND across the comma-separated
/// patterns -- by cloning it into each extended branch.
#[derive(Clone, Default)]
struct PartialBinding {
    vars: Binding,
    used_edges: HashSet<EdgeId>,
}

/// Kleene three-valued truth for `WHERE` predicate evaluation.
///
/// openCypher predicates are three-valued: a comparison involving a null/absent
/// operand is `Null`, not `False`. A `WHERE` clause keeps a row **iff** the
/// predicate is [`Tri::True`]; both [`Tri::False`] and [`Tri::Null`] drop it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tri {
    True,
    False,
    Null,
}

impl Tri {
    /// A definite boolean lifts to `True`/`False` (never `Null`).
    fn from_bool(b: bool) -> Tri {
        if b { Tri::True } else { Tri::False }
    }

    /// Kleene negation: `NOT Null = Null`.
    fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Null => Tri::Null,
        }
    }

    /// Kleene conjunction: any `False` => `False`, else any `Null` => `Null`.
    fn and(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::False, _) | (_, Tri::False) => Tri::False,
            (Tri::True, Tri::True) => Tri::True,
            _ => Tri::Null,
        }
    }

    /// Kleene disjunction: any `True` => `True`, else any `Null` => `Null`.
    fn or(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::True, _) | (_, Tri::True) => Tri::True,
            (Tri::False, Tri::False) => Tri::False,
            _ => Tri::Null,
        }
    }
}

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
        // A restricting namespace scope on a multi-pattern MATCH is rejected
        // up front in `plan_cypher` (fail-closed, Issue #3349); only a
        // non-restricting `All` (no filter) reaches the evaluator, which is a
        // no-op here, so the clause is intentionally ignored.
        namespace: _,
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

    // Cap on the materialized intermediate binding count (FIX #6): reuse the
    // configurable `max_schema_as_of_entities` limit so a pathological
    // Cartesian product cannot exhaust memory. Read once, up front.
    let binding_cap = db.historical.read().max_schema_as_of_entities();

    let eval = MultiEval {
        db,
        params,
        nodes,
        node_by_id,
    };

    // ---- Per-pattern candidate memoization (FIX #6): a pattern's first-node
    // candidate set (the unbound label/property scan) is base-independent, so
    // compute it once per pattern and reuse it across every outer binding
    // instead of re-scanning all nodes per base binding.
    let mut first_memos: Vec<Vec<Node>> = Vec::with_capacity(pattern.len());
    for p in pattern {
        let memo = match p.elements.first() {
            Some(CypherPatternElement::Node(first)) => eval.scan_node_candidates(first)?,
            _ => Vec::new(),
        };
        first_memos.push(memo);
    }

    // ---- Nested-loop binding matcher: extend the binding set by each pattern.
    // Each branch threads its `used_edges` set forward for relationship
    // uniqueness across the whole clause (FIX #1).
    let mut env: Vec<PartialBinding> = vec![PartialBinding::default()];
    for (p, first_memo) in pattern.iter().zip(first_memos.iter()) {
        let mut next: Vec<PartialBinding> = Vec::new();
        for base in &env {
            eval.match_pattern_extends(p, base, first_memo, &mut next)?;
            if next.len() > binding_cap {
                return Err(CypherError::UnsupportedFeature(format!(
                    "multi-pattern result exceeds {binding_cap} intermediate \
                     bindings; add a more selective WHERE filter or labels to \
                     narrow the Cartesian product (configurable via \
                     max_schema_as_of_entities)"
                )));
            }
        }
        env = next;
    }

    // ---- WHERE filter (cross-variable predicates resolve from the binding).
    // Three-valued (FIX #2): a row survives iff its predicate is `Tri::True`.
    if let Some(predicate) = where_clause {
        let mut filtered = Vec::with_capacity(env.len());
        for binding in env {
            if eval.eval_predicate(predicate, &binding.vars)? == Tri::True {
                filtered.push(binding);
            }
        }
        env = filtered;
    }

    // ---- Projection: each surviving binding becomes a QueryRow.
    let entity_order = return_star_var_order(pattern);
    let mut projected: Vec<(Binding, QueryRow)> = Vec::with_capacity(env.len());
    for binding in env {
        let row = eval.project(&binding.vars, return_clause, &entity_order)?;
        projected.push((binding.vars, row));
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

/// Match a reading pattern list into its raw per-row variable bindings
/// (Issue #560).
///
/// This exposes the nested-loop pattern matcher used by [`evaluate`] for reuse
/// by the write-statement executor (`crate::cypher::mutation`): it returns every
/// consistent `variable -> entity` binding (all bound node **and** relationship
/// variables), *before* any projection, so the caller can drive `SET` / `DELETE`
/// off the matched entities.
///
/// Current-state only: patterns are matched against the live graph. The caller
/// is responsible for rejecting unsupported reading shapes (variable-length
/// relationships, etc.) before calling.
///
/// # Errors
///
/// Returns [`CypherError::SemanticError`] for a structurally malformed pattern
/// and [`CypherError::UnsupportedFeature`] if the intermediate binding count
/// exceeds the configured cap (mirroring [`evaluate`]).
pub(crate) fn match_bindings(
    db: &AletheiaDB,
    pattern: &[CypherPattern],
    where_clause: Option<&CypherExpr>,
    params: &HashMap<String, CypherParameterValue>,
) -> Result<Vec<Binding>, CypherError> {
    // Materialize the current-state node set once (sorted by id for a
    // deterministic candidate enumeration and stable output order), mirroring
    // `evaluate`.
    let mut nodes: Vec<Node> = db.current.all_nodes().collect();
    nodes.sort_by_key(|n| n.id);
    let mut node_by_id: HashMap<NodeId, usize> = HashMap::with_capacity(nodes.len());
    for (idx, node) in nodes.iter().enumerate() {
        node_by_id.insert(node.id, idx);
    }

    let binding_cap = db.historical.read().max_schema_as_of_entities();

    let eval = MultiEval {
        db,
        params,
        nodes,
        node_by_id,
    };

    // Per-pattern first-node candidate memoization (base-independent scan).
    let mut first_memos: Vec<Vec<Node>> = Vec::with_capacity(pattern.len());
    for p in pattern {
        let memo = match p.elements.first() {
            Some(CypherPatternElement::Node(first)) => eval.scan_node_candidates(first)?,
            _ => Vec::new(),
        };
        first_memos.push(memo);
    }

    // Nested-loop binding matcher: extend the binding set by each pattern.
    let mut env: Vec<PartialBinding> = vec![PartialBinding::default()];
    for (p, first_memo) in pattern.iter().zip(first_memos.iter()) {
        let mut next: Vec<PartialBinding> = Vec::new();
        for base in &env {
            eval.match_pattern_extends(p, base, first_memo, &mut next)?;
            if next.len() > binding_cap {
                return Err(CypherError::UnsupportedFeature(format!(
                    "write reading pattern exceeds {binding_cap} intermediate bindings; add a \
                     more selective WHERE filter or labels to narrow the match (configurable via \
                     max_schema_as_of_entities)"
                )));
            }
        }
        env = next;
    }

    // WHERE filter (three-valued: keep a row iff its predicate is `Tri::True`).
    if let Some(predicate) = where_clause {
        let mut filtered = Vec::with_capacity(env.len());
        for binding in env {
            if eval.eval_predicate(predicate, &binding.vars)? == Tri::True {
                filtered.push(binding);
            }
        }
        env = filtered;
    }

    Ok(env.into_iter().map(|b| b.vars).collect())
}

/// Convert a Cypher literal (resolving a `$param`) into a [`PropertyValue`]
/// (Issue #560).
///
/// Shared by the multi-pattern evaluator and the write-statement executor so
/// both resolve inline/`SET`/`CREATE` values identically.
///
/// # Errors
///
/// Returns [`CypherError::ParameterError`] if a referenced `$param` is unbound.
pub(crate) fn cypher_value_to_property(
    value: &CypherValue,
    params: &HashMap<String, CypherParameterValue>,
) -> Result<PropertyValue, CypherError> {
    Ok(match value {
        CypherValue::Null => PropertyValue::Null,
        CypherValue::Bool(b) => PropertyValue::Bool(*b),
        CypherValue::Int(n) => PropertyValue::Int(*n),
        CypherValue::Float(f) => PropertyValue::Float(*f),
        CypherValue::String(s) => PropertyValue::String(std::sync::Arc::from(s.as_str())),
        CypherValue::Vector(v) => PropertyValue::Vector(std::sync::Arc::clone(v)),
        CypherValue::Parameter(name) => {
            let param = params.get(name).ok_or_else(|| {
                CypherError::ParameterError(format!("unbound parameter: ${name}"))
            })?;
            param_to_property(param)
        }
    })
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
    /// each resulting [`PartialBinding`] to `out`.
    ///
    /// `first_memo` is the pattern's first-node candidate list precomputed by an
    /// unbound scan (base-independent, so computed once per pattern and reused
    /// across every outer binding -- FIX #6). It is used only when the first
    /// node's variable is not already bound by `base`; a bound first-node
    /// variable resolves to its single existing node instead.
    fn match_pattern_extends(
        &self,
        pattern: &CypherPattern,
        base: &PartialBinding,
        first_memo: &[Node],
        out: &mut Vec<PartialBinding>,
    ) -> Result<(), CypherError> {
        let Some(CypherPatternElement::Node(first)) = pattern.elements.first() else {
            return Err(CypherError::SemanticError(
                "a graph pattern must start with a node".to_string(),
            ));
        };

        // Candidate first nodes: the single already-bound node (if the first
        // variable is bound in `base`), otherwise the memoized unbound scan.
        let bound_single: Vec<Node>;
        let candidates: &[Node] = match &first.variable {
            Some(var) => match lookup(&base.vars, var) {
                Some(EntityResult::Node(n)) => {
                    if self.node_element_matches(n, first)? {
                        bound_single = vec![n.clone()];
                        &bound_single
                    } else {
                        return Ok(());
                    }
                }
                // A variable already bound to an edge cannot also be a node.
                Some(_) => return Ok(()),
                None => first_memo,
            },
            None => first_memo,
        };

        for node in candidates {
            let mut vars = base.vars.clone();
            if let Some(var) = &first.variable
                && lookup(&vars, var).is_none()
            {
                vars.push((var.clone(), EntityResult::Node(node.clone())));
            }
            let binding = PartialBinding {
                vars,
                used_edges: base.used_edges.clone(),
            };
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
        binding: PartialBinding,
        current: NodeId,
        out: &mut Vec<PartialBinding>,
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
            // Relationship-uniqueness (FIX #1): an edge may be traversed at most
            // once per MATCH clause. Skip an edge this branch has already used
            // (named or anonymous relationship element alike).
            if binding.used_edges.contains(&edge.id) {
                continue;
            }
            let Some(far_node) = self.node_ref(far_id).cloned() else {
                // Orphaned edge whose endpoint is missing from current state.
                continue;
            };
            if !self.node_element_matches(&far_node, next_patt)? {
                continue;
            }
            let mut vars = binding.vars.clone();
            let mut used_edges = binding.used_edges.clone();
            used_edges.insert(edge.id);
            // Unify the far-node variable (a repeated variable must resolve to
            // the same node id).
            if let Some(nv) = &next_patt.variable {
                match lookup(&vars, nv) {
                    Some(existing) => {
                        if existing.node_id() != Some(far_node.id) {
                            continue;
                        }
                    }
                    None => vars.push((nv.clone(), EntityResult::Node(far_node.clone()))),
                }
            }
            // Bind (and unify) the relationship variable.
            if let Some(rv) = &rel.variable {
                match lookup(&vars, rv) {
                    Some(existing) => {
                        if entity_edge_id(existing) != Some(edge.id) {
                            continue;
                        }
                    }
                    None => vars.push((rv.clone(), EntityResult::Edge(edge.clone()))),
                }
            }
            let next = PartialBinding { vars, used_edges };
            self.extend_from(elements, idx + 2, next, far_node.id, out)?;
        }
        Ok(())
    }

    /// Every node satisfying a node element's labels and inline properties.
    ///
    /// Base-independent (it does not consult any binding), so a pattern's first
    /// node scan is computed once and reused across all outer bindings (FIX #6).
    fn scan_node_candidates(&self, patt: &CypherNodePattern) -> Result<Vec<Node>, CypherError> {
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
        cypher_value_to_property(value, self.params)
    }

    // -- Predicate / scalar evaluation ------------------------------------

    /// Evaluate a `WHERE` predicate against a binding using Kleene three-valued
    /// logic (FIX #2): a comparison with a null/absent operand yields
    /// [`Tri::Null`], `NOT Null` is `Null`, and the caller keeps a row only when
    /// the result is [`Tri::True`].
    fn eval_predicate(&self, expr: &CypherExpr, binding: &Binding) -> Result<Tri, CypherError> {
        match expr {
            CypherExpr::Comparison { left, op, right } => {
                // Edge-property leaf (Issue #3622): when one side is a property
                // access on a variable bound to an EDGE, evaluate with the shared
                // openCypher **node**-semantics -- definite True/False (never the
                // three-valued Null used for node leaves), `Ne` on an absent
                // property includes, and reserved structural fields resolve
                // against the edge struct. This makes SQL == AQL == Cypher for
                // edge predicates. Node-variable leaves fall through unchanged.
                if let Some((edge, prop)) = edge_property_operand(left, binding) {
                    let rhs = self.eval_value(right, binding)?;
                    return Ok(Tri::from_bool(edge_leaf_compare(
                        edge,
                        prop,
                        rhs.as_ref(),
                        *op,
                    )));
                }
                if let Some((edge, prop)) = edge_property_operand(right, binding) {
                    let lhs = self.eval_value(left, binding)?;
                    return Ok(Tri::from_bool(edge_leaf_compare(
                        edge,
                        prop,
                        lhs.as_ref(),
                        super::converter::flip_cypher_comp_op(*op),
                    )));
                }
                let l = self.eval_value(left, binding)?;
                let r = self.eval_value(right, binding)?;
                match (l, r) {
                    (Some(a), Some(b)) => Ok(Tri::from_bool(compare(&a, &b, *op))),
                    // A comparison with a null/absent operand is Null, not False.
                    _ => Ok(Tri::Null),
                }
            }
            // Both sides are evaluated (no short-circuit) so Kleene logic is
            // exact: `False AND Null = False`, `True OR Null = True`.
            CypherExpr::And(a, b) => Ok(self
                .eval_predicate(a, binding)?
                .and(self.eval_predicate(b, binding)?)),
            CypherExpr::Or(a, b) => Ok(self
                .eval_predicate(a, binding)?
                .or(self.eval_predicate(b, binding)?)),
            CypherExpr::Not(inner) => Ok(self.eval_predicate(inner, binding)?.not()),
            // IS NULL / IS NOT NULL are always definite (never Null).
            CypherExpr::IsNull(inner) => {
                Ok(Tri::from_bool(self.eval_value(inner, binding)?.is_none()))
            }
            CypherExpr::IsNotNull(inner) => {
                Ok(Tri::from_bool(self.eval_value(inner, binding)?.is_some()))
            }
            CypherExpr::In { expr, values } => {
                // Edge-property IN (Issue #3622): definite node-semantics bool
                // so `NOT (r.<absent> IN [...])` includes, matching AQL / SQL.
                if let Some((edge, prop)) = edge_property_operand(expr, binding) {
                    let candidates: Vec<Option<PropertyValue>> = values
                        .iter()
                        .map(|c| self.eval_value(c, binding))
                        .collect::<Result<_, _>>()?;
                    let result = match edge_leaf_value(edge, prop) {
                        Some(v) => edge_leaf_in(&v, &candidates),
                        None => false,
                    };
                    return Ok(Tri::from_bool(result));
                }
                // A null subject makes IN Null.
                let Some(needle) = self.eval_value(expr, binding)? else {
                    return Ok(Tri::Null);
                };
                // Three-valued IN (openCypher): a match short-circuits to True;
                // otherwise a null list element makes the result Null (not
                // False), so `5 IN [1, null]` is Null and drops under NOT.
                let mut saw_null = false;
                for candidate in values {
                    match self.eval_value(candidate, binding)? {
                        Some(v) if loosely_equal(&needle, &v) => return Ok(Tri::True),
                        Some(_) => {}
                        None => saw_null = true,
                    }
                }
                Ok(if saw_null { Tri::Null } else { Tri::False })
            }
            // String predicates with a null/non-string subject are Null -- unless
            // the subject is an edge property, which uses definite node-semantics
            // (Issue #3622) so `NOT (r.<absent> CONTAINS 'x')` includes.
            CypherExpr::Contains { expr, substring } => {
                if let Some((edge, prop)) = edge_property_operand(expr, binding) {
                    return Ok(Tri::from_bool(edge_leaf_string_op(edge, prop, |s| {
                        s.contains(substring.as_str())
                    })));
                }
                Ok(match self.eval_string(expr, binding)? {
                    Some(s) => Tri::from_bool(s.contains(substring)),
                    None => Tri::Null,
                })
            }
            CypherExpr::StartsWith { expr, prefix } => {
                if let Some((edge, prop)) = edge_property_operand(expr, binding) {
                    return Ok(Tri::from_bool(edge_leaf_string_op(edge, prop, |s| {
                        s.starts_with(prefix.as_str())
                    })));
                }
                Ok(match self.eval_string(expr, binding)? {
                    Some(s) => Tri::from_bool(s.starts_with(prefix)),
                    None => Tri::Null,
                })
            }
            CypherExpr::EndsWith { expr, suffix } => {
                if let Some((edge, prop)) = edge_property_operand(expr, binding) {
                    return Ok(Tri::from_bool(edge_leaf_string_op(edge, prop, |s| {
                        s.ends_with(suffix.as_str())
                    })));
                }
                Ok(match self.eval_string(expr, binding)? {
                    Some(s) => Tri::from_bool(s.ends_with(suffix)),
                    None => Tri::Null,
                })
            }
            CypherExpr::Grouped(inner) => self.eval_predicate(inner, binding),
            CypherExpr::Value(CypherValue::Bool(b)) => Ok(Tri::from_bool(*b)),
            other => match self.eval_value(other, binding)? {
                Some(PropertyValue::Bool(b)) => Ok(Tri::from_bool(b)),
                None => Ok(Tri::Null),
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
        rows: &mut Vec<(Binding, QueryRow)>,
        ret: &CypherReturn,
    ) -> Result<(), CypherError> {
        // Pair each row with its pre-computed sort keys, sort the owned tuples
        // by a stable comparator, then rebuild `rows`. Computing keys up front
        // keeps the comparator infallible (evaluation errors surface here,
        // before sorting) and avoids any panicking in-place permutation (FIX #9).
        let mut keyed: Vec<KeyedRow> = Vec::with_capacity(rows.len());
        for (binding, row) in rows.drain(..) {
            let mut key = Vec::with_capacity(ret.order_by.len());
            for item in &ret.order_by {
                key.push(self.eval_value(&item.expr, &binding)?);
            }
            keyed.push((key, (binding, row)));
        }

        keyed.sort_by(|a, b| compare_order_keys(&a.0, &b.0, ret));
        rows.extend(keyed.into_iter().map(|(_, row)| row));
        Ok(())
    }
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
///
/// For an EDGE, reserved structural fields (`type`/`label`/`source`/`target`/
/// `id`) resolve against the edge struct and **shadow** any user property of the
/// same name (Issue #3622), matching the SQL/AQL edge lane -- so
/// `ORDER BY r.type` / `WHERE r.target = ...` see the label / endpoint rather
/// than a (usually absent) user property. A genuine user key falls through to
/// the edge's properties.
fn entity_property(entity: &EntityResult, property: &str) -> Option<PropertyValue> {
    match entity {
        EntityResult::Node(n) => n.get_property(property).cloned(),
        EntityResult::Edge(e) => {
            crate::query::executor::iterators::edge_structural_value(e, property)
                .or_else(|| e.get_property(property).cloned())
        }
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

/// Entity variable names for `RETURN *`, sorted **alphabetically** (FIX #7).
///
/// openCypher / Neo4j return `*` variables in alphabetical order for a
/// deterministic, declaration-order-independent projection. The names are the
/// node and relationship variables declared across every pattern (deduped).
fn return_star_var_order(patterns: &[CypherPattern]) -> Vec<String> {
    let mut seen = HashSet::new();
    for pattern in patterns {
        for element in &pattern.elements {
            let var = match element {
                CypherPatternElement::Node(n) => n.variable.as_ref(),
                CypherPatternElement::Relationship(r) => r.variable.as_ref(),
            };
            if let Some(v) = var {
                seen.insert(v.clone());
            }
        }
    }
    let mut order: Vec<String> = seen.into_iter().collect();
    order.sort();
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
pub(crate) fn loosely_equal(a: &PropertyValue, b: &PropertyValue) -> bool {
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
/// If `expr` is a property access on a variable bound to an EDGE, return the
/// edge and property name (Issue #3622); otherwise `None`.
fn edge_property_operand<'b>(
    expr: &'b CypherExpr,
    binding: &'b Binding,
) -> Option<(&'b Edge, &'b str)> {
    let CypherExpr::Property { variable, property } = expr else {
        return None;
    };
    match lookup(binding, variable) {
        Some(EntityResult::Edge(e)) => Some((e, property.as_str())),
        _ => None,
    }
}

/// Compare an edge property leaf `edge.prop <op> other` with openCypher
/// **node**-semantics (Issue #3622): reserved structural fields shadow user
/// props; an absent property makes `Ne` true and every other operator false
/// (never the three-valued Null of the node-leaf path). A null right-hand
/// operand makes the comparison not-true.
///
/// AQL/Cypher edge-predicate asymmetry: this Cypher multi-variable path rejects
/// temporal `AS OF` for the *whole* clause (node and edge alike; see the module
/// house rule), so an edge predicate here always evaluates the current-state
/// edge. Point-in-time edge predicates (the edge reconstructed at an AS-OF
/// coordinate) are an AQL capability via `Predicate::EdgeScoped`; the semantics
/// on the edge value itself are identical across both languages and SQL.
fn edge_leaf_compare(
    edge: &Edge,
    prop: &str,
    other: Option<&PropertyValue>,
    op: CypherCompOp,
) -> bool {
    let Some(other) = other else {
        return false;
    };
    match edge_leaf_value(edge, prop) {
        Some(v) => compare(&v, other, op),
        // Absent property: openCypher node-semantics -- `Ne` includes, all else
        // excludes.
        None => matches!(op, CypherCompOp::Ne),
    }
}

/// Resolve an edge leaf's value (Issue #3622): reserved structural fields
/// (`type`/`label`/`source`/`target`/`id`) shadow user props, otherwise the
/// edge's own properties. Shared by every edge-leaf evaluator so the
/// reserved-vs-user precedence is single-sourced. Returns a [`Cow`] so a user
/// property is borrowed (no clone); only a synthesized structural value is
/// owned.
fn edge_leaf_value<'a>(edge: &'a Edge, prop: &str) -> Option<std::borrow::Cow<'a, PropertyValue>> {
    use std::borrow::Cow;
    if let Some(v) = crate::query::executor::iterators::edge_structural_value(edge, prop) {
        return Some(Cow::Owned(v));
    }
    edge.get_property(prop).map(Cow::Borrowed)
}

/// Evaluate `edge.prop IN candidates` with openCypher **node**-semantics
/// (Issue #3622): an absent property is definite `false` (never three-valued
/// Null), a present value matches iff it loosely-equals any non-null candidate.
/// This keeps `NOT (r.<absent> IN [...])` == `true`, matching AQL / SQL.
fn edge_leaf_in(v: &PropertyValue, candidates: &[Option<PropertyValue>]) -> bool {
    candidates.iter().flatten().any(|cv| loosely_equal(v, cv))
}

/// Evaluate a string edge-leaf op (`CONTAINS`/`STARTS WITH`/`ENDS WITH`) with
/// node-semantics (Issue #3622): a missing or non-string edge value is definite
/// `false`, so `NOT (r.<absent> CONTAINS 'x')` == `true`, matching AQL / SQL.
fn edge_leaf_string_op(edge: &Edge, prop: &str, f: impl FnOnce(&str) -> bool) -> bool {
    match edge_leaf_value(edge, prop).as_deref() {
        Some(PropertyValue::String(s)) => f(s.as_ref()),
        _ => false,
    }
}

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

/// Compute a stable string identity for a projected row, used for `DISTINCT`
/// dedup: bound entity ids plus scalar column values.
///
/// A string key is used because [`PropertyValue`] contains float/vector
/// payloads and does not implement `Hash`/`Eq`; the `{:?}` rendering is
/// deterministic, so equal rows produce equal keys.
fn row_dedup_key(row: &QueryRow) -> String {
    let mut key = String::new();
    if let Some(bindings) = &row.bindings {
        for (name, entity) in bindings {
            let (kind, id) = entity_kind_id(entity);
            key.push_str(&format!("b:{name}={kind}:{id};"));
        }
    }
    if let Some(columns) = &row.columns {
        for (name, value) in columns {
            key.push_str(&format!("c:{name}={value:?};"));
        }
    }
    key
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

/// Deduplicate projected rows by their stable key (first occurrence wins),
/// using a [`HashSet`] for O(n) total work (FIX #8).
fn distinct_rows(rows: Vec<(Binding, QueryRow)>) -> Vec<(Binding, QueryRow)> {
    let mut seen: HashSet<String> = HashSet::with_capacity(rows.len());
    let mut out = Vec::with_capacity(rows.len());
    for (binding, row) in rows {
        if seen.insert(row_dedup_key(&row)) {
            out.push((binding, row));
        }
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
