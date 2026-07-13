//! Seam-independent acceptance tests for the Lane B2 resource-limits core
//! (autumn migration §8 / Issue #3561 ACs: per-query timeout, row/byte caps,
//! bounded in-flight guard; #3542 caps, #3550 in-flight bound).
//!
//! These pin the parts provable **without a running server**: the async
//! per-query timeout wrapper, the row/byte cap helpers, and the RAII
//! in-flight guard (acquire/release, release-on-drop, release-on-panic). Every
//! error is the reused [`aletheiadb::http::AletheiaHttpError`] envelope — no
//! new error shapes — so the wire mapping (429 timeout / 413 too-large / 503
//! at-capacity) is inherited verbatim from the HTTP surface.

use std::sync::Arc;
use std::time::Duration;

use aletheia_server::security::SecurityConfig;
use aletheia_server::security::resource_limits::{
    InFlightLimiter, ResourceLimits, byte_cap_error, check_byte_cap, check_byte_cap_of,
    check_row_cap, row_cap_error, run_with_timeout, timeout_error,
};
use aletheiadb::auth::{AuthMode, AuthStore};
use aletheiadb::http::{AletheiaHttpError, LimitDimension};

use axum::http::StatusCode;
use axum::response::IntoResponse;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A conservative-default `SecurityConfig` (auth required, generous caps,
/// rate-limit off). Note the re-homed crate's `new` is store-first.
fn config() -> SecurityConfig {
    SecurityConfig::new(Arc::new(AuthStore::new()), AuthMode::Required)
}

/// Render an error to its HTTP status (the reused envelope's wire mapping).
fn status_of(err: AletheiaHttpError) -> StatusCode {
    err.into_response().status()
}

// ---------------------------------------------------------------------------
// ResourceLimits view sourced from SecurityConfig
// ---------------------------------------------------------------------------

#[test]
fn resource_limits_are_sourced_from_config() {
    let cfg = config();
    let limits = ResourceLimits::from_config(&cfg);
    assert_eq!(limits.timeout, cfg.query_timeout);
    assert_eq!(limits.max_result_rows, cfg.max_result_rows);
    assert_eq!(limits.max_response_bytes, cfg.max_response_bytes);
    assert_eq!(limits.in_flight_cap, cfg.max_in_flight_queries);
    // Conservative defaults: generous, non-zero caps.
    assert_eq!(limits.timeout, Duration::from_millis(30_000));
    assert_eq!(limits.max_result_rows, 10_000);
    assert_eq!(limits.max_response_bytes, 8 * 1024 * 1024);
    assert_eq!(limits.in_flight_cap, 64);
}

// ---------------------------------------------------------------------------
// (1) Async per-query timeout wrapper
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fast_op_under_timeout_completes_ok() {
    let out = run_with_timeout(Duration::from_secs(5), false, async { 7_u32 }).await;
    assert_eq!(out.expect("fast op is well within the deadline"), 7);
}

#[tokio::test]
async fn slow_op_over_timeout_is_retriable_resource_exhausted() {
    // Read-class (`is_write = false`): a timeout invites a retry (retriable).
    let out: Result<(), _> = run_with_timeout(Duration::from_millis(20), false, async {
        tokio::time::sleep(Duration::from_millis(400)).await;
    })
    .await;
    let err = out.expect_err("the op overran its deadline");
    // The reused HTTP envelope models a wall-clock timeout as a *retriable*
    // 429 RESOURCE_EXHAUSTED (`WallClockTimeout` dimension). This is the
    // src/http source of truth; the MCP surface (#3542) labels the same
    // condition UNAVAILABLE, but here we reuse AletheiaHttpError verbatim.
    match &err {
        AletheiaHttpError::ResourceLimitExceeded(e) => {
            assert_eq!(e.dimension, LimitDimension::WallClockTimeout);
            assert!(e.retriable, "a read-class timeout invites a retry");
            assert_eq!(e.limit, 20);
        }
        other => panic!("expected ResourceLimitExceeded timeout, got {other:?}"),
    }
    assert_eq!(status_of(err), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn zero_timeout_means_unlimited_and_never_fires() {
    // `Duration::ZERO` disables the deadline: the op runs inline to completion.
    let out = run_with_timeout(Duration::ZERO, false, async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        99_u32
    })
    .await;
    assert_eq!(out.expect("zero timeout = unlimited, runs inline"), 99);
}

#[test]
fn timeout_error_shape_is_the_reused_429_envelope() {
    // Read-class constructor: retriable 429.
    let err = timeout_error(Duration::from_millis(123), false);
    match &err {
        AletheiaHttpError::ResourceLimitExceeded(e) => {
            assert_eq!(e.dimension, LimitDimension::WallClockTimeout);
            assert_eq!(e.limit, 123);
            assert!(e.retriable);
        }
        other => panic!("expected timeout envelope, got {other:?}"),
    }
    assert_eq!(status_of(err), StatusCode::TOO_MANY_REQUESTS);
}

/// F3 regression (#3368 MUST-FIX-1): a **write-class** timeout must be
/// **non-retriable**. A write may already have committed by the time the
/// deadline fires, so inviting a retry risks a duplicate write — legacy uses
/// `retriable: !is_write` (`src/http/handlers.rs::enforce_query_limits`). The
/// historical bug hardcoded `retriable: true` regardless of write-class. This
/// pins both the `run_with_timeout(is_write=true)` path and the `timeout_error`
/// constructor; the read-class TRUE case stays covered by
/// `slow_op_over_timeout_is_retriable_resource_exhausted` and
/// `timeout_error_shape_is_the_reused_429_envelope`.
#[tokio::test]
async fn write_timeout_is_not_retriable() {
    // Write-class run_with_timeout: the deadline fires, the error is NOT
    // retriable.
    let out: Result<(), _> = run_with_timeout(Duration::from_millis(20), true, async {
        tokio::time::sleep(Duration::from_millis(400)).await;
    })
    .await;
    let err = out.expect_err("the write op overran its deadline");
    match &err {
        AletheiaHttpError::ResourceLimitExceeded(e) => {
            assert_eq!(e.dimension, LimitDimension::WallClockTimeout);
            assert_eq!(e.limit, 20);
            assert!(
                !e.retriable,
                "a write-class timeout must NOT invite a retry (may double-commit)"
            );
        }
        other => panic!("expected ResourceLimitExceeded timeout, got {other:?}"),
    }
    // Still the reused 429 wire mapping — only the retriable flag differs.
    assert_eq!(status_of(err), StatusCode::TOO_MANY_REQUESTS);

    // The constructor honors write-class directly, too.
    let ctor = timeout_error(Duration::from_millis(123), true);
    match &ctor {
        AletheiaHttpError::ResourceLimitExceeded(e) => {
            assert!(
                !e.retriable,
                "timeout_error(_, is_write=true) is non-retriable"
            );
        }
        other => panic!("expected timeout envelope, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// (2) Row cap + (3) Byte cap + (7) exact boundary
// ---------------------------------------------------------------------------

#[test]
fn rows_under_cap_ok_over_cap_rejected() {
    // Under the cap: Ok.
    assert!(check_row_cap(9, 10).is_ok());
    // Over the cap: a non-retriable 413.
    let err = check_row_cap(25, 10).expect_err("25 > 10");
    match &err {
        AletheiaHttpError::ResourceLimitExceeded(e) => {
            assert_eq!(e.dimension, LimitDimension::ResultRows);
            assert_eq!(e.limit, 10);
            assert_eq!(e.consumed, Some(25));
            assert!(!e.retriable, "a row cap is a caller-fault, not retriable");
        }
        other => panic!("expected ResultRows envelope, got {other:?}"),
    }
    assert_eq!(status_of(err), StatusCode::PAYLOAD_TOO_LARGE);
}

#[test]
fn bytes_under_cap_ok_over_cap_rejected() {
    assert!(check_byte_cap(1024, 4096).is_ok());
    let err = check_byte_cap(8192, 4096).expect_err("8192 > 4096");
    match &err {
        AletheiaHttpError::ResourceLimitExceeded(e) => {
            assert_eq!(e.dimension, LimitDimension::ResultBytes);
            assert_eq!(e.limit, 4096);
            assert_eq!(e.consumed, Some(8192));
            assert!(!e.retriable);
        }
        other => panic!("expected ResultBytes envelope, got {other:?}"),
    }
    assert_eq!(status_of(err), StatusCode::PAYLOAD_TOO_LARGE);
}

#[test]
fn cap_helpers_honor_the_exact_boundary() {
    // Exactly at the cap is allowed; one over is rejected (both dimensions).
    assert!(
        check_row_cap(10, 10).is_ok(),
        "count == cap is within budget"
    );
    assert!(check_row_cap(11, 10).is_err(), "count == cap + 1 is over");
    assert!(check_byte_cap(4096, 4096).is_ok());
    assert!(check_byte_cap(4097, 4096).is_err());
}

/// F4 regression (byte-cap undercount): `check_byte_cap` trusts a caller-supplied
/// byte count, so a caller that measured only part of the response (e.g. a count
/// field, or the pre-envelope payload) undercounts and wrongly passes a response
/// that is actually over the cap. `check_byte_cap_of` removes the trust by
/// serializing the FULL value itself (`serde_json::to_vec`) and measuring that —
/// mirroring legacy `src/http/handlers.rs`, which caps against the real wire
/// bytes. Under cap → `Ok(len)`; over cap → the non-retriable `413` with
/// `consumed` = the true serialized length.
#[test]
fn check_byte_cap_of_measures_the_full_serialized_value() {
    // Under the cap: returns the true serialized byte length.
    let small = vec![1u32, 2, 3];
    let true_len = serde_json::to_vec(&small).expect("serializes").len();
    let ok = check_byte_cap_of(&small, 4096).expect("small value under cap");
    assert_eq!(ok, true_len, "returns the real serialized byte length");

    // Over the cap: the non-retriable 413, consumed = full serialized length.
    let big: Vec<u32> = (0..1000).collect();
    let serialized = serde_json::to_vec(&big).expect("serializes").len();
    assert!(
        serialized > 16,
        "sanity: the big value is over the tiny cap"
    );
    let err = check_byte_cap_of(&big, 16).expect_err("serialized value over cap");
    match &err {
        AletheiaHttpError::ResourceLimitExceeded(e) => {
            assert_eq!(e.dimension, LimitDimension::ResultBytes);
            assert_eq!(e.limit, 16);
            assert_eq!(
                e.consumed,
                Some(serialized as u64),
                "consumed is the FULL serialized length, not a caller estimate"
            );
            assert!(!e.retriable);
        }
        other => panic!("expected ResultBytes envelope, got {other:?}"),
    }
    assert_eq!(status_of(err), StatusCode::PAYLOAD_TOO_LARGE);

    // The exact undercount bug the helper fixes: a caller that miscounts the
    // size (here, pretends the big value is only 10 bytes) would slip past the
    // trusting `check_byte_cap`, but `check_byte_cap_of` measures the real value.
    let caller_undercount = 10usize;
    assert!(
        check_byte_cap(caller_undercount, 16).is_ok(),
        "raw check_byte_cap trusts the (wrong) caller count and passes"
    );
    assert!(
        check_byte_cap_of(&big, 16).is_err(),
        "check_byte_cap_of measures the full value and rejects it"
    );
}

#[test]
fn zero_cap_means_unlimited() {
    // `0` = unlimited on either dimension (mirrors EffectiveQueryLimits).
    assert!(check_row_cap(usize::MAX, 0).is_ok());
    assert!(check_byte_cap(usize::MAX, 0).is_ok());
}

#[test]
fn cap_error_constructors_carry_consumed() {
    match row_cap_error(5, 9) {
        AletheiaHttpError::ResourceLimitExceeded(e) => {
            assert_eq!(e.dimension, LimitDimension::ResultRows);
            assert_eq!(e.limit, 5);
            assert_eq!(e.consumed, Some(9));
        }
        other => panic!("unexpected {other:?}"),
    }
    match byte_cap_error(5, 9) {
        AletheiaHttpError::ResourceLimitExceeded(e) => {
            assert_eq!(e.dimension, LimitDimension::ResultBytes);
        }
        other => panic!("unexpected {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// (4) Bounded in-flight guard: acquire N ok, N+1 rejected
// ---------------------------------------------------------------------------

#[test]
fn in_flight_acquires_up_to_cap_then_rejects() {
    let limiter = InFlightLimiter::new(2);
    let g1 = limiter.try_acquire().expect("1st slot");
    let g2 = limiter.try_acquire().expect("2nd slot");
    assert_eq!(limiter.live(), 2);

    let err = limiter.try_acquire().expect_err("3rd over cap");
    match &err {
        AletheiaHttpError::InFlightCapacityExceeded { cap } => assert_eq!(*cap, 2),
        other => panic!("expected InFlightCapacityExceeded, got {other:?}"),
    }
    // At-capacity is a retriable 503 UNAVAILABLE (back off and retry).
    assert_eq!(status_of(err), StatusCode::SERVICE_UNAVAILABLE);

    // Keep the guards live across the assertions above.
    drop((g1, g2));
}

#[test]
fn in_flight_cap_zero_is_unbounded() {
    let limiter = InFlightLimiter::new(0);
    let _a = limiter.try_acquire().expect("unbounded");
    let _b = limiter.try_acquire().expect("unbounded");
    let _c = limiter.try_acquire().expect("unbounded");
    assert_eq!(limiter.live(), 3);
}

#[test]
fn in_flight_limiter_from_config_reads_the_cap() {
    let cfg = config();
    let limiter = InFlightLimiter::from_config(&cfg);
    // Default cap is 64; acquire one to prove it is live.
    let _g = limiter.try_acquire().expect("slot within default cap");
    assert_eq!(limiter.live(), 1);
}

// ---------------------------------------------------------------------------
// (5) Slot released after guard drop
// ---------------------------------------------------------------------------

#[test]
fn slot_is_released_when_guard_drops() {
    let limiter = InFlightLimiter::new(1);
    {
        let _g = limiter.try_acquire().expect("1st slot");
        // The single slot is now exhausted.
        assert!(limiter.try_acquire().is_err());
        assert_eq!(limiter.live(), 1);
    }
    // Guard dropped at end of scope → slot returned.
    assert_eq!(limiter.live(), 0);
    assert!(limiter.try_acquire().is_ok(), "slot reusable after drop");
}

// ---------------------------------------------------------------------------
// (6) Slot released even when the guarded scope PANICS
// ---------------------------------------------------------------------------

#[test]
fn slot_is_released_on_panic_in_guarded_scope() {
    let limiter = InFlightLimiter::new(1);
    let in_thread = limiter.clone();
    let handle = std::thread::spawn(move || {
        let _g = in_thread.try_acquire().expect("slot in worker");
        // RAII must survive unwind: Drop runs as the stack unwinds.
        panic!("boom in guarded scope");
    });
    // The worker panicked; its guard dropped during unwind.
    assert!(handle.join().is_err(), "worker panicked as expected");
    assert_eq!(limiter.live(), 0, "panic must not leak the slot");
    let _fresh = limiter.try_acquire().expect("fresh acquire after panic");
}
