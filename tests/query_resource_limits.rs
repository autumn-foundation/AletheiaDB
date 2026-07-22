//! Public-API tests for Issue #3368's Rust query-builder resource limits.
//!
//! Covers `QueryBuilder::with_timeout`/`with_max_rows`/`with_memory_budget`,
//! the `AletheiaDBConfig::query_limits` operator ceiling, and the
//! `database_stats().resource_limits` observability surface.

use std::time::Duration;

use aletheiadb::query::limits::EngineQueryLimitsConfig;
use aletheiadb::{AletheiaDB, AletheiaDBConfig, Error, NodeId, PropertyMapBuilder, QueryError};

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
    let config = AletheiaDBConfig::builder()
        .query_limits(EngineQueryLimitsConfig::disabled())
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("db");
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
    let config = AletheiaDBConfig::builder()
        .query_limits(EngineQueryLimitsConfig {
            max_result_rows: 10,
            ..EngineQueryLimitsConfig::default()
        })
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("db");
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

    let stats = db.stats();
    assert_eq!(stats.resource_limits.timeout_terminations, 0);
    assert_eq!(stats.resource_limits.memory_terminations, 0);
    assert_eq!(stats.resource_limits.row_cap_terminations, 0);
    assert_eq!(stats.resource_limits.override_rejections, 0);
}

// ---- 5. stats().resource_limits.row_cap_terminations increments ----

#[test]
fn row_cap_termination_increments_database_stats() {
    let db = AletheiaDB::new().expect("db");
    let hub = seed_star(&db, 10);

    let before = db.stats().resource_limits.row_cap_terminations;

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

    let after = db.stats().resource_limits.row_cap_terminations;
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
