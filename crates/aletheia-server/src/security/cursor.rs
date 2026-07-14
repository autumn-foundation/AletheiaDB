//! Cursor-signing core (Lane B4 / Issue #3360).
//!
//! Fills `aletheia-server::security::cursor`. A self-contained mirror of the
//! legacy #3360 contract (`src/mcp/cursor.rs`): opaque, printable, bounded
//! **base64url** continuation tokens, **HMAC-SHA256**-signed with a per-process
//! secret, binding the originating tool identity + the pinned snapshot
//! bi-temporal coordinate + the keyset/offset position + issued-at/expiry. A
//! mangled, wrong-secret, or wrong-tool token is rejected with a structured
//! error and never served wrong data.
//!
//! # Cross-connection resume within TTL is by design (#3360)
//!
//! The signing secret is **per process**, not per connection: any connection
//! that presents a valid, unexpired, correct-tool token resumes the scan,
//! regardless of which connection minted it. This is the documented #3360
//! property (tokens are stateless bearer continuations bounded only by TTL and
//! the per-connection live cap), **not** an authorization bypass — every
//! resumed read is still RBAC-gated by the surface's `Authorized<C>` extractor,
//! and a token grants no data its bearer could not already read. Binding a
//! cursor to a single connection is a deliberate non-goal in v1.
//!
//! # Error mapping (Issue #3234 codes, at the future MCP/HTTP boundary)
//!
//! [`verify_cursor`](CursorSecret::verify_cursor) and
//! [`LiveCursorRegistry::register`] return a [`CursorError`] the surface layer
//! maps: [`CursorError::Invalid`] → `INVALID_ARGUMENT` (a caller/tamper fault:
//! re-issue the original query); [`CursorError::Expired`] and
//! [`CursorError::CapExceeded`] → `FAILED_PRECONDITION` with remediation
//! guidance. [`CursorError::class`] names the boundary class so the mapping is
//! not a substring match. See Issue #3561 §8 (cursor TTL/cap budgets).
//!
//! # Lifecycle & statelessness
//!
//! The token is **stateless**: everything needed to resume — tool, snapshot,
//! keyset position, page size, issued-at, expiry, filters — is signed *inside*
//! the token, so a resume needs no server-side session and no other request
//! parameters. The expiry is baked in at mint time (`exp = iat + ttl`), so
//! [`verify_cursor`](CursorSecret::verify_cursor) needs only `now` — TTL is
//! self-describing. The [`LiveCursorRegistry`] is the *only* in-process state: a
//! tiny per-connection live-lease count enforcing the concurrency cap, released
//! by RAII ([`CursorLease`]'s `Drop`) even on panic. Expired cursors pin no
//! storage — TTL lives in the token, not the registry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Opaque token prefix and version tag. Bumping the version invalidates old
/// tokens (they parse-fail cleanly with a structured error). Matches legacy.
pub const TOKEN_PREFIX: &str = "aletheiadb.cursor.v1.";

/// Hard ceiling on an accepted token's length. A cursor payload is tiny; a
/// token far larger than this is malformed (or hostile) and is rejected before
/// any allocation-heavy decode. Matches legacy.
pub const MAX_TOKEN_LEN: usize = 8 * 1024;

/// The boundary error class a [`CursorError`] maps to (Issue #3234). The future
/// MCP/HTTP surface reads this instead of matching on the message text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorErrorClass {
    /// Caller/tamper fault — maps to `INVALID_ARGUMENT` (non-retriable).
    InvalidArgument,
    /// Lifecycle fault (expiry / cap) — maps to `FAILED_PRECONDITION`
    /// (non-retriable) with remediation guidance.
    FailedPrecondition,
}

/// A cursor verification / registration failure, structured so the surface
/// layer maps it to a #3234 code without substring matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CursorError {
    /// Malformed / wrong-prefix / bad base64 / bad signature / wrong version /
    /// wrong tool. The message names the cause; the class is
    /// [`CursorErrorClass::InvalidArgument`].
    Invalid(String),
    /// The token's embedded TTL has elapsed. Class
    /// [`CursorErrorClass::FailedPrecondition`]; remediation: re-issue the query.
    Expired,
    /// The per-connection live-cursor cap is reached. Class
    /// [`CursorErrorClass::FailedPrecondition`]; remediation: finish or abandon
    /// an open scan, or re-issue without a cursor.
    CapExceeded {
        /// The configured cap that was hit.
        max_live_cursors: usize,
    },
}

