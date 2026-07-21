//! Deferred MCP-registry batch tools (Issue #3367 / #3352 / #3357 / #3382).
//!
//! HTTP + MCP (`#[api_doc(mcp)]`) surfaces for the ten tools whose Rust APIs
//! landed with their MCP registration deliberately deferred to the
//! coordinator-owned registry batch:
//!
//! - **Drift alarms** (Issue #3367, `semantic-temporal`): `create_drift_monitor`
//!   (write), `list_drift_monitors` (read), `delete_drift_monitor` (write),
//!   `query_drift_alarms` (read), `resolve_drift_alarm` (write).
//! - **Contradiction genealogy** (Issue #3352, `semantic-temporal`):
//!   `contradiction_genealogy` (read), `find_contradictions` (read).
//! - **Counterfactual replay** (Issue #3357, `semantic-temporal`):
//!   `counterfactual_replay` (read).
//! - **Trust propagation** (Issue #3382, `semantic-reasoning`):
//!   `trust_breakdown` (read), `list_trust_policies` (read).
//!
//! Each handler forwards its arguments to the shared
//! [`AletheiaMcpServer`](aletheiadb::mcp::AletheiaMcpServer) via
//! `dispatch_tool_json`, so the response body is byte-identical to the legacy
//! MCP surface (including the feature-off `FAILED_PRECONDITION` twin when the
//! backing cohort is not compiled in). The tools are advertised
//! **unconditionally** (feature-invariant catalog), matching the main-crate
//! registry.

use crate::edge_tools::insert_opt;
use crate::security::{Authorized, ReadClass, WriteClass};
use crate::state::ServerState;
use autumn_web::prelude::{Json, post};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Parse an MCP tool method's JSON string result into a [`Value`].
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

// ============================================================================
// Drift alarms (Issue #3367)
// ============================================================================

/// Request body for [`create_drift_monitor`] — mirrors the main-crate
/// `CreateDriftMonitorRequest`.
#[derive(Debug, Deserialize)]
pub struct CreateDriftMonitorBody {
    /// The vector property key to watch.
    pub property_key: String,
    /// Optional node-label restriction (required for `label_centroid`).
    #[serde(default)]
    pub label: Option<String>,
    /// Optional explicit node-id set.
    #[serde(default)]
    pub entities: Option<Vec<u64>>,
    /// Distance metric: `cosine`, `euclidean`, or `angular`.
    pub metric: String,
    /// Firing threshold (strict `>`).
    pub threshold: f64,
    /// Comparison window in microseconds.
    pub window_micros: u64,
    /// Firing target: `per_entity` or `label_centroid`.
    #[serde(default)]
    pub target: Option<String>,
    /// Evaluation mode: `on_write` or `scheduled`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Evaluation interval (microseconds) when `mode` is `scheduled`.
    #[serde(default)]
    pub scheduled_interval_micros: Option<u64>,
}

/// `create_drift_monitor` — declare a semantic-drift monitor (Issue #3367).
/// [`WriteClass`]. HTTP + MCP tool. Requires the `semantic-temporal` feature.
#[post("/drift_monitors")]
#[api_doc(
    description = "Declare a semantic-drift monitor: watch a vector property and fire a durable, queryable alarm when meaning drifts past a threshold over a time window",
    mcp
)]
pub async fn create_drift_monitor(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<CreateDriftMonitorBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("property_key".to_string(), Value::from(req.property_key));
    insert_opt(&mut args, "label", req.label.map(Value::from));
    insert_opt(&mut args, "entities", req.entities.map(|v| json!(v)));
    args.insert("metric".to_string(), Value::from(req.metric));
    args.insert("threshold".to_string(), json!(req.threshold));
    args.insert("window_micros".to_string(), json!(req.window_micros));
    insert_opt(&mut args, "target", req.target.map(Value::from));
    insert_opt(&mut args, "mode", req.mode.map(Value::from));
    insert_opt(
        &mut args,
        "scheduled_interval_micros",
        req.scheduled_interval_micros.map(Value::from),
    );
    tool_json(server.dispatch_tool_json("create_drift_monitor", Value::Object(args)))
}

/// Request body for [`list_drift_monitors`] (no arguments).
#[derive(Debug, Default, Deserialize)]
pub struct ListDriftMonitorsBody {}

/// `list_drift_monitors` — list all declared drift monitors (Issue #3367).
/// [`ReadClass`]. HTTP + MCP tool. Requires the `semantic-temporal` feature.
#[post("/drift_monitors/list")]
#[api_doc(description = "List all declared drift monitors and their specs", mcp)]
pub async fn list_drift_monitors(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(_req): Json<ListDriftMonitorsBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.dispatch_tool_json("list_drift_monitors", json!({})))
}

