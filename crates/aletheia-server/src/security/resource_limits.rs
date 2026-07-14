//! Per-query resource-limits core (Lane B2).
//!
//! Fills `aletheia-server::security::resource_limits`. Four pieces, all reusing
//! AletheiaDB's shared [`AletheiaHttpError`] envelope — so the wire mapping
//! (429 timeout / 413 too-large / 503 at-capacity) is inherited verbatim from
//! the HTTP `/query` surface (Issue #3542 / #3550), not reinvented:
//!
//! 1. [`ResourceLimits`] — the per-query caps view (timeout, row cap, byte cap,
//!    in-flight cap) sourced from [`SecurityConfig`].
//! 2. [`run_with_timeout`] — the async per-query wall-clock timeout wrapper.
//!    On deadline it returns the reused `429 RESOURCE_EXHAUSTED`
//!    (`WallClockTimeout`) error, mirroring `src/http` and #3542. The
//!    `retriable` flag is `!is_write` (#3368 MUST-FIX-1): a read-class timeout
//!    is transient/retriable, a write-class timeout is non-retriable (the write
//!    may already have committed). (The MCP surface labels the same condition
//!    `UNAVAILABLE`; here we reuse the HTTP envelope, whose timeout dimension is
//!    429, as the source of truth.)
//! 3. [`check_row_cap`] / [`check_byte_cap`] — the too-large `413` helpers,
//!    matching `src/http`'s `ResultRows` / `ResultBytes` dimensions.
//! 4. [`InFlightLimiter`] + [`InFlightGuard`] — the bounded in-flight-query DoS
//!    guard, an `Arc<AtomicUsize>` + RAII guard mirroring
//!    `src/http/state.rs::InFlightCounter` (#3550) exactly: a CAS-loop acquire
//!    that can never leak a slot, and a `Drop` that releases the slot even on
//!    an early return or a panic (RAII survives unwind).

use crate::security::SecurityConfig;
use aletheiadb::http::{AletheiaHttpError, LimitDimension, ResourceLimitExceeded};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// The concrete per-query caps in force for one surface, sourced from
/// [`SecurityConfig`]. A `0`/`Duration::ZERO` value on any dimension means
/// "unlimited", consistent with the HTTP `EffectiveQueryLimits` convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Per-query wall-clock timeout (`Duration::ZERO` = unlimited).
    pub timeout: Duration,
    /// Cap on rows/entities in a result (`0` = unlimited).
    pub max_result_rows: usize,
    /// Cap on serialized response bytes (`0` = unlimited).
    pub max_response_bytes: usize,
    /// Cap on concurrently-live timed-query workers (`0` = unbounded).
    pub in_flight_cap: usize,
}

impl ResourceLimits {
    /// Read the caps from a [`SecurityConfig`].
    #[must_use]
    pub fn from_config(cfg: &SecurityConfig) -> Self {
        Self {
            timeout: cfg.query_timeout,
            max_result_rows: cfg.max_result_rows,
            max_response_bytes: cfg.max_response_bytes,
            in_flight_cap: cfg.max_in_flight_queries,
        }
    }

    /// Build the [`InFlightLimiter`] this config's in-flight cap implies.
    #[must_use]
    pub fn in_flight_limiter(&self) -> InFlightLimiter {
        InFlightLimiter::new(self.in_flight_cap)
    }
}

/// Construct the reused wall-clock-timeout error: a `429 RESOURCE_EXHAUSTED`
/// carrying the effective timeout (in ms) in the `WallClockTimeout` dimension.
/// `retriable` is `!is_write`: a read-class timeout is transient (retriable), a
/// write-class timeout is non-retriable (it may already have committed — the
/// same classification `src/http::enforce_query_limits` uses, #3368 MUST-FIX-1).
#[must_use]
pub fn timeout_error(timeout: Duration, is_write: bool) -> AletheiaHttpError {
    AletheiaHttpError::ResourceLimitExceeded(ResourceLimitExceeded {
        dimension: LimitDimension::WallClockTimeout,
        limit: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        consumed: None,
        // #3368 MUST-FIX-1: a read-class timeout is transient (retriable), but a
        // write-class timeout may already have committed — retrying risks a
        // duplicate write, so it must be non-retriable. Mirrors legacy
        // `src/http/handlers.rs::enforce_query_limits` (`retriable: !is_write`).
        retriable: !is_write,
    })
}

