//! Shared database state, delivered as an autumn-web 0.5 extension.
//!
//! Mirrors `src/http/state.rs`, but bound to autumn **0.5**'s `AppState`. The
//! `Arc<AletheiaDB>` is installed once at startup via
//! [`AppState::insert_extension`](autumn_web::prelude::AppState::insert_extension)
//! and pulled back per-request through the [`SpikeState`] extractor — the same
//! extension pattern the existing 0.4 server uses, confirming it carries
//! forward to 0.5 unchanged.

use aletheiadb::AletheiaDB;
use aletheiadb::http::AletheiaHttpError;
use autumn_web::prelude::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::sync::Arc;

/// Per-request handle to the shared database.
#[derive(Clone)]
pub struct SpikeState {
    db: Arc<AletheiaDB>,
}

impl SpikeState {
    /// Wrap a shared database for installation into the app's extension bag.
    #[must_use]
    pub fn new(db: Arc<AletheiaDB>) -> Self {
        Self { db }
    }

    /// Clone the `Arc<AletheiaDB>` for moving into a blocking task.
    #[must_use]
    pub fn db_arc(&self) -> Arc<AletheiaDB> {
        self.db.clone()
    }
}

impl FromRequestParts<AppState> for SpikeState {
    type Rejection = AletheiaHttpError;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // `StateMissing` (HTTP 500) is exactly what the existing server returns
        // for an uninstalled extension — a boot-time invariant.
        state
            .extension::<SpikeState>()
            .map(|arc| (*arc).clone())
            .ok_or(AletheiaHttpError::StateMissing)
    }
}
