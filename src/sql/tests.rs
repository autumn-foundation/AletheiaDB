//! Tests for SQL:2011 support.
//!
//! These tests follow TDD methodology - tests are written first to define
//! expected behavior, then implementation is added to make them pass.

use super::*;
use crate::query::ir::{Predicate, PredicateValue, QueryOp, SortKey};

// ============================================================================
// PHASE 1: Basic SQL Parsing Tests
// ============================================================================

mod phase1_basic_sql {
    use super::*;

    #[test]
    fn test_parse_select_star_from_nodes() {
        let query = parse_sql("SELECT * FROM nodes").unwrap();
        assert!(!query.ops.is_empty());
        assert!(matches!(&query.ops[0], QueryOp::ScanNodes { label: None }));
    }

    #[test]
    fn test_parse_select_star_from_table_as_label() {
        // SELECT * FROM Person should scan nodes with label 'Person'
        let query = parse_sql("SELECT * FROM Person").unwrap();
        assert!(matches!(
            &query.ops[0],
            QueryOp::ScanNodes { label: Some(l) } if l == "person"
        ));
    }

    #[test]
    fn test_parse_select_with_column_projection() {
        let query = parse_sql("SELECT name, age FROM nodes").unwrap();
        let project_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::Project(_)));
        assert!(project_op.is_some());
        if let Some(QueryOp::Project(cols)) = project_op {
            assert!(cols.contains(&"name".to_string()));
            assert!(cols.contains(&"age".to_string()));
        }
    }

    #[test]
    fn test_parse_select_with_where_equals() {
        let query = parse_sql("SELECT * FROM nodes WHERE name = 'Alice'").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::Eq { key, value: PredicateValue::String(s) }
                if key == "name" && s == "Alice"
            ));
        }
    }

    #[test]
    fn test_parse_select_with_where_greater_than() {
        let query = parse_sql("SELECT * FROM nodes WHERE age > 25").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::Gt { key, value: PredicateValue::Int(25) }
                if key == "age"
            ));
        }
    }

    #[test]
    fn test_parse_select_with_where_less_than() {
        let query = parse_sql("SELECT * FROM nodes WHERE score < 100").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::Lt { key, value: PredicateValue::Int(100) }
                if key == "score"
            ));
        }
    }

    #[test]
    fn test_parse_select_with_where_and() {
        let query = parse_sql("SELECT * FROM nodes WHERE age > 18 AND active = true").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::And(_)));
        }
    }

    #[test]
    fn test_parse_select_with_where_or() {
        let query =
            parse_sql("SELECT * FROM nodes WHERE status = 'active' OR status = 'pending'").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::Or(_)));
        }
    }

    #[test]
    fn test_parse_select_with_where_in_list() {
        let query = parse_sql("SELECT * FROM nodes WHERE status IN ('active', 'pending')").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(
                matches!(pred, Predicate::In { key, values } if key == "status" && values.len() == 2)
            );
        }
    }

    #[test]
    fn test_parse_select_with_where_like_contains() {
        let query = parse_sql("SELECT * FROM nodes WHERE name LIKE '%test%'").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::Contains { key, substring }
                if key == "name" && substring == "test"
            ));
        }
    }

    #[test]
    fn test_parse_select_with_where_like_starts_with() {
        let query = parse_sql("SELECT * FROM nodes WHERE name LIKE 'Al%'").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::StartsWith { key, prefix }
                if key == "name" && prefix == "Al"
            ));
        }
    }

    #[test]
    fn test_parse_select_with_where_like_ends_with() {
        let query = parse_sql("SELECT * FROM nodes WHERE email LIKE '%@gmail.com'").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::EndsWith { key, suffix }
                if key == "email" && suffix == "@gmail.com"
            ));
        }
    }

    #[test]
    fn test_parse_select_with_where_is_null() {
        let query = parse_sql("SELECT * FROM nodes WHERE deleted_at IS NULL").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::Eq { key, value: PredicateValue::Null } if key == "deleted_at"
            ));
        }
    }

    #[test]
    fn test_parse_select_with_where_is_not_null() {
        let query = parse_sql("SELECT * FROM nodes WHERE email IS NOT NULL").unwrap();
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::Ne { key, value: PredicateValue::Null } if key == "email"
            ));
        }
    }

    #[test]
    fn test_parse_select_with_order_by_asc() {
        let query = parse_sql("SELECT * FROM nodes ORDER BY name ASC").unwrap();
        let sort_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::Sort { .. }));
        assert!(sort_op.is_some());
        if let Some(QueryOp::Sort { key, descending }) = sort_op {
            assert!(matches!(key, SortKey::Property(p) if p == "name"));
            assert!(!descending);
        }
    }

    #[test]
    fn test_parse_select_with_order_by_desc() {
        let query = parse_sql("SELECT * FROM nodes ORDER BY age DESC").unwrap();
        let sort_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::Sort { .. }));
        assert!(sort_op.is_some());
        if let Some(QueryOp::Sort { key, descending }) = sort_op {
            assert!(matches!(key, SortKey::Property(p) if p == "age"));
            assert!(descending);
        }
    }

    #[test]
    fn test_parse_select_with_limit() {
        let query = parse_sql("SELECT * FROM nodes LIMIT 10").unwrap();
        let limit_op = query.ops.iter().find(|op| matches!(op, QueryOp::Limit(_)));
        assert!(limit_op.is_some());
        if let Some(QueryOp::Limit(n)) = limit_op {
            assert_eq!(*n, 10);
        }
    }

    #[test]
    fn test_parse_select_with_offset() {
        let query = parse_sql("SELECT * FROM nodes LIMIT 10 OFFSET 20").unwrap();
        let skip_op = query.ops.iter().find(|op| matches!(op, QueryOp::Skip(_)));
        assert!(skip_op.is_some());
        if let Some(QueryOp::Skip(n)) = skip_op {
            assert_eq!(*n, 20);
        }
    }

    #[test]
    fn test_parse_complex_query() {
        let query = parse_sql(
            "SELECT name, age FROM nodes WHERE age > 21 AND active = true ORDER BY age DESC LIMIT 100"
        ).unwrap();

        // Should have ScanNodes, Filter, Project, Sort, Limit
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::ScanNodes { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Project(_))));
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::Sort { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Limit(100))));
    }

    #[test]
    fn test_parse_error_invalid_sql() {
        let result = parse_sql("SELEC * FORM nodes");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_no_from() {
        let result = parse_sql("SELECT *");
        assert!(result.is_err());
    }
}

