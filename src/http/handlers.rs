//! HTTP request handlers for the AletheiaDB JSON API.
//!
//! Two public route handlers are exposed:
//!
//! - `GET /status` → [`health_check`]
//! - `POST /query` → [`handle_query`] (polymorphic JSON payload, see [`QueryRequest`])
//!
//! # Example Request
//!
//! ```json
//! {
//!   "operation": "find_node",
//!   "label": "Person",
//!   "properties": { "name": "Alice" }
//! }
//! ```

use crate::api::transaction::WriteOps;
use crate::auth::AccessClass;
use crate::core::NodeId;
use crate::http::auth::AuthContext;
use crate::http::config::{
    EffectiveQueryLimits, LimitDimension, QueryLimitsOverride, RowOverflowPolicy,
};
use crate::http::converters::{
    interned_to_string, json_to_parameter_map, json_to_property_map, property_map_to_json,
    query_row_to_json,
};
use crate::http::error::{AletheiaHttpError, ResourceLimitExceeded};
use crate::http::state::{AppState, InFlightCounter};
use crate::http::trace::HttpTrace;
use crate::query::QueryBuilder;
use crate::query::converter::{parse_query, parse_query_with_params};
use crate::query::ir::{Predicate, PredicateValue};
use crate::query::read_only::detect_mutating_clause;
use autumn_web::Route;
use autumn_web::prelude::{get, post, routes};
use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

// Resource caps to prevent DoS via deep pagination / large result sets.
const MAX_DEEP_PAGINATION: usize = 10_000;
const MAX_NEIGHBOR_LIMIT: usize = 1_000;
const MAX_EXEC_RESULTS: usize = 10_000;
const MAX_BULK_ITEMS: usize = 1_000;
const MAX_BULK_EXEC_TOTAL_ROWS: usize = 20_000;

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: String,
}

/// Health check endpoint. Returns `{"status": "healthy"}` with HTTP 200.
///
/// Classified [`AccessClass::Metrics`]: every role may probe liveness, but
/// (in required-auth mode) only with a valid credential.
#[get("/status")]
pub async fn health_check(
    trace: HttpTrace,
    auth: AuthContext,
) -> Result<Json<HealthResponse>, AletheiaHttpError> {
    trace.record_operation("health");
    // Enter the root span so the probe is a first-class (non-orphaned) span.
    // There is no `.await` inside this block, so holding the guard is sound.
    #[cfg(feature = "observability")]
    let _entered = trace.span().entered();
    auth.authorize(AccessClass::Metrics)?;
    Ok(Json(HealthResponse {
        status: "healthy".to_string(),
    }))
}

// ============================================================================
// Query Endpoint
// ============================================================================

/// Optional provenance filter parameters (Issue #3348) accepted by the
/// read operations (`find_node`, `get_node`, `find_neighbors`).
///
/// Flattened into each read variant so callers pass the four keys inline;
/// all default to absent, so omitting them reproduces prior behavior exactly.
/// Validated via [`ProvenanceFilterParams::into_filter`], sharing the
/// surface-agnostic [`ProvenanceFilter`](crate::core::ProvenanceFilter)
/// semantics with the MCP surface.
#[derive(Debug, Default, Deserialize)]
pub struct ProvenanceFilterParams {
    /// Exact-match provenance source.
    #[serde(default)]
    pub provenance_source: Option<String>,
    /// Any-of provenance sources (unioned with `provenance_source`).
    #[serde(default)]
    pub provenance_sources: Option<Vec<String>>,
    /// Inclusive minimum recorded confidence, in `[0.0, 1.0]`.
    #[serde(default)]
    pub min_confidence: Option<f64>,
    /// Re-include versions with no recorded provenance.
    #[serde(default)]
    pub include_unattributed: Option<bool>,
}

impl ProvenanceFilterParams {
    /// Validate into a [`ProvenanceFilter`](crate::core::ProvenanceFilter),
    /// returning `Ok(None)` when no constraint is active (byte-identical
    /// legacy behavior). Invalid values map to a `400 BadRequest`
    /// (INVALID_ARGUMENT) whose message names the offending field (Issue
    /// #3348 AC5), never a silently-empty success.
    fn into_filter(self) -> Result<Option<crate::core::ProvenanceFilter>, AletheiaHttpError> {
        crate::core::ProvenanceFilter::validated(
            self.provenance_source,
            self.provenance_sources,
            self.min_confidence,
            self.include_unattributed.unwrap_or(false),
        )
        .map_err(|e| AletheiaHttpError::BadRequest(format!("{} (field: {})", e, e.field())))
    }
}

/// Polymorphic request for the `/query` endpoint, discriminated by `operation`.
#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
#[allow(missing_docs)] // fields are self-describing via their names and variant-level docs
pub enum QueryRequest {
    /// Find nodes matching label/property filters. Paginated.
    FindNode {
        label: Option<String>,
        properties: Option<HashMap<String, Value>>,
        limit: Option<usize>,
        offset: Option<usize>,
        /// Provenance filter (Issue #3348), all optional. See
        /// [`ProvenanceFilterParams`].
        #[serde(flatten)]
        provenance_filter: ProvenanceFilterParams,
    },
    /// Get a single node by its 64-bit ID.
    GetNode {
        node_id: u64,
        /// Provenance filter (Issue #3348), all optional.
        #[serde(flatten)]
        provenance_filter: ProvenanceFilterParams,
    },
    /// Create a new node with an optional property map.
    CreateNode {
        label: String,
        properties: Option<HashMap<String, Value>>,
    },
    /// Create many nodes atomically in one transaction.
    BulkCreateNodes { nodes: Vec<CreateNodeInput> },
    /// Fetch many nodes by ID.
    BulkGetNodes { node_ids: Vec<u64> },
    /// Update many nodes atomically in one transaction.
    BulkUpdateNodes { updates: Vec<UpdateNodeInput> },
    /// Delete many nodes atomically in one transaction.
    BulkDeleteNodes {
        node_ids: Vec<u64>,
        #[serde(default)]
        cascade: bool,
    },
    /// Find neighbors (either direction) of a node. Paginated, deduped.
    FindNeighbors {
        node_id: u64,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
        /// Provenance filter (Issue #3348), all optional.
        #[serde(default, flatten)]
        provenance_filter: ProvenanceFilterParams,
    },
    /// Execute an AQL query string, optionally with bound parameters.
    ExecuteQuery {
        query: String,
        parameters: Option<HashMap<String, Value>>,
    },
    /// Execute many AQL queries in sequence and return per-query rows.
    BulkExecuteQuery { queries: Vec<ExecuteQueryInput> },
}

/// Payload for node creation inside a bulk request.
#[derive(Debug, Deserialize)]
pub struct CreateNodeInput {
    label: String,
    properties: Option<HashMap<String, Value>>,
}

/// Payload for node update inside a bulk request.
#[derive(Debug, Deserialize)]
pub struct UpdateNodeInput {
    node_id: u64,
    properties: Option<HashMap<String, Value>>,
}

/// Payload for query execution inside a bulk request.
#[derive(Debug, Deserialize)]
pub struct ExecuteQueryInput {
    query: String,
    parameters: Option<HashMap<String, Value>>,
}

/// Wrapper for all JSON responses: `{ success: bool, data?: ..., error?: ... }`.
#[derive(Debug, Serialize)]
pub struct ApiResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// `truncated` is additive (Issue #3368): present and `true` only when a
    /// result row cap under [`RowOverflowPolicy::Truncate`] dropped rows;
    /// omitted otherwise, so existing responses are unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<bool>,
}

impl ApiResponse {
    /// Build a `{success: true, data}` success envelope.
    ///
    /// Exposed for the autumn-web migration spike (Issue #3524): the isolated
    /// `aletheia-autumn-spike` crate wraps its `GET /nodes/{id}` node in this
    /// exact envelope so its response is byte-identical to `POST /query`.
    /// Widening from `pub(crate)` to `pub` is additive and changes no behavior.
    #[must_use]
    pub fn success(data: Value) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            truncated: None,
        }
    }

    /// Success response that flags `truncated: true` when the row cap dropped
    /// rows (Issue #3368). When `truncated` is `false`, the field is omitted.
    pub(crate) fn success_with_truncation(data: Value, truncated: bool) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            truncated: truncated.then_some(true),
        }
    }
}

