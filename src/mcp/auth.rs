//! MCP-surface authentication and role-based authorization (Issue #3350,
//! Phase 2).
//!
//! # Session-scoped credential model
//!
//! The MCP server runs over **stdio**: the credential is supplied once at
//! process start (a *session-scoped* credential), not per request as on the
//! HTTP surface. Enforcement happens at the single dispatch point
//! ([`AletheiaMcpServer::dispatch_tool`]) **before** any tool executes:
//!
//! 1. **Authentication** — in [`AuthMode::Required`] the session credential
//!    is re-verified against the shared [`AuthStore`] on *every* tool call
//!    (a SHA-256 digest plus a constant-time record scan — comfortably
//!    within the sub-millisecond budget), so revoking the key in the store
//!    takes effect on the very next call. A missing, unknown, or revoked
//!    credential yields a uniform `UNAUTHENTICATED` error for every tool —
//!    including unknown tool names, so unauthenticated callers cannot probe
//!    tool *existence* via error-shape differences (the inventory itself is
//!    available through the ungated `tools/list`; the uniformity here is
//!    about not leaking anything through per-name error variance) — and the
//!    error never reveals whether a presented key exists nor echoes
//!    credential material.
//! 2. **Authorization** — the tool name is classified into an
//!    [`AccessClass`] via [`TOOL_ACCESS_CLASSES`] and checked against the
//!    principal's [`Role::allows`] matrix; a denial yields
//!    `PERMISSION_DENIED` with `details: {required_class, principal_role}`.
//!
//! [`AuthMode::Anonymous`] preserves the pre-#3350 behavior (full access)
//! and is the implicit mode of [`AletheiaMcpServer::new`] for embedded/
//! programmatic use — a Rust caller holding the `Arc<AletheiaDB>` can
//! already do anything the tools can. The `aletheia-mcp` **binary** is
//! where auth-on-by-default applies (see its env-var docs).
//!
//! # Key lifecycle
//!
//! Key management (create/list/revoke) is served by the HTTP admin surface
//! (Phase 1: `POST/GET /admin/keys`, `POST /admin/keys/revoke`). Point both
//! surfaces at the same persisted store path (`{data_dir}/auth/keys.json`)
//! and keys minted over HTTP are usable here. No MCP-side *key-lifecycle*
//! tools exist yet. The `Admin`-class MCP tools that do exist are the GDPR
//! crypto-shred surface (`designate_subject` / `erase_subject`, Issue #3359).
//!
//! The documented classification lives in
//! `docs/guides/access-control-matrix.md`; a conformance test mechanically
//! compares it against [`TOOL_ACCESS_CLASSES`] — keep them in sync.
//!
//! [`AletheiaMcpServer::dispatch_tool`]: super::AletheiaMcpServer
//! [`AletheiaMcpServer::new`]: super::AletheiaMcpServer::new

use std::sync::Arc;

use serde_json::json;

use crate::auth::{AccessClass, AuthMode, AuthStore, Principal, Role, SecretString};

use super::error::{McpError, McpErrorCode};

