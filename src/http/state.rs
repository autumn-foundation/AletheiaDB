//! Shared application state for the HTTP server.
//!
//! `AppState` wraps `Arc<AletheiaDB>` for type-safe sharing across handlers.
//! AletheiaDB is already thread-safe via interior mutability (DashMap, RwLock,
//! striped WAL locks), so no outer mutex is needed.
//!
//! # Wiring
//!
//! Install state once at startup via
//! [`autumn_web::app().on_startup(...)`](autumn_web::prelude::app) and an
//! [`AppState::insert_extension`](autumn_web::prelude::AppState::insert_extension)
//! call. Handlers then receive it through the [`AppState`] extractor defined
//! in this module, which reads the extension out of autumn's own state.

use crate::AletheiaDB;
use crate::http::config::QueryLimitsConfig;
use crate::http::error::AletheiaHttpError;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::sync::Arc;

/// Application state shared across HTTP handlers.
///
/// Holds an `Arc<AletheiaDB>` plus the per-query resource-limit configuration
/// (Issue #3368) and exposes them via [`db`](Self::db) / [`db_arc`](Self::db_arc)
/// and [`query_limits`](Self::query_limits). Cheap to clone (a single `Arc`
/// bump plus a small `Copy`-ish config).
#[derive(Clone)]
pub struct AppState {
    db: Arc<AletheiaDB>,
    query_limits: Arc<QueryLimitsConfig>,
}

impl AppState {
    /// Create new application state wrapping the given database, with the
    /// default per-query limits ([`QueryLimitsConfig::default`]).
    #[must_use]
    pub fn new(db: Arc<AletheiaDB>) -> Self {
        Self {
            db,
            query_limits: Arc::new(QueryLimitsConfig::default()),
        }
    }

    /// Install the per-query resource limits this state should enforce
    /// (Issue #3368). Builder-style; typically fed from
    /// [`ServerConfig::query_limits`](crate::http::ServerConfig::query_limits)
    /// when wiring the router.
    #[must_use]
    pub fn with_query_limits(mut self, limits: QueryLimitsConfig) -> Self {
        self.query_limits = Arc::new(limits);
        self
    }

    /// Borrow the database for direct method calls.
    #[must_use]
    pub fn db(&self) -> &AletheiaDB {
        &self.db
    }

    /// Clone the `Arc<AletheiaDB>` for passing into blocking tasks.
    #[must_use]
    pub fn db_arc(&self) -> Arc<AletheiaDB> {
        self.db.clone()
    }

    /// The per-query resource limits in force (Issue #3368).
    #[must_use]
    pub fn query_limits(&self) -> &QueryLimitsConfig {
        &self.query_limits
    }
}

impl From<Arc<AletheiaDB>> for AppState {
    fn from(db: Arc<AletheiaDB>) -> Self {
        Self::new(db)
    }
}

// `AppState` as an axum extractor: pulls the installed `Arc<AppState>`
// out of autumn's `AppState.extensions` bag. Returns `StateMissing` (HTTP 500)
// if the extension was never installed — a boot-time invariant.
impl FromRequestParts<autumn_web::prelude::AppState> for AppState {
    type Rejection = AletheiaHttpError;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &autumn_web::prelude::AppState,
    ) -> Result<Self, Self::Rejection> {
        state
            .extension::<AppState>()
            .map(|arc| (*arc).clone())
            .ok_or(AletheiaHttpError::StateMissing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_state_clone_shares_db() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state = AppState::new(db.clone());
        let state2 = state.clone();
        assert!(Arc::ptr_eq(&state.db_arc(), &state2.db_arc()));
    }

    #[test]
    fn from_arc_db_works() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state: AppState = db.clone().into();
        assert!(Arc::ptr_eq(&db, &state.db_arc()));
    }
}