/// Construct the reused row-cap error: a **non-retriable** `413` in the
/// `ResultRows` dimension. `limit` is the effective cap, `consumed` the row
/// count that exceeded it.
#[must_use]
pub fn row_cap_error(limit: usize, consumed: usize) -> AletheiaHttpError {
    AletheiaHttpError::ResourceLimitExceeded(ResourceLimitExceeded {
        dimension: LimitDimension::ResultRows,
        limit: limit as u64,
        consumed: Some(consumed as u64),
        retriable: false,
    })
}

/// Construct the reused byte-cap error: a **non-retriable** `413` in the
/// `ResultBytes` dimension. `limit` is the effective cap, `consumed` the
/// serialized byte size that exceeded it.
#[must_use]
pub fn byte_cap_error(limit: usize, consumed: usize) -> AletheiaHttpError {
    AletheiaHttpError::ResourceLimitExceeded(ResourceLimitExceeded {
        dimension: LimitDimension::ResultBytes,
        limit: limit as u64,
        consumed: Some(consumed as u64),
        retriable: false,
    })
}

/// Run `fut` under a per-query wall-clock deadline.
///
/// A `Duration::ZERO` timeout means **unlimited**: `fut` runs inline with no
/// deadline (no timer task is armed). Otherwise the future races the deadline;
/// on the deadline it resolves to the reused [`timeout_error`], whose
/// `retriable` flag is `!is_write` (read-class retriable, write-class not).
///
/// `is_write` classifies the guarded op: pass `true` for a write-path query so
/// a deadline that fires after a possible commit is reported non-retriable
/// (#3368 MUST-FIX-1), `false` for a read-path query.
///
/// Note the deadline bounds only the *await*; a synchronous, non-cancellable
/// body is not interrupted (mirroring the HTTP/MCP surfaces, which pair this
/// with the [`InFlightLimiter`] to bound the still-running worker count).
///
/// # Errors
///
/// Returns [`AletheiaHttpError::ResourceLimitExceeded`] (`WallClockTimeout`;
/// `retriable = !is_write`) when the deadline elapses before `fut` resolves.
pub async fn run_with_timeout<F, T>(
    timeout: Duration,
    is_write: bool,
    fut: F,
) -> Result<T, AletheiaHttpError>
where
    F: Future<Output = T>,
{
    if timeout.is_zero() {
        return Ok(fut.await);
    }
    match tokio::time::timeout(timeout, fut).await {
        Ok(value) => Ok(value),
        Err(_elapsed) => Err(timeout_error(timeout, is_write)),
    }
}

/// Enforce the row cap: `Ok` when `count <= max` (or `max == 0`, unlimited),
/// else the reused non-retriable `413` [`row_cap_error`]. The boundary is
/// inclusive — a result of exactly `max` rows is within budget.
///
/// # Errors
///
/// Returns [`AletheiaHttpError::ResourceLimitExceeded`] (`ResultRows`) when
/// `count` exceeds a finite `max`.
pub fn check_row_cap(count: usize, max: usize) -> Result<(), AletheiaHttpError> {
    if max != 0 && count > max {
        Err(row_cap_error(max, count))
    } else {
        Ok(())
    }
}

/// Enforce the byte cap against a **caller-supplied** byte count: `Ok` when
/// `bytes <= max` (or `max == 0`, unlimited), else the reused non-retriable
/// `413` [`byte_cap_error`]. The boundary is inclusive.
///
/// **This helper trusts `bytes`.** If the caller measures anything other than
/// the exact serialized response (e.g. a partial payload, a pre-envelope body,
/// or a `count` field), it undercounts and an over-cap response slips through.
/// Prefer [`check_byte_cap_of`], which serializes and measures the full value
/// itself, whenever you hold the value to be returned.
///
/// # Errors
///
/// Returns [`AletheiaHttpError::ResourceLimitExceeded`] (`ResultBytes`) when
/// `bytes` exceeds a finite `max`.
pub fn check_byte_cap(bytes: usize, max: usize) -> Result<(), AletheiaHttpError> {
    if max != 0 && bytes > max {
        Err(byte_cap_error(max, bytes))
    } else {
        Ok(())
    }
}