/// Convert a `serde_json::Value` into a `PredicateValue`, or `None` for
/// unsupported kinds (arrays/objects aren't valid equality predicates).
fn json_to_predicate_value(v: &Value) -> Option<PredicateValue> {
    match v {
        Value::Null => Some(PredicateValue::Null),
        Value::Bool(b) => Some(PredicateValue::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(PredicateValue::Int(i))
            } else {
                n.as_f64().map(PredicateValue::Float)
            }
        }
        Value::String(s) => Some(PredicateValue::String(s.clone())),
        _ => None,
    }
}

/// Run a CPU/IO-bound closure on the blocking pool and unwrap the join error.
///
/// The caller's current `tracing` span is propagated into the blocking thread
/// and entered for the duration of `f`, so the database's own child spans
/// (`aletheiadb.query.execute`, `aletheiadb.transaction.commit`, …) nest under
/// the HTTP root span even though they run on a different thread (Issue #3376).
async fn blocking<F, T>(f: F) -> Result<T, AletheiaHttpError>
where
    F: FnOnce() -> Result<T, AletheiaHttpError> + Send + 'static,
    T: Send + 'static,
{
    #[cfg(feature = "observability")]
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || {
        #[cfg(feature = "observability")]
        let _entered = span.enter();
        f()
    })
    .await
    .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?
}

// ============================================================================
// Sub-handlers — each returns a JSON data payload or an error with a status.
// ============================================================================

/// Build the [`WriteRequestOptions`](crate::api::transaction::WriteRequestOptions)
/// for an authenticated `/query` write: when the request carried a verified
/// principal, its name is stamped into a write-time provenance bundle
/// (Issue #3350) so "who wrote this fact" is answerable from history.
/// Anonymous-mode requests get default options (no provenance).
fn write_options_for(principal: Option<&str>) -> crate::api::transaction::WriteRequestOptions {
    let options = crate::api::transaction::WriteRequestOptions::new();
    match principal.and_then(|name| {
        // A principal-only bundle cannot fail validation (only `confidence`
        // is validated, and it is unset here); `.ok()` keeps this
        // non-panicking regardless.
        crate::core::provenance::Provenance::builder()
            .principal(name)
            .build()
            .ok()
    }) {
        Some(provenance) => options.with_provenance(provenance),
        None => options,
    }
}

async fn handle_create_node(
    db: Arc<crate::AletheiaDB>,
    label: String,
    properties: Option<HashMap<String, Value>>,
    principal: Option<String>,
) -> Result<Value, AletheiaHttpError> {
    let props = match properties {
        // A property-KEY interner capacity breach must render as `412
        // FAILED_PRECONDITION` (configurable interner cap); a bad value stays
        // `400 INVALID_ARGUMENT`. `from_db_error` routes each correctly.
        Some(p) => json_to_property_map(&p).map_err(|e| AletheiaHttpError::from_db_error(&e))?,
        None => crate::core::PropertyMap::new(),
    };

    blocking(move || {
        let node_id = db
            .create_node_with_options(&label, props, write_options_for(principal.as_deref()))
            // Issue #3378: a schema-constraint violation renders with the
            // structured #3234 envelope (code/details); every other write
            // error keeps the prior 400 BadRequest mapping.
            .map_err(|e| AletheiaHttpError::from_db_error(&e))?;
        let node = db
            .get_node(node_id)
            .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
        let props_json =
            property_map_to_json(&node.properties).map_err(AletheiaHttpError::Internal)?;
        Ok(json!({
            "id": node.id.as_u64(),
            "label": interned_to_string(node.label),
            "properties": props_json,
        }))
    })
    .await
}

/// Load a node's write-time provenance bundle for the exact version the
/// snapshot is holding (Issue #3348). Best-effort: an unreadable metadata
/// record degrades to `None` (treated as unattributed by the filter),
/// mirroring the MCP surface, rather than failing the whole read.
fn node_provenance(
    db: &crate::AletheiaDB,
    node: &crate::core::Node,
) -> Option<crate::core::Provenance> {
    match db.get_node_version_read_metadata(node.current_version) {
        Ok(Some((prov, _interval))) => prov,
        _ => None,
    }
}

async fn handle_get_node(
    db: Arc<crate::AletheiaDB>,
    node_id: u64,
    provenance_filter: ProvenanceFilterParams,
) -> Result<Value, AletheiaHttpError> {
    let nid = NodeId::new(node_id).map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;
    // Issue #3348: validate the filter up front (400 on bad input, AC5).
    let prov_filter = provenance_filter.into_filter()?;

    blocking(move || {
        let node = db
            .get_node(nid)
            .map_err(|_| AletheiaHttpError::NotFound(format!("Node {node_id} not found")))?;
        // Issue #3348: a node that exists but fails the provenance filter is
        // not in the filtered view -> an honest 404 (never a fabricated node).
        if let Some(filter) = &prov_filter
            && !filter.matches(node_provenance(&db, &node).as_ref())
        {
            return Err(AletheiaHttpError::NotFound(format!(
                "Node {node_id} not found"
            )));
        }
        // Issue #3524: reuse the shared serializer so the migration-spike's
        // `GET /nodes/{id}` produces byte-identical output. Behavior unchanged.
        crate::http::converters::node_to_query_json(&node).map_err(AletheiaHttpError::Internal)
    })
    .await
}

async fn handle_find_node(
    db: Arc<crate::AletheiaDB>,
    label: Option<String>,
    properties: Option<HashMap<String, Value>>,
    limit: Option<usize>,
    offset: Option<usize>,
    provenance_filter: ProvenanceFilterParams,
) -> Result<Value, AletheiaHttpError> {
    let limit_val = limit.unwrap_or(100);
    let offset_val = offset.unwrap_or(0);
    // Issue #3348: validate the filter up front (400 on bad input, AC5).
    let prov_filter = provenance_filter.into_filter()?;

    if offset_val.saturating_add(limit_val) > MAX_DEEP_PAGINATION {
        return Err(AletheiaHttpError::BadRequest(format!(
            "Pagination limit exceeded: offset + limit must be <= {MAX_DEEP_PAGINATION}"
        )));
    }

    // Issue #3348 (FIX A): the HTTP find_node response is a bare array with no
    // `has_more`/`next_offset` signal, so a page that filters down below
    // `limit_val` mid-scan would make a client using the standard "short page
    // => end of results" heuristic stop early and under-read later matches.
    // When a provenance filter is active we therefore OVER-FETCH and REFILL:
    // scan forward past `limit_val` until the page holds `limit_val`
    // filter-passing rows (or the source is exhausted), so a short/empty page
    // genuinely means end-of-data. The refill scan is bounded by
    // `MAX_DEEP_PAGINATION` (the same horizon enforced on `offset + limit`
    // above) -- it is never unbounded; if that cap is reached before the page
    // fills, we stop and return the shorter page (documented boundary). With no
    // filter the behavior is byte-identical to before (scan exactly `limit_val`).
    let scan_limit = if prov_filter.is_some() {
        MAX_DEEP_PAGINATION.saturating_sub(offset_val)
    } else {
        limit_val
    };

    blocking(move || {
        let mut builder = if let Some(lbl) = label {
            QueryBuilder::new().scan_label(&lbl)
        } else {
            QueryBuilder::new().scan(None)
        };

        if let Some(props) = properties {
            for (key, value) in props {
                if let Some(pred_value) = json_to_predicate_value(&value) {
                    builder = builder.filter(Predicate::eq(key, pred_value));
                }
            }
        }

        if let Some(skip) = offset {
            builder = builder.skip(skip);
        }

        let results = builder
            .limit(scan_limit)
            .execute(&db)
            .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;

        // NOTE: explicit `row_result?` rather than `.flatten()` so storage
        // errors mid-scan propagate as 500 instead of being silently dropped
        // and producing a partial `success: true` response.
        let mut nodes = Vec::new();
        for row_result in results {
            let row = row_result.map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
            if let crate::query::executor::EntityResult::Node(node) = row.entity {
                // Issue #3348: per-page provenance filter. The scan over-fetches
                // (see `scan_limit` above) so the page can be refilled to
                // `limit_val` passing rows; iteration is lazy, so we stop
                // consuming the scan as soon as the page is full.
                if let Some(filter) = &prov_filter
                    && !filter.matches(node_provenance(&db, &node).as_ref())
                {
                    continue;
                }
                let props_json =
                    property_map_to_json(&node.properties).map_err(AletheiaHttpError::Internal)?;
                nodes.push(json!({
                    "id": node.id.as_u64(),
                    "label": interned_to_string(node.label),
                    "properties": props_json,
                }));
                if nodes.len() >= limit_val {
                    break;
                }
            }
        }
        Ok(Value::Array(nodes))
    })
    .await
}

