//! Atomic multi-write batches with local refs (`apply_batch`, Issue #3231).
//!
//! An LLM building a subgraph through the single-op MCP tools must issue one
//! call per write, each its own transaction — a mid-sequence failure leaves a
//! half-built, referentially-inconsistent graph. `apply_batch` accepts an
//! **ordered** array of write operations that commit **all-or-nothing** in a
//! single `WriteTransaction` (one WAL batch append, one GroupCommit fsync),
//! where later operations may reference nodes created earlier in the same
//! batch via symbolic local refs (`"$alias"` or positional `"$0"`), in the
//! spirit of Datomic tempids / XTDB `submit-tx`.
//!
//! # Two-phase execution
//!
//! 1. **Static pre-validation** (no database access): the batch cap, op
//!    shapes, alias/ref resolution (duplicates, forward refs, unknown refs),
//!    per-op timestamps/properties/provenance, v1 scope rules, and
//!    parameter-combination rules are all checked up front. Every rejection
//!    reports a precise `failed_op_index`, and a structurally invalid batch
//!    never opens a transaction (and never burns entity IDs).
//! 2. **Execution**: one `db.write(|tx| ...)` closure issues the operations
//!    in order. Any failure aborts the whole transaction (drop-rollback —
//!    nothing was applied, nothing is ever visible to concurrent readers) and
//!    reports the failing op index. Commit-phase failures (conflicts,
//!    constraint violations) also abort atomically.
//!
//! # v1 scope (documented limitations)
//!
//! - Later ops may **reference** batch-created nodes as edge endpoints, but
//!   may not **update/delete/re-write** a batch-created entity: the core
//!   transaction's update/delete paths read committed storage, not the write
//!   buffer (read-your-writes inside an uncommitted batch is explicitly out
//!   of scope for #3231). Such ops are rejected up front with a clear error.
//! - At most one write per committed entity per batch (a second write to the
//!   same node/edge id is rejected — the second op would act on a stale read
//!   of the pre-batch state).
//! - Delete results carry no `version_id` (tombstone versions are allocated
//!   at apply time inside the commit path and are not surfaced by the
//!   transaction API).
//! - A batch-created edge removed by a later `delete_node` + `detach: true`
//!   of one of its endpoints is elided from the transaction entirely (its
//!   per-op result reports `removed_by_delete_at_index`): the committed
//!   record matches the batch's atomic net effect, and no history is written
//!   for an edge that never existed outside the batch.

use std::collections::HashMap;

use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::transaction::{ReadOps, WriteOps, WriteRequestOptions};
use crate::core::error::Error;
use crate::core::provenance::Provenance;
use crate::core::temporal::Timestamp;
use crate::core::{EdgeId, NodeId, PropertyMap};

use super::error::{McpError, McpErrorCode};
use super::server::AletheiaMcpServer;
use super::tools::ProvenanceRequest;

// ============================================================================
// Request types
// ============================================================================

/// Request to apply an ordered batch of write operations atomically.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ApplyBatchRequest {
    /// Ordered list of write operations. The whole batch commits
    /// all-or-nothing; operations may reference nodes created earlier in the
    /// same batch via `"$alias"` (a `ref` supplied on a `create_node` op) or
    /// positionally via `"$<index>"`.
    ///
    /// Kept as raw JSON values so that a malformed operation can be rejected
    /// with its precise array index (`failed_op_index`); the advertised JSON
    /// schema is the typed [`BatchOperation`] shape.
    #[schemars(
        with = "Vec<BatchOperation>",
        description = "Ordered list of write operations, applied atomically (all-or-nothing). \
                       Each element is a tagged object whose `op` field selects the operation: \
                       create_node, create_edge, update_node, update_edge, delete_node, \
                       delete_edge. Edge endpoints (and only endpoints) may reference nodes \
                       created earlier in the same batch via '$alias' (see create_node's `ref`) \
                       or positional '$<index>' local references."
    )]
    pub operations: Vec<serde_json::Value>,
}

/// A node endpoint reference: either a committed numeric node id, or a local
/// reference string (`"$alias"` / positional `"$<index>"`) naming a node
/// created earlier in the same batch.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum BatchNodeRef {
    /// A committed node id.
    Id(u64),
    /// A `"$alias"` or positional `"$<index>"` local reference to a node
    /// created earlier in the same batch.
    Ref(String),
}

