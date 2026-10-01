//! Shared database state, delivered as an autumn-web 0.5 extension.
//!
//! Mirrors `src/http/state.rs` / the spike's `SpikeState`, bound to autumn
//! **0.5**'s `AppState`. The `Arc<AletheiaDB>` is installed once at startup and
//! pulled back per-request through the [`ServerState`] extractor.

use aletheiadb::AletheiaDB;
use aletheiadb::http::AletheiaHttpError;
use aletheiadb::mcp::AletheiaMcpServer;
use autumn_web::prelude::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::sync::Arc;

/// Per-request handle to the shared database.
///
/// Also carries a **single, process-lifetime** [`AletheiaMcpServer`] shared
/// across every request. This matters for the Issue #3360 cursor tokens, which
/// are signed with (and validated against) a per-instance secret and tracked in
/// a per-instance live-cursor registry: a fresh `AletheiaMcpServer::new()` per
/// request would mint a new secret each time, so a cursor issued on one request
/// could never be resumed on the next. Holding one shared instance keeps the
/// secret + registry stable so continuation works. The shared server is
/// anonymous (its built-in tool authorization is a no-op); the autumn-web
/// `Authorized<C>` extractor remains the real access-control gate.
#[derive(Clone)]
pub struct ServerState {
    db: Arc<AletheiaDB>,
    mcp: Arc<AletheiaMcpServer>,
}

impl ServerState {
    /// Wrap a shared database for installation into the app's extension bag,
    /// constructing the shared [`AletheiaMcpServer`] once.
    #[must_use]
    pub fn new(db: Arc<AletheiaDB>) -> Self {
        let mcp = Arc::new(AletheiaMcpServer::new(db.clone()));
        Self { db, mcp }
    }

    /// Clone the `Arc<AletheiaDB>` for moving into a blocking task.
    #[must_use]
    pub fn db_arc(&self) -> Arc<AletheiaDB> {
        self.db.clone()
    }

    /// The process-lifetime shared [`AletheiaMcpServer`]. Handlers that forward
    /// raw arguments through `dispatch_tool_json` (for the Issue #3353 token
    /// budget and Issue #3360 cursor paging) must use this shared instance so
    /// the cursor signing secret and live-cursor registry persist across
    /// requests.
    #[must_use]
    pub fn mcp_server(&self) -> Arc<AletheiaMcpServer> {
        self.mcp.clone()
    }
}

impl FromRequestParts<AppState> for ServerState {
    type Rejection = AletheiaHttpError;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // `StateMissing` (HTTP 500) is exactly what the existing server returns
        // for an uninstalled extension — a boot-time invariant.
        state
            .extension::<ServerState>()
            .map(|arc| (*arc).clone())
            .ok_or(AletheiaHttpError::StateMissing)
    }
}