async fn handle_find_neighbors(
    db: Arc<crate::AletheiaDB>,
    node_id: u64,
    limit: Option<usize>,
    offset: Option<usize>,
    provenance_filter: ProvenanceFilterParams,
) -> Result<Value, AletheiaHttpError> {
    let nid = NodeId::new(node_id).map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;

    let limit_val = limit.unwrap_or(100).min(MAX_NEIGHBOR_LIMIT);
    let offset_val = offset.unwrap_or(0);
    // Issue #3348: validate the filter up front (400 on bad input, AC5).
    let prov_filter = provenance_filter.into_filter()?;

    if offset_val.saturating_add(limit_val) > MAX_DEEP_PAGINATION {
        return Err(AletheiaHttpError::BadRequest(format!(
            "Pagination limit exceeded: offset + limit must be <= {MAX_DEEP_PAGINATION}"
        )));
    }

    // Issue #3348 (FIX A): like find_node, find_neighbors returns a bare array
    // with no completeness signal, so a provenance-filtered page that would
    // fall short mid-scan is refilled by over-fetching neighbor candidates
    // until the page holds `limit_val` passing rows (or candidates are
    // exhausted). The refill scan is bounded by `MAX_DEEP_PAGINATION` (the same
    // horizon enforced on `offset + limit` above) so it is never unbounded; if
    // that cap is reached before the page fills we stop (documented boundary).
    // With no filter the behavior is unchanged (take exactly `limit_val`).
    let scan_limit = if prov_filter.is_some() {
        MAX_DEEP_PAGINATION.saturating_sub(offset_val)
    } else {
        limit_val
    };

    blocking(move || {
        let mut seen_ids = HashSet::new();
        let mut neighbors = Vec::with_capacity(limit_val);

        let outgoing_iter = db
            .get_outgoing_edges_iter(nid)
            .map(|edge_id| db.get_edge_target(edge_id).ok());
        let incoming_iter = db
            .get_incoming_edges_iter(nid)
            .map(|edge_id| db.get_edge_source(edge_id).ok());

        // `take(scan_limit)` bounds the candidate scan; when a filter is active
        // this over-fetches past `limit_val` so the page can be refilled, and
        // the explicit `break` below stops once `limit_val` passing rows are
        // collected (lazy iteration => no wasted candidate reads).
        let combined_iter = outgoing_iter
            .chain(incoming_iter)
            .flatten()
            .filter(|&neighbor_id| seen_ids.insert(neighbor_id))
            .skip(offset_val)
            .take(scan_limit);

        for neighbor_id in combined_iter {
            let node = db
                .get_node(neighbor_id)
                .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
            // Issue #3348: keep only neighbor nodes whose provenance passes.
            if let Some(filter) = &prov_filter
                && !filter.matches(node_provenance(&db, &node).as_ref())
            {
                continue;
            }
            let props_json =
                property_map_to_json(&node.properties).map_err(AletheiaHttpError::Internal)?;
            neighbors.push(json!({
                "id": node.id.as_u64(),
                "label": interned_to_string(node.label),
                "properties": props_json,
            }));
            if neighbors.len() >= limit_val {
                break;
            }
        }
        Ok(Value::Array(neighbors))
    })
    .await
}

async fn handle_execute_query(
    db: Arc<crate::AletheiaDB>,
    query: String,
    parameters: Option<HashMap<String, Value>>,
) -> Result<Value, AletheiaHttpError> {
    blocking(move || {
        let parsed_query = if let Some(params_json) = parameters {
            let params =
                json_to_parameter_map(&params_json).map_err(AletheiaHttpError::BadRequest)?;
            parse_query_with_params(&query, params)
                .map_err(|e| AletheiaHttpError::QueryParse(e.to_string()))?
        } else {
            parse_query(&query).map_err(|e| classify_query_error(e.to_string()))?
        };

        let results = db
            .execute_query(parsed_query)
            .map_err(|e| classify_query_error(e.to_string()))?;

        let rows = results
            .take(MAX_EXEC_RESULTS)
            .map(|row_result| {
                let row = row_result.map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
                query_row_to_json(row).map_err(AletheiaHttpError::Internal)
            })
            .collect::<Result<Vec<_>, AletheiaHttpError>>()?;

        Ok(Value::Array(rows))
    })
    .await
}

fn convert_node_for_response(node: crate::core::graph::Node) -> Result<Value, AletheiaHttpError> {
    let props_json = property_map_to_json(&node.properties).map_err(AletheiaHttpError::Internal)?;
    Ok(json!({
        "id": node.id.as_u64(),
        "label": interned_to_string(node.label),
        "properties": props_json,
    }))
}

fn validate_bulk_size<T>(items: &[T], name: &str) -> Result<(), AletheiaHttpError> {
    if items.is_empty() {
        return Err(AletheiaHttpError::BadRequest(format!(
            "{name} cannot be empty"
        )));
    }
    if items.len() > MAX_BULK_ITEMS {
        return Err(AletheiaHttpError::BadRequest(format!(
            "{name} exceeds max size of {MAX_BULK_ITEMS}"
        )));
    }
    Ok(())
}

async fn handle_bulk_create_nodes(
    db: Arc<crate::AletheiaDB>,
    nodes: Vec<CreateNodeInput>,
    principal: Option<String>,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&nodes, "nodes")?;
    blocking(move || {
        let created_ids = db
            .write(|tx| {
                let mut ids = Vec::with_capacity(nodes.len());
                for node in &nodes {
                    let props = match &node.properties {
                        // Propagate the TYPED error (a property-KEY interner
                        // capacity breach stays `StorageError::CapacityExceeded`)
                        // so the bulk `from_db_error` below renders it as `412
                        // FAILED_PRECONDITION` instead of flattening it to a 400.
                        Some(p) => json_to_property_map(p)?,
                        None => crate::core::PropertyMap::new(),
                    };
                    ids.push(tx.create_node_with_options(
                        &node.label,
                        props,
                        write_options_for(principal.as_deref()),
                    )?);
                }
                Ok::<_, crate::core::error::Error>(ids)
            })
            // Issue #3378: forward schema-constraint details on the bulk path too.
            .map_err(|e| AletheiaHttpError::from_db_error(&e))?;

        let mut created = Vec::with_capacity(created_ids.len());
        for id in created_ids {
            let node = db
                .get_node(id)
                .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
            created.push(convert_node_for_response(node)?);
        }
        Ok(Value::Array(created))
    })
    .await
}

async fn handle_bulk_get_nodes(
    db: Arc<crate::AletheiaDB>,
    node_ids: Vec<u64>,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&node_ids, "node_ids")?;
    blocking(move || {
        let mut found = Vec::with_capacity(node_ids.len());
        let mut missing = Vec::new();

        for node_id in node_ids {
            let nid =
                NodeId::new(node_id).map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;
            match db.get_node(nid) {
                Ok(node) => found.push(convert_node_for_response(node)?),
                Err(_) => missing.push(node_id),
            }
        }

        Ok(json!({
            "nodes": found,
            "missing_node_ids": missing,
        }))
    })
    .await
}

async fn handle_bulk_update_nodes(
    db: Arc<crate::AletheiaDB>,
    updates: Vec<UpdateNodeInput>,
    principal: Option<String>,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&updates, "updates")?;
    blocking(move || {
        let updated_ids = db
            .write(|tx| {
                let mut ids = Vec::with_capacity(updates.len());
                for update in &updates {
                    let node_id = NodeId::new(update.node_id)?;
                    let props = match &update.properties {
                        // Propagate the TYPED error (a property-KEY interner
                        // capacity breach stays `StorageError::CapacityExceeded`)
                        // so the bulk `from_db_error` below renders it as `412
                        // FAILED_PRECONDITION` instead of flattening it to a 400.
                        Some(p) => json_to_property_map(p)?,
                        None => crate::core::PropertyMap::new(),
                    };
                    tx.update_node_with_options(
                        node_id,
                        props,
                        write_options_for(principal.as_deref()),
                    )?;
                    ids.push(node_id);
                }
                Ok::<_, crate::core::error::Error>(ids)
            })
            // Issue #3378: forward schema-constraint details on the bulk path too.
            .map_err(|e| AletheiaHttpError::from_db_error(&e))?;

        let mut updated = Vec::with_capacity(updated_ids.len());
        for id in updated_ids {
            let node = db
                .get_node(id)
                .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
            updated.push(convert_node_for_response(node)?);
        }
        Ok(Value::Array(updated))
    })
    .await
}