/// One write operation inside an `apply_batch` request, tagged by `op`.
///
/// Each variant mirrors the corresponding single-op MCP tool's request
/// fields, including the optional #3221 `valid_time` and #3224 `provenance`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BatchOperation {
    /// Create a node. An optional `ref` alias makes the new node addressable
    /// by later edge operations in the same batch as `"$<ref>"`.
    CreateNode {
        /// The label/type of the node (e.g., "Person").
        #[schemars(description = "The label/type of the node (e.g., 'Person')")]
        label: String,
        /// Properties to set on the node as key-value pairs.
        #[schemars(description = "Properties to set on the node as key-value pairs")]
        properties: Option<HashMap<String, serde_json::Value>>,
        /// Optional valid time (ISO 8601 or microseconds since epoch).
        #[schemars(
            description = "Optional valid time: when this fact became true in the real world \
                           (ISO 8601 / RFC 3339 or integer microseconds since epoch). Omit to \
                           default to the transaction time."
        )]
        valid_time: Option<String>,
        /// Optional write-time provenance bundle.
        #[schemars(
            description = "Optional write-time provenance bundle (source, confidence, note, \
                           correlation_id)"
        )]
        provenance: Option<ProvenanceRequest>,
        /// Optional alias for referencing this node later in the batch.
        #[serde(rename = "ref")]
        #[schemars(
            description = "Optional alias for this new node; later operations in the same batch \
                           may reference it as '$<ref>' wherever a node id is accepted as an \
                           edge endpoint. Must be unique within the batch, must not start with \
                           '$', and must not be purely numeric (numeric refs are reserved for \
                           positional '$<index>' references)."
        )]
        alias: Option<String>,
    },
    /// Create an edge. `source_id`/`target_id` accept committed node ids and
    /// local refs interchangeably.
    CreateEdge {
        /// Source endpoint: committed node id or local ref.
        #[schemars(
            description = "Source node: a committed node id (integer) or a '$ref' local \
                           reference to a node created earlier in this batch"
        )]
        source_id: BatchNodeRef,
        /// Target endpoint: committed node id or local ref.
        #[schemars(
            description = "Target node: a committed node id (integer) or a '$ref' local \
                           reference to a node created earlier in this batch"
        )]
        target_id: BatchNodeRef,
        /// The label/type of the edge (e.g., "KNOWS").
        #[schemars(description = "The label/type of the edge (e.g., 'KNOWS')")]
        label: String,
        /// Properties to set on the edge as key-value pairs.
        #[schemars(description = "Properties to set on the edge as key-value pairs")]
        properties: Option<HashMap<String, serde_json::Value>>,
        /// Optional valid time (ISO 8601 or microseconds since epoch).
        #[schemars(
            description = "Optional valid time: when this relationship became true in the real \
                           world (ISO 8601 / RFC 3339 or integer microseconds since epoch). Omit \
                           to default to the transaction time."
        )]
        valid_time: Option<String>,
        /// Optional write-time provenance bundle.
        #[schemars(
            description = "Optional write-time provenance bundle (source, confidence, note, \
                           correlation_id)"
        )]
        provenance: Option<ProvenanceRequest>,
    },
    /// Update a committed node's properties (PATCH-merge semantics, like the
    /// single-op `update_node` tool). v1 rejects updating a node created in
    /// the same batch.
    UpdateNode {
        /// The committed node id to update (local refs are rejected in v1).
        #[schemars(
            description = "The committed node id to update. Updating a node created in the same \
                           batch ('$ref') is not supported in v1."
        )]
        node_id: BatchNodeRef,
        /// Properties to merge into the node.
        #[schemars(description = "Properties to set (merged with existing properties)")]
        properties: HashMap<String, serde_json::Value>,
        /// Optional valid time (ISO 8601 or microseconds since epoch).
        #[schemars(
            description = "Optional valid time: when this update became true in the real world \
                           (ISO 8601 / RFC 3339 or integer microseconds since epoch)"
        )]
        valid_time: Option<String>,
        /// Optional write-time provenance bundle for this version.
        #[schemars(description = "Optional write-time provenance bundle for this version")]
        provenance: Option<ProvenanceRequest>,
    },
    /// Update a committed edge's properties (PATCH-merge semantics).
    UpdateEdge {
        /// The committed edge id to update.
        #[schemars(description = "The committed edge id to update")]
        edge_id: u64,
        /// Properties to merge into the edge.
        #[schemars(description = "Properties to set (merged with existing properties)")]
        properties: HashMap<String, serde_json::Value>,
        /// Optional valid time (ISO 8601 or microseconds since epoch).
        #[schemars(
            description = "Optional valid time: when this update became true in the real world \
                           (ISO 8601 / RFC 3339 or integer microseconds since epoch)"
        )]
        valid_time: Option<String>,
        /// Optional write-time provenance bundle for this version.
        #[schemars(description = "Optional write-time provenance bundle for this version")]
        provenance: Option<ProvenanceRequest>,
    },
    /// Delete a committed node, honoring the #3209 safe-by-default DETACH
    /// contract against committed *and* batch-created edges.
    DeleteNode {
        /// The committed node id to delete (local refs are rejected in v1).
        #[schemars(
            description = "The committed node id to delete. Deleting a node created in the same \
                           batch ('$ref') is not supported in v1."
        )]
        node_id: BatchNodeRef,
        /// When true, also remove all connected edges (committed and
        /// batch-created). When false/omitted, the batch is refused if the
        /// node has connected edges at this point in the batch.
        #[schemars(
            description = "When true, also delete all edges connected to the node at this point \
                           in the batch (including edges created earlier in the batch). When \
                           false/omitted, the whole batch is refused if the node has connected \
                           edges, reporting connected_edges."
        )]
        detach: Option<bool>,
        /// Optional valid time. Not supported together with `detach: true`.
        #[schemars(
            description = "Optional valid time: when this fact stopped being true in the real \
                           world (ISO 8601 / RFC 3339 or integer microseconds since epoch). Not \
                           supported together with detach:true (cascade delete does not support \
                           backdating)."
        )]
        valid_time: Option<String>,
    },
    /// Delete a committed edge.
    DeleteEdge {
        /// The committed edge id to delete.
        #[schemars(description = "The committed edge id to delete")]
        edge_id: u64,
        /// Optional valid time (ISO 8601 or microseconds since epoch).
        #[schemars(
            description = "Optional valid time: when this relationship stopped being true in \
                           the real world (ISO 8601 / RFC 3339 or integer microseconds since \
                           epoch)"
        )]
        valid_time: Option<String>,
    },
}

// ============================================================================
// Phase 1 — static pre-validation output
// ============================================================================

/// A node endpoint after static ref resolution.
#[derive(Debug, Clone, Copy)]
enum ResolvedNode {
    /// A validated, committed node id.
    Committed(NodeId),
    /// A batch-local node, identified by the index of the `create_node` op
    /// that defines it. The real `NodeId` exists once that op executes.
    Local(usize),
}

/// A fully pre-validated operation, ready to execute inside the transaction.
enum PlannedOp {
    CreateNode {
        label: String,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
        provenance: Option<Provenance>,
        alias: Option<String>,
    },
    CreateEdge {
        source: ResolvedNode,
        target: ResolvedNode,
        label: String,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
        provenance: Option<Provenance>,
        /// When set, a later `delete_node` + `detach: true` op (at this
        /// index) removes one of this edge's committed endpoints: the edge is
        /// elided from the transaction (see module docs).
        annihilated_by: Option<usize>,
    },
    UpdateNode {
        node_id: NodeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
        provenance: Option<Provenance>,
    },
    UpdateEdge {
        edge_id: EdgeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
        provenance: Option<Provenance>,
    },
    DeleteNode {
        node_id: NodeId,
        detach: bool,
        valid_from: Option<Timestamp>,
    },
    DeleteEdge {
        edge_id: EdgeId,
        valid_from: Option<Timestamp>,
    },
}

// ============================================================================
// Phase 2 — execution failure carrier
// ============================================================================

/// Closure error carrying the failing op index out of `db.write`.
///
/// Commit-phase errors (validation, conflicts, constraints) arrive through
/// the `From<Error>` impl `db.write` requires and carry no op index
/// (`failed_op_index` is reported as JSON `null` for those).
enum BatchAbort {
    /// An operation failed with a database error at its op index.
    Op { index: usize, source: Error },
    /// The #3209 safe-by-default refusal: the node targeted by the
    /// `delete_node` op at `index` still has connected edges (committed
    /// and/or batch-created) and `detach` was not passed.
    Refused {
        index: usize,
        node_id: u64,
        connected_edges: usize,
    },
    /// A `delete_node` + `detach: true` cascade would discard an edge update
    /// made earlier in this batch — rejected rather than silently dropping
    /// the update (commit validation would abort the batch anyway, with a
    /// less precise error).
    UpdatedEdgeCascade {
        index: usize,
        edge_id: u64,
        node_id: u64,
        updated_at_index: usize,
    },
    /// The op targets a committed edge that an earlier `delete_node` +
    /// `detach: true` cascade in this batch already removed.
    EdgeRemovedByCascade {
        index: usize,
        edge_id: u64,
        removed_at_index: usize,
    },
    /// The op at `index` targets a committed integer id that a `create_node`/
    /// `create_edge` earlier in THIS batch allocated — a guessed-id write
    /// against an entity not yet committed. This is the documented v1-scope
    /// rejection ("no update/delete of batch-created refs") that the static
    /// `$alias`/`$<index>` prevalidation enforces for the ref *spelling*
    /// (`batch_committed_node`); enforced explicitly here for the integer-id
    /// form because buffer-aware transaction reads (Issue #3417) no longer
    /// surface it incidentally as `NOT_FOUND`.
    BatchCreatedRefWrite {
        index: usize,
        entity: &'static str,
        id: u64,
        created_at_index: usize,
        verb: &'static str,
    },
    /// A commit-phase failure (no attributable op index).
    Commit { source: Error },
    /// An internal invariant breach in the batch handler itself (never the
    /// caller's fault) — reported as `INTERNAL`, not as a caller-fault code.
    Internal { index: usize, message: String },
}

