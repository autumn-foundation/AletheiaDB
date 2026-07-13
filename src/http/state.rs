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
use std::sync::atomic::{AtomicUsize, Ordering};

/// Shared, process-wide counter of `/query` requests currently occupying a
/// wall-clock-timeout worker (Issue #3368 DoS guard; the HTTP-surface
/// counterpart of the MCP `in_flight_queries` counter).
///
/// A finite `timeout_ms` makes the `/query` handler race the underlying
/// blocking computation on a **detached, non-cancellable** worker: when the
/// caller times out the worker is discarded but keeps running to completion.
/// This counter — cloned (Arc-shared) across every per-request [`AppState`] —
/// bounds how many such workers can be live at once. A worker holds its slot
/// for its whole lifetime (even after the caller has timed out and discarded
/// it), because the [`InFlightGuard`] is moved into the worker and dropped only
/// when the worker itself finishes.
#[derive(Clone)]
pub(crate) struct InFlightCounter {
    count: Arc<AtomicUsize>,
}

/// RAII slot in the bounded in-flight-query pool (Issue #3368). Decrements the
/// shared counter on drop — which, by construction, happens inside the spawned
/// worker when the WORKER finishes (never when the caller merely times out), so
/// a discarded-but-still-running query keeps holding its slot until it actually
/// completes.
pub(crate) struct InFlightGuard {
    count: Arc<AtomicUsize>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }
}

impl InFlightCounter {
    /// A fresh counter starting at zero live workers.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Try to reserve a slot in the bounded in-flight-query pool.
    ///
    /// Returns `Some(guard)` on success (the guard releases the slot on drop),
    /// or `None` when the pool is already at `cap` (the caller then rejects the
    /// query). A `cap` of `0` means unbounded — a slot is always granted (still
    /// accounted, so [`live`](Self::live) can observe the count). Uses a CAS
    /// loop so a spurious over-count can never leak a slot.
    pub(crate) fn try_acquire(&self, cap: usize) -> Option<InFlightGuard> {
        let mut current = self.count.load(Ordering::Acquire);
        loop {
            if cap != 0 && current >= cap {
                return None;
            }
            match self.count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(InFlightGuard {
                        count: Arc::clone(&self.count),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    /// Current number of live in-flight workers (test/introspection).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn live(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
}

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
    in_flight: InFlightCounter,
}

impl AppState {
    /// Create new application state wrapping the given database, with the
    /// default per-query limits ([`QueryLimitsConfig::default`]).
    #[must_use]
    pub fn new(db: Arc<AletheiaDB>) -> Self {
        Self {
            db,
            query_limits: Arc::new(QueryLimitsConfig::default()),
            in_flight: InFlightCounter::new(),
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

    /// The shared in-flight-query counter (Issue #3368 DoS guard). Cloned
    /// (Arc-shared) so the `/query` handler can reserve/release worker slots
    /// against the same process-wide count across every per-request clone.
    #[must_use]
    pub(crate) fn in_flight_counter(&self) -> InFlightCounter {
        self.in_flight.clone()
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
    fn app_state_clone_shares_in_flight_counter() {
        // Every per-request clone must see the same process-wide worker count,
        // otherwise the DoS cap could be bypassed by cloning (Issue #3368).
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state = AppState::new(db);
        let counter = state.in_flight_counter();
        let clone = state.clone();
        let _guard = counter.try_acquire(0).expect("unbounded acquire");
        assert_eq!(
            clone.in_flight_counter().live(),
            1,
            "a slot reserved via one clone is visible through another"
        );
    }

    #[test]
    fn in_flight_cap_rejects_at_limit_and_releases_on_drop() {
        let counter = InFlightCounter::new();
        let g1 = counter.try_acquire(1).expect("first slot");
        assert_eq!(counter.live(), 1);
        // At the cap: no further slot.
        assert!(counter.try_acquire(1).is_none(), "cap reached → rejected");
        drop(g1);
        assert_eq!(counter.live(), 0, "slot released on guard drop");
        assert!(
            counter.try_acquire(1).is_some(),
            "a slot is available again after release"
        );
    }

    #[test]
    fn in_flight_cap_zero_is_unbounded() {
        let counter = InFlightCounter::new();
        let mut guards = Vec::new();
        for _ in 0..1000 {
            guards.push(counter.try_acquire(0).expect("cap 0 always grants"));
        }
        assert_eq!(counter.live(), 1000);
    }

    #[test]
    fn from_arc_db_works() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state: AppState = db.clone().into();
        assert!(Arc::ptr_eq(&db, &state.db_arc()));
    }
}
