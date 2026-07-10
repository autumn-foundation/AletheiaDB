//! Stateless, snapshot-anchored keyset continuation cursors (Issue #3360).
//!
//! Offset pagination (#3226) over a live, concurrently-written database is
//! quietly broken for the caller that matters most: between page 1 and page 2
//! other agents write, offsets shift, and the reader sees duplicates or misses
//! rows -- each page computed against a *different* database state. This module
//! replaces that with an opaque **cursor** contract that is
//!
//! - **snapshot-anchored** -- every page of one scan is evaluated at the
//!   bi-temporal coordinate captured on the first page, leveraging the existing
//!   MVCC / temporal read semantics, so the union of all pages equals exactly
//!   the result of an unbounded query *at that one moment*; concurrent writes
//!   committed after the anchor are invisible to the scan (no duplicates, no
//!   gaps, no post-cursor leakage);
//! - **keyset (depth-independent)** -- continuation carries the last-returned
//!   id, so fetching page N seeks straight to it instead of recomputing and
//!   discarding the preceding N-1 pages (offset pagination's linear tax);
//! - **stateless-safe for the client** -- the token is a printable,
//!   bounded-length, self-describing string an LLM can echo back verbatim with
//!   no escaping hazards, and continuing needs no other parameters;
//! - **tamper-evident** -- the token is signed with a per-process secret, so a
//!   mangled or forged token is rejected with a structured error, never served
//!   wrong data.
//!
//! # Design choice: stateless token + minimal in-process registry
//!
//! The cursor is **stateless**: everything needed to resume -- the originating
//! tool, the pinned bi-temporal snapshot, the keyset position, the page size,
//! and the discriminating query parameters -- is encoded *in the token itself*,
//! not in server-side session state. This means a resume works even if the
//! server forgot it, and the design has no unbounded server memory growth.
//!
//! A small in-process **registry** is kept purely to enforce the
//! per-connection *cap on concurrently live cursors* (AC5) and to make resource
//! reclamation observable: it maps a cursor id to its expiry and is pruned of
//! expired entries on every issue. It is authoritative for the *cap*; the
//! token's embedded `iat` (issued-at) is authoritative for the *TTL*, so a
//! token whose registry entry was already reclaimed still expires correctly by
//! its own timestamp (stateless-safe). The registry pins no storage and only
//! ever holds tiny `(id, expiry)` pairs bounded by the cap.
//!
//! ## Snapshot semantics under concurrent writes (documented, not hand-waved)
//!
//! The first page records `(valid_time, transaction_time)` -- for the
//! current-state tools this is simply "now" at first-page time; for
//! `find_nodes_at_time` it is the caller's requested coordinate. Every
//! continuation re-evaluates the query *as of that pinned transaction time*.
//! Consequently, for the whole lifetime of a cursor scan:
//!
//! - a node/edge **created** after the anchor (transaction time later than the
//!   pin) is **not** seen by any page -- it did not exist in the snapshot;
//! - a node/edge **deleted** after the anchor is **still** seen -- the snapshot
//!   predates the deletion (bi-temporal append-only history);
//! - a node/edge **updated** after the anchor is returned **as it was** at the
//!   anchor, not its latest state.
//!
//! The scan therefore reflects exactly one consistent moment. This is a
//! deliberate contrast with Qdrant/Weaviate/Milvus scroll APIs, which drift
//! under concurrent writes because they have no temporal coordinate to anchor
//! to.
//!
//! ## Lifecycle
//!
//! Cursors have a bounded **TTL** (default 5 minutes) and cross-restart
//! durability is intentionally *not* provided: the signing secret is
//! per-process, so a token minted before a restart fails signature
//! verification afterward and the caller simply re-issues the query. Resuming
//! after expiry, exceeding the live-cursor cap, or presenting a tampered token
//! all return a structured [`McpError`] naming the cause with remediation
//! guidance.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::{McpError, McpErrorCode};
use crate::core::temporal::time;

/// Opaque token prefix and version tag. Bumping the version invalidates old
/// tokens (they parse-fail cleanly with a structured error).
const TOKEN_PREFIX: &str = "aletheiadb.cursor.v1.";