impl From<Error> for BatchAbort {
    fn from(source: Error) -> Self {
        BatchAbort::Commit { source }
    }
}

/// Batch-local adjacency ledger entry for an edge created in this batch.
struct BatchEdge {
    source: NodeId,
    target: NodeId,
    annihilated_by: Option<usize>,
}

impl BatchEdge {
    fn touches(&self, node: NodeId) -> bool {
        self.source == node || self.target == node
    }

    /// Whether the edge still (logically) exists when the op at `index`
    /// executes: it has not been removed by an earlier detach-delete.
    fn alive_at(&self, index: usize) -> bool {
        match self.annihilated_by {
            None => true,
            Some(k) => k >= index,
        }
    }
}

// ============================================================================
// Handler
// ============================================================================

impl AletheiaMcpServer {
    /// Apply an ordered batch of write operations atomically (Issue #3231).
    ///
    /// See the [module docs](self) for the contract; returns the JSON
    /// response as a string, like the other typed tool wrappers.
    pub fn apply_batch(&self, req: ApplyBatchRequest) -> String {
        Self::extract_text(self.handle_apply_batch(
            serde_json::to_value(req).expect("request serialization should not fail"),
        ))
    }

    /// Single dispatch entry for the `apply_batch` tool.
    pub(crate) fn handle_apply_batch(&self, args: serde_json::Value) -> CallToolResult {
        // AUTHZ SEAM: a future authorization check / principal stamp for
        // batched writes belongs here — one choke point before any operation
        // is parsed, validated, or executed.
        let req: ApplyBatchRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Phase 1: static pre-validation. No database access; a rejected
        // batch never opens a transaction and never burns entity IDs.
        let planned = match self.prevalidate_batch(&req.operations) {
            Ok(p) => p,
            Err(result) => return *result,
        };

        // An empty batch is a harmless, idempotent no-op success.
        if planned.is_empty() {
            return self.success_json(json!({
                "success": true,
                "operation_count": 0,
                "results": [],
                "ref_map": {}
            }));
        }

        // Phase 2: execute all operations in one write transaction. Any
        // error rolls the whole transaction back (drop-rollback): none of
        // the batch's writes are ever applied or visible.
        let outcome = self.db().write(
            |tx| -> Result<
                (
                    Vec<serde_json::Value>,
                    serde_json::Map<String, serde_json::Value>,
                ),
                BatchAbort,
            > { self.execute_batch(tx, &planned) },
        );

        match outcome {
            Ok((results, ref_map)) => self.success_json(json!({
                "success": true,
                "operation_count": planned.len(),
                "results": results,
                "ref_map": ref_map
            })),
            Err(abort) => self.batch_abort_result(abort),
        }
    }

    // ------------------------------------------------------------------
    // Phase 1: static pre-validation
    // ------------------------------------------------------------------