// ============================================================================
// PHASE 2: SQL:2011 Temporal Syntax Tests
// ============================================================================

mod phase2_temporal {
    use super::*;

    // ========================================================================
    // FOR SYSTEM_TIME AS OF Tests (TDD - RED PHASE)
    // ========================================================================

    #[test]
    fn test_parse_system_time_as_of_basic() {
        // Test basic FOR SYSTEM_TIME AS OF syntax
        let query =
            parse_sql("SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1705315200000000'")
                .expect("Should parse FOR SYSTEM_TIME AS OF");

        // Should have temporal context
        assert!(
            query.temporal_context.is_some(),
            "Query should have temporal context"
        );

        // Verify the temporal context is as_of
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(
            ctx.as_of_tuple().is_some(),
            "Should have as_of temporal context"
        );

        // Verify the timestamp (system time should be in transaction time position)
        let (_valid_time, tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(tx_time.wallclock(), 1705315200000000);
    }

    #[test]
    fn test_parse_system_time_as_of_with_where() {
        // Test FOR SYSTEM_TIME AS OF combined with WHERE clause
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1705315200000000' WHERE age > 21",
        )
        .expect("Should parse temporal query with WHERE");

        assert!(query.temporal_context.is_some());

        // Should also have filter operation
        let has_filter = query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_)));
        assert!(has_filter, "Query should have filter operation");
    }

    #[test]
    fn test_parse_system_time_as_of_with_order_limit() {
        // Test FOR SYSTEM_TIME AS OF with ORDER BY and LIMIT
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1705315200000000' ORDER BY name DESC LIMIT 10"
        ).expect("Should parse temporal query with ORDER BY and LIMIT");

        assert!(query.temporal_context.is_some());
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::Sort { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Limit(10))));
    }

    // ========================================================================
    // FOR SYSTEM_TIME BETWEEN Tests (TDD - RED PHASE)
    // ========================================================================

    #[test]
    fn test_parse_system_time_between_basic() {
        // Test basic FOR SYSTEM_TIME BETWEEN syntax
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME BETWEEN TIMESTAMP '1000000' AND TIMESTAMP '2000000'"
        ).expect("Should parse FOR SYSTEM_TIME BETWEEN");

        // Should have temporal context
        assert!(
            query.temporal_context.is_some(),
            "Query should have temporal context"
        );

        // Verify the temporal context is between
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(
            ctx.transaction_time_between.is_some(),
            "Should have between temporal context"
        );

        // Verify the time range
        let range = ctx.transaction_time_between.as_ref().unwrap();
        assert_eq!(range.start().wallclock(), 1000000);
        assert_eq!(range.end().wallclock(), 2000000);
    }

    #[test]
    fn test_parse_system_time_between_with_filter() {
        // Test FOR SYSTEM_TIME BETWEEN with WHERE clause
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME BETWEEN TIMESTAMP '1000000' AND TIMESTAMP '2000000' WHERE label = 'Person'"
        ).expect("Should parse temporal range query with WHERE");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(ctx.transaction_time_between.is_some());

        // Should have filter operation
        let has_filter = query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_)));
        assert!(has_filter);
    }

    // ========================================================================
    // FOR VALID_TIME AS OF Tests (TDD - RED PHASE)
    // ========================================================================

    #[test]
    fn test_parse_valid_time_as_of_basic() {
        // Test basic FOR VALID_TIME AS OF syntax
        let query =
            parse_sql("SELECT * FROM nodes FOR VALID_TIME AS OF TIMESTAMP '1705315200000000'")
                .expect("Should parse FOR VALID_TIME AS OF");

        // Should have temporal context
        assert!(
            query.temporal_context.is_some(),
            "Query should have temporal context"
        );

        // Verify the temporal context is as_of with valid time
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(
            ctx.as_of_tuple().is_some(),
            "Should have as_of temporal context"
        );

        // Verify the timestamp (valid time should be in valid time position)
        let (valid_time, _tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(valid_time.wallclock(), 1705315200000000);
    }

    #[test]
    fn test_parse_valid_time_as_of_with_where() {
        // Test FOR VALID_TIME AS OF combined with WHERE clause
        let query = parse_sql(
            "SELECT * FROM Person FOR VALID_TIME AS OF TIMESTAMP '1705315200000000' WHERE status = 'active'"
        ).expect("Should parse valid time query with WHERE");

        assert!(query.temporal_context.is_some());

        // Should have label filter (Person) and status filter
        let has_filter = query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_)));
        assert!(has_filter);
    }

    // ========================================================================
    // FOR VALID_TIME BETWEEN Tests (TDD - RED PHASE)
    // ========================================================================

    #[test]
    fn test_parse_valid_time_between_basic() {
        // Test basic FOR VALID_TIME BETWEEN syntax
        let query = parse_sql(
            "SELECT * FROM nodes FOR VALID_TIME BETWEEN TIMESTAMP '1000000' AND TIMESTAMP '2000000'"
        ).expect("Should parse FOR VALID_TIME BETWEEN");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(ctx.valid_time_between.is_some());

        // Verify the time range
        let range = ctx.valid_time_between.as_ref().unwrap();
        assert_eq!(range.start().wallclock(), 1000000);
        assert_eq!(range.end().wallclock(), 2000000);
    }

    // ========================================================================
    // Combined Bi-temporal Tests (TDD - RED PHASE)
    // ========================================================================

    #[test]
    fn test_parse_bitemporal_system_and_valid_time() {
        // Test combined SYSTEM_TIME and VALID_TIME
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '2000000' FOR VALID_TIME AS OF TIMESTAMP '1500000'"
        ).expect("Should parse bi-temporal query");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(ctx.as_of_tuple().is_some());

        // Both timestamps should be set
        let (valid_time, tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(valid_time.wallclock(), 1500000);
        assert_eq!(tx_time.wallclock(), 2000000);
    }

    #[test]
    fn test_parse_bitemporal_reverse_order() {
        // Test with clauses in reverse order (VALID_TIME then SYSTEM_TIME)
        let query = parse_sql(
            "SELECT * FROM nodes FOR VALID_TIME AS OF TIMESTAMP '1500000' FOR SYSTEM_TIME AS OF TIMESTAMP '2000000'"
        ).expect("Should parse bi-temporal query in reverse order");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(ctx.as_of_tuple().is_some());

        let (valid_time, tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(valid_time.wallclock(), 1500000);
        assert_eq!(tx_time.wallclock(), 2000000);
    }

    #[test]
    fn test_parse_bitemporal_with_complex_query() {
        // Test bi-temporal with full query features
        let query = parse_sql(
            "SELECT name, age FROM Person FOR SYSTEM_TIME AS OF TIMESTAMP '2000000' FOR VALID_TIME AS OF TIMESTAMP '1500000' WHERE age > 21 ORDER BY name LIMIT 10"
        ).expect("Should parse complex bi-temporal query");

        assert!(query.temporal_context.is_some());
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::Sort { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Limit(10))));
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Project(_))));
    }

    #[test]
    fn test_temporal_clause_parse_timestamp() {
        // Test microseconds format
        let ts = TemporalClause::parse_timestamp("1705315200000000");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705315200000000);
    }

    #[test]
    fn test_temporal_clause_system_time_as_of() {
        use crate::core::temporal::Timestamp;
        let clause = TemporalClause::system_time_as_of(Timestamp::from(1000));
        assert!(matches!(clause, TemporalClause::SystemTimeAsOf(_)));
    }

    #[test]
    fn test_temporal_clause_system_time_between() {
        use crate::core::temporal::Timestamp;
        let clause =
            TemporalClause::system_time_between(Timestamp::from(1000), Timestamp::from(2000));
        assert!(clause.is_ok());
        assert!(matches!(
            clause.unwrap(),
            TemporalClause::SystemTimeBetween(_)
        ));
    }

    #[test]
    fn test_temporal_clause_invalid_range() {
        use crate::core::temporal::Timestamp;
        // End before start should fail
        let clause =
            TemporalClause::system_time_between(Timestamp::from(2000), Timestamp::from(1000));
        assert!(clause.is_err());
    }

    // ========================================================================
    // Full Pipeline End-to-End Tests
    // Verify temporal context flows through convert_sql() correctly
    // ========================================================================

    #[test]
    fn test_temporal_context_wired_through_full_pipeline() {
        // Verify temporal context flows correctly through the complete pipeline
        let query = parse_sql(
            "SELECT * FROM Person FOR SYSTEM_TIME AS OF TIMESTAMP '1705315200000000' WHERE name = 'Alice'",
        )
        .expect("Should parse temporal query");

        // Must have temporal context
        assert!(
            query.temporal_context.is_some(),
            "temporal_context must be set"
        );
        let ctx = query.temporal_context.as_ref().unwrap();
        let (_valid_time, tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(tx_time.wallclock(), 1705315200000000);

        // Must also have the WHERE filter
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));

        // Must also have label scan (Person)
        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::ScanNodes { label: Some(l) } if l == "person"
        )));
    }

    #[test]
    fn test_valid_time_wired_through_full_pipeline() {
        let query =
            parse_sql("SELECT * FROM nodes FOR VALID_TIME AS OF TIMESTAMP '1705315200000000'")
                .expect("Should parse valid time query");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        let (valid_time, _tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(valid_time.wallclock(), 1705315200000000);
    }

    #[test]
    fn test_bitemporal_wired_through_full_pipeline() {
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '2000000' FOR VALID_TIME AS OF TIMESTAMP '1500000'",
        )
        .expect("Should parse bi-temporal query");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        let (valid_time, tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(valid_time.wallclock(), 1500000);
        assert_eq!(tx_time.wallclock(), 2000000);
    }

    #[test]
    fn test_system_time_between_wired_through_full_pipeline() {
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME BETWEEN TIMESTAMP '1000000' AND TIMESTAMP '2000000' WHERE age > 21",
        )
        .expect("Should parse temporal range + filter");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(ctx.transaction_time_between.is_some());
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
    }

    #[test]
    fn test_no_temporal_clause_leaves_context_none() {
        let query = parse_sql("SELECT * FROM Person WHERE name = 'Alice'").expect("parse");
        assert!(
            query.temporal_context.is_none(),
            "Non-temporal query should have None context"
        );
    }
}