/// Hard ceiling on an accepted token's length. A cursor payload is tiny; a
/// token far larger than this is malformed (or hostile) and is rejected before
/// any allocation-heavy decode.
const MAX_TOKEN_LEN: usize = 8 * 1024;

/// Default cursor time-to-live: 5 minutes. Long enough for an agent to page
/// through a large result set, short enough that abandoned cursors reclaim
/// their registry slot promptly.
pub(crate) const DEFAULT_CURSOR_TTL: Duration = Duration::from_secs(300);

/// Default cap on concurrently live cursors per connection. Exceeding it is a
/// `FAILED_PRECONDITION` telling the caller to finish or abandon an existing
/// scan (or re-issue the query) rather than a silent success.
pub(crate) const DEFAULT_MAX_LIVE_CURSORS: usize = 128;

/// The self-describing, signed contents of a cursor token.
///
/// Field names are kept terse because the struct is serialized into every
/// token the client round-trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorPayload {
    /// Payload schema version (matches [`TOKEN_PREFIX`]'s tag).
    pub v: u8,
    /// The originating tool name (e.g. `"list_nodes"`). A token minted by one
    /// tool is rejected if replayed against another -- the snapshot and filter
    /// shape only make sense for the tool that produced them.
    pub tool: String,
    /// Pinned snapshot valid-time (microseconds since epoch).
    pub svt: i64,
    /// Pinned snapshot transaction-time (microseconds since epoch).
    pub stt: i64,
    /// Exclusive keyset lower bound: the last id returned on the prior page.
    /// The next page returns ids strictly greater than this. `None` means
    /// "from the beginning" -- kept distinct from `Some(0)` because ids are
    /// 0-based, so id 0 is a real row that a `0` sentinel would wrongly skip.
    pub after: Option<u64>,
    /// Offset fallback for tools whose result order is not a simple id keyset
    /// (e.g. `traverse`'s DFS order). Snapshot-anchored, so still consistent;
    /// see the module docs' per-tool notes.
    pub off: u64,
    /// Page size carried across the whole scan so every page is the same size.
    pub limit: usize,
    /// Issued-at (microseconds since epoch); authoritative for TTL.
    pub iat: i64,
    /// Random per-cursor id; the registry key for cap + reclamation.
    pub cid: String,
    /// The discriminating request parameters (label, property filter, edge
    /// label, direction, ...) so continuation needs no other arguments.
    pub filters: serde_json::Value,
}

impl CursorPayload {
    /// Build the seed of a first-page cursor. `iat`/`cid` are stamped by
    /// [`CursorManager::issue`]; the keyset position starts at the beginning
    /// (`after = None`, `off = 0`).
    pub(crate) fn seed(
        tool: &str,
        snapshot: (i64, i64),
        limit: usize,
        filters: serde_json::Value,
    ) -> Self {
        Self {
            v: 1,
            tool: tool.to_string(),
            svt: snapshot.0,
            stt: snapshot.1,
            after: None,
            off: 0,
            limit,
            iat: 0,
            cid: String::new(),
            filters,
        }
    }
}

/// Issues, signs, verifies and lifecycle-manages [`CursorPayload`] tokens.
///
/// One manager is shared (via `Arc`) across a server instance == one MCP
/// connection, so its live-cursor cap is a per-connection cap.
#[derive(Debug)]
pub(crate) struct CursorManager {
    /// Per-process HMAC key. Randomized at construction, never exposed; makes
    /// tokens unforgeable and (deliberately) non-durable across restarts.
    secret: [u8; 32],
    ttl: Duration,
    max_live: usize,
    /// Live cursor ids -> expiry (microseconds since epoch). Pruned on issue.
    live: Mutex<HashMap<String, i64>>,
}

impl CursorManager {
    /// Create a manager with the default TTL and cap and a fresh random secret.
    pub(crate) fn new() -> Self {
        Self::with_config(DEFAULT_CURSOR_TTL, DEFAULT_MAX_LIVE_CURSORS)
    }

    /// Create a manager with an explicit TTL and live-cursor cap (used by the
    /// server's `with_cursor_config` builder and by tests exercising expiry
    /// and the cap).
    pub(crate) fn with_config(ttl: Duration, max_live: usize) -> Self {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        Self {
            secret,
            ttl,
            max_live: max_live.max(1),
            live: Mutex::new(HashMap::new()),
        }
    }

