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
//! - `MERGE <pattern> [ON CREATE SET ...] [ON MATCH SET ...]` (Issue #3548):
//!   match the pattern if present, otherwise create the *whole* pattern
//!   (openCypher whole-pattern semantics). A bare match records no new version;
//!   a create records new versions; `ON MATCH SET` on a matched entity records
//!   a version (it is an update). Matching reuses the current-state matcher
//!   (`match_bindings`) as a pre-transaction (check-then-act) read.
//!
//!   For a **single-node** MERGE pattern (one node, no relationship), matching
//!   additionally consults a statement-local ledger of nodes created earlier in
//!   the same statement, so repeated single-node MERGEs of the same key --
//!   across multiple reading-clause rows OR multiple MERGE clauses -- dedup to a
//!   single node (openCypher requires exactly one). A **relationship/path**
//!   MERGE keeps whole-pattern semantics: the end node is (re)created per row
//!   when the whole path is absent (the well-known openCypher MERGE-on-a-path
//!   behavior), so it is *not* deduped against the ledger.
//!
//!   `ON` and `MERGE` are now **reserved keywords** (a backward-incompatible
//!   consequence of adding the MERGE grammar): they can no longer be used as
//!   bare identifiers, labels, relationship types, or property keys.
//!
//! Deferred to follow-ups and rejected cleanly: `REMOVE`, label mutation
//! (`SET n:Label`), whole-entity replacement (`SET n = {...}`), variable-length
//! relationships in a write, multi-label MERGE nodes, and aggregate/property
//! `RETURN` projections.

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
use super::multi_pattern::{Binding, cypher_value_to_property, loosely_equal, match_bindings};

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
        // Relationships CREATEd by THIS statement, as (edge, source, target).
        // The committed adjacency reads used by the plain-DELETE safety check do
        // not yet see a buffered same-statement CREATE, so this statement-local
        // ledger lets a plain DELETE of such an endpoint refuse cleanly instead
        // of aborting at commit with a cryptic dangling-edge error (Minor 1).
        let mut created_edges: Vec<(EdgeId, NodeId, NodeId)> = Vec::new();
        // Nodes CREATEd by THIS statement, as (node, label, resolved properties).
        // A single-node MERGE consults this ledger (in addition to committed
        // current state) so repeated single-node MERGEs of the same key across
        // rows/clauses dedup to one node -- the committed `match_bindings` read
        // never sees this transaction's own buffered creates (Core bug fix).
        let mut created_nodes: Vec<(NodeId, String, PropertyMap)> = Vec::new();

        for base in &base_rows {
            let mut binding = base.clone();
            for clause in &write.clauses {
                apply_clause(
                    tx,
                    db,
                    clause,
                    &mut binding,
                    params,
                    &mut deleted_nodes,
                    &mut deleted_edges,
                    &mut created_edges,
                    &mut created_nodes,
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
        match clause {
            CypherWriteClause::Create(patterns) => {
                for pattern in patterns {
                    reject_varlength(pattern, "a CREATE pattern")?;
                    validate_create_pattern(pattern)?;
                }
            }
            CypherWriteClause::Merge { pattern, .. } => {
                reject_varlength(pattern, "a MERGE pattern")?;
                validate_create_pattern(pattern)?;
                validate_merge_pattern(pattern)?;
            }
            CypherWriteClause::Set(_) | CypherWriteClause::Delete { .. } => {}
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

/// Statically validate a `MERGE` pattern's nodes: AletheiaDB nodes are
/// single-labelled, so a multi-label node (`(n:A:B)`) can never be created and
/// is rejected before any transaction opens. A node with zero labels is allowed
/// (it must resolve to a variable already bound by a leading `MATCH`; that is
/// checked at runtime).
fn validate_merge_pattern(pattern: &CypherPattern) -> std::result::Result<(), CypherError> {
    for element in &pattern.elements {
        if let CypherPatternElement::Node(node) = element
            && node.labels.len() > 1
        {
            return Err(CypherError::UnsupportedFeature(
                "MERGE does not support multi-label nodes (AletheiaDB nodes are single-labelled)"
                    .to_string(),
            ));
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

#[allow(clippy::too_many_arguments)]
fn apply_clause(
    tx: &mut crate::api::transaction::WriteTransaction,
    db: &AletheiaDB,
    clause: &CypherWriteClause,
    binding: &mut Binding,
    params: &Params,
    deleted_nodes: &mut HashSet<NodeId>,
    deleted_edges: &mut HashSet<EdgeId>,
    created_edges: &mut Vec<(EdgeId, NodeId, NodeId)>,
    created_nodes: &mut Vec<(NodeId, String, PropertyMap)>,
) -> Result<()> {
    match clause {
        CypherWriteClause::Create(patterns) => {
            for pattern in patterns {
                create_pattern(tx, pattern, binding, params, created_edges, created_nodes)?;
            }
            Ok(())
        }
        CypherWriteClause::Set(items) => {
            apply_set(tx, items, binding, params, deleted_nodes, deleted_edges)
        }
        CypherWriteClause::Delete { detach, targets } => apply_delete(
            tx,
            *detach,
            targets,
            binding,
            deleted_nodes,
            deleted_edges,
            created_edges,
        ),
        CypherWriteClause::Merge {
            pattern,
            on_create,
            on_match,
        } => apply_merge(
            tx,
            db,
            pattern,
            on_create,
            on_match,
            binding,
            params,
            deleted_nodes,
            deleted_edges,
            created_edges,
            created_nodes,
        ),
    }
}

/// Apply a `MERGE` clause (Issue #3548): match the pattern if it already exists,
/// otherwise create the *entire* pattern (openCypher whole-pattern semantics).
///
/// The match is a pre-transaction (check-then-act) read against committed
/// current state via [`match_bindings`], filtered to rows consistent with the
/// variables already bound in this row (so a leading variable bound by a prior
/// `MATCH` constrains the match rather than re-scanning freely).
///
/// # Bi-temporal correctness
///
/// - A bare **match** records **no** new version: `on_match` is empty, so
///   [`apply_set`] performs no update.
/// - A **create** records new versions (one per created node/edge).
/// - `ON MATCH SET` on a matched entity records a new version (it is an update).
/// - `ON CREATE SET` folds into the same first version window as the create's
///   own `SET` coalescing (a separate `update_node`, mirroring `CREATE ... SET`).
///
/// # Same-statement dedup (single-node MERGE)
///
/// The committed [`match_bindings`] read cannot see this transaction's own
/// buffered creates, so a naive per-row MERGE would create openCypher-illegal
/// duplicates for a **single-node** pattern (e.g.
/// `MATCH (p:Person) MERGE (c:City {name:'NYC'})` over N persons must yield ONE
/// NYC, not N). To fix this, when the pattern is a single node and no committed
/// match exists, we also consult a statement-local ledger of nodes created
/// earlier in this same statement (`created_nodes`), matching by label and the
/// pattern's specified properties. A ledger hit is treated as a MATCH (bind it,
/// apply `on_match`, no new node/version); a miss creates and records the node.
///
/// A **relationship/path** MERGE deliberately does NOT dedup against the ledger:
/// openCypher matches the *whole path*, so
/// `MATCH (p:Person) MERGE (p)-[:R]->(c:City {name:'NYC'})` correctly creates
/// one NYC per person (a NYC connected only to p1 does not satisfy p2's path).
///
/// # v1 limitation: multi-match
///
/// The single-binding-per-row write executor cannot expand one MERGE into
/// multiple output rows. A MERGE whose pattern matches MORE THAN ONE committed
/// entity is therefore **rejected** with a structured `UnsupportedFeature`
/// error rather than silently binding the first (openCypher would bind all and
/// apply `ON MATCH SET` to every one). Add a uniquely-identifying property or a
/// unique constraint so the pattern identifies at most one entity.
///
/// # v1 concurrency
///
/// Without a unique constraint on the merged property, the pre-transaction
/// match is a documented check-then-act window (mirrors Issue #560): two racing
/// MERGE-creates can both create. Declaring
/// `db.unique_constraint(label, prop).enable()` closes the window --- the second
/// committer aborts (first-committer-wins).
#[allow(clippy::too_many_arguments)]
fn apply_merge(
    tx: &mut crate::api::transaction::WriteTransaction,
    db: &AletheiaDB,
    pattern: &CypherPattern,
    on_create: &[CypherSetItem],
    on_match: &[CypherSetItem],
    binding: &mut Binding,
    params: &Params,
    deleted_nodes: &HashSet<NodeId>,
    deleted_edges: &HashSet<EdgeId>,
    created_edges: &mut Vec<(EdgeId, NodeId, NodeId)>,
    created_nodes: &mut Vec<(NodeId, String, PropertyMap)>,
) -> Result<()> {
    // Pre-transaction match against committed current state, filtered to rows
    // consistent with variables already bound in this row. Collected into a Vec
    // so the borrow of `binding` ends before the branches mutate it.
    let committed: Vec<Binding> = match_bindings(db, std::slice::from_ref(pattern), None, params)?
        .into_iter()
        .filter(|row| consistent_with(row, binding))
        .collect();

    // Second finding: a MERGE pattern that matches more than one committed
    // entity cannot be expressed by the single-binding-per-row executor. Reject
    // rather than silently binding the first / dropping rows (openCypher would
    // bind ALL matches and apply ON MATCH SET to every one).
    if committed.len() > 1 {
        return Err(CypherError::UnsupportedFeature(
            "MERGE pattern matched multiple existing entities; MERGE requires a pattern that \
             identifies at most one (add a uniquely-identifying property or a unique constraint)"
                .to_string(),
        )
        .into());
    }

    if let Some(row) = committed.into_iter().next() {
        // MATCH branch: adopt the newly-discovered bindings (a leading
        // variable already present is rebound to the same id), then apply
        // ON MATCH SET only. A bare match (empty on_match) records nothing.
        for (name, entity) in row {
            bind(binding, &name, entity);
        }
        apply_set(tx, on_match, binding, params, deleted_nodes, deleted_edges)?;
        return Ok(());
    }

    // No committed match. For a single-node pattern that would be created (an
    // unbound/labelled node), consult this statement's created-node ledger so
    // repeated single-node MERGEs of the same key dedup to one node.
    if let Some(node) = single_creatable_node(pattern, binding)
        && let Some(id) = find_created_single_node(node, created_nodes, params)?
    {
        // Ledger MATCH branch: bind the buffered-created node and apply
        // ON MATCH SET only (no create, no whole-pattern duplicate).
        if let Some(var) = &node.variable {
            bind(binding, var, EntityResult::NodeId(id));
        }
        apply_set(tx, on_match, binding, params, deleted_nodes, deleted_edges)?;
        return Ok(());
    }

    // CREATE branch: create the whole pattern (a bound leading variable is
    // reused; the unbound remainder is created), then ON CREATE SET.
    create_pattern(tx, pattern, binding, params, created_edges, created_nodes)?;
    apply_set(tx, on_create, binding, params, deleted_nodes, deleted_edges)?;
    Ok(())
}

/// If a MERGE pattern is a single node that would be *created* (exactly one
/// label and not an already-bound variable), return that node element; else
/// `None`. A bound leading variable (`MATCH (a) MERGE (a) ...`) already resolves
/// to an existing node, and a relationship/path pattern keeps whole-pattern
/// semantics, so neither participates in single-node ledger dedup.
fn single_creatable_node<'p>(
    pattern: &'p CypherPattern,
    binding: &Binding,
) -> Option<&'p CypherNodePattern> {
    let [CypherPatternElement::Node(node)] = pattern.elements.as_slice() else {
        return None;
    };
    if node.labels.len() != 1 {
        return None;
    }
    if let Some(var) = &node.variable
        && lookup(binding, var).is_some()
    {
        return None; // already bound to an existing node
    }
    Some(node)
}

/// Find a node created earlier in this same statement whose label equals the
/// pattern node's label and whose stored properties satisfy every property the
/// pattern node specifies. Mirrors the current-state matcher's label+property
/// check against buffered creates.
fn find_created_single_node(
    node: &CypherNodePattern,
    created_nodes: &[(NodeId, String, PropertyMap)],
    params: &Params,
) -> Result<Option<NodeId>> {
    let want_label = &node.labels[0];
    for (id, label, props) in created_nodes {
        if label != want_label {
            continue;
        }
        if node_props_satisfied(&node.properties, props, params)? {
            return Ok(Some(*id));
        }
    }
    Ok(None)
}

/// Whether every `(key, value)` the pattern node specifies is present in a
/// created node's stored property map and equal (with int/float coercion,
/// matching the current-state matcher's `loosely_equal`).
fn node_props_satisfied(
    props: &[(String, CypherValue)],
    stored: &PropertyMap,
    params: &Params,
) -> Result<bool> {
    for (key, want) in props {
        let want = cypher_value_to_property(want, params)?;
        match stored.get(key) {
            Some(actual) if loosely_equal(actual, &want) => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// A matched candidate row is usable only if it agrees with every variable
/// already bound in the current row: for each shared variable the entity ids
/// must be identical. Variables bound only in the candidate (the freshly
/// matched remainder) impose no constraint.
fn consistent_with(row: &Binding, binding: &Binding) -> bool {
    binding
        .iter()
        .all(|(name, entity)| match lookup(row, name) {
            Some(other) => entity_same(entity, other),
            None => true,
        })
}

/// Two [`EntityResult`]s refer to the same graph entity (same node id or same
/// edge id).
fn entity_same(a: &EntityResult, b: &EntityResult) -> bool {
    if let (Some(x), Some(y)) = (node_id_of(a), node_id_of(b)) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (edge_id_of(a), edge_id_of(b)) {
        return x == y;
    }
    false
}

/// Create the nodes and relationships described by one `CREATE` pattern,
/// binding each named element into `binding`.
fn create_pattern(
    tx: &mut crate::api::transaction::WriteTransaction,
    pattern: &CypherPattern,
    binding: &mut Binding,
    params: &Params,
    created_edges: &mut Vec<(EdgeId, NodeId, NodeId)>,
    created_nodes: &mut Vec<(NodeId, String, PropertyMap)>,
) -> Result<()> {
    let mut prev_node: Option<NodeId> = None;
    let mut idx = 0;
    while idx < pattern.elements.len() {
        match &pattern.elements[idx] {
            CypherPatternElement::Node(node) => {
                let id = resolve_or_create_node(tx, node, binding, params, created_nodes)?;
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
                let right = resolve_or_create_node(tx, right_pat, binding, params, created_nodes)?;

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
                created_edges.push((edge_id, source, target));
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
    created_nodes: &mut Vec<(NodeId, String, PropertyMap)>,
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
    let id = tx.create_node(&node.labels[0], props.clone())?;
    // Record in the statement-local ledger so a later single-node MERGE of the
    // same (label, properties) key can dedup to this buffered-created node.
    created_nodes.push((id, node.labels[0].clone(), props));
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
    deleted_edges: &HashSet<EdgeId>,
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
            if deleted_edges.contains(&edge_id) {
                continue; // no-op on an edge this statement already deleted
            }
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
    created_edges: &[(EdgeId, NodeId, NodeId)],
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
            // not mis-count them, then cascade. `delete_node_cascade` itself
            // unions committed adjacency with same-transaction buffered CREATEs,
            // so a relationship created earlier in this statement is removed too;
            // we mirror that here into `deleted_edges` for the plain-DELETE check.
            for edge_id in connected_edges(tx, node_id)? {
                deleted_edges.insert(edge_id);
            }
            for (edge_id, source, target) in created_edges {
                if *source == node_id || *target == node_id {
                    deleted_edges.insert(*edge_id);
                }
            }
            tx.delete_node_cascade(node_id)?;
        } else {
            // openCypher safety rule: refuse to orphan edges. Ignore edges this
            // same statement already deleted. Committed adjacency does not yet
            // include a relationship CREATEd earlier in this same statement
            // (it is still buffered), so fold the statement-local created-edge
            // ledger in too — otherwise this plain DELETE would slip past the
            // check and abort at commit with a cryptic dangling-edge error
            // (Minor 1).
            let mut remaining: Vec<EdgeId> = connected_edges(tx, node_id)?
                .into_iter()
                .filter(|e| !deleted_edges.contains(e))
                .collect();
            for (edge_id, source, target) in created_edges {
                if (*source == node_id || *target == node_id)
                    && !deleted_edges.contains(edge_id)
                    && !remaining.contains(edge_id)
                {
                    remaining.push(*edge_id);
                }
            }
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