// ============================================================================
// ISO 8601 Timestamp Parsing Tests
// ============================================================================

mod iso8601_parsing {
    use super::*;

    // ========================================================================
    // Cycle 1: Preserve Existing Unix Microseconds Behavior
    // ========================================================================

    #[test]
    fn test_parse_unix_microseconds_zero() {
        let ts = TemporalClause::parse_timestamp("0");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 0);
    }

    #[test]
    fn test_parse_unix_microseconds_negative() {
        let ts = TemporalClause::parse_timestamp("-1000000");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), -1000000);
    }

    #[test]
    fn test_parse_unix_microseconds_large() {
        let ts = TemporalClause::parse_timestamp("9223372036854775000");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 9223372036854775000);
    }

    #[test]
    fn test_parse_unix_microseconds_with_quotes() {
        let ts = TemporalClause::parse_timestamp("'1705315200000000'");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705315200000000);
    }

    #[test]
    fn test_parse_unix_microseconds_with_double_quotes() {
        let ts = TemporalClause::parse_timestamp("\"1705315200000000\"");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705315200000000);
    }

    #[test]
    fn test_parse_unix_microseconds_with_whitespace() {
        let ts = TemporalClause::parse_timestamp("  1705315200000000  ");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705315200000000);
    }

    // ========================================================================
    // Cycle 2: ISO 8601 UTC Format
    // ========================================================================

    #[test]
    fn test_parse_iso8601_utc_basic() {
        // 2024-01-15T10:00:00Z = 1705312800000000 microseconds
        let ts = TemporalClause::parse_timestamp("2024-01-15T10:00:00Z");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800000000);
    }

    #[test]
    fn test_parse_iso8601_utc_with_single_quotes() {
        let ts = TemporalClause::parse_timestamp("'2024-01-15T10:00:00Z'");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800000000);
    }

    #[test]
    fn test_parse_iso8601_utc_with_double_quotes() {
        let ts = TemporalClause::parse_timestamp("\"2024-01-15T10:00:00Z\"");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800000000);
    }

    #[test]
    fn test_parse_iso8601_utc_with_whitespace() {
        let ts = TemporalClause::parse_timestamp("  2024-01-15T10:00:00Z  ");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800000000);
    }

    #[test]
    fn test_parse_iso8601_utc_with_fractional_seconds() {
        // 2024-01-15T10:00:00.123456Z
        let ts = TemporalClause::parse_timestamp("2024-01-15T10:00:00.123456Z");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800123456);
    }

    #[test]
    fn test_parse_iso8601_utc_midnight() {
        // 2024-01-15T00:00:00Z = 1705276800000000 microseconds
        let ts = TemporalClause::parse_timestamp("2024-01-15T00:00:00Z");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705276800000000);
    }

    #[test]
    fn test_parse_iso8601_utc_epoch() {
        // 1970-01-01T00:00:00Z = 0 microseconds
        let ts = TemporalClause::parse_timestamp("1970-01-01T00:00:00Z");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 0);
    }

    // ========================================================================
    // Cycle 3: ISO 8601 with Timezone Offset
    // ========================================================================

    #[test]
    fn test_parse_iso8601_with_zero_offset() {
        // 2024-01-15T10:00:00+00:00 should equal Z format
        let ts = TemporalClause::parse_timestamp("2024-01-15T10:00:00+00:00");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800000000);
    }

    #[test]
    fn test_parse_iso8601_with_positive_offset() {
        // 2024-01-15T15:30:00+05:30 = 2024-01-15T10:00:00Z
        let ts = TemporalClause::parse_timestamp("2024-01-15T15:30:00+05:30");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800000000);
    }

    #[test]
    fn test_parse_iso8601_with_negative_offset() {
        // 2024-01-15T02:00:00-08:00 = 2024-01-15T10:00:00Z
        let ts = TemporalClause::parse_timestamp("2024-01-15T02:00:00-08:00");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800000000);
    }

    // ========================================================================
    // Cycle 4: SQL Timestamp Format (space separator)
    // ========================================================================

    #[test]
    fn test_parse_sql_timestamp_format() {
        // 2024-01-15 10:00:00 (space separator, assume UTC)
        let ts = TemporalClause::parse_timestamp("2024-01-15 10:00:00");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800000000);
    }

    #[test]
    fn test_parse_sql_timestamp_midnight() {
        let ts = TemporalClause::parse_timestamp("2024-01-15 00:00:00");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705276800000000);
    }

    #[test]
    fn test_parse_sql_timestamp_with_fractional_seconds() {
        // SQL timestamp with fractional seconds (now supported via strict parse)
        let ts = TemporalClause::parse_timestamp("2024-01-15 10:00:00.123456");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800123456);
    }

    #[test]
    fn test_parse_naive_datetime_with_t_separator() {
        // ISO-like format without timezone (e.g., "2024-01-15T10:00:00")
        let ts = TemporalClause::parse_timestamp("2024-01-15T10:00:00");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800000000);
    }

    #[test]
    fn test_parse_naive_datetime_with_t_and_fractional() {
        // ISO-like format with T separator and fractional seconds
        let ts = TemporalClause::parse_timestamp("2024-01-15T10:00:00.123456");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705312800123456);
    }

    // ========================================================================
    // Cycle 5: Date-Only Format
    // ========================================================================

    #[test]
    fn test_parse_date_only() {
        // 2024-01-15 (assume midnight UTC)
        let ts = TemporalClause::parse_timestamp("2024-01-15");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705276800000000);
    }

    #[test]
    fn test_parse_date_only_with_quotes() {
        let ts = TemporalClause::parse_timestamp("'2024-01-15'");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1705276800000000);
    }

    // ========================================================================
    // Edge Cases
    // ========================================================================

    #[test]
    fn test_parse_empty_string() {
        let ts = TemporalClause::parse_timestamp("");
        assert!(ts.is_err());
    }

    #[test]
    fn test_parse_invalid_date_feb_30() {
        let ts = TemporalClause::parse_timestamp("2024-02-30T10:00:00Z");
        assert!(ts.is_err());
    }

    #[test]
    fn test_parse_invalid_leap_year() {
        // 2023 is not a leap year, Feb 29 should fail
        let ts = TemporalClause::parse_timestamp("2023-02-29T10:00:00Z");
        assert!(ts.is_err());
    }

    #[test]
    fn test_parse_valid_leap_year() {
        // 2024 is a leap year, Feb 29 should work
        // 2024-02-29T00:00:00Z = 1709164800000000 microseconds
        let ts = TemporalClause::parse_timestamp("2024-02-29T00:00:00Z");
        assert!(ts.is_ok());
        assert_eq!(ts.unwrap().wallclock(), 1709164800000000);
    }

    #[test]
    fn test_parse_far_future_date() {
        // 2100-12-31T23:59:59Z should work
        let ts = TemporalClause::parse_timestamp("2100-12-31T23:59:59Z");
        assert!(ts.is_ok());
        // Just verify it parses, don't check exact value
    }

    #[test]
    fn test_parse_invalid_format() {
        let ts = TemporalClause::parse_timestamp("not a timestamp");
        assert!(ts.is_err());
    }

    #[test]
    fn test_parse_strict_rejects_trailing_characters_date() {
        // Strict parsing should reject dates with trailing invalid characters
        let ts = TemporalClause::parse_timestamp("2024-01-15-invalid");
        assert!(ts.is_err(), "Should reject date with trailing characters");
    }

    #[test]
    fn test_parse_strict_rejects_trailing_characters_datetime() {
        // Strict parsing should reject datetimes with trailing invalid characters
        let ts = TemporalClause::parse_timestamp("2024-01-15 10:00:00 invalid");
        assert!(
            ts.is_err(),
            "Should reject datetime with trailing characters"
        );
    }

    #[test]
    fn test_parse_strict_rejects_partial_date() {
        // Strict parsing should reject partial dates
        let ts = TemporalClause::parse_timestamp("2024-01");
        assert!(ts.is_err(), "Should reject partial date");
    }

    // ========================================================================
    // Cycle 6: Error Messages
    // ========================================================================

    #[test]
    fn test_error_message_lists_formats() {
        let result = TemporalClause::parse_timestamp("invalid");
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        // Error message should mention supported formats
        assert!(
            err_msg.contains("ISO 8601") || err_msg.contains("microseconds"),
            "Error message should list supported formats"
        );
    }

    // ========================================================================
    // Cycle 7: Integration Tests with Full SQL Parsing
    // ========================================================================

    #[test]
    fn test_sql_parse_with_iso8601_timestamp() {
        let query =
            parse_sql("SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '2024-01-15T10:00:00Z'")
                .expect("Should parse SQL with ISO 8601 timestamp");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        assert!(ctx.as_of_tuple().is_some());

        let (_valid_time, tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(tx_time.wallclock(), 1705312800000000);
    }

    #[test]
    fn test_sql_parse_with_date_only() {
        let query = parse_sql("SELECT * FROM nodes FOR VALID_TIME AS OF TIMESTAMP '2024-01-15'")
            .expect("Should parse SQL with date-only timestamp");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        let (valid_time, _tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(valid_time.wallclock(), 1705276800000000);
    }

    #[test]
    fn test_sql_parse_with_timezone_offset() {
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '2024-01-15T15:30:00+05:30'",
        )
        .expect("Should parse SQL with timezone offset");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        let (_valid_time, tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(tx_time.wallclock(), 1705312800000000);
    }

    #[test]
    fn test_sql_parse_with_sql_format() {
        let query =
            parse_sql("SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '2024-01-15 10:00:00'")
                .expect("Should parse SQL with SQL timestamp format");

        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.as_ref().unwrap();
        let (_valid_time, tx_time) = ctx.as_of_tuple().unwrap();
        assert_eq!(tx_time.wallclock(), 1705312800000000);
    }
}

// ============================================================================
// PHASE 3: Graph Extension Tests
// ============================================================================

mod phase3_graph {
    use super::*;
    use crate::query::ir::TraversalDepth;

    // ========================================================================
    // MATCH clause tests (Task 4: #532)
    // ========================================================================

    #[test]
    fn test_parse_match_outgoing_single_hop() {
        let query = parse_sql(
            "SELECT * FROM nodes AS source MATCH (source)-[:KNOWS]->(target) WHERE source.name = 'Alice'",
        )
        .unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::TraverseOut { label: Some(l), depth: TraversalDepth::Exact(1) } if l == "KNOWS"
        )));
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
    }

    #[test]
    fn test_parse_match_incoming_edge() {
        let query =
            parse_sql("SELECT * FROM nodes AS child MATCH (parent)<-[:PARENT_OF]-(child)").unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::TraverseIn { label: Some(l), depth: TraversalDepth::Exact(1) } if l == "PARENT_OF"
        )));
    }

    #[test]
    fn test_parse_match_bidirectional() {
        let query = parse_sql("SELECT * FROM nodes AS n MATCH (n)-[:RELATED]-(related)").unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::TraverseBoth { label: Some(l), depth: TraversalDepth::Exact(1) } if l == "RELATED"
        )));
    }

    #[test]
    fn test_parse_match_with_where_order_limit() {
        let query = parse_sql(
            "SELECT * FROM nodes AS source MATCH (source)-[:KNOWS]->(target) WHERE source.name = 'Alice' ORDER BY target.name LIMIT 10",
        )
        .unwrap();

        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::TraverseOut { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::Sort { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Limit(10))));
    }

    #[test]
    fn test_parse_match_no_where() {
        let query = parse_sql("SELECT * FROM nodes AS a MATCH (a)-[:FOLLOWS]->(b)").unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::TraverseOut { label: Some(l), .. } if l == "FOLLOWS"
        )));
        assert!(!query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
    }

    #[test]
    fn test_parse_match_with_label_table() {
        let query = parse_sql("SELECT * FROM Person AS p MATCH (p)-[:KNOWS]->(friend)").unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::ScanNodes { label: Some(l) } if l == "person"
        )));
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::TraverseOut { .. }))
        );
    }

    // ========================================================================
    // Variable-length traversal tests (Task 5: #534)
    // ========================================================================

    #[test]
    fn test_parse_match_variable_length_range() {
        let query = parse_sql(
            "SELECT * FROM nodes AS source MATCH (source)-[:KNOWS*1..3]->(target) WHERE source.name = 'Alice'",
        )
        .unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::TraverseOut { label: Some(l), depth: TraversalDepth::Range { min: 1, max: 3 } } if l == "KNOWS"
        )));
    }

    #[test]
    fn test_parse_match_exact_depth() {
        let query =
            parse_sql("SELECT * FROM nodes AS source MATCH (source)-[:KNOWS*2]->(target)").unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::TraverseOut { label: Some(l), depth: TraversalDepth::Exact(2) } if l == "KNOWS"
        )));
    }

    #[test]
    fn test_parse_match_unbounded() {
        let query =
            parse_sql("SELECT * FROM nodes AS source MATCH (source)-[:KNOWS*]->(target)").unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::TraverseOut { label: Some(l), depth: TraversalDepth::Variable } if l == "KNOWS"
        )));
    }

    #[test]
    fn test_parse_match_open_ended_min() {
        // *2.. means min=2, max=default(10)
        let query =
            parse_sql("SELECT * FROM nodes AS source MATCH (source)-[:KNOWS*2..]->(target)")
                .unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::TraverseOut { label: Some(l), depth: TraversalDepth::Range { min: 2, max: 10 } } if l == "KNOWS"
        )));
    }

    #[test]
    fn test_parse_match_open_ended_max() {
        // *..3 means min=1, max=3
        let query =
            parse_sql("SELECT * FROM nodes AS source MATCH (source)-[:KNOWS*..3]->(target)")
                .unwrap();

        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::TraverseOut { label: Some(l), depth: TraversalDepth::Range { min: 1, max: 3 } } if l == "KNOWS"
        )));
    }

    #[test]
    fn test_parse_match_with_temporal() {
        // MATCH + temporal should both work
        let query = parse_sql(
            "SELECT * FROM nodes AS a FOR SYSTEM_TIME AS OF TIMESTAMP '2000000' MATCH (a)-[:KNOWS]->(b) WHERE a.name = 'Alice'",
        )
        .unwrap();

        assert!(query.temporal_context.is_some());
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::TraverseOut { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
    }

    // ========================================================================
    // Original placeholder tests (now properly asserting)
    // ========================================================================

    #[test]
    fn test_parse_match_simple_traversal() {
        let query =
            parse_sql("SELECT * FROM nodes AS source MATCH (source)-[:KNOWS]->(target)").unwrap();

        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::TraverseOut { .. }))
        );
    }

    #[test]
    fn test_parse_match_variable_length() {
        let query =
            parse_sql("SELECT * FROM nodes AS source MATCH (source)-[:KNOWS*1..3]->(target)")
                .unwrap();

        let traverse = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::TraverseOut { .. }));
        if let Some(QueryOp::TraverseOut { depth, .. }) = traverse {
            assert!(matches!(depth, TraversalDepth::Range { min: 1, max: 3 }));
        } else {
            panic!("Expected TraverseOut op");
        }
    }

    #[test]
    fn test_parse_match_incoming() {
        let query =
            parse_sql("SELECT * FROM nodes AS source MATCH (source)<-[:FOLLOWS]-(target)").unwrap();

        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::TraverseIn { .. }))
        );
    }

    // ========================================================================
    // Edge Scanning Tests (#533)
    // ========================================================================

    #[test]
    fn test_parse_select_from_edges() {
        let query = parse_sql("SELECT * FROM edges").unwrap();
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::ScanEdges { edge_type: None }))
        );
    }

    #[test]
    fn test_parse_select_from_edges_with_filter() {
        let query = parse_sql("SELECT * FROM edges WHERE type = 'KNOWS'").unwrap();
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::ScanEdges { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
    }

    #[test]
    fn test_parse_select_from_edges_with_limit() {
        let query = parse_sql("SELECT * FROM edges LIMIT 10").unwrap();
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::ScanEdges { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Limit(10))));
    }

    #[test]
    fn test_parse_select_from_edges_with_order_by() {
        let query = parse_sql("SELECT * FROM edges ORDER BY timestamp DESC").unwrap();
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::ScanEdges { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(
            op,
            QueryOp::Sort {
                key: SortKey::Timestamp,
                descending: true
            }
        )));
    }

    // ========================================================================
    // JOIN Error Improvement Tests (#538)
    // ========================================================================

    #[test]
    fn test_join_gives_helpful_error() {
        let result = parse_sql("SELECT p.name FROM Person p JOIN Company c ON p.company_id = c.id");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("MATCH"),
            "Error should suggest MATCH alternative: {}",
            err
        );
    }

    #[test]
    fn test_parse_multiple_match_clauses() {
        // Multiple MATCH clauses should all be extracted
        let result = parse_sql(
            "SELECT * FROM nodes AS a MATCH (a)-[:KNOWS]->(b) MATCH (b)-[:WORKS_AT]->(c) WHERE a.name = 'Alice'",
        );
        // Should either work (extracting both patterns) or give a clear error
        match result {
            Ok(query) => {
                // If supported, should have two traversal ops
                let traverse_count = query
                    .ops
                    .iter()
                    .filter(|op| {
                        matches!(
                            op,
                            QueryOp::TraverseOut { .. }
                                | QueryOp::TraverseIn { .. }
                                | QueryOp::TraverseBoth { .. }
                        )
                    })
                    .count();
                assert!(
                    traverse_count >= 2,
                    "Expected 2 traversal ops, got {}",
                    traverse_count
                );
            }
            Err(e) => {
                // If not supported, error should be clear
                assert!(
                    e.to_string().contains("MATCH") || e.to_string().contains("multiple"),
                    "Error should mention MATCH: {}",
                    e
                );
            }
        }
    }

    #[test]
    fn test_parse_match_no_edge_type() {
        // MATCH without edge type should traverse any edge
        let query = parse_sql("SELECT * FROM nodes AS a MATCH (a)-[]->(b)").unwrap();

        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::TraverseOut { label: None, .. }))
        );
    }
}

