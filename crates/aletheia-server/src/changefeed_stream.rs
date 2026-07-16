//! Changefeed surface on the autumn-web server (Issue #3375).
//!
//! Two handlers wrap Lane 2's merged push-changefeed primitive
//! ([`AletheiaDB::subscribe_changes`](aletheiadb::AletheiaDB::subscribe_changes)):
//!
//! 1. [`changes_stream`] — `GET /changes/stream`, a **route-only** Server-Sent
//!    Events stream of committed changes. It carries `#[api_doc(description)]`
//!    but deliberately **no** `mcp` attribute, so it is projected to HTTP +
//!    OpenAPI but is **not** an MCP tool (mirroring the `/metrics` exposition).
//!    Each committed change becomes one `data:` SSE frame; a lagged subscription
//!    emits a terminal `event: lagged` frame carrying the resume token, then the
//!    stream ends.
//!
//! 2. [`await_changes`] — `POST /changes/await`, the MCP-over-HTTP projection of
//!    the `await_changes` long-poll tool (`#[api_doc(mcp)]`), delegating to
//!    [`AletheiaMcpServer::await_changes`](aletheiadb::mcp::AletheiaMcpServer::await_changes).
//!
//! Both are [`ReadClass`]. The blocking `recv_timeout` long-poll is driven on a
//! `spawn_blocking` worker so it never starves the async runtime.

use std::convert::Infallible;
use std::time::Duration;

use aletheiadb::http::AletheiaHttpError;
use aletheiadb::mcp::AwaitChangesRequest;
use aletheiadb::{ChangeFilter, ChangeRecord, ChangeType, Error, RecvError, StorageError, time};
use autumn_web::prelude::{Json, Query, get, post};
use autumn_web::sse::{Event, Sse, keep_alive};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;

use crate::security::{Authorized, ReadClass};
use crate::state::ServerState;

/// How long each `recv_timeout` poll blocks before looping to re-check liveness
/// and let the SSE keep-alive fire. Bounded so a disconnected idle client is
/// reaped within one interval rather than pinning a subscription indefinitely.
const STREAM_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Parse an MCP tool method's JSON string result into a [`Value`] for the HTTP
/// response (a non-JSON result degrades to a JSON string).
fn tool_json(s: String) -> Json<Value> {
    Json(serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))
}

/// Serialize a [`ChangeRecord`] into the changefeed row JSON shared with the
/// `list_changes` / `await_changes` MCP surfaces (Issue #3375), so every
/// changefeed surface emits an identical change shape.
fn change_record_json(r: &ChangeRecord) -> Value {
    json!({
        "entity_id": r.entity_id,
        "version_id": r.version_id,
        "kind": r.kind.as_str(),
        "change_type": r.change_type.as_str(),
        "label": r.label,
        "transaction_time": time::to_iso8601(r.transaction_time()),
        "transaction_time_range": {
            "start": time::to_iso8601(r.transaction_time_range.start()),
            "end": time::to_iso8601(r.transaction_time_range.end()),
        },
        "valid_time_range": {
            "start": time::to_iso8601(r.valid_time_range.start()),
            "end": time::to_iso8601(r.valid_time_range.end()),
        },
    })
}

/// Split a comma-separated query value into trimmed, non-empty tokens.
fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Query params for [`changes_stream`]. Each filter dimension is a comma-separated
/// query value (e.g. `?node_labels=Person,Company&change_types=created,modified`).
#[derive(Debug, Default, Deserialize)]
pub struct ChangesStreamQuery {
    /// Node labels to match (comma-separated, exact). If set, only matching node
    /// changes are streamed; combine with `edge_types` to receive both kinds.
    pub node_labels: Option<String>,
    /// Edge types to match (comma-separated, exact).
    pub edge_types: Option<String>,
    /// Change types to match (comma-separated): `created` / `modified` / `deleted`.
    pub change_types: Option<String>,
}

/// Map a single change-type token to its [`ChangeType`], rejecting unknowns with
/// a 400 `INVALID_ARGUMENT`.
fn parse_change_type(token: &str) -> Result<ChangeType, AletheiaHttpError> {
    match token {
        "created" => Ok(ChangeType::Created),
        "modified" => Ok(ChangeType::Modified),
        "deleted" => Ok(ChangeType::Deleted),
        other => Err(AletheiaHttpError::BadRequest(format!(
            "Invalid change_type '{other}': expected one of created, modified, deleted"
        ))),
    }
}

/// Build a [`ChangeFilter`] from the stream query params.
fn build_filter(q: &ChangesStreamQuery) -> Result<ChangeFilter, AletheiaHttpError> {
    let mut filter = ChangeFilter::all();
    if let Some(raw) = &q.node_labels {
        filter = filter.with_node_labels(split_csv(raw));
    }
    if let Some(raw) = &q.edge_types {
        filter = filter.with_edge_types(split_csv(raw));
    }
    if let Some(raw) = &q.change_types {
        let parsed = split_csv(raw)
            .iter()
            .map(|t| parse_change_type(t))
            .collect::<Result<Vec<_>, _>>()?;
        filter = filter.with_change_types(parsed);
    }
    Ok(filter)
}