async fn handle_bulk_delete_nodes(
    db: Arc<crate::AletheiaDB>,
    node_ids: Vec<u64>,
    cascade: bool,
    principal: Option<String>,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&node_ids, "node_ids")?;
    blocking(move || {
        let deleted_count = db
            .write(|tx| {
                for node_id in &node_ids {
                    let nid = NodeId::new(*node_id)?;
                    if cascade {
                        tx.delete_node_cascade_with_options(
                            nid,
                            write_options_for(principal.as_deref()),
                        )?;
                    } else {
                        tx.delete_node_with_options(nid, write_options_for(principal.as_deref()))?;
                    }
                }
                Ok::<_, crate::core::error::Error>(node_ids.len())
            })
            // Issue #3629/#3234: a delete that fails a precondition (e.g. the
            // #3416 concurrent-orphan validation guard → FAILED_PRECONDITION) or
            // loses a concurrency race (→ CONFLICT, retriable) is classified into
            // the structured envelope, not blanket-collapsed to a 400. Genuine
            // bad input (an invalid id) still maps to 400 via the classifier's
            // BadRequest fallback.
            .map_err(|e| AletheiaHttpError::from_db_error(&e))?;

        Ok(json!({
            "deleted_count": deleted_count,
            "cascade": cascade,
        }))
    })
    .await
}

async fn handle_bulk_execute_query(
    db: Arc<crate::AletheiaDB>,
    queries: Vec<ExecuteQueryInput>,
) -> Result<Value, AletheiaHttpError> {
    validate_bulk_size(&queries, "queries")?;
    blocking(move || {
        let mut all_results = Vec::with_capacity(queries.len());
        let mut total_rows = 0usize;
        for (idx, query_item) in queries.into_iter().enumerate() {
            let parsed_query = if let Some(params_json) = query_item.parameters {
                let params =
                    json_to_parameter_map(&params_json).map_err(AletheiaHttpError::BadRequest)?;
                parse_query_with_params(&query_item.query, params)
                    .map_err(|e| AletheiaHttpError::QueryParse(e.to_string()))?
            } else {
                parse_query(&query_item.query).map_err(|e| classify_query_error(e.to_string()))?
            };

            let rows = db
                .execute_query(parsed_query)
                .map_err(|e| classify_query_error(e.to_string()))?
                .take(MAX_EXEC_RESULTS)
                .map(|row_result| {
                    let row = row_result.map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
                    query_row_to_json(row).map_err(AletheiaHttpError::Internal)
                })
                .collect::<Result<Vec<_>, AletheiaHttpError>>()?;

            total_rows = total_rows.saturating_add(rows.len());
            if total_rows > MAX_BULK_EXEC_TOTAL_ROWS {
                return Err(AletheiaHttpError::BadRequest(format!(
                    "Bulk query result budget exceeded at query {}: total rows {} > {}",
                    idx + 1,
                    total_rows,
                    MAX_BULK_EXEC_TOTAL_ROWS
                )));
            }

            all_results.push(json!({
                "query": query_item.query,
                "rows": rows,
            }));
        }
        Ok(Value::Array(all_results))
    })
    .await
}

/// Heuristic: parser errors → 400, everything else → 500.
fn classify_query_error(msg: String) -> AletheiaHttpError {
    let lowered = msg.to_lowercase();
    if lowered.contains("syntax") || lowered.contains("parse") {
        AletheiaHttpError::QueryParse(msg)
    } else {
        AletheiaHttpError::Internal(msg)
    }
}

/// Classify a `/query` operation into the [`AccessClass`] it requires.
///
/// Exhaustive match — no wildcard arm — so adding a new `QueryRequest`
/// variant forces an explicit classification decision at compile time.
/// This mapping is the seed of the authorization conformance matrix.
fn query_access_class(req: &QueryRequest) -> AccessClass {
    match req {
        // Read-only lookups and traversals.
        QueryRequest::FindNode { .. }
        | QueryRequest::GetNode { .. }
        | QueryRequest::BulkGetNodes { .. }
        | QueryRequest::FindNeighbors { .. } => AccessClass::Read,
        // Graph mutations.
        QueryRequest::CreateNode { .. }
        | QueryRequest::BulkCreateNodes { .. }
        | QueryRequest::BulkUpdateNodes { .. }
        | QueryRequest::BulkDeleteNodes { .. } => AccessClass::Write,
        // Query-string execution: classify per statement using the shared
        // read-only guard (the same one the MCP `query` tool uses). A
        // statement with no mutating clause needs only Read.
        QueryRequest::ExecuteQuery { query, .. } => {
            if detect_mutating_clause(query).is_none() {
                AccessClass::Read
            } else {
                AccessClass::Write
            }
        }
        // A bulk batch is Read only if *every* statement is read-only.
        QueryRequest::BulkExecuteQuery { queries } => {
            if queries
                .iter()
                .all(|q| detect_mutating_clause(&q.query).is_none())
            {
                AccessClass::Read
            } else {
                AccessClass::Write
            }
        }
    }
}

/// The `/query` request envelope (Issue #3368).
///
/// Wraps the polymorphic [`QueryRequest`] (flattened, so the `operation` tag
/// and its fields stay at the top level) and an optional per-call `limits`
/// override. Both `limits` and its inner fields are `#[serde(default)]`, so a
/// request body with no `"limits"` key parses and behaves exactly as before.
#[derive(Debug, Deserialize)]
pub struct QueryEnvelope {
    #[serde(flatten)]
    request: QueryRequest,
    #[serde(default)]
    limits: Option<QueryLimitsOverride>,
}

/// Apply the effective row cap to a result `Value` (Issue #3368).
///
/// Caps the **top-level array length only**: row limits apply to
/// array-shaped results (list-like reads); a single-entity object result is
/// returned unchanged, and nested arrays inside an object are never counted or
/// truncated. Under [`RowOverflowPolicy::Truncate`] the array is capped and
/// `truncated` is `true`; under [`RowOverflowPolicy::Reject`] an over-cap
/// result is a structured `413` error (never retriable — the result is simply
/// too large).
///
/// Only ever invoked for read-class operations (Issue #3368 review MUST-FIX 1):
/// a committed write's acknowledgement is never truncated or rejected here.
fn apply_row_cap(
    data: Value,
    limits: &EffectiveQueryLimits,
) -> Result<(Value, bool), AletheiaHttpError> {
    if limits.max_result_rows == 0 {
        return Ok((data, false));
    }
    match data {
        Value::Array(mut rows) if rows.len() > limits.max_result_rows => {
            let consumed = rows.len();
            match limits.row_overflow {
                RowOverflowPolicy::Truncate => {
                    rows.truncate(limits.max_result_rows);
                    Ok((Value::Array(rows), true))
                }
                RowOverflowPolicy::Reject => Err(AletheiaHttpError::ResourceLimitExceeded(
                    ResourceLimitExceeded {
                        dimension: LimitDimension::ResultRows,
                        limit: limits.max_result_rows as u64,
                        consumed: Some(consumed as u64),
                        retriable: false,
                    },
                )),
            }
        }
        other => Ok((other, false)),
    }
}

