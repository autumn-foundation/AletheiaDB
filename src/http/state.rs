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
use crate::http::error::AletheiaHttpError;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::sync::Arc;

/// Application state shared across HTTP handlers.
///
/// Holds an `Arc<AletheiaDB>` and exposes it via [`db`](Self::db) and
/// [`db_arc`](Self::db_arc). Cheap to clone (a single `Arc` bump).
#[derive(Clone)]
pub struct AppState {
    db: Arc<AletheiaDB>,
}

impl AppState {
    /// Create new application state wrapping the given database.
    #[must_use]
    pub fn new(db: Arc<AletheiaDB>) -> Self {
        Self { db }
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