/// Enforce the byte cap by serializing the **full** `value` and measuring the
/// real serialized length — the undercount-proof counterpart to
/// [`check_byte_cap`] (F4). Mirrors `src/http/handlers.rs`, which caps against
/// `serde_json::to_vec(&body)` (the exact wire bytes), never a caller estimate.
///
/// Returns the true serialized byte length on success (`Ok(len)`, useful when
/// the caller then writes those same bytes), or the reused non-retriable `413`
/// [`byte_cap_error`] (`ResultBytes`, `consumed` = the full serialized length)
/// when it exceeds a finite `max`. A `max` of `0` means unlimited (the value is
/// still serialized to report its length). The boundary is inclusive.
///
/// # Errors
///
/// Returns [`AletheiaHttpError::ResourceLimitExceeded`] (`ResultBytes`) when the
/// serialized length exceeds a finite `max`, or [`AletheiaHttpError::Internal`]
/// if `value` fails to serialize (mirroring the HTTP surface's serialize-error
/// path).
pub fn check_byte_cap_of<T: serde::Serialize>(
    value: &T,
    max: usize,
) -> Result<usize, AletheiaHttpError> {
    let len = serde_json::to_vec(value)
        .map_err(|e| AletheiaHttpError::Internal(format!("failed to serialize response: {e}")))?
        .len();
    if max != 0 && len > max {
        Err(byte_cap_error(max, len))
    } else {
        Ok(len)
    }
}

/// Bounded in-flight-query admission control (Issue #3550 DoS guard).
///
/// An `Arc<AtomicUsize>` of live workers plus a `cap`, mirroring
/// `src/http/state.rs::InFlightCounter` (#3550): a CAS-loop acquire that at the
/// cap rejects with a retriable `503 UNAVAILABLE` rather than admitting another
/// worker, and an RAII [`InFlightGuard`] that releases the slot on drop — which
/// is guaranteed even on an early return or a panic (Rust runs `Drop` as the
/// stack unwinds). A `cap` of `0` means unbounded (slots are still counted so
/// [`live`](Self::live) is observable).
#[derive(Clone)]
pub struct InFlightLimiter {
    count: Arc<AtomicUsize>,
    cap: usize,
}

/// RAII slot in the bounded in-flight pool. Decrements the shared counter on
/// drop; hold it for exactly as long as the work it guards is live.
#[derive(Debug)]
pub struct InFlightGuard {
    count: Arc<AtomicUsize>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }
}

impl InFlightLimiter {
    /// A fresh limiter with the given cap (`0` = unbounded), starting empty.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(0)),
            cap,
        }
    }

    /// Build a limiter from a [`SecurityConfig`]'s in-flight cap.
    #[must_use]
    pub fn from_config(cfg: &SecurityConfig) -> Self {
        Self::new(cfg.max_in_flight_queries)
    }

    /// Try to reserve a slot.
    ///
    /// Returns `Ok(guard)` on success (the guard releases the slot on drop), or
    /// the reused [`AletheiaHttpError::InFlightCapacityExceeded`] (retriable
    /// `503`) when the pool is already at `cap`. A `cap` of `0` always admits.
    /// Uses a CAS loop so a spurious over-count can never leak a slot.
    ///
    /// # Errors
    ///
    /// Returns [`AletheiaHttpError::InFlightCapacityExceeded`] when the pool is
    /// full.
    pub fn try_acquire(&self) -> Result<InFlightGuard, AletheiaHttpError> {
        let mut current = self.count.load(Ordering::Acquire);
        loop {
            if self.cap != 0 && current >= self.cap {
                return Err(AletheiaHttpError::InFlightCapacityExceeded { cap: self.cap });
            }
            match self.count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(InFlightGuard {
                        count: Arc::clone(&self.count),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    /// Current number of live in-flight slots.
    #[must_use]
    pub fn live(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// The configured cap (`0` = unbounded).
    #[must_use]
    pub fn cap(&self) -> usize {
        self.cap
    }
}
