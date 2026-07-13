//! Cypher write-statement execution (Issue #560).
//!
//! Executes the mutating Cypher subset -- `CREATE`, `SET`, `DELETE`, and
//! `DETACH DELETE` -- by driving AletheiaDB's **native write APIs** (via a single
//! [`WriteTransaction`](crate::api::transaction::WriteTransaction)) rather than
//! lowering into the read-only [`Query`](crate::query::Query) IR. Going through
//! the native APIs means every mutation records the correct bi-temporal version
//! (a new transaction-time record; a `SET`/`DELETE` supersedes without erasing
//! history) exactly as the direct Rust API would.
//!
//! # Reachability / read-only guarantee
//!
//! This path is reached **only** through
//! [`AletheiaDB::execute_cypher`](crate::db::AletheiaDB::execute_cypher) /
//! `execute_cypher_with_params`. The MCP `query` tool and HTTP `/query` endpoint
//! reject every mutating clause *before the parser runs*
//! (`crate::query::read_only::detect_mutating_clause`), so a write statement can
//! never execute through those read-only surfaces.
//!
//! # Atomicity
//!
//! All writes produced by one statement commit in a single
//! [`AletheiaDB::write`](crate::db::AletheiaDB::write) transaction: they are
//! applied all-or-nothing and share one commit timestamp. The reading `MATCH`
//! is evaluated against current state *before* the write transaction opens
//! (a v1 check-then-act window, acceptable because the writes themselves are
//! atomic).
//!
//! # v1 scope (honest structured rejections, never a silently-wrong write)
//!
//! - `CREATE` of single-labelled nodes and typed, directed relationships, with
//!   inline properties; endpoints may reference matched/earlier-created
//!   variables.
//! - `SET n.prop = value` (PATCH-merge onto the existing property map).
//! - `DELETE` / `DETACH DELETE` of node and relationship variables, with the
//!   openCypher safety rule: a plain `DELETE` of a node that still has
//!   relationships is refused (use `DETACH DELETE`).
//! - `RETURN` of bound variables (bare or `AS`-aliased).
//!
//! Deferred to follow-ups and rejected cleanly: `MERGE`, `REMOVE`, label
//! mutation (`SET n:Label`), whole-entity replacement (`SET n = {...}`),
//! variable-length relationships in a write, and aggregate/property `RETURN`
//! projections.

use std::collections::HashSet;

use crate::api::transaction::{ReadOps, WriteOps};
use crate::core::error::{Error, Result, TransactionError};
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::{PropertyMap, PropertyMapBuilder};
use crate::db::AletheiaDB;
use crate::query::executor::{EntityResult, QueryResults, QueryRow};

use super::ast::{
    CypherDirection, CypherNodePattern, CypherPattern, CypherPatternElement, CypherReturn,
    CypherReturnItem, CypherSetItem, CypherStatement, CypherValue, CypherWriteClause,
    CypherWriteStatement,
};
use super::converter::CypherParameterValue;
use super::error::CypherError;
use super::multi_pattern::{Binding, cypher_value_to_property, match_bindings};

type Params = std::collections::HashMap<String, CypherParameterValue>;

/// Execute a parsed Cypher write statement (Issue #560).
///
/// # Errors
///
/// Returns a query error for any unsupported construct (mapped from
/// [`CypherError`]) or a storage/transaction error from the underlying write.
pub fn execute(
    db: &AletheiaDB,
    statement: &CypherStatement,
    params: &Params,
) -> Result<QueryResults> {
    let CypherStatement::Write(write) = statement else {
        return Err(CypherError::UnsupportedFeature(
            "mutation executor requires a write statement".to_string(),
        )
        .into());
    };

    // Static validation independent of the data (fails before any transaction).
    validate(write)?;

    // Read phase: matched rows drive the write clauses. A statement with no
    // reading part (a bare `CREATE`) runs once against a single empty binding.
    let base_rows: Vec<Binding> = match &write.reading {
        Some(reading) => {
            match_bindings(db, &reading.pattern, reading.where_clause.as_ref(), params)?
        }
        None => vec![Vec::new()],
    };

    // Write phase: apply every clause per row inside ONE transaction so the whole
    // statement commits all-or-nothing. RETURN snapshots are captured here and
    // materialized (with fresh post-commit reads) afterwards.
    let mut return_snapshots: Vec<Vec<(String, EntityResult)>> = Vec::new();
    db.write::<_, (), Error>(|tx| {
        // Track entities deleted by this statement so a later clause/row does not
        // double-delete, and so the plain-DELETE safety check ignores edges this
        // same statement already removed.
        let mut deleted_nodes: HashSet<NodeId> = HashSet::new();
        let mut deleted_edges: HashSet<EdgeId> = HashSet::new();

        for base in &base_rows {
            let mut binding = base.clone();
            for clause in &write.clauses {
                apply_clause(
                    tx,
                    clause,
                    &mut binding,
                    params,
                    &mut deleted_nodes,
                    &mut deleted_edges,
                )?;
            }
            if let Some(ret) = &write.return_clause {
                return_snapshots.push(collect_return_snapshots(ret, &binding)?);
            }
        }
        Ok(())
    })?;

    // Materialize RETURN rows after commit: re-read each entity for its
    // post-write state, falling back to the captured snapshot for entities the
    // statement deleted.
    let mut rows = Vec::with_capacity(return_snapshots.len());
    for snapshot in return_snapshots {
        rows.push(materialize_row(db, snapshot));
    }
    Ok(QueryResults::from_rows(rows))
}

