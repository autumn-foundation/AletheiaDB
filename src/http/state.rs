//! Application state for HTTP server
//!
//! This module provides `AppState` for sharing GallifreyDB across actix-web handlers.
//! Following the pattern established in the MCP server, we use `Arc<GallifreyDB>`
//! without an outer RwLock since GallifreyDB already provides interior mutability.
//!
//! # Thread Safety
//!
//! GallifreyDB is designed for concurrent access:
//! - All internal collections use DashMap (lock-free concurrent HashMap)
//! - Indexes use RwLock for read-heavy workloads
//! - WAL uses striped locking for write throughput
//!
//! Therefore, wrapping in `Arc<RwLock<GallifreyDB>>` would add unnecessary
//! global contention and reduce performance.
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::{GallifreyDB, http::AppState};
//! use actix_web::{web, App, HttpServer, HttpResponse};
//! use std::sync::Arc;
//!
//! async fn create_node(state: web::Data<AppState>) -> HttpResponse {
//!     let node_id = state.db().create_node("Person", properties).unwrap();
//!     HttpResponse::Ok().json(node_id)
//! }
//!
//! #[actix_web::main]
//! async fn main() -> std::io::Result<()> {
//!     let db = Arc::new(GallifreyDB::new().unwrap());
//!     let app_state = AppState::new(db);
//!
//!     HttpServer::new(move || {
//!         App::new()
//!             .app_data(web::Data::new(app_state.clone()))
//!             .route("/nodes", web::post().to(create_node))
//!     })
//!     .bind("127.0.0.1:8080")?
//!     .run()
//!     .await
//! }
//! ```

use crate::GallifreyDB;
use std::sync::Arc;

/// Application state shared across actix-web handlers
///
/// Wraps `Arc<GallifreyDB>` for type-safe sharing. GallifreyDB is already
/// thread-safe via interior mutability (DashMap, RwLock, Mutex), so no
/// additional locking is needed.
///
/// # Example
/// ```
/// use gallifreydb::{GallifreyDB, http::AppState};
/// use std::sync::Arc;
///
/// let db = Arc::new(GallifreyDB::new().unwrap());
/// let state = AppState::new(db);
///
/// // Clone for sharing across handlers
/// let state2 = state.clone();
/// ```
#[derive(Clone)]
pub struct AppState {
    db: Arc<GallifreyDB>,
}

impl AppState {
    /// Create new application state with the given database
    pub fn new(db: Arc<GallifreyDB>) -> Self {
        Self { db }
    }

    /// Get a reference to the database
    ///
    /// Returns a reference to the GallifreyDB instance, dereferencing the Arc
    /// for convenient method calls.
    pub fn db(&self) -> &GallifreyDB {
        &self.db
    }

    /// Get the Arc<GallifreyDB> directly
    ///
    /// Useful when you need to clone the Arc or pass it to other components.
    pub fn db_arc(&self) -> Arc<GallifreyDB> {
        self.db.clone()
    }
}

impl From<Arc<GallifreyDB>> for AppState {
    fn from(db: Arc<GallifreyDB>) -> Self {
        Self::new(db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_state_clone_shares_db() {
        let db = Arc::new(GallifreyDB::new().unwrap());
        let state = AppState::new(db.clone());
        let state2 = state.clone();

        // Both should point to the same Arc
        assert!(Arc::ptr_eq(&state.db_arc(), &state2.db_arc()));
    }

    #[test]
    fn test_from_impl() {
        let db = Arc::new(GallifreyDB::new().unwrap());
        let state: AppState = db.clone().into();

        assert!(Arc::ptr_eq(&db, &state.db_arc()));
    }
}