impl CursorError {
    /// The boundary class this error maps to at the MCP/HTTP surface.
    #[must_use]
    pub fn class(&self) -> CursorErrorClass {
        match self {
            CursorError::Invalid(_) => CursorErrorClass::InvalidArgument,
            CursorError::Expired | CursorError::CapExceeded { .. } => {
                CursorErrorClass::FailedPrecondition
            }
        }
    }
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CursorError::Invalid(msg) => write!(f, "{msg}"),
            CursorError::Expired => write!(
                f,
                "cursor has expired; re-issue the original query to start a new scan"
            ),
            CursorError::CapExceeded { max_live_cursors } => write!(
                f,
                "too many live cursors (cap {max_live_cursors}); finish or abandon an \
                 open scan, or re-issue the query without a cursor"
            ),
        }
    }
}

impl std::error::Error for CursorError {}

/// The self-describing, signed contents of a cursor token.
///
/// Field names are kept terse because the struct is serialized into every token
/// the client round-trips. Mirrors legacy `super::CursorPayload`, plus an
/// explicit `exp` so verification is TTL-self-describing (needs only `now`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPayload {
    /// Payload schema version (matches [`TOKEN_PREFIX`]'s tag).
    pub v: u8,
    /// The originating tool name (e.g. `"list_nodes"`). A token minted by one
    /// tool is rejected if replayed against another.
    pub tool: String,
    /// Pinned snapshot valid-time (microseconds since epoch).
    pub svt: i64,
    /// Pinned snapshot transaction-time (microseconds since epoch).
    pub stt: i64,
    /// Exclusive keyset lower bound: the last id returned on the prior page.
    /// `None` means "from the beginning" — kept distinct from `Some(0)` because
    /// ids are 0-based.
    pub after: Option<u64>,
    /// Offset fallback for tools whose result order is not a simple id keyset
    /// (e.g. `traverse`'s DFS order). Snapshot-anchored, so still consistent.
    pub off: u64,
    /// Page size carried across the whole scan so every page is the same size.
    pub limit: usize,
    /// Issued-at (microseconds since epoch).
    pub iat: i64,
    /// Expiry (microseconds since epoch) = `iat + ttl`; authoritative for TTL.
    pub exp: i64,
    /// Random per-cursor id; the registry key for cap + reclamation.
    pub cid: String,
    /// The discriminating request parameters (label, property filter, edge
    /// label, direction, ...) so continuation needs no other arguments.
    pub filters: serde_json::Value,
}

impl CursorPayload {
    /// Build the seed of a first-page cursor. `tool`/`iat`/`exp`/`cid` are
    /// stamped by [`CursorSecret::mint_cursor`]; the keyset position starts at
    /// the beginning (`after = None`, `off = 0`).
    #[must_use]
    pub fn seed(snapshot: (i64, i64), limit: usize, filters: serde_json::Value) -> Self {
        Self {
            v: 1,
            tool: String::new(),
            svt: snapshot.0,
            stt: snapshot.1,
            after: None,
            off: 0,
            limit,
            iat: 0,
            exp: 0,
            cid: String::new(),
            filters,
        }
    }
}

/// Per-process HMAC secret for cursor tokens. Randomized at construction (or
/// supplied explicitly for deterministic tests), never exposed and never
/// logged: [`Debug`] is redacted, the bytes are held in a [`Zeroizing`] buffer
/// (wiped on drop), and there is no accessor for them. Makes tokens unforgeable
/// and — deliberately — non-durable across a restart (a new process gets a new
/// secret, so old tokens fail verification and the caller re-issues).
#[derive(Clone)]
pub struct CursorSecret {
    secret: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for CursorSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never reveal key material.
        f.write_str("CursorSecret(<redacted>)")
    }
}

impl CursorSecret {
    /// A fresh secret from the OS CSPRNG. Use in production (one per process).
    /// This is the **only** production constructor.
    #[must_use]
    pub fn random() -> Self {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        Self {
            secret: Zeroizing::new(secret),
        }
    }

    /// Build a secret from explicit bytes. Intended **only** for deterministic
    /// tests and for callers that derive the secret from an external key
    /// source; never hard-code a secret in production — use [`random`](Self::random)
    /// there. `#[doc(hidden)]` so it does not advertise itself as a general
    /// constructor, while staying `pub` for the crate's integration tests.
    #[doc(hidden)]
    #[must_use]
    pub fn from_bytes(secret: [u8; 32]) -> Self {
        Self {
            secret: Zeroizing::new(secret),
        }
    }

    /// Mint a signed, opaque token from a seed `payload`, binding `tool_name`,
    /// stamping `iat = now` and `exp = now + ttl`, and generating a random
    /// cursor id when the seed does not carry one (a continuation reuses its
    /// existing `cid`). The returned string is base64url-printable and bounded.
    #[must_use]
    pub fn mint_cursor(
        &self,
        mut payload: CursorPayload,
        tool_name: &str,
        now: i64,
        ttl: Duration,
    ) -> String {
        payload.v = 1;
        payload.tool = tool_name.to_string();
        payload.iat = now;
        let ttl_micros = i64::try_from(ttl.as_micros()).unwrap_or(i64::MAX);
        payload.exp = now.saturating_add(ttl_micros);
        if payload.cid.is_empty() {
            payload.cid = Self::random_id();
        }
        self.encode(&payload)
    }