    /// The configured TTL (documented in tool responses / guide).
    pub(crate) fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Number of currently-live (unexpired, registered) cursors. Used by
    /// resource-reclamation tests.
    #[cfg(test)]
    pub(crate) fn live_count(&self) -> usize {
        let mut live = self.live.lock().expect("cursor registry lock poisoned");
        let now = time::now().wallclock();
        live.retain(|_, &mut expiry| expiry > now);
        live.len()
    }

    /// Issue a signed token for `payload`, stamping a fresh `iat` and
    /// registering it for cap enforcement + reclamation.
    ///
    /// A **first-page** cursor (empty `payload.cid`) is minted a fresh id and
    /// counted against the per-connection cap; issuing one when the cap is
    /// already reached (after pruning expired entries) is a
    /// `FAILED_PRECONDITION`. A **continuation** cursor (non-empty `cid`,
    /// carried over from the page the caller is resuming) reuses that id and
    /// only refreshes its expiry, so the cap counts distinct *scans*, not
    /// pages -- a thousand-page scan holds exactly one slot. The refreshed
    /// expiry makes the TTL an idle timeout between successive pages.
    pub(crate) fn issue(&self, mut payload: CursorPayload) -> Result<String, McpError> {
        let now = time::now().wallclock();
        let expiry = now.saturating_add(self.ttl.as_micros() as i64);

        {
            let mut live = self.live.lock().expect("cursor registry lock poisoned");
            // Reclaim expired cursors first: expired cursors never pin a slot.
            live.retain(|_, &mut e| e > now);
            if payload.cid.is_empty() {
                // First page: enforce the cap and mint a fresh scan id.
                if live.len() >= self.max_live {
                    return Err(McpError::new(
                        McpErrorCode::FailedPrecondition,
                        format!(
                            "Too many live cursors (cap {}). Finish or abandon an open scan, or \
                             re-issue the query without use_cursor.",
                            self.max_live
                        ),
                    )
                    .details(serde_json::json!({
                        "max_live_cursors": self.max_live,
                        "live_cursors": live.len(),
                    })));
                }
                payload.cid = Self::random_id();
            }
            // Register (first page) or refresh (continuation) this scan's slot.
            live.insert(payload.cid.clone(), expiry);
        }
        payload.iat = now;

        Ok(self.encode(&payload))
    }