/// Request body for [`delete_drift_monitor`].
#[derive(Debug, Deserialize)]
pub struct DeleteDriftMonitorBody {
    /// The id of the monitor to delete.
    pub id: u64,
}

/// `delete_drift_monitor` — delete a monitor by id (Issue #3367).
/// [`WriteClass`]. HTTP + MCP tool. Requires the `semantic-temporal` feature.
#[post("/drift_monitors/delete")]
#[api_doc(
    description = "Delete a drift monitor by id, removing it from future evaluation",
    mcp
)]
pub async fn delete_drift_monitor(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<DeleteDriftMonitorBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("id".to_string(), Value::from(req.id));
    tool_json(server.dispatch_tool_json("delete_drift_monitor", Value::Object(args)))
}

/// Request body for [`query_drift_alarms`].
#[derive(Debug, Default, Deserialize)]
pub struct QueryDriftAlarmsBody {
    /// Restrict to a single monitor id.
    #[serde(default)]
    pub monitor_id: Option<u64>,
    /// Restrict to a label.
    #[serde(default)]
    pub label: Option<String>,
    /// Restrict by resolved state.
    #[serde(default)]
    pub resolved: Option<bool>,
    /// Inclusive lower bound on fire (transaction) time.
    #[serde(default)]
    pub time_range_start: Option<String>,
    /// Exclusive upper bound on fire (transaction) time.
    #[serde(default)]
    pub time_range_end: Option<String>,
    /// Maximum alarms to return (clamped to 1000).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `query_drift_alarms` — query fired drift alarms (Issue #3367). [`ReadClass`].
/// HTTP + MCP tool. Requires the `semantic-temporal` feature.
#[post("/drift_alarms/query")]
#[api_doc(
    description = "Query fired drift alarms filtered by monitor, label, resolved state, and fire-time window",
    mcp
)]
pub async fn query_drift_alarms(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<QueryDriftAlarmsBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    insert_opt(&mut args, "monitor_id", req.monitor_id.map(Value::from));
    insert_opt(&mut args, "label", req.label.map(Value::from));
    insert_opt(&mut args, "resolved", req.resolved.map(Value::from));
    insert_opt(
        &mut args,
        "time_range_start",
        req.time_range_start.map(Value::from),
    );
    insert_opt(
        &mut args,
        "time_range_end",
        req.time_range_end.map(Value::from),
    );
    insert_opt(&mut args, "limit", req.limit.map(Value::from));
    tool_json(server.dispatch_tool_json("query_drift_alarms", Value::Object(args)))
}

/// Request body for [`resolve_drift_alarm`].
#[derive(Debug, Deserialize)]
pub struct ResolveDriftAlarmBody {
    /// The node id of the alarm to resolve.
    pub alarm_id: u64,
}

/// `resolve_drift_alarm` — resolve an alarm as a recorded update (Issue #3367).
/// [`WriteClass`]. HTTP + MCP tool. Requires the `semantic-temporal` feature.
#[post("/drift_alarms/resolve")]
#[api_doc(
    description = "Resolve a drift alarm as a recorded, AS OF-stable update (the alarm is never deleted)",
    mcp
)]
pub async fn resolve_drift_alarm(
    _auth: Authorized<WriteClass>,
    state: ServerState,
    Json(req): Json<ResolveDriftAlarmBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("alarm_id".to_string(), Value::from(req.alarm_id));
    tool_json(server.dispatch_tool_json("resolve_drift_alarm", Value::Object(args)))
}

// ============================================================================
// Contradiction genealogy (Issue #3352)
// ============================================================================

/// A competing-claim reference in a [`ContradictionGenealogyBody`].
#[derive(Debug, Deserialize)]
pub struct ClaimRefBody {
    /// The claim's entity kind: `node` or `edge`.
    pub entity_kind: String,
    /// The entity id.
    pub id: u64,
    /// The version id that asserted the claim.
    pub version: u64,
}