    /// Decode, verify the signature (constant-time), and lifecycle-check a token
    /// presented for `expected_tool` at `now`.
    ///
    /// # Errors
    ///
    /// - [`CursorError::Invalid`] for a malformed / wrong-prefix / bad-base64 /
    ///   bad-signature (tampered or wrong-secret) / wrong-version / wrong-tool
    ///   token — a caller/tamper fault (`INVALID_ARGUMENT`);
    /// - [`CursorError::Expired`] when `now >= exp` (`FAILED_PRECONDITION`).
    ///
    /// A verification failure never returns a (wrong) payload.
    pub fn verify_cursor(
        &self,
        token: &str,
        expected_tool: &str,
        now: i64,
    ) -> Result<CursorPayload, CursorError> {
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
        let presented_sig = B64
            .decode(sig_b64)
            .map_err(|_| Self::invalid("cursor token signature is not valid base64url"))?;

        // Verify the signature over the exact encoded payload bytes before
        // trusting anything inside it (reject tampering up front). Constant-time
        // compare via subtle::ConstantTimeEq so a match/mismatch position is not
        // timing-observable; unequal lengths yield a `0` Choice.
        let expected_tag = self.sign(payload_b64.as_bytes());
        if presented_sig.ct_eq(&expected_tag[..]).unwrap_u8() != 1 {
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
        // TTL is authoritative from the embedded expiry, so an expired token is
        // rejected regardless of any registry state (stateless-safe).
        if now >= payload.exp {
            return Err(CursorError::Expired);
        }

        Ok(payload)
    }

    /// Encode + sign a fully-stamped payload into an opaque token string.
    fn encode(&self, payload: &CursorPayload) -> String {
        let json = serde_json::to_vec(payload).expect("cursor payload serialization is infallible");
        let payload_b64 = B64.encode(json);
        let tag = self.sign(payload_b64.as_bytes());
        let sig_b64 = B64.encode(tag);
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

    fn invalid(msg: &str) -> CursorError {
        CursorError::Invalid(msg.to_string())
    }
}

/// HMAC-SHA256 (RFC 2104) using `sha2`. The key is 32 bytes, shorter than
/// SHA-256's 64-byte block, so it is zero-padded rather than hashed. Mirrors
/// legacy `src/mcp/cursor.rs::hmac_sha256` (no extra `hmac` crate).
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

/// Shared per-connection live-lease counts. `conn_id -> live lease count`.
type LiveCounts = Arc<Mutex<HashMap<u64, usize>>>;

/// Per-connection cap on concurrently live cursors (#3360 default 128).
///
/// The *only* in-process cursor state: a tiny per-connection count, not any
/// token or snapshot storage. [`register`](Self::register) admits a scan when
/// the connection is under the cap and hands back a RAII [`CursorLease`]; the
/// lease's `Drop` releases the slot — guaranteed even on an early return or a
/// panic (Rust runs `Drop` as the stack unwinds). A `cap` of `0` means
/// unbounded (slots are still counted so [`live_count`](Self::live_count) is
/// observable). Expired cursors pin no storage: TTL is enforced by the token's
/// embedded expiry, not by this registry.
#[derive(Clone)]
pub struct LiveCursorRegistry {
    cap: usize,
    live: LiveCounts,
}

/// RAII lease for one live cursor scan on a connection. Decrements the
/// connection's live count on drop; hold it for exactly as long as the scan is
/// resumable.
#[derive(Debug)]
pub struct CursorLease {
    conn_id: u64,
    live: LiveCounts,
}

impl Drop for CursorLease {
    fn drop(&mut self) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = live.get_mut(&self.conn_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                // Reclaim the map slot so an idle connection pins nothing.
                live.remove(&self.conn_id);
            }
        }
    }
}

impl LiveCursorRegistry {
    /// A fresh registry with the given per-connection cap (`0` = unbounded),
    /// starting empty.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            live: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The configured per-connection cap.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Admit a new live cursor on `conn_id`.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError::CapExceeded`] (→ `FAILED_PRECONDITION`) when the
    /// connection already holds `cap` live cursors.
    pub fn register(&self, conn_id: u64) -> Result<CursorLease, CursorError> {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let count = live.entry(conn_id).or_insert(0);
        if self.cap != 0 && *count >= self.cap {
            return Err(CursorError::CapExceeded {
                max_live_cursors: self.cap,
            });
        }
        *count += 1;
        Ok(CursorLease {
            conn_id,
            live: self.live.clone(),
        })
    }

    /// Number of currently-live leases on `conn_id` (for observability/tests).
    #[must_use]
    pub fn live_count(&self, conn_id: u64) -> usize {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.get(&conn_id).copied().unwrap_or(0)
    }
}
