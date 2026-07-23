//! Public-API tests for Issue #3368's Rust query-builder resource limits.
//!
//! Covers `QueryBuilder::with_timeout`/`with_max_rows`/`with_memory_budget`,
//! the `AletheiaDBConfig::query_limits` operator ceiling, and the
//! `AletheiaDB::query_limit_counters()` observability surface.

use std::time::Duration;

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::query::limits::EngineQueryLimitsConfig;
use aletheiadb::{AletheiaDB, AletheiaDBConfig, Error, NodeId, PropertyMapBuilder, QueryError};

/// Build a DB with the given engine query-limits on an **isolated per-test WAL
/// directory**. The default unified-config `wal_dir` is the CWD-relative
/// `aletheiadb/wal`, which every `with_unified_config` test in a binary would
/// share — colliding (WAL corruption: "Unknown WAL operation type") under a
/// parallel test runner such as the coverage job. Each test therefore gets its
/// own tempdir. The returned `TempDir` must be kept alive for the DB's lifetime.
fn db_with_query_limits(limits: EngineQueryLimitsConfig) -> (AletheiaDB, tempfile::TempDir) {
    let wal_home = tempfile::tempdir().expect("create tempdir");
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_home.path().join("wal"))
                .build(),
        )
        .query_limits(limits)
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("create db from unified config");
    (db, wal_home)
}

/// Build a small star graph: one hub node with `n` outgoing `KNOWS` edges to
/// freshly created leaf nodes. Returns the hub id.
fn seed_star(db: &AletheiaDB, n: usize) -> NodeId {
    let hub = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Hub").build(),
        )
        .expect("create hub");
    for i in 0..n {
        let leaf = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("Leaf{i}"))
                    .build(),
            )
            .expect("create leaf");
        db.create_edge(hub, leaf, "KNOWS", PropertyMapBuilder::new().build())
            .expect("create edge");
    }
    hub
}

fn resource_exhausted_dimension(err: &Error) -> Option<&'static str> {
    match err {
        Error::Query(QueryError::ResourceExhausted { dimension, .. }) => Some(dimension),
        _ => None,
    }
}

// ---- 1. Builder row cap ----

#[test]
fn with_max_rows_terminates_the_drain_once_the_cap_is_exceeded() {
    let db = AletheiaDB::new().expect("db");
    let hub = seed_star(&db, 10);

    let results = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .with_max_rows(2)
        .execute(&db)
        .expect("execute should succeed lazily");

    let mut ok_rows = 0usize;
    let mut saw_exhausted = false;
    for row in results {
        match row {
            Ok(_) => ok_rows += 1,
            Err(e) => {
                assert_eq!(
                    resource_exhausted_dimension(&e),
                    Some("result_rows"),
                    "unexpected error: {e:?}"
                );
                saw_exhausted = true;
                break;
            }
        }
    }
    assert!(
        saw_exhausted,
        "expected the drain to hit the result_rows cap; got {ok_rows} ok rows with no error"
    );
    assert!(
        ok_rows <= 2,
        "guard let through more than the cap: {ok_rows}"
    );
}

// ---- 2. with_timeout ----

#[test]
fn with_timeout_zero_is_unlimited_when_no_ceiling_is_configured() {
    // Disabled engine limits => every dimension unlimited, overrides ignored,
    // so a zero-timeout override cannot be rejected as "requested unlimited
    // under a finite ceiling".
    let (db, _wal) = db_with_query_limits(EngineQueryLimitsConfig::disabled());
    let hub = seed_star(&db, 3);

    let rows: Vec<_> = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .with_timeout(Duration::from_millis(0))
        .execute(&db)
        .expect("execute")
        .collect::<Result<Vec<_>, _>>()
        .expect("zero timeout under no ceiling must not terminate the query");

    assert_eq!(rows.len(), 3);
}

#[test]
fn with_timeout_small_but_sufficient_succeeds_and_returns_rows() {
    let db = AletheiaDB::new().expect("db");
    let hub = seed_star(&db, 3);

    let rows: Vec<_> = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .with_timeout(Duration::from_secs(5))
        .execute(&db)
        .expect("execute")
        .collect::<Result<Vec<_>, _>>()
        .expect("a generous timeout on a tiny query must succeed");

    assert_eq!(rows.len(), 3);
}

// ---- 3. Over-ceiling override ----

#[test]
fn override_above_operator_ceiling_is_rejected_and_counted() {
    let (db, _wal) = db_with_query_limits(EngineQueryLimitsConfig {
        max_result_rows: 10,
        ..EngineQueryLimitsConfig::default()
    });
    let hub = seed_star(&db, 3);

    let before = db.query_limit_counters().override_rejected;

    let result = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .with_max_rows(1000)
        .execute(&db);

    match result {
        Err(Error::Query(QueryError::InvalidParameter { .. })) => {}
        Err(other) => panic!("expected InvalidParameter, got {other:?}"),
        Ok(_) => panic!("override above the ceiling must be rejected before execution"),
    }

    let after = db.query_limit_counters().override_rejected;
    assert_eq!(after, before + 1);
}

// ---- 4. Default AletheiaDB::new() is unaffected ----

#[test]
fn default_db_is_unaffected_and_reports_zero_resource_limit_stats() {
    let db = AletheiaDB::new().expect("db");
    let hub = seed_star(&db, 5);

    let rows: Vec<_> = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .execute(&db)
        .expect("execute")
        .collect::<Result<Vec<_>, _>>()
        .expect("plain query on a default db must succeed");
    assert_eq!(rows.len(), 5);

    let counters = db.query_limit_counters();
    assert_eq!(counters.wall_clock_timeout, 0);
    assert_eq!(counters.memory_bytes, 0);
    assert_eq!(counters.result_rows, 0);
    assert_eq!(counters.override_rejected, 0);
}