/// The access class required by every MCP tool, as a total map over the
/// advertised inventory (the dispatch table is string-keyed, so totality is
/// enforced by conformance tests instead of an exhaustive `match`):
///
/// - every advertised tool must appear here (no unclassified tools);
/// - every entry here must be an advertised tool (no stale entries);
/// - the table must match `docs/guides/access-control-matrix.md`.
///
/// `query` is classified `Read` because the tool is read-only *by contract*:
/// mutating clauses are rejected by `detect_mutating_clause` before
/// execution and never write.
pub(crate) const TOOL_ACCESS_CLASSES: &[(&str, AccessClass)] = &[
    // ---- Read: lookups, traversals, history, schema/extent discovery, and
    // the read-only declarative `query` tool.
    ("get_node", AccessClass::Read),
    ("list_nodes", AccessClass::Read),
    ("count_nodes", AccessClass::Read),
    ("get_edge", AccessClass::Read),
    ("list_edges", AccessClass::Read),
    ("count_edges", AccessClass::Read),
    ("get_outgoing_edges", AccessClass::Read),
    ("get_incoming_edges", AccessClass::Read),
    ("traverse", AccessClass::Read),
    ("find_similar", AccessClass::Read),
    // Embedding generation & text semantic search (Issue #2906) — read-only.
    ("embed_query", AccessClass::Read),
    ("embed_text", AccessClass::Read),
    ("semantic_search", AccessClass::Read),
    // Semantic-search analysis tools (Issue #2907) — read-only.
    ("semantic_path", AccessClass::Read),
    ("concept_analogy", AccessClass::Read),
    ("concept_mean", AccessClass::Read),
    ("find_duplicate_candidates", AccessClass::Read),
    ("semantic_horizon", AccessClass::Read),
    ("context_aspects", AccessClass::Read),
    ("list_vector_indexes", AccessClass::Read),
    ("list_unique_constraints", AccessClass::Read),
    ("get_node_at_time", AccessClass::Read),
    ("get_edge_at_time", AccessClass::Read),
    ("find_nodes_at_time", AccessClass::Read),
    ("list_changes", AccessClass::Read),
    // Push-changefeed long-poll — read-only (Issue #3375).
    ("await_changes", AccessClass::Read),
    ("get_node_at_valid_time", AccessClass::Read),
    ("get_node_at_transaction_time", AccessClass::Read),
    ("get_node_history", AccessClass::Read),
    ("diff_node_versions", AccessClass::Read),
    ("get_edge_at_valid_time", AccessClass::Read),
    ("get_edge_at_transaction_time", AccessClass::Read),
    ("get_edge_history", AccessClass::Read),
    ("diff_edge_versions", AccessClass::Read),
    // Belief-revision audit (Issue #3362) — read-only.
    ("get_belief_revisions", AccessClass::Read),
    // Temporal drift-alarm reads (Issue #3367) — read-only.
    ("list_drift_monitors", AccessClass::Read),
    ("query_drift_alarms", AccessClass::Read),
    // Contradiction genealogy (Issue #3352) — read-only.
    ("contradiction_genealogy", AccessClass::Read),
    ("find_contradictions", AccessClass::Read),
    // Counterfactual replay (Issue #3357) — read-only (view, real DB unmutated).
    ("counterfactual_replay", AccessClass::Read),
    // Trust propagation reads (Issue #3382) — read-only.
    ("trust_breakdown", AccessClass::Read),
    ("list_trust_policies", AccessClass::Read),
    ("hybrid_query", AccessClass::Read),
    ("query", AccessClass::Read),
    ("get_schema", AccessClass::Read),
    ("temporal_extent", AccessClass::Read),
    // Derivation-lineage closure queries — read-only (Issue #3371).
    ("lineage_upstream", AccessClass::Read),
    ("lineage_downstream", AccessClass::Read),
    // Signed audit export reads history and signs it — no mutation (Issue #3358).
    ("audit_export", AccessClass::Read),
    // Provenance hash chain verification / anchor export — read-only (Issue #3351).
    ("verify_chain", AccessClass::Read),
    ("export_chain_head", AccessClass::Read),
    // Namespace discovery — read-only (Issue #3349, PR3b).
    ("list_namespaces", AccessClass::Read),
    ("describe_namespace", AccessClass::Read),
    // ---- Metrics: operational health/stats.
    ("database_stats", AccessClass::Metrics),
    // ---- Write: graph mutations plus index/constraint state changes.
    ("create_node", AccessClass::Write),
    ("update_node", AccessClass::Write),
    ("delete_node", AccessClass::Write),
    ("delete_node_cascade", AccessClass::Write),
    ("retract_node", AccessClass::Write),
    ("create_edge", AccessClass::Write),
    ("update_edge", AccessClass::Write),
    ("delete_edge", AccessClass::Write),
    ("retract_edge", AccessClass::Write),
    ("apply_batch", AccessClass::Write),
    ("enable_vector_index", AccessClass::Write),
    ("enable_unique_constraint", AccessClass::Write),
    // Embedding-backed writes (Issue #2906).
    ("create_node_with_embedding", AccessClass::Write),
    ("update_node_embedding", AccessClass::Write),
    // Namespace creation — a write (Issue #3349, PR3b).
    ("create_namespace", AccessClass::Write),
    // Temporal drift-alarm writes (Issue #3367).
    ("create_drift_monitor", AccessClass::Write),
    ("delete_drift_monitor", AccessClass::Write),
    ("resolve_drift_alarm", AccessClass::Write),
    // ---- Admin: GDPR crypto-shred designation & irreversible erasure
    // (Issue #3359, Slice 4b) — the first Admin-class MCP tools. Erasure
    // destroys per-subject key material, an irreversible privileged op.
    // Key lifecycle (create/list/revoke) remains served by the HTTP admin
    // surface (Phase 1) over the shared persisted store.
    ("designate_subject", AccessClass::Admin),
    ("erase_subject", AccessClass::Admin),
];