/// Request body for [`contradiction_genealogy`].
#[derive(Debug, Default, Deserialize)]
pub struct ContradictionGenealogyBody {
    /// Entity kind when targeting one entity's property.
    #[serde(default)]
    pub entity_kind: Option<String>,
    /// Entity id when targeting one entity's property.
    #[serde(default)]
    pub id: Option<u64>,
    /// Property key when targeting one entity's property.
    #[serde(default)]
    pub property: Option<String>,
    /// Explicit competing-claim set (alternative to entity_kind/id/property).
    #[serde(default)]
    pub claims: Option<Vec<ClaimRefBody>>,
    /// Time-travel: only claims recorded at or before this transaction time.
    #[serde(default)]
    pub as_of_transaction_time: Option<String>,
    /// Cap on competing claims returned.
    #[serde(default)]
    pub max_claims: Option<usize>,
    /// Cap on per-source summaries returned.
    #[serde(default)]
    pub max_sources: Option<usize>,
}

/// `contradiction_genealogy` — reconstruct how conflicting claims about one
/// fact evolved (Issue #3352). [`ReadClass`]. HTTP + MCP tool. Requires the
/// `semantic-temporal` feature.
#[post("/contradictions/genealogy")]
#[api_doc(
    description = "Reconstruct how conflicting claims about one fact evolved across bi-temporal history and provenance, with a divergence point, per-source trust summary, and prose narrative",
    mcp
)]
pub async fn contradiction_genealogy(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<ContradictionGenealogyBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    insert_opt(&mut args, "entity_kind", req.entity_kind.map(Value::from));
    insert_opt(&mut args, "id", req.id.map(Value::from));
    insert_opt(&mut args, "property", req.property.map(Value::from));
    if let Some(claims) = req.claims {
        let arr: Vec<Value> = claims
            .into_iter()
            .map(|c| {
                json!({
                    "entity_kind": c.entity_kind,
                    "id": c.id,
                    "version": c.version,
                })
            })
            .collect();
        args.insert("claims".to_string(), Value::Array(arr));
    }
    insert_opt(
        &mut args,
        "as_of_transaction_time",
        req.as_of_transaction_time.map(Value::from),
    );
    insert_opt(&mut args, "max_claims", req.max_claims.map(Value::from));
    insert_opt(&mut args, "max_sources", req.max_sources.map(Value::from));
    tool_json(server.dispatch_tool_json("contradiction_genealogy", Value::Object(args)))
}

/// Request body for [`find_contradictions`].
#[derive(Debug, Default, Deserialize)]
pub struct FindContradictionsBody {
    /// Entity kinds to scan: `nodes`, `edges`, or `both`.
    #[serde(default)]
    pub entity_kind: Option<String>,
    /// Node-label / edge-type filter.
    #[serde(default)]
    pub label: Option<String>,
    /// Single property to analyze.
    #[serde(default)]
    pub property: Option<String>,
    /// Valid-time window lower bound.
    #[serde(default)]
    pub valid_time_start: Option<String>,
    /// Valid-time window upper bound.
    #[serde(default)]
    pub valid_time_end: Option<String>,
    /// Transaction-time window lower bound.
    #[serde(default)]
    pub transaction_time_start: Option<String>,
    /// Transaction-time window upper bound.
    #[serde(default)]
    pub transaction_time_end: Option<String>,
    /// Page size (default 100, clamped to 1000).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Page offset.
    #[serde(default)]
    pub offset: Option<usize>,
}

/// `find_contradictions` — scan for entity+property pairs holding conflicting
/// values over overlapping valid-time intervals (Issue #3352). [`ReadClass`].
/// HTTP + MCP tool. Requires the `semantic-temporal` feature.
#[post("/contradictions/find")]
#[api_doc(
    description = "Scan the database (by label / property / time window, paginated) for entity+property pairs holding conflicting values over overlapping valid-time intervals",
    mcp
)]
pub async fn find_contradictions(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<FindContradictionsBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    insert_opt(&mut args, "entity_kind", req.entity_kind.map(Value::from));
    insert_opt(&mut args, "label", req.label.map(Value::from));
    insert_opt(&mut args, "property", req.property.map(Value::from));
    insert_opt(
        &mut args,
        "valid_time_start",
        req.valid_time_start.map(Value::from),
    );
    insert_opt(
        &mut args,
        "valid_time_end",
        req.valid_time_end.map(Value::from),
    );
    insert_opt(
        &mut args,
        "transaction_time_start",
        req.transaction_time_start.map(Value::from),
    );
    insert_opt(
        &mut args,
        "transaction_time_end",
        req.transaction_time_end.map(Value::from),
    );
    insert_opt(&mut args, "limit", req.limit.map(Value::from));
    insert_opt(&mut args, "offset", req.offset.map(Value::from));
    tool_json(server.dispatch_tool_json("find_contradictions", Value::Object(args)))
}

// ============================================================================
// Counterfactual replay (Issue #3357)
// ============================================================================