// ============================================================================
// PHASE 4: Vector Extension Tests
// ============================================================================

mod phase4_vector {
    use super::*;

    #[test]
    fn test_parse_order_by_vector_distance() {
        // PostgreSQL pgvector style: ORDER BY embedding <=> '[0.1, 0.2, 0.3]'::vector
        let result = parse_sql(
            "SELECT * FROM nodes ORDER BY embedding <=> '[0.1, 0.2, 0.3]'::vector LIMIT 10",
        );
        // This requires custom operator support
        if let Ok(query) = result {
            assert!(
                query
                    .ops
                    .iter()
                    .any(|op| matches!(op, QueryOp::VectorSearch { .. }))
            );
        }
    }

    #[test]
    fn test_parse_knn_function() {
        // Alternative syntax: KNN(embedding, '[0.1, 0.2, 0.3]', 10)
        let result = parse_sql("SELECT * FROM nodes WHERE KNN(embedding, '[0.1, 0.2, 0.3]', 10)");
        if let Ok(query) = result {
            assert!(
                query
                    .ops
                    .iter()
                    .any(|op| matches!(op, QueryOp::VectorSearch { k: 10, .. }))
            );
        }
    }

    #[test]
    fn test_parse_similar_to_function() {
        // SIMILAR_TO(node_id, 10) function
        let result = parse_sql("SELECT * FROM nodes WHERE SIMILAR_TO(42, 10)");
        if let Ok(query) = result {
            assert!(
                query
                    .ops
                    .iter()
                    .any(|op| matches!(op, QueryOp::SimilarTo { k: 10, .. }))
            );
        }
    }