    /// Statically validate the raw operation array into executable
    /// [`PlannedOp`]s, or produce the structured rejection.
    ///
    /// Boxed error to keep the happy-path return small (clippy
    /// result_large_err).
    fn prevalidate_batch(
        &self,
        raw_ops: &[serde_json::Value],
    ) -> Result<Vec<PlannedOp>, Box<CallToolResult>> {
        // Batch cap first (Issue #3226 convention: echo the limit).
        let limit = self.max_batch_operations;
        if raw_ops.len() > limit {
            return Err(Box::new(self.error_result(
                McpError::new(
                    McpErrorCode::InvalidArgument,
                    format!(
                        "Batch of {} operations exceeds the maximum of {} per apply_batch call; \
                         split it into smaller batches.",
                        raw_ops.len(),
                        limit
                    ),
                )
                .details(json!({
                    "limit": limit,
                    "submitted": raw_ops.len(),
                    // No single op is at fault; null keeps the details shape
                    // uniform with every other apply_batch error.
                    "failed_op_index": serde_json::Value::Null
                })),
            )));
        }

        // Parse each op individually so a malformed element is rejected with
        // its precise index (a wholesale Vec deserialization would lose it).
        let mut ops = Vec::with_capacity(raw_ops.len());
        for (index, raw) in raw_ops.iter().enumerate() {
            match serde_json::from_value::<BatchOperation>(raw.clone()) {
                Ok(op) => ops.push(op),
                Err(e) => {
                    return Err(self.batch_invalid(
                        index,
                        format!("Invalid operation at index {index}: {e}"),
                        serde_json::Map::new(),
                    ));
                }
            }
        }

        // Collect alias definitions (create_node ops only) and validate them.
        let mut aliases: HashMap<&str, usize> = HashMap::new();
        for (index, op) in ops.iter().enumerate() {
            if let BatchOperation::CreateNode {
                alias: Some(alias), ..
            } = op
            {
                if alias.is_empty() {
                    return Err(self.batch_invalid(
                        index,
                        format!("Operation {index}: `ref` must not be empty"),
                        serde_json::Map::new(),
                    ));
                }
                if alias.starts_with('$') {
                    return Err(self.batch_invalid(
                        index,
                        format!(
                            "Operation {index}: `ref` \"{alias}\" must not start with '$' \
                             (the '$' prefix is used when referencing, not when defining)"
                        ),
                        serde_json::Map::new(),
                    ));
                }
                if alias.chars().all(|c| c.is_ascii_digit()) {
                    return Err(self.batch_invalid(
                        index,
                        format!(
                            "Operation {index}: `ref` \"{alias}\" must not be purely numeric; \
                             numeric references are reserved for positional '$<index>' refs"
                        ),
                        serde_json::Map::new(),
                    ));
                }
                if let Some(&first) = aliases.get(alias.as_str()) {
                    let mut details = serde_json::Map::new();
                    details.insert("ref".to_string(), json!(alias));
                    details.insert("first_defined_at_index".to_string(), json!(first));
                    return Err(self.batch_invalid(
                        index,
                        format!(
                            "Operation {index}: duplicate `ref` \"{alias}\" \
                             (first defined at operation {first})"
                        ),
                        details,
                    ));
                }
                aliases.insert(alias.as_str(), index);
            }
        }

        // Main pass: resolve refs, parse timestamps/properties/provenance,
        // and enforce the v1 scope rules.
        let mut planned = Vec::with_capacity(ops.len());
        // Committed node ids deleted by an earlier op -> that op's index.
        let mut deleted_nodes: HashMap<u64, (usize, bool)> = HashMap::new(); // id -> (index, detach)
        // Committed entity ids already written by an earlier op (one write
        // per committed entity per batch).
        let mut written_nodes: HashMap<u64, usize> = HashMap::new();
        let mut written_edges: HashMap<u64, usize> = HashMap::new();

        for (index, op) in ops.iter().enumerate() {
            let planned_op = match op {
                BatchOperation::CreateNode {
                    label,
                    properties,
                    valid_time,
                    provenance,
                    alias,
                } => PlannedOp::CreateNode {
                    label: label.clone(),
                    properties: self.batch_properties(index, properties.as_ref())?,
                    valid_from: self.batch_valid_time(index, valid_time)?,
                    provenance: self.batch_provenance(index, provenance.clone())?,
                    alias: alias.clone(),
                },
                BatchOperation::CreateEdge {
                    source_id,
                    target_id,
                    label,
                    properties,
                    valid_time,
                    provenance,
                } => {
                    let source =
                        self.batch_resolve_ref(index, source_id, &aliases, &ops, &deleted_nodes)?;
                    let target =
                        self.batch_resolve_ref(index, target_id, &aliases, &ops, &deleted_nodes)?;
                    PlannedOp::CreateEdge {
                        source,
                        target,
                        label: label.clone(),
                        properties: self.batch_properties(index, properties.as_ref())?,
                        valid_from: self.batch_valid_time(index, valid_time)?,
                        provenance: self.batch_provenance(index, provenance.clone())?,
                        annihilated_by: None, // filled below, after all deletes are known
                    }
                }
                BatchOperation::UpdateNode {
                    node_id,
                    properties,
                    valid_time,
                    provenance,
                } => {
                    let node_id =
                        self.batch_committed_node(index, node_id, "update", &mut written_nodes)?;
                    PlannedOp::UpdateNode {
                        node_id,
                        properties: self.batch_properties(index, Some(properties))?,
                        valid_from: self.batch_valid_time(index, valid_time)?,
                        provenance: self.batch_provenance(index, provenance.clone())?,
                    }
                }
                BatchOperation::UpdateEdge {
                    edge_id,
                    properties,
                    valid_time,
                    provenance,
                } => {
                    let edge_id = self.batch_committed_edge(index, *edge_id, &mut written_edges)?;
                    PlannedOp::UpdateEdge {
                        edge_id,
                        properties: self.batch_properties(index, Some(properties))?,
                        valid_from: self.batch_valid_time(index, valid_time)?,
                        provenance: self.batch_provenance(index, provenance.clone())?,
                    }
                }
                BatchOperation::DeleteNode {
                    node_id,
                    detach,
                    valid_time,
                } => {
                    let detach = detach.unwrap_or(false);
                    let valid_from = self.batch_valid_time(index, valid_time)?;
                    if detach && valid_from.is_some() {
                        return Err(self.batch_invalid(
                            index,
                            format!(
                                "Operation {index}: valid_time is not supported together with \
                                 detach:true; cascade delete does not support backdating. Delete \
                                 the connected edges individually with valid_time, or omit \
                                 valid_time to cascade-delete at now."
                            ),
                            serde_json::Map::new(),
                        ));
                    }
                    let node_id =
                        self.batch_committed_node(index, node_id, "delete", &mut written_nodes)?;
                    deleted_nodes.insert(node_id.as_u64(), (index, detach));
                    PlannedOp::DeleteNode {
                        node_id,
                        detach,
                        valid_from,
                    }
                }
                BatchOperation::DeleteEdge {
                    edge_id,
                    valid_time,
                } => {
                    let edge_id = self.batch_committed_edge(index, *edge_id, &mut written_edges)?;
                    PlannedOp::DeleteEdge {
                        edge_id,
                        valid_from: self.batch_valid_time(index, valid_time)?,
                    }
                }
            };
            planned.push(planned_op);
        }

        // Annihilation pass: a batch-created edge whose committed endpoint is
        // later removed by `delete_node` + `detach: true` is elided from the
        // transaction — the cascade "removes" it (module docs). The earliest
        // such delete wins. A to-be-elided edge carrying an explicit
        // valid_time is REJECTED instead: eliding it would silently erase a
        // backdated bi-temporal fact that single-op sequencing (create,
        // commit, then delete) preserves in history.
        let mut annihilators: Vec<Option<usize>> = vec![None; planned.len()];
        for (index, op) in planned.iter().enumerate() {
            let PlannedOp::CreateEdge {
                source,
                target,
                valid_from,
                ..
            } = op
            else {
                continue;
            };
            let mut annihilator: Option<usize> = None;
            for endpoint in [source, target] {
                if let ResolvedNode::Committed(node_id) = endpoint
                    && let Some(&(k, detach)) = deleted_nodes.get(&node_id.as_u64())
                    && detach
                    && k > index
                    && annihilator.is_none_or(|prev| k < prev)
                {
                    annihilator = Some(k);
                }
            }
            if let Some(k) = annihilator {
                if valid_from.is_some() {
                    let mut details = serde_json::Map::new();
                    details.insert("removed_by_delete_at_index".to_string(), json!(k));
                    return Err(self.batch_invalid(
                        index,
                        format!(
                            "Operation {index}: this create_edge carries an explicit valid_time, \
                             but operation {k} detach-deletes one of its endpoints later in the \
                             batch, which would elide the edge and silently erase the backdated \
                             fact. Drop the create_edge from this batch, or commit it in a \
                             separate call before the delete (single-op sequencing preserves the \
                             fact in history)."
                        ),
                        details,
                    ));
                }
                annihilators[index] = Some(k);
            }
        }
        for (op, annihilator) in planned.iter_mut().zip(annihilators) {
            if let (Some(k), PlannedOp::CreateEdge { annihilated_by, .. }) = (annihilator, op) {
                *annihilated_by = Some(k);
            }
        }

        Ok(planned)
    }

    /// Build the structured `INVALID_ARGUMENT` rejection for a specific op.
    fn batch_invalid(
        &self,
        index: usize,
        message: String,
        mut details: serde_json::Map<String, serde_json::Value>,
    ) -> Box<CallToolResult> {
        details.insert("failed_op_index".to_string(), json!(index));
        Box::new(
            self.error_result(
                McpError::new(McpErrorCode::InvalidArgument, message)
                    .details(serde_json::Value::Object(details)),
            ),
        )
    }

    /// Parse an op's optional `valid_time`.
    fn batch_valid_time(
        &self,
        index: usize,
        valid_time: &Option<String>,
    ) -> Result<Option<Timestamp>, Box<CallToolResult>> {
        match valid_time {
            None => Ok(None),
            Some(s) => match self.parse_timestamp(s) {
                Ok(ts) => Ok(Some(ts)),
                Err(e) => Err(self.batch_invalid(
                    index,
                    format!("Operation {index}: invalid valid_time: {e}"),
                    serde_json::Map::new(),
                )),
            },
        }
    }

    /// Convert an op's optional properties object.
    fn batch_properties(
        &self,
        index: usize,
        properties: Option<&HashMap<String, serde_json::Value>>,
    ) -> Result<PropertyMap, Box<CallToolResult>> {
        match properties {
            None => Ok(PropertyMap::default()),
            Some(p) => self.json_to_property_map(p).map_err(|e| {
                self.batch_invalid(
                    index,
                    format!("Operation {index}: invalid properties: {e}"),
                    serde_json::Map::new(),
                )
            }),
        }
    }