// ---- 5. query_limit_counters().result_rows increments ----

#[test]
fn row_cap_termination_increments_database_stats() {
    let db = AletheiaDB::new().expect("db");
    let hub = seed_star(&db, 10);

    let before = db.query_limit_counters().result_rows;

    let results = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .with_max_rows(2)
        .execute(&db)
        .expect("execute");
    // Drain fully so the guard observes the breach.
    for row in results {
        if row.is_err() {
            break;
        }
    }

    let after = db.query_limit_counters().result_rows;
    assert_eq!(after, before + 1);
}

// ---- 6. Memory budget ----

#[test]
fn with_memory_budget_terminates_on_a_multi_row_query() {
    let db = AletheiaDB::new().expect("db");
    let hub = seed_star(&db, 50);

    let results = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .with_memory_budget(1)
        .execute(&db)
        .expect("execute should succeed lazily");

    let mut saw_exhausted = false;
    for row in results {
        if let Err(e) = row {
            assert_eq!(resource_exhausted_dimension(&e), Some("memory_bytes"));
            saw_exhausted = true;
            break;
        }
    }
    assert!(saw_exhausted, "expected a memory_bytes termination");
}

// ---- 7. LLM self-correction loop (AC5) ----
//
// The issue's headline differentiator: an over-limit error must carry enough
// structured information for a caller (an LLM) to tighten its query and succeed
// within the same session, without human help. This test performs exactly that
// loop programmatically: run an over-limit query, read the error's structured
// `dimension`/`limit`/`consumed`, then re-issue a scoped-down query that fits —
// and assert it succeeds. It also pins the retriability contract that tells a
// caller *whether* to retry (timeout) vs *repair* (row/memory).

/// Destructure a `ResourceExhausted` into its structured fields, as a caller
/// inspecting the #3234 `details` payload would.
fn resource_exhausted_parts(err: &Error) -> Option<(&'static str, u64, u64, bool)> {
    match err {
        Error::Query(QueryError::ResourceExhausted {
            dimension,
            limit,
            consumed,
            retriable,
        }) => Some((dimension, *limit, *consumed, *retriable)),
        _ => None,
    }
}

#[test]
fn over_limit_error_details_let_a_caller_self_correct_in_one_retry() {
    let db = AletheiaDB::new().expect("db");
    let hub = seed_star(&db, 10);

    // Attempt 1: a query that over-runs a tight protective row cap.
    let cap = 2usize;
    let first = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .with_max_rows(cap)
        .execute(&db)
        .expect("execute should succeed lazily")
        .collect::<Result<Vec<_>, _>>();

    let err = first.expect_err("the drain must surface the row-cap breach");
    let (dimension, limit, consumed, retriable) =
        resource_exhausted_parts(&err).expect("must be a ResourceExhausted error");

    // The caller reads the structured details to decide how to adapt.
    assert_eq!(dimension, "result_rows");
    assert_eq!(limit, cap as u64);
    assert!(
        consumed > limit,
        "consumed ({consumed}) must exceed the limit ({limit}) to justify tightening"
    );
    // A result-cap breach is a caller fault to REPAIR, not retry blindly.
    assert!(!retriable, "a row-cap breach must be non-retriable");

    // Attempt 2 (the self-correction): bound the query's own output to the cap
    // the error just disclosed, so it now fits. In an agent loop this is the
    // "read details -> shrink scope" step; here `limit` came straight off the
    // error payload.
    let rows = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .limit(limit as usize)
        .with_max_rows(cap)
        .execute(&db)
        .expect("execute should succeed lazily")
        .collect::<Result<Vec<_>, _>>()
        .expect("the tightened query must succeed within the disclosed cap");
    assert_eq!(
        rows.len(),
        cap,
        "the corrected query returns exactly the capped number of rows"
    );
}

#[test]
fn timeout_error_is_retriable_but_resource_caps_are_not() {
    let db = AletheiaDB::new().expect("db");
    let hub = seed_star(&db, 50);

    // A wall-clock timeout is retriable: re-running a read is safe, and a
    // backed-off retry may land inside budget. Force one deterministically:
    // `with_timeout` fixes the deadline at `execute()` time, and the result
    // stream is lazy, so sleeping past the (1 ms) deadline before draining
    // makes the guard's pre-pull check at row 0 fire regardless of row count.
    let lazy = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .with_timeout(std::time::Duration::from_millis(1))
        .execute(&db)
        .expect("execute lazily");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let timeout_err = lazy
        .collect::<Result<Vec<_>, _>>()
        .expect_err("the deadline elapsed before the drain, so the guard must fire");
    let (dimension, _, _, retriable) =
        resource_exhausted_parts(&timeout_err).expect("ResourceExhausted");
    assert_eq!(dimension, "wall_clock_timeout");
    assert!(
        retriable,
        "a wall-clock timeout must be retriable: {timeout_err:?}"
    );

    // A memory-budget breach is deterministic: the same request reproduces the
    // same oversized result, so it is non-retriable (repair, don't retry).
    let mem_err = db
        .query()
        .start(hub)
        .traverse("KNOWS")
        .with_memory_budget(1)
        .execute(&db)
        .expect("execute lazily")
        .collect::<Result<Vec<_>, _>>()
        .expect_err("memory budget must trip");
    let (dimension, _, _, retriable) =
        resource_exhausted_parts(&mem_err).expect("ResourceExhausted");
    assert_eq!(dimension, "memory_bytes");
    assert!(!retriable, "a memory-budget breach must be non-retriable");
}