    /// Decode, verify and lifecycle-check a token presented for `expected_tool`.
    ///
    /// Failure modes, each a structured [`McpError`] naming the cause:
    /// - malformed / wrong-prefix / bad base64 / bad signature / wrong version
    ///   / wrong tool -> `INVALID_ARGUMENT` (a caller/tampering fault: re-issue
    ///   the original query);
    /// - expired past TTL -> `FAILED_PRECONDITION` (re-issue the query).
    ///
    /// A verification failure never returns a (wrong) payload.
    pub(crate) fn decode(
        &self,
        token: &str,
        expected_tool: &str,
    ) -> Result<CursorPayload, McpError> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(Self::invalid("cursor token is too long to be valid"));
        }
        let body = token
            .strip_prefix(TOKEN_PREFIX)
            .ok_or_else(|| Self::invalid("cursor token has an unrecognized format"))?;
        let (payload_b64, sig_b64) = body
            .split_once('.')
            .ok_or_else(|| Self::invalid("cursor token is malformed (missing signature)"))?;

        let payload_bytes = B64
            .decode(payload_b64)
            .map_err(|_| Self::invalid("cursor token payload is not valid base64url"))?;
        let sig = B64
            .decode(sig_b64)
            .map_err(|_| Self::invalid("cursor token signature is not valid base64url"))?;

        // Verify the signature over the exact encoded payload bytes before
        // trusting anything inside it (reject tampering up front).
        let expected_sig = self.sign(payload_b64.as_bytes());
        if !ct_eq(&sig, &expected_sig) {
            return Err(Self::invalid(
                "cursor token signature is invalid (tampered or issued by another server)",
            ));
        }

        let payload: CursorPayload = serde_json::from_slice(&payload_bytes)
            .map_err(|_| Self::invalid("cursor token payload is not decodable"))?;

        if payload.v != 1 {
            return Err(Self::invalid("cursor token version is unsupported"));
        }
        if payload.tool != expected_tool {
            return Err(Self::invalid(&format!(
                "cursor was issued for tool '{}', not '{}'; re-issue the query with this tool",
                payload.tool, expected_tool
            )));
        }

        // TTL is authoritative from the embedded issued-at, so an expired
        // token is rejected even if its registry slot was already reclaimed.
        let now = time::now().wallclock();
        let expiry = payload.iat.saturating_add(self.ttl.as_micros() as i64);
        if now >= expiry {
            return Err(McpError::new(
                McpErrorCode::FailedPrecondition,
                "cursor has expired; re-issue the original query to start a new scan",
            )
            .details(serde_json::json!({ "ttl_seconds": self.ttl.as_secs() })));
        }

        Ok(payload)
    }

    /// Encode + sign a fully-stamped payload into an opaque token string.
    fn encode(&self, payload: &CursorPayload) -> String {
        let json = serde_json::to_vec(payload).expect("cursor payload serialization is infallible");
        let payload_b64 = B64.encode(json);
        let sig = self.sign(payload_b64.as_bytes());
        let sig_b64 = B64.encode(sig);
        format!("{TOKEN_PREFIX}{payload_b64}.{sig_b64}")
    }

    /// HMAC-SHA256 of `msg` under the per-process secret.
    fn sign(&self, msg: &[u8]) -> [u8; 32] {
        hmac_sha256(&self.secret, msg)
    }

    fn random_id() -> String {
        let mut bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut bytes);
        B64.encode(bytes)
    }

    fn invalid(msg: &str) -> McpError {
        McpError::new(McpErrorCode::InvalidArgument, msg)
    }
}