    /// Validate an op's optional provenance bundle (Issue #3224 semantics:
    /// an all-absent bundle is normalized to `None`).
    ///
    /// Deliberately routes through [`Self::parse_opt_provenance`] — the SAME
    /// helper every single-op tool uses — so a future principal stamp on the
    /// provenance path (#3350 authn/RBAC) lands in exactly one place. Only
    /// the error is re-shaped here, to carry the batch's `failed_op_index`.
    fn batch_provenance(
        &self,
        index: usize,
        provenance: Option<ProvenanceRequest>,
    ) -> Result<Option<Provenance>, Box<CallToolResult>> {
        self.parse_opt_provenance(provenance).map_err(|result| {
            let text = Self::extract_text(result);
            let detail = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or(text);
            self.batch_invalid(
                index,
                format!("Operation {index}: {detail}"),
                serde_json::Map::new(),
            )
        })
    }

    /// Resolve an edge-endpoint reference statically.
    fn batch_resolve_ref(
        &self,
        index: usize,
        node_ref: &BatchNodeRef,
        aliases: &HashMap<&str, usize>,
        ops: &[BatchOperation],
        deleted_nodes: &HashMap<u64, (usize, bool)>,
    ) -> Result<ResolvedNode, Box<CallToolResult>> {
        match node_ref {
            BatchNodeRef::Id(id) => {
                let node_id = NodeId::new(*id).map_err(|e| {
                    self.batch_invalid(
                        index,
                        format!("Operation {index}: {e}"),
                        serde_json::Map::new(),
                    )
                })?;
                // Referencing a node this batch already deleted can never
                // commit (validation would abort at commit time); reject it
                // here with the precise index instead.
                if let Some(&(deleted_at, _)) = deleted_nodes.get(id) {
                    let mut details = serde_json::Map::new();
                    details.insert("node_id".to_string(), json!(id));
                    details.insert("deleted_at_index".to_string(), json!(deleted_at));
                    return Err(self.batch_invalid(
                        index,
                        format!(
                            "Operation {index}: references node {id}, which operation \
                             {deleted_at} deletes earlier in this batch"
                        ),
                        details,
                    ));
                }
                Ok(ResolvedNode::Committed(node_id))
            }
            BatchNodeRef::Ref(s) => {
                let Some(name) = s.strip_prefix('$') else {
                    return Err(self.batch_invalid(
                        index,
                        format!(
                            "Operation {index}: node reference \"{s}\" must be an integer node \
                             id or a local reference starting with '$'"
                        ),
                        serde_json::Map::new(),
                    ));
                };
                // Caller-supplied aliases first; purely numeric names are
                // positional indices (aliases are validated non-numeric).
                // The positional form is all-ASCII-digits only, so "$+5"
                // never silently parses positionally (usize::from_str would
                // accept a leading '+') — it falls through to the
                // unknown-ref error. Leading zeros are fine: "$007" == "$7".
                let defined_at = if let Some(&d) = aliases.get(name) {
                    Some(d)
                } else if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) {
                    name.parse::<usize>()
                        .ok()
                        .and_then(|p| (p < ops.len()).then_some(p))
                } else {
                    None
                };
                let Some(defined_at) = defined_at else {
                    let mut details = serde_json::Map::new();
                    details.insert("ref".to_string(), json!(s));
                    return Err(self.batch_invalid(
                        index,
                        format!(
                            "Operation {index}: unknown local reference \"{s}\" (no create_node \
                             in this batch defines it)"
                        ),
                        details,
                    ));
                };
                if !matches!(ops[defined_at], BatchOperation::CreateNode { .. }) {
                    let mut details = serde_json::Map::new();
                    details.insert("ref".to_string(), json!(s));
                    details.insert("defined_at_index".to_string(), json!(defined_at));
                    return Err(self.batch_invalid(
                        index,
                        format!(
                            "Operation {index}: local reference \"{s}\" points at operation \
                             {defined_at}, which is not a create_node"
                        ),
                        details,
                    ));
                }
                if defined_at >= index {
                    let mut details = serde_json::Map::new();
                    details.insert("ref".to_string(), json!(s));
                    details.insert("defined_at_index".to_string(), json!(defined_at));
                    return Err(self.batch_invalid(
                        index,
                        format!(
                            "Operation {index}: forward reference \"{s}\" — the node it names \
                             is only created later, at operation {defined_at}; reorder the \
                             batch so creations precede references"
                        ),
                        details,
                    ));
                }
                Ok(ResolvedNode::Local(defined_at))
            }
        }
    }

    /// Validate a committed-node target for update/delete ops: local refs
    /// are a documented v1 rejection, and each committed entity accepts at
    /// most one write per batch.
    fn batch_committed_node(
        &self,
        index: usize,
        node_ref: &BatchNodeRef,
        verb: &str,
        written_nodes: &mut HashMap<u64, usize>,
    ) -> Result<NodeId, Box<CallToolResult>> {
        let id = match node_ref {
            BatchNodeRef::Id(id) => *id,
            // Only '$'-prefixed strings are local refs and get the v1-scope
            // message; any other string (e.g. a quoted number like "123") is
            // a plain type error, reported as such.
            BatchNodeRef::Ref(s) if s.starts_with('$') => {
                let mut details = serde_json::Map::new();
                details.insert("ref".to_string(), json!(s));
                return Err(self.batch_invalid(
                    index,
                    format!(
                        "Operation {index}: cannot {verb} \"{s}\" — updating or deleting a node \
                         created in the same batch is not supported in v1. Commit the creation \
                         first, then {verb} it in a follow-up call."
                    ),
                    details,
                ));
            }
            BatchNodeRef::Ref(s) => {
                return Err(self.batch_invalid(
                    index,
                    format!(
                        "Operation {index}: node_id must be an integer node id, got string \
                         \"{s}\""
                    ),
                    serde_json::Map::new(),
                ));
            }
        };
        let node_id = NodeId::new(id).map_err(|e| {
            self.batch_invalid(
                index,
                format!("Operation {index}: {e}"),
                serde_json::Map::new(),
            )
        })?;
        if let Some(&first) = written_nodes.get(&id) {
            let mut details = serde_json::Map::new();
            details.insert("node_id".to_string(), json!(id));
            details.insert("first_write_op_index".to_string(), json!(first));
            return Err(self.batch_invalid(
                index,
                format!(
                    "Operation {index}: node {id} is already written by operation {first}; a \
                     batch accepts at most one write per committed entity (the second write \
                     would act on a stale read of the pre-batch state)"
                ),
                details,
            ));
        }
        written_nodes.insert(id, index);
        Ok(node_id)
    }

    /// Validate a committed-edge target for update/delete ops (one write per
    /// committed entity per batch).
    fn batch_committed_edge(
        &self,
        index: usize,
        id: u64,
        written_edges: &mut HashMap<u64, usize>,
    ) -> Result<EdgeId, Box<CallToolResult>> {
        let edge_id = EdgeId::new(id).map_err(|e| {
            self.batch_invalid(
                index,
                format!("Operation {index}: {e}"),
                serde_json::Map::new(),
            )
        })?;
        if let Some(&first) = written_edges.get(&id) {
            let mut details = serde_json::Map::new();
            details.insert("edge_id".to_string(), json!(id));
            details.insert("first_write_op_index".to_string(), json!(first));
            return Err(self.batch_invalid(
                index,
                format!(
                    "Operation {index}: edge {id} is already written by operation {first}; a \
                     batch accepts at most one write per committed entity (the second write \
                     would act on a stale read of the pre-batch state)"
                ),
                details,
            ));
        }
        written_edges.insert(id, index);
        Ok(edge_id)
    }

    // ------------------------------------------------------------------
    // Phase 2: execution inside one write transaction
    // ------------------------------------------------------------------

    /// Execute the pre-validated operations in order inside `tx`, returning
    /// the per-op result objects (input order) and the alias -> real-id map.
    #[allow(clippy::type_complexity)]
    fn execute_batch(
        &self,
        tx: &mut crate::api::transaction::WriteTransaction,
        planned: &[PlannedOp],
    ) -> Result<
        (
            Vec<serde_json::Value>,
            serde_json::Map<String, serde_json::Value>,
        ),
        BatchAbort,
    > {
        // Real NodeId for each create_node op, by op index (local-ref target).
        let mut created_nodes: Vec<Option<NodeId>> = vec![None; planned.len()];
        // Batch-local adjacency ledger: edges created in this batch, with
        // resolved endpoints. `count_connected_edges`/`delete_node_cascade`
        // only see committed storage, so the #3209 contract inside a batch
        // is enforced from this ledger (Issue #3231).
        let mut batch_edges: Vec<BatchEdge> = Vec::new();
        // Committed edges deleted so far (explicitly or by cascade) -> op index.
        let mut deleted_committed_edges: HashMap<u64, usize> = HashMap::new();
        // Committed edges updated so far -> (endpoints, op index), to refuse
        // a later cascade silently discarding the update.
        let mut updated_committed_edges: HashMap<u64, (NodeId, NodeId, usize)> = HashMap::new();
        // Integer ids of nodes/edges CREATED earlier in this batch, mapped to
        // the op index that created them. An update/delete targeting one of
        // these is a guessed-id write against an entity not yet committed —
        // the v1-scope rejection the static `$alias` prevalidation
        // (`batch_committed_node`) enforces for the ref SPELLING. Enforced
        // explicitly here because buffer-aware transaction reads (Issue #3417)
        // removed the incidental `NOT_FOUND` that used to catch the integer-id
        // form (pinned by
        // apply_batch_guessed_id_of_batch_created_node_aborts_whole_batch).
        let mut created_node_ids: HashMap<u64, usize> = HashMap::new();
        let mut created_edge_ids: HashMap<u64, usize> = HashMap::new();

        let mut results = Vec::with_capacity(planned.len());
        let mut ref_map = serde_json::Map::new();

        let op_err = |index: usize| move |source: Error| BatchAbort::Op { index, source };

        for (index, op) in planned.iter().enumerate() {
            match op {
                PlannedOp::CreateNode {
                    label,
                    properties,
                    valid_from,
                    provenance,
                    alias,
                } => {
                    let options = write_options(*valid_from, provenance.clone());
                    let node_id = tx
                        .create_node_with_options(label, properties.clone(), options)
                        .map_err(op_err(index))?;
                    let version_id = tx.buffered_node_version(node_id).map(|v| v.as_u64());
                    created_nodes[index] = Some(node_id);
                    created_node_ids.insert(node_id.as_u64(), index);
                    if let Some(alias) = alias {
                        ref_map.insert(alias.clone(), json!(node_id.as_u64()));
                    }
                    results.push(json!({
                        "op": "create_node",
                        "index": index,
                        "ref": alias,
                        "node_id": node_id.as_u64(),
                        "version_id": version_id
                    }));
                }
                PlannedOp::CreateEdge {
                    source,
                    target,
                    label,
                    properties,
                    valid_from,
                    provenance,
                    annihilated_by,
                } => {
                    let source = resolve_local(index, *source, &created_nodes)?;
                    let target = resolve_local(index, *target, &created_nodes)?;
                    if let Some(k) = annihilated_by {
                        // Elided edge: a later detach-delete of an endpoint
                        // removes it (see module docs). Never issued to the
                        // transaction; recorded in the ledger for counting.
                        batch_edges.push(BatchEdge {
                            source,
                            target,
                            annihilated_by: Some(*k),
                        });
                        results.push(json!({
                            "op": "create_edge",
                            "index": index,
                            "edge_id": serde_json::Value::Null,
                            "version_id": serde_json::Value::Null,
                            "source_id": source.as_u64(),
                            "target_id": target.as_u64(),
                            "removed_by_delete_at_index": k
                        }));
                        continue;
                    }
                    let options = write_options(*valid_from, provenance.clone());
                    let edge_id = tx
                        .create_edge_with_options(
                            source,
                            target,
                            label,
                            properties.clone(),
                            options,
                        )
                        .map_err(op_err(index))?;
                    let version_id = tx.buffered_edge_version(edge_id).map(|v| v.as_u64());
                    created_edge_ids.insert(edge_id.as_u64(), index);
                    batch_edges.push(BatchEdge {
                        source,
                        target,
                        annihilated_by: None,
                    });
                    results.push(json!({
                        "op": "create_edge",
                        "index": index,
                        "edge_id": edge_id.as_u64(),
                        "version_id": version_id,
                        "source_id": source.as_u64(),
                        "target_id": target.as_u64()
                    }));
                }
                PlannedOp::UpdateNode {
                    node_id,
                    properties,
                    valid_from,
                    provenance,
                } => {
                    if let Some(&created_at) = created_node_ids.get(&node_id.as_u64()) {
                        return Err(BatchAbort::BatchCreatedRefWrite {
                            index,
                            entity: "node",
                            id: node_id.as_u64(),
                            created_at_index: created_at,
                            verb: "update",
                        });
                    }
                    let options = write_options(*valid_from, provenance.clone());
                    tx.update_node_with_options(*node_id, properties.clone(), options)
                        .map_err(op_err(index))?;
                    let version_id = tx.buffered_node_version(*node_id).map(|v| v.as_u64());
                    results.push(json!({
                        "op": "update_node",
                        "index": index,
                        "node_id": node_id.as_u64(),
                        "version_id": version_id
                    }));
                }
                PlannedOp::UpdateEdge {
                    edge_id,
                    properties,
                    valid_from,
                    provenance,
                } => {
                    if let Some(&created_at) = created_edge_ids.get(&edge_id.as_u64()) {
                        return Err(BatchAbort::BatchCreatedRefWrite {
                            index,
                            entity: "edge",
                            id: edge_id.as_u64(),
                            created_at_index: created_at,
                            verb: "update",
                        });
                    }
                    if let Some(&removed_at) = deleted_committed_edges.get(&edge_id.as_u64()) {
                        return Err(BatchAbort::EdgeRemovedByCascade {
                            index,
                            edge_id: edge_id.as_u64(),
                            removed_at_index: removed_at,
                        });
                    }
                    // Endpoints recorded so a later detach-delete of either
                    // endpoint refuses rather than discarding this update.
                    let edge = tx.get_edge(*edge_id).map_err(op_err(index))?;
                    let options = write_options(*valid_from, provenance.clone());
                    tx.update_edge_with_options(*edge_id, properties.clone(), options)
                        .map_err(op_err(index))?;
                    let version_id = tx.buffered_edge_version(*edge_id).map(|v| v.as_u64());
                    updated_committed_edges
                        .insert(edge_id.as_u64(), (edge.source, edge.target, index));
                    results.push(json!({
                        "op": "update_edge",
                        "index": index,
                        "edge_id": edge_id.as_u64(),
                        "version_id": version_id
                    }));
                }
                PlannedOp::DeleteNode {
                    node_id,
                    detach,
                    valid_from,
                } => {
                    if let Some(&created_at) = created_node_ids.get(&node_id.as_u64()) {
                        return Err(BatchAbort::BatchCreatedRefWrite {
                            index,
                            entity: "node",
                            id: node_id.as_u64(),
                            created_at_index: created_at,
                            verb: "delete",
                        });
                    }
                    let edges_removed = self.batch_delete_node(
                        tx,
                        index,
                        *node_id,
                        *detach,
                        *valid_from,
                        &batch_edges,
                        &mut deleted_committed_edges,
                        &updated_committed_edges,
                    )?;
                    results.push(json!({
                        "op": "delete_node",
                        "index": index,
                        "node_id": node_id.as_u64(),
                        "detached": detach,
                        "edges_removed": edges_removed
                    }));
                }
                PlannedOp::DeleteEdge {
                    edge_id,
                    valid_from,
                } => {
                    if let Some(&created_at) = created_edge_ids.get(&edge_id.as_u64()) {
                        return Err(BatchAbort::BatchCreatedRefWrite {
                            index,
                            entity: "edge",
                            id: edge_id.as_u64(),
                            created_at_index: created_at,
                            verb: "delete",
                        });
                    }
                    if let Some(&removed_at) = deleted_committed_edges.get(&edge_id.as_u64()) {
                        return Err(BatchAbort::EdgeRemovedByCascade {
                            index,
                            edge_id: edge_id.as_u64(),
                            removed_at_index: removed_at,
                        });
                    }
                    tx.delete_edge_with_valid_time(*edge_id, *valid_from)
                        .map_err(op_err(index))?;
                    deleted_committed_edges.insert(edge_id.as_u64(), index);
                    results.push(json!({
                        "op": "delete_edge",
                        "index": index,
                        "edge_id": edge_id.as_u64()
                    }));
                }
            }
        }

        Ok((results, ref_map))
    }

    /// Execute a `delete_node` op under the #3209 safe-by-default contract,
    /// counted against committed **and** batch-created adjacency.
    ///
    /// Returns the number of edges removed (committed edges cascaded now,
    /// plus batch-created edges elided by this op).
    #[allow(clippy::too_many_arguments)]
    fn batch_delete_node(
        &self,
        tx: &mut crate::api::transaction::WriteTransaction,
        index: usize,
        node_id: NodeId,
        detach: bool,
        valid_from: Option<Timestamp>,
        batch_edges: &[BatchEdge],
        deleted_committed_edges: &mut HashMap<u64, usize>,
        updated_committed_edges: &HashMap<u64, (NodeId, NodeId, usize)>,
    ) -> Result<usize, BatchAbort> {
        // Existence check through the transaction (same snapshot the delete
        // will use), so a nonexistent node reports NOT_FOUND at this index.
        // A guessed numeric id that a create earlier in this batch allocated
        // is rejected BEFORE reaching here by the explicit batch-created-ref
        // guard in `execute_batch` (Issue #3417) — this read only sees
        // genuinely committed nodes.
        tx.get_node(node_id)
            .map_err(|source| BatchAbort::Op { index, source })?;

        // DISTINCT committed edges (a self-loop appears in both adjacency
        // directions but is one edge — mirrors the retract_node dedup),
        // minus committed edges this batch already deleted.
        let mut committed_edges = tx
            .get_outgoing_edges(node_id)
            .map_err(|source| BatchAbort::Op { index, source })?;
        committed_edges.extend(
            tx.get_incoming_edges(node_id)
                .map_err(|source| BatchAbort::Op { index, source })?,
        );
        committed_edges.sort_unstable();
        committed_edges.dedup();
        let live_committed: Vec<EdgeId> = committed_edges
            .into_iter()
            .filter(|e| !deleted_committed_edges.contains_key(&e.as_u64()))
            .collect();

        // Batch-created edges still alive at this op (ledger; one entry per
        // edge, so a batch self-loop naturally counts once).
        let live_batch = batch_edges
            .iter()
            .filter(|e| e.touches(node_id) && e.alive_at(index))
            .count();

        let connected_edges = live_committed.len() + live_batch;

        // Refuse-by-default: never orphan committed or batch-created edges.
        if connected_edges > 0 && !detach {
            return Err(BatchAbort::Refused {
                index,
                node_id: node_id.as_u64(),
                connected_edges,
            });
        }

        if detach {
            // A cascade would silently discard an edge update made earlier
            // in this batch; refuse with the precise indices instead (commit
            // validation would abort the batch anyway, less precisely).
            // Deterministic attribution: when several updated edges touch
            // this node, report the EARLIEST update (minimum
            // updated_at_index), never arbitrary HashMap iteration order.
            let mut conflict: Option<(u64, usize)> = None;
            for (edge_id, (source, target, updated_at)) in updated_committed_edges {
                if (*source == node_id || *target == node_id)
                    && !deleted_committed_edges.contains_key(edge_id)
                    && conflict.is_none_or(|(_, prev)| *updated_at < prev)
                {
                    conflict = Some((*edge_id, *updated_at));
                }
            }
            if let Some((edge_id, updated_at_index)) = conflict {
                return Err(BatchAbort::UpdatedEdgeCascade {
                    index,
                    edge_id,
                    node_id: node_id.as_u64(),
                    updated_at_index,
                });
            }

            // Manual cascade over committed edges (delete_node_cascade would
            // re-enumerate committed adjacency and double-delete edges this
            // batch already removed). Batch-created edges touching this node
            // were elided at their create op (annihilated_by == index).
            for edge_id in &live_committed {
                tx.delete_edge(*edge_id)
                    .map_err(|source| BatchAbort::Op { index, source })?;
                deleted_committed_edges.insert(edge_id.as_u64(), index);
            }
            tx.delete_node_with_valid_time(node_id, None)
                .map_err(|source| BatchAbort::Op { index, source })?;

            let elided_batch = batch_edges
                .iter()
                .filter(|e| e.touches(node_id) && e.annihilated_by == Some(index))
                .count();
            Ok(live_committed.len() + elided_batch)
        } else {
            // No connected edges: a plain delete cannot orphan anything.
            tx.delete_node_with_valid_time(node_id, valid_from)
                .map_err(|source| BatchAbort::Op { index, source })?;
            Ok(0)
        }
    }

    // ------------------------------------------------------------------
    // Error mapping (#3234 contract)
    // ------------------------------------------------------------------

    /// Map a [`BatchAbort`] to the structured error response. Every shape
    /// carries `details.failed_op_index` (JSON `null` for commit-phase
    /// failures, which have no attributable op).
    fn batch_abort_result(&self, abort: BatchAbort) -> CallToolResult {
        match abort {
            BatchAbort::Op { index, source } => self.batch_db_error(Some(index), &source),
            BatchAbort::Commit { source } => self.batch_db_error(None, &source),
            BatchAbort::Internal { index, message } => self.error_result(
                McpError::new(McpErrorCode::Internal, message)
                    .details(json!({ "failed_op_index": index })),
            ),
            BatchAbort::Refused {
                index,
                node_id,
                connected_edges,
            } => {
                // The #3209 refusal in the #3234 structured shape, with the
                // batch's failed_op_index added and the legacy top-level
                // fields preserved (mirrors handle_delete_node).
                let message = format!(
                    "Operation {index}: node {node_id} has {connected_edges} connected edge(s) \
                     at this point in the batch; refusing to delete. Pass `detach: true` on \
                     this operation to delete the node and its connected edges, or remove the \
                     edges first. The whole batch was rolled back."
                );
                let mut top_level = serde_json::Map::new();
                top_level.insert("success".to_string(), json!(false));
                top_level.insert("node_id".to_string(), json!(node_id));
                top_level.insert("connected_edges".to_string(), json!(connected_edges));
                top_level.insert("detach_required".to_string(), json!(true));
                Self::error_result_with_top_level(
                    McpError::new(McpErrorCode::FailedPrecondition, message).details(json!({
                        "failed_op_index": index,
                        "node_id": node_id,
                        "connected_edges": connected_edges,
                        "detach_required": true
                    })),
                    top_level,
                )
            }
            BatchAbort::UpdatedEdgeCascade {
                index,
                edge_id,
                node_id,
                updated_at_index,
            } => self.error_result(
                McpError::new(
                    McpErrorCode::FailedPrecondition,
                    format!(
                        "Operation {index}: detach-deleting node {node_id} would discard the \
                         update to edge {edge_id} made by operation {updated_at_index}; remove \
                         either the update or the delete. The whole batch was rolled back."
                    ),
                )
                .details(json!({
                    "failed_op_index": index,
                    "node_id": node_id,
                    "edge_id": edge_id,
                    "updated_at_index": updated_at_index
                })),
            ),
            BatchAbort::EdgeRemovedByCascade {
                index,
                edge_id,
                removed_at_index,
            } => self.error_result(
                McpError::new(
                    McpErrorCode::FailedPrecondition,
                    format!(
                        "Operation {index}: edge {edge_id} was already removed earlier in this \
                         batch by the detach delete at operation {removed_at_index}. The whole \
                         batch was rolled back."
                    ),
                )
                .details(json!({
                    "failed_op_index": index,
                    "edge_id": edge_id,
                    "removed_at_index": removed_at_index
                })),
            ),
            // Mirrors the static `$alias` prevalidation in
            // `batch_committed_node`: same INVALID_ARGUMENT code and the same
            // "created in the same batch is not supported in v1" phrasing, for
            // the guessed integer-id form.
            BatchAbort::BatchCreatedRefWrite {
                index,
                entity,
                id,
                created_at_index,
                verb,
            } => {
                let mut details = serde_json::Map::new();
                details.insert("failed_op_index".to_string(), json!(index));
                details.insert(format!("{entity}_id"), json!(id));
                details.insert("created_at_index".to_string(), json!(created_at_index));
                self.error_result(
                    McpError::new(
                        McpErrorCode::InvalidArgument,
                        format!(
                            "Operation {index}: cannot {verb} {entity} {id} — updating or \
                             deleting a {entity} created in the same batch (operation \
                             {created_at_index}) is not supported in v1. Commit the creation \
                             first, then {verb} it in a follow-up call. The whole batch was \
                             rolled back."
                        ),
                    )
                    .details(serde_json::Value::Object(details)),
                )
            }
        }
    }

    /// Classify a database error per #3234, augmented with the batch's
    /// `failed_op_index`. Mirrors `db_error`'s unique-violation enrichment
    /// (structured details plus legacy top-level fields).
    fn batch_db_error(&self, index: Option<usize>, e: &Error) -> CallToolResult {
        let mut details = serde_json::Map::new();
        details.insert("failed_op_index".to_string(), json!(index));
        let mut top_level = serde_json::Map::new();

        if let Some(crate::core::error::ConstraintError::UniqueViolation {
            label,
            property,
            value,
            existing_node_id,
        }) = e.as_constraint()
        {
            details.insert("label".to_string(), json!(label));
            details.insert("property".to_string(), json!(property));
            details.insert("value".to_string(), json!(value));
            details.insert(
                "existing_node_id".to_string(),
                json!(existing_node_id.as_u64()),
            );
            top_level.insert("success".to_string(), json!(false));
            top_level.insert("constraint_violation".to_string(), json!(true));
            top_level.insert("label".to_string(), json!(label));
            top_level.insert("property".to_string(), json!(property));
            top_level.insert("value".to_string(), json!(value));
            top_level.insert(
                "existing_node_id".to_string(),
                json!(existing_node_id.as_u64()),
            );
        }

        Self::error_result_with_top_level(
            McpError::from_db_error(e).details(serde_json::Value::Object(details)),
            top_level,
        )
    }
}