/// Request body for [`counterfactual_replay`].
#[derive(Debug, Deserialize)]
pub struct CounterfactualReplayBody {
    /// A human-readable name for the counterfactual view.
    pub name: String,
    /// Exclude all writes attributed to this single source.
    #[serde(default)]
    pub exclude_source: Option<String>,
    /// Exclude all writes attributed to any source in this set.
    #[serde(default)]
    pub exclude_sources: Option<Vec<String>>,
    /// Inclusive lower bound on a write's transaction time for exclusion.
    #[serde(default)]
    pub within_transaction_from: Option<String>,
    /// Exclusive upper bound on a write's transaction time for exclusion.
    #[serde(default)]
    pub within_transaction_to: Option<String>,
    /// Cap on recorded versions to materialize.
    #[serde(default)]
    pub max_replay_versions: Option<usize>,
}

/// `counterfactual_replay` — materialize a read-only counterfactual view that
/// excludes a source's writes and report the blast radius (Issue #3357).
/// [`ReadClass`] (the real DB is never mutated). HTTP + MCP tool. Requires the
/// `semantic-temporal` feature.
#[post("/counterfactual/replay")]
#[api_doc(
    description = "Materialize a read-only counterfactual view that excludes a source's writes and report the blast radius; the real database is never mutated and the response carries a counterfactual: true marker",
    mcp
)]
pub async fn counterfactual_replay(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<CounterfactualReplayBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("name".to_string(), Value::from(req.name));
    insert_opt(
        &mut args,
        "exclude_source",
        req.exclude_source.map(Value::from),
    );
    insert_opt(
        &mut args,
        "exclude_sources",
        req.exclude_sources.map(|v| json!(v)),
    );
    insert_opt(
        &mut args,
        "within_transaction_from",
        req.within_transaction_from.map(Value::from),
    );
    insert_opt(
        &mut args,
        "within_transaction_to",
        req.within_transaction_to.map(Value::from),
    );
    insert_opt(
        &mut args,
        "max_replay_versions",
        req.max_replay_versions.map(Value::from),
    );
    tool_json(server.dispatch_tool_json("counterfactual_replay", Value::Object(args)))
}

// ============================================================================
// Trust propagation (Issue #3382)
// ============================================================================

/// Request body for [`trust_breakdown`].
#[derive(Debug, Deserialize)]
pub struct TrustBreakdownBody {
    /// The root fact's entity kind: `node` or `edge`.
    pub entity_kind: String,
    /// The root entity's id.
    pub id: u64,
    /// The root fact's version id.
    pub version: u64,
    /// Maximum transitive depth to expand.
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Maximum descendant nodes to serialize.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Optional AS OF transaction-time coordinate.
    #[serde(default)]
    pub as_of_transaction_time: Option<String>,
}

/// `trust_breakdown` — explain a fact's computed confidence as a tree over its
/// derivation lineage (Issue #3382). [`ReadClass`]. HTTP + MCP tool. Requires
/// the `semantic-reasoning` feature.
#[post("/trust/breakdown")]
#[api_doc(
    description = "Explain a fact's computed confidence as a tree over its derivation lineage: each node's version-pinned reference, status, computed confidence, classification, and combinator",
    mcp
)]
pub async fn trust_breakdown(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<TrustBreakdownBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    let mut args = Map::new();
    args.insert("entity_kind".to_string(), Value::from(req.entity_kind));
    args.insert("id".to_string(), Value::from(req.id));
    args.insert("version".to_string(), Value::from(req.version));
    insert_opt(&mut args, "max_depth", req.max_depth.map(Value::from));
    insert_opt(&mut args, "limit", req.limit.map(Value::from));
    insert_opt(
        &mut args,
        "as_of_transaction_time",
        req.as_of_transaction_time.map(Value::from),
    );
    tool_json(server.dispatch_tool_json("trust_breakdown", Value::Object(args)))
}

/// Request body for [`list_trust_policies`] (no arguments).
#[derive(Debug, Default, Deserialize)]
pub struct ListTrustPoliciesBody {}

/// `list_trust_policies` — list the active trust-propagation policies
/// (Issue #3382). [`ReadClass`]. HTTP + MCP tool. Requires the
/// `semantic-reasoning` feature.
#[post("/trust/policies")]
#[api_doc(
    description = "List the active trust-propagation policies: the database default combinator + missing-confidence rule and per-label overrides",
    mcp
)]
pub async fn list_trust_policies(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(_req): Json<ListTrustPoliciesBody>,
) -> Json<Value> {
    let server = state.mcp_server();
    tool_json(server.dispatch_tool_json("list_trust_policies", json!({})))
}