/// Map a `subscribe_changes` failure to an HTTP error. A subscription-cap breach
/// (`CapacityExceeded`) is transient overload → 503 `UNAVAILABLE`, `retriable:
/// true` (borrowing the in-flight-capacity envelope); anything else is 500.
fn map_subscribe_err(e: Error) -> AletheiaHttpError {
    if let Error::Storage(StorageError::CapacityExceeded { limit, .. }) = &e {
        return AletheiaHttpError::InFlightCapacityExceeded { cap: *limit };
    }
    AletheiaHttpError::Internal(e.to_string())
}

/// `GET /changes/stream` — Server-Sent Events stream of committed changes
/// (Issue #3375), filtered by optional node-label / edge-type / change-type query
/// params. [`ReadClass`].
///
/// This is an **HTTP + OpenAPI** surface only — deliberately NOT an MCP tool (no
/// `api_doc(mcp)`), matching the `/metrics` exposition. The MCP projection of the
/// same changefeed surface is the [`await_changes`] long-poll tool.
///
/// The blocking `recv_timeout` drain runs on a `spawn_blocking` worker feeding a
/// bounded channel, so it never starves the async runtime; the worker exits (and
/// the subscription deregisters on drop) as soon as the client disconnects. A
/// lagged subscription emits a terminal `event: lagged` frame with the resume
/// token, after which the caller resumes losslessly via `list_changes`.
///
/// # Errors
///
/// Returns 400 `INVALID_ARGUMENT` for an unknown `change_types` token and 503
/// `UNAVAILABLE` when the changefeed is at its concurrent-subscription cap.
#[get("/changes/stream")]
#[api_doc(description = "SSE stream of committed changes")]
pub async fn changes_stream(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Query(q): Query<ChangesStreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AletheiaHttpError> {
    let filter = build_filter(&q)?;
    let db = state.db_arc();
    let sub = db.subscribe_changes(filter).map_err(map_subscribe_err)?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(16);
    tokio::task::spawn_blocking(move || {
        loop {
            match sub.recv_timeout(STREAM_POLL_INTERVAL) {
                // Idle timeout: keep looping (the SSE keep-alive covers the wire),
                // but stop if the client has gone so we don't pin the subscription.
                Ok(recs) if recs.is_empty() => {
                    if tx.is_closed() {
                        return;
                    }
                }
                Ok(recs) => {
                    for record in &recs {
                        let event = match Event::default().json_data(change_record_json(record)) {
                            Ok(ev) => ev,
                            // A record that will not serialize is skipped rather
                            // than tearing down the whole stream.
                            Err(_) => continue,
                        };
                        if tx.blocking_send(Ok(event)).is_err() {
                            return;
                        }
                    }
                }
                Err(RecvError::Lagged { resume_token }) => {
                    let payload = match &resume_token {
                        Some(token) => json!({ "resume_token": token }),
                        None => json!({}),
                    };
                    if let Ok(event) = Event::default().event("lagged").json_data(payload) {
                        let _ = tx.blocking_send(Ok(event));
                    }
                    return;
                }
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(keep_alive()))
}

/// `POST /changes/await` — the MCP-over-HTTP projection of the `await_changes`
/// long-poll tool (Issue #3375). [`ReadClass`]. HTTP + MCP tool.
///
/// Delegates to [`AletheiaMcpServer::await_changes`]; the blocking long-poll is
/// driven on a `spawn_blocking` worker so it does not starve the async runtime.
pub async fn await_changes_impl(state: ServerState, req: AwaitChangesRequest) -> Json<Value> {
    let server = state.mcp_server();
    let out = tokio::task::spawn_blocking(move || server.await_changes(req))
        .await
        .unwrap_or_else(|_| {
            // A panicked worker is an internal fault; surface the #3234 envelope.
            json!({
                "error": {
                    "code": "INTERNAL",
                    "message": "await_changes worker terminated unexpectedly",
                    "retriable": false
                }
            })
            .to_string()
        });
    tool_json(out)
}

/// `POST /changes/await` — long-poll for the next committed changes. [`ReadClass`].
/// HTTP + MCP tool (the streaming counterpart is the route-only `/changes/stream`).
#[post("/changes/await")]
#[api_doc(
    description = "Long-poll for the next committed changes matching an optional filter (blocking up to timeout_ms)",
    mcp
)]
pub async fn await_changes(
    _auth: Authorized<ReadClass>,
    state: ServerState,
    Json(req): Json<AwaitChangesRequest>,
) -> Json<Value> {
    await_changes_impl(state, req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_csv_trims_and_drops_empties() {
        assert_eq!(split_csv("Person, Company ,"), vec!["Person", "Company"]);
        assert!(split_csv("").is_empty());
        assert!(split_csv(" , ").is_empty());
    }

    #[test]
    fn parse_change_type_accepts_known_and_rejects_unknown() {
        assert_eq!(parse_change_type("created").unwrap(), ChangeType::Created);
        assert_eq!(parse_change_type("modified").unwrap(), ChangeType::Modified);
        assert_eq!(parse_change_type("deleted").unwrap(), ChangeType::Deleted);
        assert!(parse_change_type("upserted").is_err());
    }

    #[test]
    fn build_filter_rejects_bad_change_type() {
        let q = ChangesStreamQuery {
            node_labels: None,
            edge_types: None,
            change_types: Some("created,bogus".to_string()),
        };
        assert!(build_filter(&q).is_err());
    }
}