/// Look up the [`AccessClass`] a tool requires. `None` for unknown tool
/// names (the caller lets those fall through to the normal unknown-tool
/// error — *after* authentication has passed).
pub(crate) fn tool_access_class(name: &str) -> Option<AccessClass> {
    TOOL_ACCESS_CLASSES
        .iter()
        .find(|(tool, _)| *tool == name)
        .map(|(_, class)| *class)
}

/// Authentication configuration for
/// [`AletheiaMcpServer::with_auth`](super::AletheiaMcpServer::with_auth).
///
/// Bundles the [`AuthMode`], the shared [`AuthStore`] (share the same
/// `Arc`/persisted path with the HTTP surface to use keys minted there), and
/// the optional session credential presented by this stdio session.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use aletheiadb::AletheiaDB;
/// use aletheiadb::auth::{AuthMode, AuthStore, Role, SecretString};
/// use aletheiadb::mcp::{AletheiaMcpServer, McpAuthConfig};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let store = Arc::new(AuthStore::new());
/// let (_principal, key) = store.create_key("agent", Role::Reader)?;
///
/// let server = AletheiaMcpServer::with_auth(
///     Arc::new(AletheiaDB::new()?),
///     McpAuthConfig::new(AuthMode::Required, store)
///         .with_credential(SecretString::new(key.as_str())),
/// );
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct McpAuthConfig {
    mode: AuthMode,
    store: Arc<AuthStore>,
    credential: Option<SecretString>,
}

impl McpAuthConfig {
    /// Create an auth configuration with no session credential yet.
    ///
    /// In [`AuthMode::Required`] a config without a credential produces a
    /// server whose every tool call returns `UNAUTHENTICATED` — the server
    /// still runs, mirroring the HTTP surface's uniform-401 contract.
    pub fn new(mode: AuthMode, store: Arc<AuthStore>) -> Self {
        Self {
            mode,
            store,
            credential: None,
        }
    }

    /// Attach the session credential (e.g. from `ALETHEIADB_MCP_API_KEY`).
    ///
    /// The credential is re-verified against the store on every tool call,
    /// so a revocation in the (possibly shared) store takes effect on the
    /// next call.
    #[must_use]
    pub fn with_credential(mut self, credential: SecretString) -> Self {
        self.credential = Some(credential);
        self
    }

    /// The configured [`AuthMode`].
    pub fn mode(&self) -> AuthMode {
        self.mode
    }
}

/// The resolved per-session authentication state held by the server.
#[derive(Clone)]
pub(crate) enum SessionAuth {
    /// No authentication: every tool call is fully privileged.
    Anonymous,
    /// Every tool call must authenticate via the session credential.
    Required {
        store: Arc<AuthStore>,
        credential: Option<SecretString>,
    },
}