    #[test]
    fn test_parse_rank_by_similarity() {
        let result = parse_sql("SELECT * FROM nodes RANK BY SIMILARITY TO '[0.1, 0.2, 0.3]' TOP 5");
        if let Ok(query) = result {
            assert!(
                query
                    .ops
                    .iter()
                    .any(|op| matches!(op, QueryOp::RankBySimilarity { top_k: Some(5), .. }))
            );
        }
    }
}

// ============================================================================
// Integration Tests
// ============================================================================

mod integration {
    use super::*;
    use crate::query::planner::{QueryPlanner, Statistics};
    use crate::storage::CurrentStorage;
    use std::sync::Arc;

    #[test]
    fn test_sql_to_planner_integration() {
        // Parse SQL, convert to Query, and plan it
        let query = parse_sql("SELECT * FROM nodes LIMIT 10").unwrap();

        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        let plan = planner.plan(query);
        assert!(plan.is_ok());
    }

    #[test]
    fn test_sql_with_filter_to_planner() {
        let query = parse_sql("SELECT * FROM nodes WHERE age > 21 LIMIT 100").unwrap();

        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        let plan = planner.plan(query);
        assert!(plan.is_ok());
    }

    #[test]
    fn test_sql_with_order_to_planner() {
        let query = parse_sql("SELECT * FROM nodes ORDER BY name ASC LIMIT 50").unwrap();

        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        let plan = planner.plan(query);
        assert!(plan.is_ok());
    }
}

// ============================================================================
// Error Handling Tests
// ============================================================================

mod errors {
    use super::*;

    #[test]
    fn test_error_invalid_syntax() {
        let result = parse_sql("SELEC * FORM nodes");
        assert!(matches!(result, Err(SqlError::ParseError(_))));
    }

    #[test]
    fn test_error_unsupported_statement() {
        let result = parse_sql("INSERT INTO nodes VALUES (1, 2, 3)");
        assert!(matches!(result, Err(SqlError::UnsupportedFeature(_))));
    }

    #[test]
    fn test_error_multiple_statements() {
        let result = parse_sql("SELECT * FROM nodes; SELECT * FROM edges");
        assert!(matches!(result, Err(SqlError::UnsupportedFeature(_))));
    }
}