/// Enforce the time and result-shaping limit dimensions around an operation
/// future (Issue #3368):
///
/// 1. **Wall-clock timeout** — the operation is raced against `timeout_ms`; on
///    elapse the client gets a prompt `429`. Applies to **all** operations
///    (it bounds the client's wait). (The underlying blocking computation is
///    not cancelled — it runs to completion on the blocking pool — but the HTTP
///    response deadline is bounded. True engine-level cancellation is the
///    query-executor lane's concern.) The timeout's `retriable` flag is `true`
///    only for read-class ops: a **write** may have committed on the blocking
///    pool, so its timeout is `retriable: false` to avoid a duplicate retry
///    (review MUST-FIX 1).
/// 2. **Result-row cap** — truncate-or-reject per policy, **read-class only**.
///    A committed write's small acknowledgement (ids / version ids) is never
///    truncated or `413`'d after the fact.
///
/// The **result-byte cap** is enforced by the caller ([`handle_query`]) on the
/// fully-serialized response envelope, so it too is read-class only and is not
/// applied here.
///
/// # In-flight worker DoS guard (Issue #3368)
///
/// Because a timed op's worker is **detached and non-cancellable** (see above),
/// a caller sending tiny `timeout_ms` overrides — or hammering the documented
/// retriable-timeout retry loop — could pile up unbounded still-running
/// expensive queries and exhaust threads/CPU/memory. So the **timed** branch is
/// bounded: a slot is reserved from `in_flight` (cap = `cap`, `0` = unbounded)
/// **before** the worker is spawned; at the cap the query is rejected `503`
/// `UNAVAILABLE` (retriable) instead of spawning yet another worker. The
/// [`InFlightGuard`](crate::http::state) is moved **into** the spawned worker so
/// its slot is released only when the WORKER finishes — a caller that times out
/// and discards the result keeps holding its slot until the work actually
/// completes. The inline/unlimited branch (`timeout_ms == 0`) never spawns and
/// is therefore never guarded.
///
/// A dimension whose effective value is `0` is unlimited and skipped. Returns
/// the (possibly truncated) result plus a `truncated` flag.
async fn enforce_query_limits<F>(
    in_flight: InFlightCounter,
    cap: usize,
    limits: &EffectiveQueryLimits,
    is_write: bool,
    op: F,
) -> Result<(Value, bool), AletheiaHttpError>
where
    F: Future<Output = Result<Value, AletheiaHttpError>> + Send + 'static,
{
    // 1. Wall-clock timeout (all ops — it bounds the client's wait).
    let data = if limits.timeout_ms > 0 {
        // Admission control: reserve an in-flight slot BEFORE spawning the
        // detached worker; at the cap, reject rather than pile up another
        // non-cancellable query (Issue #3368 DoS guard). `cap == 0` is
        // unbounded (a slot is always granted).
        let guard = in_flight
            .try_acquire(cap)
            .ok_or(AletheiaHttpError::InFlightCapacityExceeded { cap })?;

        // Spawn the op on its own task with the guard moved in, so the slot is
        // released only when the WORKER finishes — even if the caller below
        // times out and discards this handle, the worker runs to completion
        // still holding its slot. Instrument with the current span so the DB's
        // child spans keep nesting under the HTTP root span (Issue #3376).
        #[cfg(feature = "observability")]
        let worker = {
            use tracing::Instrument;
            async move {
                let _guard = guard;
                op.await
            }
            .instrument(tracing::Span::current())
        };
        #[cfg(not(feature = "observability"))]
        let worker = async move {
            let _guard = guard;
            op.await
        };
        let handle = tokio::spawn(worker);

        match tokio::time::timeout(Duration::from_millis(limits.timeout_ms), handle).await {
            // Worker finished within the deadline (guard already dropped inside
            // it, so its slot is released before we observe completion).
            Ok(Ok(result)) => result?,
            // The worker task panicked or was cancelled without a result. Do
            // NOT treat this as a timeout — a deterministic panic must not be
            // reported as an infinitely-retriable timeout. The guard still drops
            // as the panic unwinds, so the slot is released. Carry no
            // engine-internal detail (no panic payload) in the message, matching
            // the MCP surface's worker-died error (Issue #3368).
            Ok(Err(_join_err)) => {
                return Err(AletheiaHttpError::Internal(
                    "query worker terminated unexpectedly before producing a result".to_string(),
                ));
            }
            // Deadline elapsed first. Return promptly; we deliberately do NOT
            // abort `handle` — the detached worker keeps running to completion,
            // holding its in-flight slot until it finishes.
            Err(_elapsed) => {
                return Err(AletheiaHttpError::ResourceLimitExceeded(
                    ResourceLimitExceeded {
                        dimension: LimitDimension::WallClockTimeout,
                        limit: limits.timeout_ms,
                        consumed: None,
                        // A write may already have committed — do not invite a
                        // duplicate retry.
                        retriable: !is_write,
                    },
                ));
            }
        }
    } else {
        // Inline / unlimited path: never spawns, so it is never guarded.
        op.await?
    };

    // 2. Result-row cap — READ-class ops only. A write acknowledgement is never
    // truncated or rejected after the write has committed.
    if is_write {
        return Ok((data, false));
    }
    let (data, truncated) = apply_row_cap(data, limits)?;
    Ok((data, truncated))
}

/// Polymorphic `/query` endpoint. Dispatches on the `operation` tag.
///
/// Authorization happens *before* any database work: the request's operation
/// is classified via [`query_access_class`] and checked against the caller's
/// role. Per-query resource limits (Issue #3368) are then resolved from the
/// server defaults + the optional per-call `limits` override (a request that
/// asks for more than the operator ceiling is rejected `422` here, before any
/// DB work), and enforced around execution by [`enforce_query_limits`].
///
/// The whole handler body runs inside the [`HttpTrace`] root span (Issue
/// #3376), so the database's query/write child spans nest underneath it, the
/// operation name / result count / error code are stamped onto the span, and a
/// failure carries the active trace id back to the caller for correlation.
#[post("/query")]
pub async fn handle_query(
    trace: HttpTrace,
    auth: AuthContext,
    state: AppState,
    Json(envelope): Json<QueryEnvelope>,
) -> Response {
    let QueryEnvelope {
        request: req,
        limits,
    } = envelope;

    // Compute and stamp per-request attributes only when the span will actually
    // be recorded (Issue #3376, MED2 / Perf). A feature-absent build compiles
    // this block out entirely; a runtime-disabled or head-sampled-out span
    // short-circuits `is_recording()` so the (otherwise-wasted) attribute
    // computation — including `request_is_temporal`'s statement scan — never
    // runs.
    #[cfg(feature = "observability")]
    let recording = trace.is_recording();
    #[cfg(feature = "observability")]
    if recording {
        trace.record_operation(operation_name(&req));
        trace.record_temporal_scope(request_is_temporal(&req));
    }
    #[cfg(feature = "otel")]
    maybe_capture_statement(&trace, &req);
    // In an observability-off build the extractor still runs (zero-sized no-op),
    // but nothing consumes `trace`; keep it "used" without renaming the field
    // (which is genuinely used under `observability`).
    #[cfg(not(feature = "observability"))]
    let _ = &trace;

    #[cfg(feature = "observability")]
    let span = trace.span();

    // Classify the operation once. The access class drives BOTH authorization
    // and limit semantics: a write is exempt from the result row/byte caps and
    // its wall-clock timeout is non-retriable (the write may already have
    // committed — Issue #3368 review MUST-FIX 1).
    let access_class = query_access_class(&req);
    let is_write = matches!(access_class, AccessClass::Write);

    // Authorization + limit resolution + dispatch, wrapped in the trace root
    // span so DB child spans nest underneath it and the trace id is available
    // for correlation on any failure (Issue #3376). Resource limits (Issue
    // #3368) are resolved and enforced inside, so a limit rejection (422/413/
    // 429) is stamped and trace-correlated exactly like any other error.
    let work = async move {
        // Authorization first: auth rejects (401/403) before limit logic runs.
        auth.authorize(access_class)?;

        // Read-only replica enforcement (Issue #3355, Slice A): a write-class
        // request against a replica-role node is refused here, before any
        // limit resolution or database work. Read-class requests are
        // unaffected.
        if is_write && state.db().is_replica() {
            return Err(AletheiaHttpError::read_only_replica());
        }

        // Resolve effective limits; an over-ceiling per-call override is a 422
        // rejected up front, before any database work.
        let effective = state
            .query_limits()
            .effective(limits.as_ref())
            .map_err(AletheiaHttpError::InvalidLimitOverride)?;

        // In-flight worker DoS guard (Issue #3368): the shared counter + the
        // configured cap, passed to `enforce_query_limits` which reserves a slot
        // before spawning the timed worker (or rejects `503` at the cap).
        let in_flight = state.in_flight_counter();
        let in_flight_cap = state.query_limits().max_in_flight_queries;

        let db = state.db_arc();
        // Verified principal name (if any) for provenance stamping on write
        // operations (Issue #3350). Anonymous mode yields None.
        let principal = auth.principal().map(|p| p.name.clone());

        // Build (but do not yet await) the operation future so the wall-clock
        // timeout in `enforce_query_limits` wraps the actual execution.
        let op = async move {
            match req {
                QueryRequest::CreateNode { label, properties } => {
                    handle_create_node(db, label, properties, principal).await
                }
                QueryRequest::GetNode {
                    node_id,
                    provenance_filter,
                } => handle_get_node(db, node_id, provenance_filter).await,
                QueryRequest::BulkCreateNodes { nodes } => {
                    handle_bulk_create_nodes(db, nodes, principal).await
                }
                QueryRequest::BulkGetNodes { node_ids } => {
                    handle_bulk_get_nodes(db, node_ids).await
                }
                QueryRequest::BulkUpdateNodes { updates } => {
                    handle_bulk_update_nodes(db, updates, principal).await
                }
                QueryRequest::BulkDeleteNodes { node_ids, cascade } => {
                    handle_bulk_delete_nodes(db, node_ids, cascade, principal).await
                }
                QueryRequest::FindNode {
                    label,
                    properties,
                    limit,
                    offset,
                    provenance_filter,
                } => {
                    handle_find_node(db, label, properties, limit, offset, provenance_filter).await
                }
                QueryRequest::FindNeighbors {
                    node_id,
                    limit,
                    offset,
                    provenance_filter,
                } => handle_find_neighbors(db, node_id, limit, offset, provenance_filter).await,
                QueryRequest::ExecuteQuery { query, parameters } => {
                    handle_execute_query(db, query, parameters).await
                }
                QueryRequest::BulkExecuteQuery { queries } => {
                    handle_bulk_execute_query(db, queries).await
                }
            }
        };

        let (data, truncated) =
            enforce_query_limits(in_flight, in_flight_cap, &effective, is_write, op).await?;
        Ok::<(Value, bool, EffectiveQueryLimits), AletheiaHttpError>((data, truncated, effective))
    };

    #[cfg(feature = "observability")]
    let result = {
        use tracing::Instrument;
        work.instrument(span.clone()).await
    };
    #[cfg(not(feature = "observability"))]
    let result = work.await;

    // Build the final response. On success the result count is stamped on the
    // span (Issue #3376) and the wire envelope is serialized exactly ONCE
    // (Issue #3368 perf-High) so the result-byte cap is measured against the
    // real wire bytes. Any error — including a limit rejection surfaced here —
    // is funneled through the single trace-aware error path below so it carries
    // the `error.code` span attribute and the `trace_id`/`x-trace-id`.
    let outcome: Result<Response, AletheiaHttpError> = match result {
        Ok((data, truncated, effective)) => {
            #[cfg(feature = "observability")]
            if recording {
                trace.record_result_count(result_count(&data));
            }
            let body = ApiResponse::success_with_truncation(data, truncated);
            match serde_json::to_vec(&body) {
                Ok(bytes) => {
                    // Result-byte cap — READ-class only, and only when a finite
                    // cap is set. A committed write's acknowledgement has already
                    // taken effect; a post-commit 413 would lie about durability,
                    // so writes are exempt. Measured against the real wire bytes
                    // (never back through `Json(..)`, which would serialize a
                    // second time — these bytes are byte-identical to it).
                    if !is_write
                        && effective.max_response_bytes > 0
                        && bytes.len() > effective.max_response_bytes
                    {
                        Err(AletheiaHttpError::ResourceLimitExceeded(
                            ResourceLimitExceeded {
                                dimension: LimitDimension::ResultBytes,
                                limit: effective.max_response_bytes as u64,
                                consumed: Some(bytes.len() as u64),
                                retriable: false,
                            },
                        ))
                    } else {
                        Ok((
                            StatusCode::OK,
                            [(
                                header::CONTENT_TYPE,
                                HeaderValue::from_static("application/json"),
                            )],
                            bytes,
                        )
                            .into_response())
                    }
                }
                Err(e) => Err(AletheiaHttpError::Internal(e.to_string())),
            }
        }
        Err(e) => Err(e),
    };

    match outcome {
        Ok(response) => response,
        Err(e) => {
            #[cfg(feature = "observability")]
            if recording {
                trace.record_error(e.code_str());
            }
            #[cfg(feature = "observability")]
            let trace_id = span.in_scope(crate::http::trace::active_trace_id);
            #[cfg(not(feature = "observability"))]
            let trace_id = crate::http::trace::active_trace_id();
            e.into_response_with_trace(trace_id)
        }
    }
}