impl From<McpAuthConfig> for SessionAuth {
    fn from(config: McpAuthConfig) -> Self {
        match config.mode {
            AuthMode::Anonymous => SessionAuth::Anonymous,
            AuthMode::Required => SessionAuth::Required {
                store: config.store,
                credential: config.credential,
            },
        }
    }
}

impl SessionAuth {
    /// Authorize one tool call: authenticate the session credential, then
    /// check the tool's [`AccessClass`] against the principal's role.
    ///
    /// Ordering is deliberate: authentication happens before (and
    /// independently of) tool-name resolution, so unauthenticated callers
    /// cannot probe tool existence via error-shape differences (`tools/list`
    /// itself is ungated — the property enforced here is uniform errors,
    /// not inventory secrecy).
    /// For authenticated callers an unknown name returns `Ok(())` here and
    /// falls through to the dispatcher's normal unknown-tool error.
    pub(crate) fn authorize_tool(&self, tool_name: &str) -> Result<(), McpError> {
        let Some(principal) = self.authenticate()? else {
            // Anonymous mode: fully privileged.
            return Ok(());
        };
        match tool_access_class(tool_name) {
            None => Ok(()),
            Some(class) if principal.role.allows(class) => Ok(()),
            Some(class) => Err(permission_denied_error(principal.role, class)),
        }
    }

    /// Authenticate the session credential.
    ///
    /// `Ok(None)` = anonymous mode (no principal, full access);
    /// `Ok(Some(p))` = verified principal; `Err` = `UNAUTHENTICATED`.
    fn authenticate(&self) -> Result<Option<Principal>, McpError> {
        match self {
            SessionAuth::Anonymous => Ok(None),
            SessionAuth::Required { store, credential } => credential
                .as_ref()
                .and_then(|c| store.verify(c.expose()))
                .map(Some)
                .ok_or_else(unauthenticated_error),
        }
    }

    /// The currently verified session principal, if any (re-verifies the
    /// credential, so a revoked key yields `None`).
    pub(crate) fn principal(&self) -> Option<Principal> {
        self.authenticate().ok().flatten()
    }
}

/// The uniform `UNAUTHENTICATED` error.
///
/// Uniform by construction (a constant message, no details), so it can
/// never reveal whether a presented key exists nor echo the credential.
pub(crate) fn unauthenticated_error() -> McpError {
    McpError::new(McpErrorCode::Unauthenticated, "authentication required")
}

/// A `PERMISSION_DENIED` error with `{required_class, principal_role}`
/// details, mirroring the HTTP 403 body's message text.
pub(crate) fn permission_denied_error(role: Role, class: AccessClass) -> McpError {
    McpError::new(
        McpErrorCode::PermissionDenied,
        format!("role '{role}' does not permit {class} access"),
    )
    .details(json!({
        "required_class": class.to_string(),
        "principal_role": role.to_string(),
    }))
}

/// Startup validation for the MCP server's auth configuration.
///
/// [`AuthMode::Required`] (the conservative default) with zero credentials
/// available means every tool call would be rejected — refuse to start
/// instead so the operator notices immediately. Mirrors the HTTP surface's
/// `validate_auth_startup`.
///
/// # Errors
///
/// Returns a human-readable operator-guidance message when `mode` is
/// `Required` and `store` holds no keys.
pub fn validate_mcp_auth_startup(mode: AuthMode, store: &AuthStore) -> Result<(), String> {
    match mode {
        AuthMode::Anonymous => Ok(()),
        AuthMode::Required => {
            if store.is_empty() {
                Err(
                    "authentication is required (the default) but no credentials are \
                     available: set a bootstrap admin key (ALETHEIADB_BOOTSTRAP_ADMIN_KEY), \
                     point the server at an existing persisted key store \
                     ({data_dir}/auth/keys.json under ALETHEIADB_DATA_DIR — keys minted \
                     via the HTTP admin endpoints work here), or explicitly opt into \
                     anonymous mode (ALETHEIADB_AUTH_MODE=anonymous)"
                        .to_string(),
                )
            } else {
                Ok(())
            }
        }
    }
}