// ---------------------------------------------------------------------------
// Static validation
// ---------------------------------------------------------------------------

/// Reject unsupported write shapes up front, before any transaction opens.
fn validate(write: &CypherWriteStatement) -> std::result::Result<(), CypherError> {
    if let Some(reading) = &write.reading {
        for pattern in &reading.pattern {
            reject_varlength(pattern, "the reading MATCH of a write statement")?;
        }
    }
    for clause in &write.clauses {
        if let CypherWriteClause::Create(patterns) = clause {
            for pattern in patterns {
                reject_varlength(pattern, "a CREATE pattern")?;
                validate_create_pattern(pattern)?;
            }
        }
    }
    if let Some(ret) = &write.return_clause {
        validate_return(ret)?;
    }
    Ok(())
}

/// Reject a variable-length relationship anywhere in a pattern.
fn reject_varlength(pattern: &CypherPattern, ctx: &str) -> std::result::Result<(), CypherError> {
    for element in &pattern.elements {
        if let CypherPatternElement::Relationship(rel) = element
            && rel.depth.is_some()
        {
            return Err(CypherError::UnsupportedFeature(format!(
                "variable-length relationships are not supported in {ctx}"
            )));
        }
    }
    Ok(())
}

/// Statically validate a `CREATE` pattern's relationships (direction + type).
fn validate_create_pattern(pattern: &CypherPattern) -> std::result::Result<(), CypherError> {
    for element in &pattern.elements {
        if let CypherPatternElement::Relationship(rel) = element {
            if matches!(rel.direction, CypherDirection::Both) {
                return Err(CypherError::UnsupportedFeature(
                    "CREATE requires a directed relationship (use -> or <-), not an undirected -"
                        .to_string(),
                ));
            }
            if rel.rel_types.len() != 1 {
                return Err(CypherError::UnsupportedFeature(
                    "CREATE requires exactly one relationship type, e.g. -[:KNOWS]->".to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// Validate the `RETURN` of a write statement: only bound variables (bare or
/// `AS`-aliased) or `*`, with no ordering/pagination/deduplication in v1.
fn validate_return(ret: &CypherReturn) -> std::result::Result<(), CypherError> {
    if ret.distinct || !ret.order_by.is_empty() || ret.skip.is_some() || ret.limit.is_some() {
        return Err(CypherError::UnsupportedFeature(
            "DISTINCT / ORDER BY / SKIP / LIMIT are not supported on the RETURN of a write \
             statement in v1"
                .to_string(),
        ));
    }
    for item in &ret.items {
        match item {
            CypherReturnItem::Star | CypherReturnItem::Variable(_) => {}
            CypherReturnItem::Expression {
                expr: super::ast::CypherExpr::Variable(_),
                ..
            } => {}
            _ => {
                return Err(CypherError::UnsupportedFeature(
                    "RETURN after a write supports only bound variables (optionally AS-aliased) \
                     or *; property, function, and aggregate projections are not supported in v1"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Clause application
// ---------------------------------------------------------------------------

fn apply_clause(
    tx: &mut crate::api::transaction::WriteTransaction,
    clause: &CypherWriteClause,
    binding: &mut Binding,
    params: &Params,
    deleted_nodes: &mut HashSet<NodeId>,
    deleted_edges: &mut HashSet<EdgeId>,
) -> Result<()> {
    match clause {
        CypherWriteClause::Create(patterns) => {
            for pattern in patterns {
                create_pattern(tx, pattern, binding, params)?;
            }
            Ok(())
        }
        CypherWriteClause::Set(items) => apply_set(tx, items, binding, params, deleted_nodes),
        CypherWriteClause::Delete { detach, targets } => {
            apply_delete(tx, *detach, targets, binding, deleted_nodes, deleted_edges)
        }
    }
}

/// Create the nodes and relationships described by one `CREATE` pattern,
/// binding each named element into `binding`.
fn create_pattern(
    tx: &mut crate::api::transaction::WriteTransaction,
    pattern: &CypherPattern,
    binding: &mut Binding,
    params: &Params,
) -> Result<()> {
    let mut prev_node: Option<NodeId> = None;
    let mut idx = 0;
    while idx < pattern.elements.len() {
        match &pattern.elements[idx] {
            CypherPatternElement::Node(node) => {
                let id = resolve_or_create_node(tx, node, binding, params)?;
                prev_node = Some(id);
                idx += 1;
            }
            CypherPatternElement::Relationship(rel) => {
                let left = prev_node.ok_or_else(|| {
                    Error::from(CypherError::SemanticError(
                        "a relationship must be preceded by a node in a CREATE pattern".to_string(),
                    ))
                })?;
                let Some(CypherPatternElement::Node(right_pat)) = pattern.elements.get(idx + 1)
                else {
                    return Err(CypherError::SemanticError(
                        "a relationship must be followed by a node in a CREATE pattern".to_string(),
                    )
                    .into());
                };
                let right = resolve_or_create_node(tx, right_pat, binding, params)?;

                // Direction/type validated statically; exactly one type present.
                let rel_type = &rel.rel_types[0];
                let (source, target) = match rel.direction {
                    CypherDirection::Outgoing => (left, right),
                    CypherDirection::Incoming => (right, left),
                    CypherDirection::Both => {
                        return Err(CypherError::UnsupportedFeature(
                            "CREATE requires a directed relationship".to_string(),
                        )
                        .into());
                    }
                };
                let props = build_props(&rel.properties, params)?;
                let edge_id = tx.create_edge(source, target, rel_type, props)?;
                if let Some(var) = &rel.variable {
                    bind(binding, var, EntityResult::EdgeId(edge_id));
                }
                prev_node = Some(right);
                idx += 2;
            }
        }
    }
    Ok(())
}

/// Resolve a node element to an existing bound node, or create a new node and
/// bind it. A bound variable is reused; an unbound/anonymous node is created and
/// must carry exactly one label.
fn resolve_or_create_node(
    tx: &mut crate::api::transaction::WriteTransaction,
    node: &CypherNodePattern,
    binding: &mut Binding,
    params: &Params,
) -> Result<NodeId> {
    if let Some(var) = &node.variable
        && let Some(existing) = lookup(binding, var)
    {
        // Reusing a bound variable: it must already be a node, and CREATE must
        // not also (re)specify a label/properties on it.
        let Some(id) = node_id_of(existing) else {
            return Err(CypherError::SemanticError(format!(
                "variable '{var}' is bound to a relationship and cannot be used as a node in CREATE"
            ))
            .into());
        };
        if !node.labels.is_empty() || !node.properties.is_empty() {
            return Err(CypherError::UnsupportedFeature(format!(
                "CREATE cannot re-declare labels or properties on already-bound variable '{var}'"
            ))
            .into());
        }
        return Ok(id);
    }

    // Creating a new node: AletheiaDB nodes are single-labelled.
    if node.labels.len() != 1 {
        return Err(CypherError::UnsupportedFeature(
            "CREATE requires exactly one label per new node (AletheiaDB nodes are single-labelled)"
                .to_string(),
        )
        .into());
    }
    let props = build_props(&node.properties, params)?;
    let id = tx.create_node(&node.labels[0], props)?;
    if let Some(var) = &node.variable {
        bind(binding, var, EntityResult::NodeId(id));
    }
    Ok(id)
}

/// Apply a `SET` clause: PATCH-merge each `n.prop = value` onto its bound
/// entity. Assignments to the same variable are coalesced into one update so a
/// multi-property `SET` records a single new version.
fn apply_set(
    tx: &mut crate::api::transaction::WriteTransaction,
    items: &[CypherSetItem],
    binding: &Binding,
    params: &Params,
    deleted_nodes: &HashSet<NodeId>,
) -> Result<()> {
    // Accumulate per-target property maps in first-seen order.
    let mut node_updates: Vec<(NodeId, PropertyMapBuilder)> = Vec::new();
    let mut edge_updates: Vec<(EdgeId, PropertyMapBuilder)> = Vec::new();

    for item in items {
        let entity = lookup(binding, &item.variable).ok_or_else(|| {
            Error::from(CypherError::SemanticError(format!(
                "SET references unbound variable '{}'",
                item.variable
            )))
        })?;
        let value = cypher_value_to_property(&item.value, params)?;

        if let Some(node_id) = node_id_of(entity) {
            if deleted_nodes.contains(&node_id) {
                continue; // no-op on an entity this statement already deleted
            }
            let slot = match node_updates.iter_mut().find(|(id, _)| *id == node_id) {
                Some((_, builder)) => builder,
                None => {
                    node_updates.push((node_id, PropertyMapBuilder::new()));
                    &mut node_updates.last_mut().expect("just pushed").1
                }
            };
            take_insert(slot, &item.property, value);
        } else if let Some(edge_id) = edge_id_of(entity) {
            let slot = match edge_updates.iter_mut().find(|(id, _)| *id == edge_id) {
                Some((_, builder)) => builder,
                None => {
                    edge_updates.push((edge_id, PropertyMapBuilder::new()));
                    &mut edge_updates.last_mut().expect("just pushed").1
                }
            };
            take_insert(slot, &item.property, value);
        } else {
            return Err(CypherError::SemanticError(format!(
                "SET target variable '{}' is not bound to a node or relationship",
                item.variable
            ))
            .into());
        }
    }

    for (node_id, builder) in node_updates {
        tx.update_node(node_id, builder.build())?;
    }
    for (edge_id, builder) in edge_updates {
        tx.update_edge(edge_id, builder.build())?;
    }
    Ok(())
}

/// Apply a `DELETE` / `DETACH DELETE` clause.
///
/// Relationships are deleted before nodes so that `DELETE r, a` and `DELETE a, r`
/// behave identically (openCypher deletes are order-independent). A plain
/// `DELETE` of a node that still has relationships (excluding any this statement
/// already deleted) is refused; `DETACH DELETE` cascade-removes them.
fn apply_delete(
    tx: &mut crate::api::transaction::WriteTransaction,
    detach: bool,
    targets: &[String],
    binding: &Binding,
    deleted_nodes: &mut HashSet<NodeId>,
    deleted_edges: &mut HashSet<EdgeId>,
) -> Result<()> {
    // Partition targets into edges and nodes.
    let mut edge_targets: Vec<EdgeId> = Vec::new();
    let mut node_targets: Vec<NodeId> = Vec::new();
    for var in targets {
        let entity = lookup(binding, var).ok_or_else(|| {
            Error::from(CypherError::SemanticError(format!(
                "DELETE references unbound variable '{var}'"
            )))
        })?;
        if let Some(edge_id) = edge_id_of(entity) {
            edge_targets.push(edge_id);
        } else if let Some(node_id) = node_id_of(entity) {
            node_targets.push(node_id);
        } else {
            return Err(CypherError::SemanticError(format!(
                "DELETE target variable '{var}' is not bound to a node or relationship"
            ))
            .into());
        }
    }

    // Delete relationships first.
    for edge_id in edge_targets {
        if deleted_edges.contains(&edge_id) || tx.get_edge(edge_id).is_err() {
            continue; // already gone
        }
        tx.delete_edge(edge_id)?;
        deleted_edges.insert(edge_id);
    }

    // Then nodes.
    for node_id in node_targets {
        if deleted_nodes.contains(&node_id) || tx.get_node(node_id).is_err() {
            continue;
        }
        if detach {
            // Record the connected edges as deleted so a later plain DELETE does
            // not mis-count them, then cascade.
            for edge_id in connected_edges(tx, node_id)? {
                deleted_edges.insert(edge_id);
            }
            tx.delete_node_cascade(node_id)?;
        } else {
            // openCypher safety rule: refuse to orphan edges. Ignore edges this
            // same statement already deleted.
            let remaining: Vec<EdgeId> = connected_edges(tx, node_id)?
                .into_iter()
                .filter(|e| !deleted_edges.contains(e))
                .collect();
            if !remaining.is_empty() {
                return Err(TransactionError::ValidationFailed {
                    reason: format!(
                        "Cannot delete node {} because it still has {} relationship(s); use \
                         DETACH DELETE to remove the node and its relationships",
                        node_id.as_u64(),
                        remaining.len()
                    ),
                }
                .into());
            }
            tx.delete_node(node_id)?;
        }
        deleted_nodes.insert(node_id);
    }
    Ok(())
}

/// Distinct edge ids connected to a node (a self-loop counts once).
fn connected_edges(
    tx: &mut crate::api::transaction::WriteTransaction,
    node_id: NodeId,
) -> Result<Vec<EdgeId>> {
    let mut ids = tx.get_outgoing_edges(node_id)?;
    ids.extend(tx.get_incoming_edges(node_id)?);
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

// ---------------------------------------------------------------------------
// RETURN
// ---------------------------------------------------------------------------

/// Capture the bound entities a `RETURN` projects, as `(output_name, snapshot)`
/// pairs, from the row's final binding. Snapshots are materialized (re-read)
/// after commit.
fn collect_return_snapshots(
    ret: &CypherReturn,
    binding: &Binding,
) -> Result<Vec<(String, EntityResult)>> {
    let mut out = Vec::new();
    for item in &ret.items {
        match item {
            CypherReturnItem::Star => {
                for (name, entity) in binding {
                    out.push((name.clone(), entity.clone()));
                }
            }
            CypherReturnItem::Variable(name) => {
                out.push((name.clone(), lookup_owned(binding, name)?));
            }
            CypherReturnItem::Expression {
                expr: super::ast::CypherExpr::Variable(name),
                alias,
            } => {
                let output = alias.clone().unwrap_or_else(|| name.clone());
                out.push((output, lookup_owned(binding, name)?));
            }
            _ => {
                return Err(CypherError::UnsupportedFeature(
                    "RETURN after a write supports only bound variables or *".to_string(),
                )
                .into());
            }
        }
    }
    Ok(out)
}

/// Materialize one RETURN row: re-read each captured entity for its post-write
/// state, falling back to the captured snapshot when the entity was deleted.
fn materialize_row(db: &AletheiaDB, snapshot: Vec<(String, EntityResult)>) -> QueryRow {
    let fresh: Vec<(String, EntityResult)> = snapshot
        .into_iter()
        .map(|(name, entity)| (name, refresh_entity(db, entity)))
        .collect();

    if fresh.len() == 1 {
        let (_, entity) = fresh.into_iter().next().expect("len == 1");
        QueryRow::from_entity(entity)
    } else {
        QueryRow::from_bindings(fresh, None)
    }
}

/// Re-read an entity's current state by id, keeping the snapshot if it is gone.
fn refresh_entity(db: &AletheiaDB, entity: EntityResult) -> EntityResult {
    if let Some(node_id) = node_id_of(&entity) {
        return db
            .get_node(node_id)
            .map(EntityResult::Node)
            .unwrap_or(entity);
    }
    if let Some(edge_id) = edge_id_of(&entity) {
        return db
            .get_edge(edge_id)
            .map(EntityResult::Edge)
            .unwrap_or(entity);
    }
    entity
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Build a [`PropertyMap`] from pattern/`SET` `(key, value)` pairs.
fn build_props(
    props: &[(String, CypherValue)],
    params: &Params,
) -> std::result::Result<PropertyMap, CypherError> {
    let mut builder = PropertyMapBuilder::new();
    for (key, value) in props {
        let pv = cypher_value_to_property(value, params)?;
        builder = builder.insert(key, pv);
    }
    Ok(builder.build())
}

/// Insert into a builder held behind a mutable reference (builder methods are
/// by-value, so swap through a temporary).
fn take_insert(
    slot: &mut PropertyMapBuilder,
    key: &str,
    value: crate::core::property::PropertyValue,
) {
    let taken = std::mem::take(slot);
    *slot = taken.insert(key, value);
}

/// Look up a variable's binding by name.
fn lookup<'b>(binding: &'b Binding, var: &str) -> Option<&'b EntityResult> {
    binding.iter().find(|(name, _)| name == var).map(|(_, e)| e)
}

/// Look up a variable, cloning its binding, erroring if unbound.
fn lookup_owned(binding: &Binding, var: &str) -> Result<EntityResult> {
    lookup(binding, var).cloned().ok_or_else(|| {
        CypherError::SemanticError(format!("RETURN references unbound variable '{var}'")).into()
    })
}

/// Bind (or rebind) a variable to an entity.
fn bind(binding: &mut Binding, var: &str, entity: EntityResult) {
    if let Some(slot) = binding.iter_mut().find(|(name, _)| name == var) {
        slot.1 = entity;
    } else {
        binding.push((var.to_string(), entity));
    }
}

/// The node id an [`EntityResult`] refers to, if it is a node.
fn node_id_of(entity: &EntityResult) -> Option<NodeId> {
    match entity {
        EntityResult::Node(n) => Some(n.id),
        EntityResult::NodeId(id) => Some(*id),
        _ => None,
    }
}

/// The edge id an [`EntityResult`] refers to, if it is an edge.
fn edge_id_of(entity: &EntityResult) -> Option<EdgeId> {
    match entity {
        EntityResult::Edge(e) => Some(e.id),
        EntityResult::EdgeId(id) => Some(*id),
        _ => None,
    }
}