/// The stable operation-name attribute for a `/query` request (bounded set,
/// never user data).
#[cfg(feature = "observability")]
fn operation_name(req: &QueryRequest) -> &'static str {
    match req {
        QueryRequest::FindNode { .. } => "find_node",
        QueryRequest::GetNode { .. } => "get_node",
        QueryRequest::CreateNode { .. } => "create_node",
        QueryRequest::BulkCreateNodes { .. } => "bulk_create_nodes",
        QueryRequest::BulkGetNodes { .. } => "bulk_get_nodes",
        QueryRequest::BulkUpdateNodes { .. } => "bulk_update_nodes",
        QueryRequest::BulkDeleteNodes { .. } => "bulk_delete_nodes",
        QueryRequest::FindNeighbors { .. } => "find_neighbors",
        QueryRequest::ExecuteQuery { .. } => "execute_query",
        QueryRequest::BulkExecuteQuery { .. } => "bulk_execute_query",
    }
}

/// Result-count attribute for a response payload: array length, `nodes` array
/// length for the bulk-get shape, otherwise `1` for a single entity.
#[cfg(feature = "observability")]
fn result_count(data: &Value) -> usize {
    match data {
        Value::Array(items) => items.len(),
        Value::Object(map) => map
            .get("nodes")
            .and_then(Value::as_array)
            .map_or(1, Vec::len),
        _ => 1,
    }
}

/// Whether a request carries a temporal (`AS OF`) scope. Only statement-based
/// operations can; detection is a cheap case-insensitive substring check that
/// never inspects property values.
#[cfg(feature = "observability")]
fn request_is_temporal(req: &QueryRequest) -> bool {
    /// Allocation-free, case-insensitive scan for the `as of` temporal marker.
    /// The previous `to_ascii_lowercase().contains(..)` allocated a full
    /// lowercased copy of the statement on every request; scanning byte windows
    /// with `eq_ignore_ascii_case` matches identically for the ASCII needle
    /// while allocating nothing (Issue #3376, MED2 / Perf).
    fn is_as_of(query: &str) -> bool {
        const NEEDLE: &[u8] = b"as of";
        query
            .as_bytes()
            .windows(NEEDLE.len())
            .any(|w| w.eq_ignore_ascii_case(NEEDLE))
    }
    match req {
        QueryRequest::ExecuteQuery { query, .. } => is_as_of(query),
        QueryRequest::BulkExecuteQuery { queries } => queries.iter().any(|q| is_as_of(&q.query)),
        _ => false,
    }
}

/// Attach sanitized statement text to the span when statement capture is
/// explicitly enabled. Only single-statement `execute_query` is captured; the
/// statement is the query template (parameters are `$`-bound, so no property
/// values are interpolated). Off by default (Issue #3376 privacy model).
#[cfg(feature = "otel")]
fn maybe_capture_statement(trace: &HttpTrace, req: &QueryRequest) {
    if !crate::observability::otel::is_active() || !crate::observability::otel::capture_statements()
    {
        return;
    }
    if let QueryRequest::ExecuteQuery { query, .. } = req {
        trace.record_statement(query);
    }
}