// ============================================================================
// Free helpers
// ============================================================================

/// Build the core write options from a planned op's valid_time/provenance.
fn write_options(
    valid_from: Option<Timestamp>,
    provenance: Option<Provenance>,
) -> WriteRequestOptions {
    let mut options = WriteRequestOptions::new();
    if let Some(valid_from) = valid_from {
        options = options.with_valid_from(valid_from);
    }
    if let Some(provenance) = provenance {
        options = options.with_provenance(provenance);
    }
    options
}

/// Turn a statically resolved endpoint into a real `NodeId` at execution
/// time. A `Local` target always names an earlier, already-executed
/// `create_node` op (pre-validation guarantees it); a missing id here is an
/// internal invariant breach, reported as such rather than panicking.
fn resolve_local(
    index: usize,
    resolved: ResolvedNode,
    created_nodes: &[Option<NodeId>],
) -> Result<NodeId, BatchAbort> {
    match resolved {
        ResolvedNode::Committed(id) => Ok(id),
        ResolvedNode::Local(defined_at) => created_nodes
            .get(defined_at)
            .copied()
            .flatten()
            .ok_or_else(|| BatchAbort::Internal {
                index,
                message: format!(
                    "internal invariant breach: local ref target (op {defined_at}) has no \
                     allocated node id"
                ),
            }),
    }
}