/// Constant-time byte-slice equality (avoids leaking signature-match position
/// via timing). Dependency-free so it never gates on an optional crate.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// HMAC-SHA256 (RFC 2104) using the crate's already-present `sha2`. The key is
/// 32 bytes, shorter than SHA-256's 64-byte block, so it is zero-padded rather
/// than hashed.
fn hmac_sha256(key: &[u8; 32], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..key.len() {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(tool: &str) -> CursorPayload {
        CursorPayload::seed(
            tool,
            (1_000, 2_000),
            10,
            serde_json::json!({"label": "Person"}),
        )
    }

    #[test]
    fn round_trips_a_valid_token() {
        let mgr = CursorManager::new();
        let token = mgr.issue(seed("list_nodes")).expect("issue");
        let decoded = mgr.decode(&token, "list_nodes").expect("decode");
        assert_eq!(decoded.tool, "list_nodes");
        assert_eq!(decoded.svt, 1_000);
        assert_eq!(decoded.stt, 2_000);
        assert_eq!(decoded.limit, 10);
        assert_eq!(decoded.filters["label"], "Person");
        assert!(!decoded.cid.is_empty(), "issue stamps a cursor id");
        assert!(decoded.iat > 0, "issue stamps an issued-at");
    }

    #[test]
    fn token_is_printable_bounded_and_escape_free() {
        let mgr = CursorManager::new();
        let token = mgr.issue(seed("list_nodes")).expect("issue");
        assert!(token.starts_with(TOKEN_PREFIX));
        assert!(token.len() < MAX_TOKEN_LEN);
        // base64url + '.' + prefix chars only: safe to embed verbatim in JSON,
        // URLs, and shell without escaping.
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "token must be printable and escape-free, got {token}"
        );
    }

    #[test]
    fn tampered_payload_is_rejected_as_invalid_argument() {
        let mgr = CursorManager::new();
        let token = mgr.issue(seed("list_nodes")).expect("issue");
        // Flip a character in the payload segment.
        let body = token.strip_prefix(TOKEN_PREFIX).unwrap();
        let (payload_b64, sig_b64) = body.split_once('.').unwrap();
        let mut chars: Vec<char> = payload_b64.chars().collect();
        // Mutate a middle char to a different valid base64url char.
        let idx = chars.len() / 2;
        chars[idx] = if chars[idx] == 'A' { 'B' } else { 'A' };
        let mutated: String = chars.into_iter().collect();
        let tampered = format!("{TOKEN_PREFIX}{mutated}.{sig_b64}");

        let err = mgr
            .decode(&tampered, "list_nodes")
            .expect_err("must reject");
        assert_eq!(err.code(), McpErrorCode::InvalidArgument);
        assert!(!err.is_retriable());
    }

    #[test]
    fn garbage_and_wrong_prefix_tokens_are_invalid_argument() {
        let mgr = CursorManager::new();
        for bad in [
            "",
            "not-a-cursor",
            "aletheiadb.cursor.v1.@@@.@@@",
            TOKEN_PREFIX,
        ] {
            let err = mgr.decode(bad, "list_nodes").expect_err("must reject");
            assert_eq!(
                err.code(),
                McpErrorCode::InvalidArgument,
                "token {bad:?} should be INVALID_ARGUMENT"
            );
        }
    }

    #[test]
    fn cursor_from_another_server_fails_signature() {
        let a = CursorManager::new();
        let b = CursorManager::new();
        let token = a.issue(seed("list_nodes")).expect("issue");
        // A different process/secret cannot verify a's token.
        let err = b.decode(&token, "list_nodes").expect_err("must reject");
        assert_eq!(err.code(), McpErrorCode::InvalidArgument);
    }

    #[test]
    fn cursor_replayed_against_wrong_tool_is_rejected() {
        let mgr = CursorManager::new();
        let token = mgr.issue(seed("list_nodes")).expect("issue");
        let err = mgr
            .decode(&token, "find_nodes_at_time")
            .expect_err("must reject cross-tool replay");
        assert_eq!(err.code(), McpErrorCode::InvalidArgument);
    }

    #[test]
    fn expired_cursor_is_failed_precondition() {
        // Zero TTL -> any token is already expired on decode.
        let mgr = CursorManager::with_config(Duration::from_secs(0), 128);
        let token = mgr.issue(seed("list_nodes")).expect("issue");
        let err = mgr.decode(&token, "list_nodes").expect_err("must reject");
        assert_eq!(err.code(), McpErrorCode::FailedPrecondition);
        assert!(!err.is_retriable());
    }

    #[test]
    fn exceeding_live_cursor_cap_is_failed_precondition() {
        let mgr = CursorManager::with_config(DEFAULT_CURSOR_TTL, 2);
        mgr.issue(seed("list_nodes")).expect("first");
        mgr.issue(seed("list_nodes")).expect("second");
        let err = mgr.issue(seed("list_nodes")).expect_err("cap exceeded");
        assert_eq!(err.code(), McpErrorCode::FailedPrecondition);
        assert_eq!(err.to_json()["details"]["max_live_cursors"], 2);
    }

    #[test]
    fn continuation_pages_reuse_the_scan_slot_and_do_not_consume_the_cap() {
        // Cap of 1: a single scan must be able to page many times without
        // tripping the cap, because continuation reuses the scan's cursor id.
        let mgr = CursorManager::with_config(DEFAULT_CURSOR_TTL, 1);
        let first = mgr.issue(seed("list_nodes")).expect("first page");
        let mut decoded = mgr.decode(&first, "list_nodes").expect("decode first");
        for page in 0..50u64 {
            decoded.after = Some(page + 1);
            let token = mgr
                .issue(decoded.clone())
                .expect("continuation must not trip the cap");
            decoded = mgr
                .decode(&token, "list_nodes")
                .expect("decode continuation");
        }
        assert_eq!(mgr.live_count(), 1, "one scan holds exactly one slot");
    }

    #[test]
    fn expired_cursors_are_reclaimed_and_do_not_pin_the_cap() {
        // TTL 0: every issued cursor is immediately expired, so the registry
        // prunes it on the next issue and the cap is never actually reached --
        // proving expired cursors reclaim their slot (no indefinite pinning).
        let mgr = CursorManager::with_config(Duration::from_secs(0), 2);
        for _ in 0..10 {
            mgr.issue(seed("list_nodes"))
                .expect("expired cursors must not pin the cap");
        }
        assert_eq!(mgr.live_count(), 0, "expired cursors reclaimed");
    }
}