/// Collect the crate's HTTP routes as a `Vec<Route>`.
///
/// Kept in this module so the `routes![...]` macro resolves the companion
/// functions generated by `#[get]` / `#[post]` in the same scope where the
/// handlers are declared.
#[must_use]
pub fn all_routes() -> Vec<Route> {
    let mut r = routes![health_check, handle_query];
    r.extend(crate::http::admin::admin_routes());
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Configurable interner cap (finding 1): a REAL HTTP `create_node` handler
    /// call whose property KEY tips the interner over must render as the
    /// structured `412 FAILED_PRECONDITION` envelope with `{resource,current,
    /// limit}` details naming `persistence.max_interned_strings` — NOT the prior
    /// `400 BadRequest`. Lowers the process-global interner cap, so it runs in a
    /// SUBPROCESS to isolate the mutation from concurrent tests.
    #[test]
    fn interner_cap_property_key_breach_http_is_412_via_subprocess() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "http::handlers::tests::interner_cap_property_key_breach_http_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for HTTP interner-cap test");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success(), "HTTP interner-cap helper failed");
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("HTTP interner-cap helper did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[tokio::test]
    #[ignore]
    async fn interner_cap_property_key_breach_http_helper() {
        use crate::core::GLOBAL_INTERNER;
        use std::collections::HashMap;

        let db = Arc::new(crate::AletheiaDB::new().expect("db init"));

        // Pin the cap at the live interner size so the NEXT new intern — the fresh
        // property KEY, interned by `json_to_property_map` inside the handler —
        // tips it over.
        let base = GLOBAL_INTERNER.len();
        GLOBAL_INTERNER.set_max_capacity(base);

        let mut props = HashMap::new();
        props.insert(
            "uniq_http_property_key_tips_over".to_string(),
            serde_json::json!("some_value"),
        );

        let err = handle_create_node(db, "TipLabel".to_string(), Some(props), None)
            .await
            .expect_err("a property-KEY interner breach must fail the create");

        // AFTER the fix: the structured 412 FAILED_PRECONDITION envelope.
        // (BEFORE the fix this was `AletheiaHttpError::BadRequest` → 400.)
        match &err {
            AletheiaHttpError::Structured {
                code,
                status,
                retriable,
                message,
                details,
            } => {
                assert_eq!(*code, "FAILED_PRECONDITION");
                assert_eq!(*status, axum::http::StatusCode::PRECONDITION_FAILED);
                assert!(!retriable);
                assert!(
                    message.contains("persistence.max_interned_strings"),
                    "message must name the knob, got: {message}"
                );
                let d = details.as_ref().expect("details must be present");
                assert_eq!(d["resource"], "string interner");
                assert!(d["limit"].is_number());
                assert!(d["current"].is_number());
            }
            other => panic!("expected Structured FAILED_PRECONDITION, got {other:?}"),
        }
    }

    #[test]
    fn json_to_predicate_value_covers_supported_kinds() {
        assert_eq!(
            json_to_predicate_value(&Value::Null),
            Some(PredicateValue::Null)
        );
        assert_eq!(
            json_to_predicate_value(&Value::Bool(true)),
            Some(PredicateValue::Bool(true))
        );
        assert_eq!(
            json_to_predicate_value(&Value::Bool(false)),
            Some(PredicateValue::Bool(false))
        );
        assert_eq!(
            json_to_predicate_value(&Value::Number(42.into())),
            Some(PredicateValue::Int(42))
        );
        assert_eq!(
            json_to_predicate_value(&Value::Number(serde_json::Number::from_f64(42.5).unwrap())),
            Some(PredicateValue::Float(42.5))
        );
        assert_eq!(
            json_to_predicate_value(&Value::String("hello".to_string())),
            Some(PredicateValue::String("hello".to_string()))
        );
    }

    #[test]
    fn json_to_predicate_value_rejects_composite_kinds() {
        assert_eq!(json_to_predicate_value(&Value::Array(vec![])), None);
        assert_eq!(
            json_to_predicate_value(&Value::Object(serde_json::Map::new())),
            None
        );
    }

    /// Conformance matrix: every `/query` operation maps to the expected
    /// access class (Issue #3350). The match in `query_access_class` has no
    /// wildcard arm, so new operations cannot silently skip classification.
    #[test]
    fn query_access_class_conformance_matrix() {
        use AccessClass::{Read, Write};

        let read_ops = [
            json!({"operation": "find_node", "label": "Person"}),
            json!({"operation": "get_node", "node_id": 1}),
            json!({"operation": "bulk_get_nodes", "node_ids": [1, 2]}),
            json!({"operation": "find_neighbors", "node_id": 1}),
            json!({"operation": "execute_query", "query": "MATCH (n:Person) RETURN n"}),
            json!({"operation": "bulk_execute_query", "queries": [
                {"query": "MATCH (n) RETURN n"},
                {"query": "MATCH (m:X) RETURN m"},
            ]}),
        ];
        for op in &read_ops {
            let req: QueryRequest = serde_json::from_value(op.clone()).expect("deserialize");
            assert_eq!(query_access_class(&req), Read, "expected Read for {op}");
        }

        let write_ops = [
            json!({"operation": "create_node", "label": "Person"}),
            json!({"operation": "bulk_create_nodes", "nodes": [{"label": "P"}]}),
            json!({"operation": "bulk_update_nodes", "updates": [{"node_id": 1}]}),
            json!({"operation": "bulk_delete_nodes", "node_ids": [1]}),
            json!({"operation": "execute_query", "query": "CREATE (n:Person {name: 'X'})"}),
            json!({"operation": "bulk_execute_query", "queries": [
                {"query": "MATCH (n) RETURN n"},
                {"query": "MATCH (n) DETACH DELETE n"},
            ]}),
        ];
        for op in &write_ops {
            let req: QueryRequest = serde_json::from_value(op.clone()).expect("deserialize");
            assert_eq!(query_access_class(&req), Write, "expected Write for {op}");
        }
    }

    /// Runtime route-inventory conformance (Issue #3350 Phase 2): every
    /// route the HTTP server actually registers must be a route the auth
    /// matrix classifies. `query_access_class` covers `/query`'s operation
    /// enum exhaustively at compile time; this test closes the remaining
    /// gap — a brand-new *route* (a new handler in `all_routes`) cannot ship
    /// without an explicit classification decision here and in
    /// `docs/guides/access-control-matrix.md`.
    #[test]
    fn all_registered_routes_are_classified() {
        // (method, path), mirroring docs/guides/access-control-matrix.md:
        //   GET /status              -> metrics
        //   POST /query              -> per-operation (query_access_class)
        //   POST/GET /admin/keys and POST /admin/keys/revoke -> admin
        //   POST /admin/promote      -> admin (replica promotion, #3355)
        let classified = [
            ("GET", "/status"),
            ("POST", "/query"),
            ("POST", "/admin/keys"),
            ("GET", "/admin/keys"),
            ("POST", "/admin/keys/revoke"),
            ("POST", "/admin/promote"),
        ];
        let routes = all_routes();
        assert_eq!(
            routes.len(),
            classified.len(),
            "route inventory changed — update the classification set (and the \
             documented matrix in docs/guides/access-control-matrix.md)"
        );
        for route in &routes {
            assert!(
                classified.contains(&(route.method.as_str(), route.path)),
                "unclassified HTTP route: {} {} — classify it in \
                 docs/guides/access-control-matrix.md and add it here",
                route.method,
                route.path
            );
        }
    }

    // ------------------------------------------------------------------
    // Per-query resource limits (Issue #3368)
    // ------------------------------------------------------------------

    /// The `#[serde(flatten)]` envelope keeps `operation` at the top level and
    /// carries the optional `limits` override — and a body with no `limits`
    /// key still parses (backward compatible).
    #[test]
    fn query_envelope_flattens_operation_and_optional_limits() {
        // With a limits override.
        let env: QueryEnvelope = serde_json::from_value(json!({
            "operation": "get_node",
            "node_id": 7,
            "limits": { "timeout_ms": 500, "max_result_rows": 3 }
        }))
        .expect("deserialize envelope with limits");
        assert!(matches!(
            env.request,
            QueryRequest::GetNode { node_id: 7, .. }
        ));
        let ov = env.limits.expect("limits present");
        assert_eq!(ov.timeout_ms, Some(500));
        assert_eq!(ov.max_result_rows, Some(3));

        // Without a limits key — unchanged legacy body.
        let env: QueryEnvelope = serde_json::from_value(json!({
            "operation": "get_node",
            "node_id": 9
        }))
        .expect("deserialize envelope without limits");
        assert!(matches!(
            env.request,
            QueryRequest::GetNode { node_id: 9, .. }
        ));
        assert!(env.limits.is_none());
    }

    fn effective(
        timeout_ms: u64,
        max_result_rows: usize,
        max_response_bytes: usize,
        row_overflow: RowOverflowPolicy,
    ) -> EffectiveQueryLimits {
        EffectiveQueryLimits {
            timeout_ms,
            max_result_rows,
            max_response_bytes,
            row_overflow,
        }
    }

    /// An unbounded in-flight counter (cap 0) for the limit tests that do not
    /// exercise the Issue #3368 DoS guard — behavior is identical to the
    /// pre-guard path.
    fn no_cap() -> InFlightCounter {
        InFlightCounter::new()
    }

    /// A future that outlives the timeout produces a 429 timeout error, with
    /// the underlying computation left running (documented caveat). Read-class
    /// op → `retriable: true`. Uses paused time for determinism.
    #[tokio::test(start_paused = true)]
    async fn enforce_wall_clock_timeout_trips_429() {
        let limits = effective(10, 0, 0, RowOverflowPolicy::Truncate);
        let slow = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(json!([]))
        };
        let err = enforce_query_limits(no_cap(), 0, &limits, false, slow)
            .await
            .unwrap_err();
        match err {
            AletheiaHttpError::ResourceLimitExceeded(e) => {
                assert_eq!(e.dimension, LimitDimension::WallClockTimeout);
                assert_eq!(e.limit, 10);
                assert!(e.retriable, "a read-class timeout is retriable");
            }
            other => panic!("expected timeout, got {other:?}"),
        }
    }

    /// A write-class op that outlives the timeout is `retriable: false` — the
    /// write may already have committed on the blocking pool (review MUST-FIX 1).
    #[tokio::test(start_paused = true)]
    async fn enforce_write_timeout_is_not_retriable() {
        let limits = effective(10, 0, 0, RowOverflowPolicy::Truncate);
        let slow = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(json!({ "id": 1 }))
        };
        let err = enforce_query_limits(no_cap(), 0, &limits, true, slow)
            .await
            .unwrap_err();
        match err {
            AletheiaHttpError::ResourceLimitExceeded(e) => {
                assert_eq!(e.dimension, LimitDimension::WallClockTimeout);
                assert!(
                    !e.retriable,
                    "a write-class timeout must not invite a duplicate retry"
                );
            }
            other => panic!("expected timeout, got {other:?}"),
        }
    }

    /// A fast future under the timeout passes through untouched.
    #[tokio::test]
    async fn enforce_fast_op_under_timeout_succeeds() {
        let limits = effective(1_000, 0, 0, RowOverflowPolicy::Truncate);
        let fast = async { Ok(json!({ "ok": true })) };
        let (data, truncated) = enforce_query_limits(no_cap(), 0, &limits, false, fast)
            .await
            .unwrap();
        assert_eq!(data, json!({ "ok": true }));
        assert!(!truncated);
    }

    /// Zero timeout means unlimited — a slow op is not interrupted.
    #[tokio::test(start_paused = true)]
    async fn enforce_zero_timeout_is_unlimited() {
        let limits = effective(0, 0, 0, RowOverflowPolicy::Truncate);
        let op = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(json!([1, 2, 3]))
        };
        let (data, truncated) = enforce_query_limits(no_cap(), 0, &limits, false, op)
            .await
            .unwrap();
        assert_eq!(data, json!([1, 2, 3]));
        assert!(!truncated);
    }

    // ------------------------------------------------------------------
    // In-flight worker DoS guard (Issue #3368 hardening of #3446)
    // ------------------------------------------------------------------

    /// (a) At the cap, an extra TIMED query is REJECTED with `503 UNAVAILABLE`
    /// and is NOT spawned — the discarded-but-running worker keeps its slot, so
    /// admission control fires before any new worker starts.
    #[tokio::test]
    async fn enforce_in_flight_cap_rejects_extra_timed_query_without_spawning() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let counter = no_cap();
        let cap = 1;
        // Occupy the only slot, standing in for a still-running detached worker.
        let held = counter.try_acquire(cap).expect("first slot");

        let started = Arc::new(AtomicBool::new(false));
        let started_in_op = Arc::clone(&started);
        let op = async move {
            started_in_op.store(true, Ordering::SeqCst);
            Ok(json!({ "ran": true }))
        };
        // A finite timeout → the guarded, spawning path.
        let limits = effective(1_000, 0, 0, RowOverflowPolicy::Truncate);
        let err = enforce_query_limits(counter.clone(), cap, &limits, false, op)
            .await
            .unwrap_err();
        match err {
            AletheiaHttpError::InFlightCapacityExceeded { cap: c } => assert_eq!(c, 1),
            other => panic!("expected 503 in-flight capacity error, got {other:?}"),
        }
        // Give any (erroneously) spawned worker a chance to run.
        tokio::task::yield_now().await;
        assert!(
            !started.load(Ordering::SeqCst),
            "a rejected query must NOT be spawned"
        );
        drop(held);
    }

    /// (b) Slots are released after workers finish, so later timed queries
    /// succeed. A completed worker drops its guard before its result is
    /// observed, returning the pool to zero.
    #[tokio::test]
    async fn enforce_in_flight_slot_releases_after_worker_finishes() {
        let counter = no_cap();
        let cap = 1;
        let limits = effective(1_000, 0, 0, RowOverflowPolicy::Truncate);

        let (d1, _) = enforce_query_limits(counter.clone(), cap, &limits, false, async {
            Ok(json!({ "n": 1 }))
        })
        .await
        .expect("first timed query succeeds");
        assert_eq!(d1, json!({ "n": 1 }));
        assert_eq!(
            counter.live(),
            0,
            "the finished worker released its slot before completion was observed"
        );

        // A second timed query at the same cap now succeeds (slot free again).
        let (d2, _) = enforce_query_limits(counter.clone(), cap, &limits, false, async {
            Ok(json!({ "n": 2 }))
        })
        .await
        .expect("second timed query succeeds after release");
        assert_eq!(d2, json!({ "n": 2 }));
    }

    /// (c) The inline/unlimited path (`timeout_ms == 0`) never spawns and is
    /// therefore unaffected by the cap: it runs even when the pool is full.
    #[tokio::test]
    async fn enforce_inline_zero_timeout_ignores_in_flight_cap() {
        let counter = no_cap();
        let cap = 1;
        // Saturate the pool.
        let held = counter.try_acquire(cap).expect("occupy the only slot");

        let limits = effective(0, 0, 0, RowOverflowPolicy::Truncate);
        let (data, truncated) = enforce_query_limits(counter.clone(), cap, &limits, false, async {
            Ok(json!("inline"))
        })
        .await
        .expect("inline path runs despite a full pool");
        assert_eq!(data, json!("inline"));
        assert!(!truncated);
        // The inline path never touched the counter.
        assert_eq!(counter.live(), 1, "inline path must not reserve a slot");
        drop(held);
    }

    #[tokio::test]
    async fn enforce_row_cap_truncate_flags_truncated() {
        let limits = effective(0, 2, 0, RowOverflowPolicy::Truncate);
        let op = async { Ok(json!([1, 2, 3, 4, 5])) };
        let (data, truncated) = enforce_query_limits(no_cap(), 0, &limits, false, op)
            .await
            .unwrap();
        assert_eq!(data, json!([1, 2]));
        assert!(truncated);
    }

    #[tokio::test]
    async fn enforce_row_cap_reject_errors() {
        let limits = effective(0, 2, 0, RowOverflowPolicy::Reject);
        let op = async { Ok(json!([1, 2, 3])) };
        let err = enforce_query_limits(no_cap(), 0, &limits, false, op)
            .await
            .unwrap_err();
        match err {
            AletheiaHttpError::ResourceLimitExceeded(e) => {
                assert_eq!(e.dimension, LimitDimension::ResultRows);
                assert_eq!(e.limit, 2);
                assert_eq!(e.consumed, Some(3));
            }
            other => panic!("expected row-reject, got {other:?}"),
        }
    }

    /// A WRITE op is exempt from the row cap: an over-cap array acknowledgement
    /// is neither truncated nor rejected (review MUST-FIX 1).
    #[tokio::test]
    async fn enforce_write_op_is_exempt_from_row_cap() {
        // Reject policy with a cap of 1; a 3-element write ack must still pass.
        let limits = effective(0, 1, 0, RowOverflowPolicy::Reject);
        let op = async { Ok(json!([1, 2, 3])) };
        let (data, truncated) = enforce_query_limits(no_cap(), 0, &limits, true, op)
            .await
            .unwrap();
        assert_eq!(data, json!([1, 2, 3]), "write ack must not be truncated");
        assert!(!truncated);
    }

    /// Row cap only applies to arrays; a single-object result is untouched.
    #[tokio::test]
    async fn enforce_row_cap_ignores_non_arrays() {
        let limits = effective(0, 1, 0, RowOverflowPolicy::Reject);
        let op = async { Ok(json!({ "id": 1, "big": [1, 2, 3, 4] })) };
        let (data, truncated) = enforce_query_limits(no_cap(), 0, &limits, false, op)
            .await
            .unwrap();
        assert_eq!(data, json!({ "id": 1, "big": [1, 2, 3, 4] }));
        assert!(!truncated);
    }

    /// The byte cap now lives in `handle_query`, applied to the fully-serialized
    /// response envelope. This unit test drives that same envelope serialization
    /// and measurement step directly (the end-to-end path is covered in the
    /// `tests/http_query_limits.rs` integration suite).
    #[test]
    fn envelope_byte_cap_measures_the_wire_body() {
        let max_response_bytes = 8usize;
        let data = json!(["a very long string that exceeds eight bytes"]);
        let body = ApiResponse::success_with_truncation(data, false);
        let bytes = serde_json::to_vec(&body).unwrap();
        // The serialized envelope wraps `data` in `{"success":true,"data":...}`,
        // so it is strictly larger than the raw data and far exceeds 8 bytes.
        assert!(bytes.len() > max_response_bytes);

        let over_cap = bytes.len() > max_response_bytes;
        assert!(over_cap, "envelope must trip the byte cap");

        // A generous cap does not trip.
        assert!(bytes.len() < 4096);
    }

    #[test]
    fn classify_query_error_routes_parse_errors_to_bad_request() {
        assert!(matches!(
            classify_query_error("Syntax error at line 1".into()),
            AletheiaHttpError::QueryParse(_)
        ));
        assert!(matches!(
            classify_query_error("Parse error".into()),
            AletheiaHttpError::QueryParse(_)
        ));
        assert!(matches!(
            classify_query_error("Storage failure".into()),
            AletheiaHttpError::Internal(_)
        ));
    }
}
