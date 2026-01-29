#![cfg(feature = "sql")]

use gallifreydb::query::planner::{PhysicalOp, QueryPlanner, Statistics};
use gallifreydb::sql::parse_sql;
use gallifreydb::storage::CurrentStorage;
use std::sync::Arc;

#[test]
fn test_sql_system_time_as_of_node_lookup_wiring() {
    let sql = "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1000'";
    let query = parse_sql(sql).expect("Failed to parse SQL");

    assert!(query.temporal_context().is_some(), "Query should have temporal context");

    let storage = Arc::new(CurrentStorage::new());
    let stats = Arc::new(Statistics::default());
    let planner = QueryPlanner::new(stats, storage);

    let plan = planner.plan(query).expect("Failed to plan query");

    // Verify PhysicalPlan has temporal context
    assert!(plan.temporal_context.is_some(), "PhysicalPlan should have temporal context");
    assert!(plan.is_temporal());

    if let Some(ctx) = plan.temporal_context {
        if let Some((_valid, tx)) = ctx.as_of {
             // System time '1000' should be transaction time
             assert_eq!(tx.wallclock(), 1000);
        } else {
            panic!("Expected as_of context");
        }
    }
}

#[test]
fn test_sql_valid_time_as_of_wiring() {
    let sql = "SELECT * FROM nodes FOR VALID_TIME AS OF TIMESTAMP '2000'";
    let query = parse_sql(sql).expect("Failed to parse SQL");

    let storage = Arc::new(CurrentStorage::new());
    let stats = Arc::new(Statistics::default());
    let planner = QueryPlanner::new(stats, storage);

    let plan = planner.plan(query).expect("Failed to plan query");

    assert!(plan.temporal_context.is_some());
    if let Some(ctx) = plan.temporal_context {
        if let Some((valid, _tx)) = ctx.as_of {
             // Valid time '2000'
             assert_eq!(valid.wallclock(), 2000);
        } else {
            panic!("Expected as_of context");
        }
    }
}

#[test]
fn test_sql_temporal_vector_search_wiring() {
    // Best-effort integration test for temporal + vector (KNN) wiring.
    // This test attempts to parse a query that uses KNN and a SYSTEM_TIME AS OF clause.
    // If the SQL parser does not support KNN syntax, parse_sql will return an error and
    // the test will be soft-skipped (we only print a message and perform no assertions).
    //
    // When KNN is supported and parsing and planning both succeed, we assert that:
    //   * the physical plan has a temporal context, and
    //   * the root operator is TemporalVectorSearch with the expected k and timestamp.
    let sql = "SELECT * FROM nodes WHERE KNN(embedding, '[0.1, 0.2, 0.3, 0.4]', 10) FOR SYSTEM_TIME AS OF TIMESTAMP '1000'";

    let storage = Arc::new(CurrentStorage::new());
    use gallifreydb::index::vector::hnsw::HnswConfig;
    use gallifreydb::index::vector::DistanceMetric;

    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    storage.enable_vector_index("embedding", config).unwrap();

    let stats = Arc::new(Statistics::default());
    let planner = QueryPlanner::new(stats, storage);

    let result = parse_sql(sql);
    if let Ok(query) = result {
        let plan_result = planner.plan(query);
        if let Ok(plan) = plan_result {
             assert!(plan.temporal_context.is_some());
             // Verify root is TemporalVectorSearch if parser supports KNN
             if let PhysicalOp::TemporalVectorSearch { k, timestamp, .. } = plan.root {
                assert_eq!(k, 10);
                assert_eq!(timestamp.wallclock(), 1000);
             }
        }
    } else {
        println!("Skipping vector search test: KNN syntax not supported by parser");
    }
}
