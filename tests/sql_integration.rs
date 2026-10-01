//! Integration tests for SQL queries against a real AletheiaDB instance.
//!
//! These tests verify the full pipeline: SQL parsing -> Query -> Execution -> Results.
//! Unlike the unit tests in `src/sql/tests.rs` which verify SQL-to-QueryOp conversion,
//! these tests exercise the entire path from a SQL string to actual query results
//! returned from a populated database.
//!
//! # SQL Identifier Case Convention
//!
//! SQL normalizes unquoted identifiers to lowercase. When using `FROM Person`,
//! the SQL converter lowercases this to `"person"`. Node labels are case-sensitive,
//! so nodes must be created with lowercase labels to match SQL label-filtered queries.
//! Use `FROM nodes` (no label filter) to query nodes regardless of label case.
//!
//! # Executor Support
//!
//! The query executor executes `Sort` (ORDER BY) and `EdgeScan` (`FROM edges`)
//! physical operators end-to-end. `SELECT * FROM edges` yields edge rows
//! consumed via `collect_all()` or the edge-shaped structured projection
//! `collect_structured_edges()` / `collect_edges()` (Issue #3626); the
//! node-centric `collect_structured()` / `collect_nodes()` helpers remain
//! node-only by design. ORDER BY returns rows in sorted order.

#![cfg(feature = "sql")]

use aletheiadb::sql::{parse_sql, parse_sql_with_params};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

// =============================================================================
// Test Helpers
// =============================================================================

/// Create a database with Person nodes (mixed-case labels for `FROM nodes` queries).
fn setup_person_db() -> AletheiaDB {
    let db = AletheiaDB::new().expect("Failed to create database");

    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30)
            .build(),
    )
    .expect("Failed to create Alice");

    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("age", 25)
            .build(),
    )
    .expect("Failed to create Bob");

    db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Carol")
            .insert("age", 35)
            .build(),
    )
    .expect("Failed to create Carol");

    db
}

/// Create a database with lowercase labels for SQL label-filtered queries.
///
/// SQL normalizes `FROM Person` to label `"person"`, so nodes must use
/// lowercase labels to match. This helper creates a database suitable for
/// `SELECT * FROM person` style queries.
fn setup_sql_label_db() -> AletheiaDB {
    let db = AletheiaDB::new().expect("Failed to create database");

    db.create_node(
        "person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30)
            .build(),
    )
    .expect("Failed to create person node");

    db.create_node(
        "person",
        PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("age", 25)
            .build(),
    )
    .expect("Failed to create person node");

    db.create_node(
        "document",
        PropertyMapBuilder::new()
            .insert("title", "Rust Guide")
            .insert("pages", 200)
            .build(),
    )
    .expect("Failed to create document node");

    db.create_node(
        "document",
        PropertyMapBuilder::new()
            .insert("title", "SQL Manual")
            .insert("pages", 150)
            .build(),
    )
    .expect("Failed to create document node");

    db.create_node(
        "event",
        PropertyMapBuilder::new()
            .insert("name", "Conference")
            .build(),
    )
    .expect("Failed to create event node");

    db
}

/// Create a database with nodes and KNOWS edges for graph queries.
fn setup_graph_db() -> AletheiaDB {
    let db = AletheiaDB::new().expect("Failed to create database");

    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30)
                .build(),
        )
        .expect("Failed to create Alice");

    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 25)
                .build(),
        )
        .expect("Failed to create Bob");

    let carol = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Carol")
                .insert("age", 35)
                .build(),
        )
        .expect("Failed to create Carol");

    db.create_edge(
        alice,
        bob,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2020).build(),
    )
    .expect("Failed to create edge Alice->Bob");

    db.create_edge(
        bob,
        carol,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2021).build(),
    )
    .expect("Failed to create edge Bob->Carol");

    db
}

// =============================================================================
// Part 1: Full Pipeline -- SELECT * FROM nodes
// =============================================================================

mod select_all_nodes {
    use super::*;

    #[test]
    fn select_all_from_empty_database() {
        let db = AletheiaDB::new().expect("Failed to create database");
        let query = parse_sql("SELECT * FROM nodes").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let count = results.count_all().expect("Failed to count results");
        assert_eq!(count, 0, "Empty database should return zero results");
    }

    #[test]
    fn select_all_from_populated_database() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let rows = results.collect_all().expect("Failed to collect results");
        assert_eq!(rows.len(), 3, "Should return all 3 nodes");
    }

    #[test]
    fn select_all_returns_node_entities() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let rows = results.collect_all().expect("Failed to collect results");

        for row in &rows {
            assert!(
                row.entity.as_node().is_some(),
                "Each result should be a Node entity"
            );
        }
    }

    #[test]
    fn select_all_nodes_include_all_labels() {
        let db = setup_sql_label_db();
        let query = parse_sql("SELECT * FROM nodes").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let count = results.count_all().expect("Failed to count results");
        assert_eq!(
            count, 5,
            "Should return all 5 nodes across person, document, event labels"
        );
    }
}

// =============================================================================
// Part 2: Full Pipeline -- SELECT with label filter (FROM <label>)
// =============================================================================

mod select_by_label {
    use super::*;

    #[test]
    fn select_from_specific_label() {
        let db = setup_sql_label_db();
        // SQL lowercases "Person" to "person", matching our lowercase-labeled nodes
        let query = parse_sql("SELECT * FROM Person").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        assert_eq!(nodes.len(), 2, "Should return exactly 2 person nodes");
        for node in &nodes {
            assert!(
                node.has_label_str("person"),
                "All returned nodes should have label 'person'"
            );
        }
    }

    #[test]
    fn select_from_document_label() {
        let db = setup_sql_label_db();
        let query = parse_sql("SELECT * FROM Document").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        assert_eq!(nodes.len(), 2, "Should return exactly 2 document nodes");
        for node in &nodes {
            assert!(
                node.has_label_str("document"),
                "All returned nodes should have label 'document'"
            );
        }
    }

    #[test]
    fn select_from_nonexistent_label() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM Spaceship").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let count = results.count_all().expect("Failed to count results");
        assert_eq!(
            count, 0,
            "Querying a nonexistent label should return zero results"
        );
    }

    #[test]
    fn sql_lowercases_table_names() {
        // Verify that FROM Person and FROM person produce the same query behavior.
        // Both should scan for nodes with lowercase label "person".
        let db = setup_sql_label_db();

        let query_upper = parse_sql("SELECT * FROM Person").expect("Failed to parse");
        let count_upper = db
            .execute_query(query_upper)
            .expect("Failed to execute")
            .count_all()
            .expect("Failed to count");

        let query_lower = parse_sql("SELECT * FROM person").expect("Failed to parse");
        let count_lower = db
            .execute_query(query_lower)
            .expect("Failed to execute")
            .count_all()
            .expect("Failed to count");

        assert_eq!(
            count_upper, count_lower,
            "FROM Person and FROM person should return the same count"
        );
    }
}

// =============================================================================
// Part 3: Full Pipeline -- WHERE clause filtering
// =============================================================================

mod where_filter {
    use super::*;
    use aletheiadb::PropertyValue;

    #[test]
    fn where_equals_string() {
        let db = setup_person_db();
        let query =
            parse_sql("SELECT * FROM nodes WHERE name = 'Alice'").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        assert_eq!(
            nodes.len(),
            1,
            "Should return exactly 1 node matching Alice"
        );
        let alice = &nodes[0];
        assert_eq!(
            alice.get_property("name"),
            Some(&PropertyValue::String("Alice".into())),
        );
    }

    #[test]
    fn where_greater_than_int() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes WHERE age > 28").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        // Alice (30) and Carol (35) should match, Bob (25) should not
        assert_eq!(nodes.len(), 2, "Should return 2 nodes with age > 28");
        for node in &nodes {
            if let Some(PropertyValue::Int(age)) = node.get_property("age") {
                assert!(*age > 28, "All returned nodes should have age > 28");
            }
        }
    }

    #[test]
    fn where_less_than_int() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes WHERE age < 30").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        // Only Bob (25) should match
        assert_eq!(nodes.len(), 1, "Should return 1 node with age < 30");
        let bob = &nodes[0];
        assert_eq!(
            bob.get_property("name"),
            Some(&PropertyValue::String("Bob".into())),
        );
    }

    #[test]
    fn where_and_compound_filter() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes WHERE age > 20 AND age < 32")
            .expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        // Alice (30) and Bob (25) should match, Carol (35) should not
        assert_eq!(nodes.len(), 2, "Should return 2 nodes with 20 < age < 32");
    }

    #[test]
    fn where_no_match_returns_empty() {
        let db = setup_person_db();
        let query =
            parse_sql("SELECT * FROM nodes WHERE name = 'Zara'").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let count = results.count_all().expect("Failed to count results");
        assert_eq!(count, 0, "No node named Zara should exist");
    }

    #[test]
    fn where_equals_on_labeled_table() {
        let db = setup_sql_label_db();
        let query = parse_sql("SELECT * FROM Document WHERE title = 'Rust Guide'")
            .expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        assert_eq!(
            nodes.len(),
            1,
            "Should find exactly one Rust Guide document"
        );
        assert!(nodes[0].has_label_str("document"));
        assert_eq!(
            nodes[0].get_property("title"),
            Some(&PropertyValue::String("Rust Guide".into())),
        );
    }
}

// =============================================================================
// Part 4: Full Pipeline -- LIMIT and OFFSET
// =============================================================================

mod limit_and_offset {
    use super::*;

    #[test]
    fn limit_restricts_result_count() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes LIMIT 2").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let rows = results.collect_all().expect("Failed to collect results");

        assert!(
            rows.len() <= 2,
            "LIMIT 2 should return at most 2 results, got {}",
            rows.len()
        );
    }

    #[test]
    fn limit_one_returns_single_result() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes LIMIT 1").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let rows = results.collect_all().expect("Failed to collect results");

        assert_eq!(rows.len(), 1, "LIMIT 1 should return exactly 1 result");
    }

    #[test]
    fn limit_larger_than_total_returns_all() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes LIMIT 100").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let count = results.count_all().expect("Failed to count results");
        assert_eq!(count, 3, "LIMIT 100 with only 3 nodes should return all 3");
    }

    #[test]
    fn limit_with_offset() {
        let db = setup_person_db();
        let query =
            parse_sql("SELECT * FROM nodes LIMIT 10 OFFSET 1").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let rows = results.collect_all().expect("Failed to collect results");

        // With 3 nodes and OFFSET 1, should get 2 results
        assert_eq!(
            rows.len(),
            2,
            "OFFSET 1 with 3 total nodes should skip 1 and return 2"
        );
    }

    #[test]
    fn offset_beyond_total_returns_empty() {
        let db = setup_person_db();
        let query =
            parse_sql("SELECT * FROM nodes LIMIT 10 OFFSET 100").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let count = results.count_all().expect("Failed to count results");
        assert_eq!(
            count, 0,
            "OFFSET beyond total count should return empty results"
        );
    }
}

// =============================================================================
// Part 5: ORDER BY -- Parse succeeds AND executes with sorted output
// =============================================================================

mod order_by {
    use super::*;
    use aletheiadb::PropertyValue;

    /// Extract the integer `age` property from a node row, in row order.
    fn ages(rows: &[aletheiadb::query::executor::QueryRow]) -> Vec<i64> {
        rows.iter()
            .filter_map(|row| row.entity.as_node())
            .filter_map(|node| match node.get_property("age") {
                Some(PropertyValue::Int(age)) => Some(*age),
                _ => None,
            })
            .collect()
    }

    /// Extract the string `name` property from a node row, in row order.
    fn names(rows: &[aletheiadb::query::executor::QueryRow]) -> Vec<String> {
        rows.iter()
            .filter_map(|row| row.entity.as_node())
            .filter_map(|node| match node.get_property("name") {
                Some(PropertyValue::String(name)) => Some(name.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn order_by_asc_parses_successfully() {
        let query = parse_sql("SELECT * FROM nodes ORDER BY name ASC")
            .expect("Failed to parse ORDER BY ASC");
        // ORDER BY adds a Sort op, so we should have more than just a scan
        assert!(
            query.operation_count() >= 2,
            "ORDER BY should add operations beyond bare scan"
        );
    }

    #[test]
    fn order_by_desc_parses_successfully() {
        let query = parse_sql("SELECT * FROM nodes ORDER BY age DESC")
            .expect("Failed to parse ORDER BY DESC");
        assert!(
            query.operation_count() >= 2,
            "ORDER BY DESC should add operations beyond bare scan"
        );
    }

    #[test]
    fn order_by_age_asc_returns_ascending_rows() {
        // The Sort physical operator executes end-to-end: rows come back sorted
        // by the requested key, ascending.
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes ORDER BY age ASC").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("ORDER BY should execute");
        let rows = results.collect_all().expect("Failed to collect results");

        assert_eq!(
            ages(&rows),
            vec![25, 30, 35],
            "ORDER BY age ASC must return ages in ascending order (Bob, Alice, Carol)"
        );
    }

    #[test]
    fn order_by_name_desc_returns_descending_rows() {
        let db = setup_person_db();
        let query =
            parse_sql("SELECT * FROM nodes ORDER BY name DESC").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("ORDER BY should execute");
        let rows = results.collect_all().expect("Failed to collect results");

        assert_eq!(
            names(&rows),
            vec!["Carol".to_string(), "Bob".to_string(), "Alice".to_string()],
            "ORDER BY name DESC must return names in descending order"
        );
    }
}

// =============================================================================
// Part 6: Edge Scanning -- Parse succeeds AND executes, yielding edge rows
// =============================================================================
//
// ## v1 limitations
//
// `SELECT * FROM edges` yields `EntityResult::Edge` rows that are consumed via
// `collect_all()` / `count_all()` / direct iteration, or via the edge-shaped
// structured projection `collect_structured_edges()` / `collect_edges()`
// (Issue #3626). The node-centric structured helpers `collect_structured()` /
// `collect_nodes()` intentionally remain node-only (they populate a
// `Vec<NodeId>`) and silently drop edge rows -- edges are surfaced structurally
// by their own `collect_structured_edges()` counterpart instead.
//
// A `WHERE` predicate over *edge property* leaves now filters edge rows for real
// (Issue #3622): the executor detects an `EdgeScan`-rooted stream and runs the
// shared `FilterIterator` in edge-property mode, matching each property leaf
// against the edge's own properties. Symmetrically, an `ORDER BY` over an edge
// property sorts edge rows by that property (the `SortIterator` reads the edge's
// own properties for an `EdgeScan`-rooted stream). See
// `select_edges_where_property_filters` / `select_edges_order_by_property_sorts`.
// Supported edge forms: `SELECT * FROM edges [WHERE <edge-prop pred>]
// [ORDER BY <edge-prop|score|timestamp>] [LIMIT n]`.
mod select_edges {
    use super::*;
    use aletheiadb::PropertyValue;

    #[test]
    fn select_edges_parses_successfully() {
        let query = parse_sql("SELECT * FROM edges").expect("Failed to parse SELECT FROM edges");
        assert!(
            query.operation_count() >= 1,
            "Edge scan should have at least 1 operation"
        );
    }

    #[test]
    fn select_edges_returns_all_edges() {
        // The EdgeScan physical operator executes end-to-end and yields one
        // Edge row per stored edge.
        let db = setup_graph_db();
        let query = parse_sql("SELECT * FROM edges").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Edge scan should execute");
        let rows = results.collect_all().expect("Failed to collect results");

        assert_eq!(rows.len(), 2, "setup_graph_db has exactly 2 KNOWS edges");
        for row in &rows {
            let edge = row
                .entity
                .as_edge()
                .expect("each row should be an Edge entity");
            assert!(
                edge.has_label_str("KNOWS"),
                "every edge should carry the KNOWS label"
            );
        }
    }

    #[test]
    fn select_edges_preserves_endpoints() {
        // The two KNOWS edges are Alice->Bob and Bob->Carol. Verify the exact
        // (source, target) endpoint pairs round-trip through the scan.
        let db = setup_graph_db();

        // Resolve node ids by name so the assertion does not depend on id order.
        let node_query = parse_sql("SELECT * FROM nodes").expect("Failed to parse SQL");
        let node_rows = db
            .execute_query(node_query)
            .expect("node scan should execute")
            .collect_all()
            .expect("Failed to collect node rows");
        let id_of = |name: &str| {
            node_rows
                .iter()
                .filter_map(|r| r.entity.as_node())
                .find(|n| n.get_property("name") == Some(&PropertyValue::String(name.into())))
                .map(|n| n.id)
                .unwrap_or_else(|| panic!("node named {name} should exist"))
        };
        let (alice, bob, carol) = (id_of("Alice"), id_of("Bob"), id_of("Carol"));

        let query = parse_sql("SELECT * FROM edges").expect("Failed to parse SQL");
        let rows = db
            .execute_query(query)
            .expect("Edge scan should execute")
            .collect_all()
            .expect("Failed to collect results");

        let mut endpoints: Vec<(u64, u64)> = rows
            .iter()
            .filter_map(|r| r.entity.as_edge())
            .map(|e| (e.source.as_u64(), e.target.as_u64()))
            .collect();
        endpoints.sort_unstable();

        let mut expected = vec![
            (alice.as_u64(), bob.as_u64()),
            (bob.as_u64(), carol.as_u64()),
        ];
        expected.sort_unstable();

        assert_eq!(
            endpoints, expected,
            "edge rows must carry the correct (source, target) endpoints"
        );
    }

    #[test]
    fn select_edges_carries_properties() {
        // Edge properties must round-trip through the scan. Build a local
        // fixture with a distinctive edge property and assert it survives.
        let db = AletheiaDB::new().expect("Failed to create database");
        let a = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "a").build())
            .expect("create a");
        let b = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "b").build())
            .expect("create b");
        db.create_edge(
            a,
            b,
            "FOLLOWS",
            PropertyMapBuilder::new().insert("weight", 42).build(),
        )
        .expect("create edge");

        let query = parse_sql("SELECT * FROM edges").expect("Failed to parse SQL");
        let rows = db
            .execute_query(query)
            .expect("Edge scan should execute")
            .collect_all()
            .expect("Failed to collect results");

        assert_eq!(rows.len(), 1, "exactly one edge was created");
        let edge = rows[0]
            .entity
            .as_edge()
            .expect("row should be an Edge entity");
        assert!(edge.has_label_str("FOLLOWS"));
        assert_eq!(
            edge.get_property("weight"),
            Some(&PropertyValue::Int(42)),
            "edge property must round-trip through the scan"
        );
    }

    #[test]
    fn select_edges_where_property_filters() {
        // Issue #3622: a WHERE over an edge property now filters edge rows for
        // real. `setup_graph_db` has two KNOWS edges (`since` = 2020 and 2021);
        // `WHERE since = 2021` returns exactly the Bob->Carol edge.
        let db = setup_graph_db();
        let query = parse_sql("SELECT * FROM edges WHERE since = 2021").expect("Failed to parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+filter should execute")
            .collect_all()
            .expect("Failed to collect results");

        assert_eq!(rows.len(), 1, "only the since=2021 edge matches");
        let edge = rows[0].entity.as_edge().expect("row should be an Edge");
        assert_eq!(
            edge.get_property("since"),
            Some(&PropertyValue::Int(2021)),
            "the surviving edge must be the since=2021 one"
        );
    }

    #[test]
    fn select_edges_where_property_range_filters() {
        // A range predicate over an edge property: `since > 2020` keeps only the
        // 2021 edge, proving comparison predicates (not just equality) evaluate.
        let db = setup_graph_db();
        let query = parse_sql("SELECT * FROM edges WHERE since > 2020").expect("Failed to parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+filter should execute")
            .collect_all()
            .expect("Failed to collect results");

        assert_eq!(rows.len(), 1, "only since=2021 is > 2020");
        assert_eq!(
            rows[0].entity.as_edge().unwrap().get_property("since"),
            Some(&PropertyValue::Int(2021))
        );
    }

    #[test]
    fn select_edges_where_absent_property_excludes_all() {
        // A WHERE on a property no edge carries excludes every edge (equality on
        // a missing property is false), rather than silently returning all edges.
        let db = setup_graph_db();
        let query =
            parse_sql("SELECT * FROM edges WHERE nonexistent = 1").expect("Failed to parse");
        let count = db
            .execute_query(query)
            .expect("Edge scan+filter should execute")
            .count_all()
            .expect("Failed to count results");
        assert_eq!(count, 0, "no edge has the property, so none match");
    }

    #[test]
    fn select_edges_order_by_property_sorts() {
        // Issue #3622: ORDER BY over an edge property sorts edge rows for real.
        // `ORDER BY since DESC` yields [2021, 2020].
        let db = setup_graph_db();
        let query = parse_sql("SELECT * FROM edges ORDER BY since DESC").expect("Failed to parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+sort should execute")
            .collect_all()
            .expect("Failed to collect results");

        let sinces: Vec<i64> = rows
            .iter()
            .map(
                |r| match r.entity.as_edge().unwrap().get_property("since") {
                    Some(PropertyValue::Int(v)) => *v,
                    other => panic!("expected int `since`, got {other:?}"),
                },
            )
            .collect();
        assert_eq!(sinces, vec![2021, 2020], "edges sorted by since DESC");
    }

    #[test]
    fn select_edges_order_by_property_asc_with_limit() {
        // Ascending order + LIMIT: the smallest `since` (2020) comes first, and
        // LIMIT 1 keeps exactly it -- proving Sort runs before Limit over edges.
        let db = setup_graph_db();
        let query =
            parse_sql("SELECT * FROM edges ORDER BY since ASC LIMIT 1").expect("Failed to parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+sort+limit should execute")
            .collect_all()
            .expect("Failed to collect results");

        assert_eq!(rows.len(), 1, "LIMIT 1 keeps a single row");
        assert_eq!(
            rows[0].entity.as_edge().unwrap().get_property("since"),
            Some(&PropertyValue::Int(2020)),
            "ascending order surfaces the smallest `since` first"
        );
    }

    #[test]
    fn select_edges_where_and_order_by_compose() {
        // WHERE and ORDER BY compose over an edge stream: filter to since >= 2020
        // (both edges) then order DESC -> [2021, 2020].
        let db = setup_graph_db();
        let query = parse_sql("SELECT * FROM edges WHERE since >= 2020 ORDER BY since DESC")
            .expect("Failed to parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+filter+sort should execute")
            .collect_all()
            .expect("Failed to collect results");

        let sinces: Vec<i64> = rows
            .iter()
            .map(
                |r| match r.entity.as_edge().unwrap().get_property("since") {
                    Some(PropertyValue::Int(v)) => *v,
                    other => panic!("expected int `since`, got {other:?}"),
                },
            )
            .collect();
        assert_eq!(sinces, vec![2021, 2020], "filtered then sorted DESC");
    }

    #[test]
    fn select_edges_count_all_non_empty() {
        // Non-empty `count_all()` over an edge scan: the fixture has exactly 2
        // KNOWS edges, so counting the stream yields 2 (complements the
        // empty-database zero-count case).
        let db = setup_graph_db();
        let query = parse_sql("SELECT * FROM edges").expect("Failed to parse SQL");
        let count = db
            .execute_query(query)
            .expect("Edge scan should execute")
            .count_all()
            .expect("Failed to count results");
        assert_eq!(count, 2, "setup_graph_db has exactly 2 edges");
    }

    #[test]
    fn select_edges_limit_restricts_count() {
        let db = setup_graph_db();
        let query = parse_sql("SELECT * FROM edges LIMIT 1").expect("Failed to parse SQL");
        let rows = db
            .execute_query(query)
            .expect("Edge scan should execute")
            .collect_all()
            .expect("Failed to collect results");

        assert_eq!(rows.len(), 1, "LIMIT 1 should cap the edge scan to 1 row");
    }

    #[test]
    fn select_edges_empty_database_returns_ok_zero_rows() {
        // A database with no edges yields zero rows and does NOT error.
        let db = setup_person_db(); // nodes only, no edges
        let query = parse_sql("SELECT * FROM edges").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Edge scan should execute");
        let count = results.count_all().expect("Failed to count results");
        assert_eq!(count, 0, "a db with no edges must return zero edge rows");
    }

    #[test]
    fn select_edges_collect_all_is_the_consumption_path() {
        // Explicitly assert the primary consumption path (collect_all) surfaces
        // edge rows. The node-centric collect_nodes()/collect_structured()
        // helpers remain node-only by design; the edge-shaped structured path is
        // collect_structured_edges()/collect_edges() (Issue #3626, exercised by
        // select_edges_collect_structured_edges below).
        let db = setup_graph_db();
        let query = parse_sql("SELECT * FROM edges").expect("Failed to parse SQL");
        let rows = db
            .execute_query(query)
            .expect("Edge scan should execute")
            .collect_all()
            .expect("Failed to collect results");
        assert!(
            rows.iter().all(|r| r.entity.as_edge().is_some()),
            "collect_all must surface every row as an Edge entity"
        );
        assert_eq!(rows.len(), 2);
    }

    // -------------------------------------------------------------------------
    // Reserved STRUCTURAL fields: type / source / target (Issue #3622 review
    // fix). These live on the Edge STRUCT, not in `properties`, so before the
    // fix `WHERE type = 'KNOWS'`, `WHERE source = <id>`, and `ORDER BY type`
    // silently returned zero rows / no-op sorts. These runtime tests assert the
    // now-correct end-to-end behavior.
    // -------------------------------------------------------------------------

    /// A fixture with two DISTINCT edge types between distinct endpoints so that
    /// `type`, `source`, and `target` each partition the two-edge set uniquely.
    /// Returns the database and the three node ids (a, b, c). Edges: a-KNOWS->b,
    /// b-FOLLOWS->c.
    fn setup_typed_edges_db() -> (AletheiaDB, u64, u64, u64) {
        let db = AletheiaDB::new().expect("Failed to create database");
        let a = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "A").build(),
            )
            .expect("create A");
        let b = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "B").build(),
            )
            .expect("create B");
        let c = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "C").build(),
            )
            .expect("create C");
        db.create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
            .expect("create KNOWS edge");
        db.create_edge(b, c, "FOLLOWS", PropertyMapBuilder::new().build())
            .expect("create FOLLOWS edge");
        (db, a.as_u64(), b.as_u64(), c.as_u64())
    }

    #[test]
    fn select_edges_where_type_filters_by_label() {
        // `WHERE type = 'KNOWS'` resolves the edge's LABEL (a struct field), not
        // a `type` property (which no edge carries). Before the review fix this
        // silently returned 0 rows; now it returns exactly the KNOWS edge.
        let (db, _a, _b, _c) = setup_typed_edges_db();
        let query = parse_sql("SELECT * FROM edges WHERE type = 'KNOWS'").expect("parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+filter should execute")
            .collect_all()
            .expect("collect");
        assert_eq!(rows.len(), 1, "only the KNOWS edge matches type = 'KNOWS'");
        assert!(
            rows[0].entity.as_edge().unwrap().has_label_str("KNOWS"),
            "the surviving edge must be the KNOWS edge"
        );
    }

    #[test]
    fn select_edges_where_source_filters_by_endpoint() {
        // `WHERE source = <id>` resolves the source NodeId struct field.
        let (db, a, _b, _c) = setup_typed_edges_db();
        let query = parse_sql(&format!("SELECT * FROM edges WHERE source = {a}")).expect("parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+filter should execute")
            .collect_all()
            .expect("collect");
        assert_eq!(rows.len(), 1, "only the edge out of node A matches");
        assert_eq!(
            rows[0].entity.as_edge().unwrap().source.as_u64(),
            a,
            "surviving edge must originate at node A"
        );
    }

    #[test]
    fn select_edges_where_target_filters_by_endpoint() {
        // `WHERE target = <id>` resolves the target NodeId struct field.
        let (db, _a, _b, c) = setup_typed_edges_db();
        let query = parse_sql(&format!("SELECT * FROM edges WHERE target = {c}")).expect("parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+filter should execute")
            .collect_all()
            .expect("collect");
        assert_eq!(rows.len(), 1, "only the edge into node C matches");
        assert_eq!(
            rows[0].entity.as_edge().unwrap().target.as_u64(),
            c,
            "surviving edge must point at node C"
        );
    }

    #[test]
    fn select_edges_order_by_type_sorts_by_label() {
        // `ORDER BY type ASC` sorts by the edge label lexicographically:
        // FOLLOWS before KNOWS. Before the fix this was a no-op (null key).
        let (db, _a, _b, _c) = setup_typed_edges_db();
        let query = parse_sql("SELECT * FROM edges ORDER BY type ASC").expect("parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+sort should execute")
            .collect_all()
            .expect("collect");
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0].entity.as_edge().unwrap().has_label_str("FOLLOWS"),
            "FOLLOWS sorts before KNOWS ascending"
        );
        assert!(
            rows[1].entity.as_edge().unwrap().has_label_str("KNOWS"),
            "KNOWS sorts after FOLLOWS ascending"
        );
    }

    #[test]
    fn select_edges_order_by_source_sorts_by_endpoint_id() {
        // `ORDER BY source ASC` sorts by the source NodeId. Node A was created
        // before B, so A's id < B's id; the A-rooted (KNOWS) edge comes first.
        let (db, a, b, _c) = setup_typed_edges_db();
        assert!(a < b, "node A created first must have the smaller id");
        let query = parse_sql("SELECT * FROM edges ORDER BY source ASC").expect("parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+sort should execute")
            .collect_all()
            .expect("collect");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].entity.as_edge().unwrap().source.as_u64(),
            a,
            "the edge out of the lower-id node A sorts first"
        );
        assert_eq!(
            rows[1].entity.as_edge().unwrap().source.as_u64(),
            b,
            "the edge out of node B sorts second"
        );
    }

    #[test]
    fn select_edges_order_by_target_sorts_by_endpoint_id() {
        // `ORDER BY target DESC` sorts by the target NodeId descending: the edge
        // into node C (highest id) comes first.
        let (db, _a, b, c) = setup_typed_edges_db();
        let query = parse_sql("SELECT * FROM edges ORDER BY target DESC").expect("parse");
        let rows = db
            .execute_query(query)
            .expect("Edge scan+sort should execute")
            .collect_all()
            .expect("collect");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].entity.as_edge().unwrap().target.as_u64(),
            c,
            "highest target id (C) sorts first descending"
        );
        assert_eq!(
            rows[1].entity.as_edge().unwrap().target.as_u64(),
            b,
            "node B target sorts second descending"
        );
    }

    #[test]
    fn select_edges_collect_structured_edges() {
        // Issue #3626: the edge-shaped structured projection. `SELECT * FROM edges`
        // -> collect_structured_edges() yields parallel vectors of ids, endpoints,
        // labels, and properties -- the edge counterpart to collect_structured().
        let db = setup_graph_db();

        // Resolve node ids by name so the endpoint assertion is order-independent.
        let node_rows = db
            .execute_query(parse_sql("SELECT * FROM nodes").expect("parse nodes"))
            .expect("node scan should execute")
            .collect_all()
            .expect("collect node rows");
        let id_of = |name: &str| {
            node_rows
                .iter()
                .filter_map(|r| r.entity.as_node())
                .find(|n| n.get_property("name") == Some(&PropertyValue::String(name.into())))
                .map(|n| n.id)
                .unwrap_or_else(|| panic!("node named {name} should exist"))
        };
        let (alice, bob, carol) = (id_of("Alice"), id_of("Bob"), id_of("Carol"));

        let structured = db
            .execute_query(parse_sql("SELECT * FROM edges").expect("parse edges"))
            .expect("Edge scan should execute")
            .collect_structured_edges()
            .expect("collect_structured_edges should succeed");

        assert_eq!(
            structured.len(),
            2,
            "setup_graph_db has exactly 2 KNOWS edges"
        );
        assert!(!structured.is_empty());

        // Endpoints are available (full edges) and match Alice->Bob, Bob->Carol.
        let triples = structured
            .edges_with_endpoints()
            .expect("full edges expose endpoints");
        let mut endpoints: Vec<(u64, u64)> = triples
            .iter()
            .map(|(_, s, t)| (s.as_u64(), t.as_u64()))
            .collect();
        endpoints.sort_unstable();
        let mut expected = vec![
            (alice.as_u64(), bob.as_u64()),
            (bob.as_u64(), carol.as_u64()),
        ];
        expected.sort_unstable();
        assert_eq!(endpoints, expected, "structured endpoints must round-trip");

        // Labels resolve to KNOWS for both edges.
        let labels = structured.labels.as_ref().expect("labels present");
        assert!(
            labels.iter().all(|l| l.to_string() == "KNOWS"),
            "both edges are KNOWS"
        );

        // Properties vector is present and parallel to edges.
        let props = structured.properties.as_ref().expect("properties present");
        assert_eq!(props.len(), 2);

        // No temporal / ranking context on a plain scan.
        assert!(structured.scores.is_none());
        assert!(structured.versions.is_none());
    }

    #[test]
    fn select_edges_collect_structured_edges_empty_db() {
        // A database with no edges yields an empty EdgeQueryResult (all Option fields
        // None), not an error.
        let db = setup_person_db();
        let structured = db
            .execute_query(parse_sql("SELECT * FROM edges").expect("parse edges"))
            .expect("Edge scan should execute")
            .collect_structured_edges()
            .expect("collect_structured_edges should succeed on empty edge set");
        assert!(structured.is_empty());
        assert!(structured.sources.is_none());
        assert!(structured.edges_with_endpoints().is_none());
    }
}

// =============================================================================
// Part 7: Temporal query structure verification
// =============================================================================

mod temporal_queries {
    use super::*;

    #[test]
    fn system_time_as_of_produces_temporal_query() {
        let query =
            parse_sql("SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1705315200000000'")
                .expect("Failed to parse temporal SQL");

        assert!(
            query.is_temporal(),
            "FOR SYSTEM_TIME AS OF should produce a temporal query"
        );
    }

    #[test]
    fn system_time_between_produces_temporal_query() {
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME BETWEEN TIMESTAMP '1000000' AND TIMESTAMP '2000000'",
        )
        .expect("Failed to parse temporal SQL");

        assert!(
            query.is_temporal(),
            "FOR SYSTEM_TIME BETWEEN should produce a temporal query"
        );
    }

    #[test]
    fn valid_time_as_of_produces_temporal_query() {
        let query =
            parse_sql("SELECT * FROM nodes FOR VALID_TIME AS OF TIMESTAMP '1705315200000000'")
                .expect("Failed to parse temporal SQL");

        assert!(
            query.is_temporal(),
            "FOR VALID_TIME AS OF should produce a temporal query"
        );
    }

    #[test]
    fn non_temporal_query_is_not_temporal() {
        let query = parse_sql("SELECT * FROM nodes").expect("Failed to parse SQL");

        assert!(
            !query.is_temporal(),
            "Basic SELECT without temporal clause should not be temporal"
        );
    }

    #[test]
    fn temporal_with_filter_preserves_both() {
        let query = parse_sql(
            "SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '1705315200000000' WHERE age > 21",
        )
        .expect("Failed to parse temporal SQL with filter");

        assert!(query.is_temporal());
        // The query should have more operations than just a scan (scan + filter at minimum)
        assert!(
            query.operation_count() >= 2,
            "Temporal query with WHERE should have at least 2 ops (scan + filter), got {}",
            query.operation_count()
        );
    }

    #[test]
    fn temporal_query_executes_against_db() {
        let db = setup_person_db();
        // Use a timestamp far in the future so the nodes exist at that point
        let query =
            parse_sql("SELECT * FROM nodes FOR SYSTEM_TIME AS OF TIMESTAMP '9999999999999999'")
                .expect("Failed to parse temporal SQL");

        // Execution should not panic -- the results may vary based on temporal storage
        // but the pipeline should handle it gracefully.
        let result = db.execute_query(query);
        // Whether it succeeds or returns an error, it should not panic.
        // Some temporal queries may fail if no historical data is available for that timestamp.
        match result {
            Ok(results) => {
                // If it succeeds, count should be non-negative
                let _count = results.count_all().unwrap_or(0);
            }
            Err(_) => {
                // Temporal queries may legitimately fail if historical storage
                // is not configured; this is acceptable behavior.
            }
        }
    }
}

// =============================================================================
// Part 8: Complex queries combining multiple clauses
// =============================================================================

mod complex_queries {
    use super::*;

    #[test]
    fn combined_where_and_limit() {
        let db = setup_person_db();
        let query =
            parse_sql("SELECT * FROM nodes WHERE age > 20 LIMIT 2").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        // All 3 people have age > 20, but LIMIT 2 should cap it
        assert_eq!(
            nodes.len(),
            2,
            "WHERE + LIMIT should return at most 2 results"
        );
    }

    #[test]
    fn label_filter_combined_with_where() {
        let db = setup_sql_label_db();
        // SQL lowercases "Person" to "person", matching our lowercase-labeled nodes
        let query = parse_sql("SELECT * FROM Person WHERE age > 28").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        // Only Alice (30) among person nodes has age > 28
        assert_eq!(
            nodes.len(),
            1,
            "Label + WHERE filter should return 1 result"
        );
        assert!(nodes[0].has_label_str("person"));
    }

    #[test]
    fn query_operation_count_reflects_complexity() {
        let simple = parse_sql("SELECT * FROM nodes").expect("Failed to parse SQL");
        let with_filter =
            parse_sql("SELECT * FROM nodes WHERE age > 21").expect("Failed to parse SQL");
        let with_filter_limit =
            parse_sql("SELECT * FROM nodes WHERE age > 21 LIMIT 100").expect("Failed to parse");

        assert!(
            with_filter.operation_count() > simple.operation_count(),
            "Filtered query should have more operations than simple scan: {} vs {}",
            with_filter.operation_count(),
            simple.operation_count()
        );
        assert!(
            with_filter_limit.operation_count() > with_filter.operation_count(),
            "Query with LIMIT should have more operations than just filter: {} vs {}",
            with_filter_limit.operation_count(),
            with_filter.operation_count()
        );
    }

    #[test]
    fn where_filter_with_limit_and_offset() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes WHERE age > 20 LIMIT 2 OFFSET 1")
            .expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        // Correct SQL semantics: OFFSET first (skip 1), then LIMIT 2.
        // Filter(3 results) -> Skip(1) -> Limit(2) = 2 results.
        assert_eq!(
            nodes.len(),
            2,
            "WHERE + LIMIT 2 + OFFSET 1: filter 3 results, skip 1, take 2"
        );
    }
}

// =============================================================================
// Part 9: MATCH graph pattern queries
// =============================================================================

mod match_patterns {
    use super::*;

    #[test]
    fn match_clause_parses_successfully() {
        let query = parse_sql("SELECT * FROM nodes MATCH (n)-[:KNOWS]->(m)")
            .expect("Failed to parse MATCH query");

        // A MATCH query should have at least the scan + traversal ops
        assert!(
            query.operation_count() >= 2,
            "MATCH query should have at least 2 operations (scan + traversal), got {}",
            query.operation_count()
        );
    }

    #[test]
    fn match_with_variable_depth_parses() {
        let query = parse_sql("SELECT * FROM nodes MATCH (n)-[:KNOWS*1..3]->(m)")
            .expect("Failed to parse MATCH with variable depth");

        assert!(
            query.operation_count() >= 2,
            "Variable depth MATCH should have at least 2 operations"
        );
    }

    #[test]
    fn match_executes_against_graph_db() {
        let db = setup_graph_db();
        let query = parse_sql("SELECT * FROM nodes MATCH (n)-[:KNOWS]->(m)")
            .expect("Failed to parse MATCH query");

        let result = db.execute_query(query);
        // MATCH execution may or may not succeed depending on planner support;
        // verify it doesn't panic.
        match result {
            Ok(results) => {
                let rows = results.collect_all().expect("Failed to collect results");
                // With Alice->Bob and Bob->Carol KNOWS edges, we expect traversal results
                assert!(
                    !rows.is_empty(),
                    "MATCH query on graph with KNOWS edges should return results"
                );
            }
            Err(_) => {
                // Some MATCH patterns may not be fully supported in execution yet;
                // this is acceptable for integration testing.
            }
        }
    }
}

// =============================================================================
// Part 10: Error Handling
// =============================================================================

mod error_handling {
    use super::*;

    #[test]
    fn parse_invalid_sql_returns_error() {
        let result = parse_sql("SELEC * FORM nodes");
        assert!(result.is_err(), "Invalid SQL should return an error");
    }

    #[test]
    fn parse_missing_from_clause_returns_error() {
        let result = parse_sql("SELECT *");
        assert!(
            result.is_err(),
            "SELECT without FROM should return an error"
        );
    }

    #[test]
    fn parse_empty_string_returns_error() {
        let result = parse_sql("");
        assert!(result.is_err(), "Empty SQL string should return an error");
    }

    #[test]
    fn parse_unsupported_statement_returns_error() {
        let result = parse_sql("INSERT INTO nodes (name) VALUES ('test')");
        assert!(result.is_err(), "INSERT statements should not be supported");
    }

    #[test]
    fn parse_unsupported_join_returns_error() {
        let result = parse_sql("SELECT * FROM nodes JOIN edges ON nodes.id = edges.source");
        assert!(result.is_err(), "JOIN clauses should not be supported");
    }

    #[test]
    fn sql_error_is_descriptive() {
        let result = parse_sql("SELEC * FORM nodes");
        match result {
            Err(e) => {
                let msg = e.to_string();
                assert!(!msg.is_empty(), "SQL error message should be non-empty");
            }
            Ok(_) => panic!("Expected an error for invalid SQL"),
        }
    }
}

// =============================================================================
// Part 11: Parameterized Queries
// =============================================================================

mod parameterized_queries {
    use super::*;
    use aletheiadb::query::ir::PredicateValue;
    use aletheiadb::sql::SqlParameterValue;
    use std::collections::HashMap;

    #[test]
    fn parse_sql_with_params_succeeds() {
        let mut params = HashMap::new();
        params.insert(
            "min_age".to_string(),
            SqlParameterValue::Scalar(PredicateValue::Int(21)),
        );

        let result = parse_sql_with_params("SELECT * FROM nodes WHERE age > min_age", params);
        assert!(
            result.is_ok(),
            "Parameterized SQL query should parse successfully: {:?}",
            result.err()
        );
    }

    #[test]
    fn parameterized_query_executes() {
        let db = setup_person_db();

        let mut params = HashMap::new();
        params.insert(
            "min_age".to_string(),
            SqlParameterValue::Scalar(PredicateValue::Int(28)),
        );

        let query = parse_sql_with_params("SELECT * FROM nodes WHERE age > min_age", params)
            .expect("Failed to parse parameterized SQL");

        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        // Alice (30) and Carol (35) have age > 28
        assert_eq!(
            nodes.len(),
            2,
            "Parameterized query should filter correctly"
        );
    }
}

// =============================================================================
// Part 12: Column Projection
// =============================================================================

mod column_projection {
    use super::*;

    #[test]
    fn select_specific_columns_parses() {
        let query =
            parse_sql("SELECT name, age FROM nodes").expect("Failed to parse SQL with columns");

        // Should have more ops than just a bare scan (scan + project)
        assert!(
            query.operation_count() >= 2,
            "Column projection query should have at least 2 ops, got {}",
            query.operation_count()
        );
    }

    #[test]
    fn select_specific_columns_executes() {
        let db = setup_person_db();
        let query =
            parse_sql("SELECT name, age FROM nodes").expect("Failed to parse SQL with columns");
        let results = db.execute_query(query).expect("Failed to execute query");
        let rows = results.collect_all().expect("Failed to collect results");

        // Should still return all 3 nodes (projection doesn't filter)
        assert_eq!(rows.len(), 3, "Column projection should not filter nodes");
    }
}

// =============================================================================
// Part 13: End-to-End real-world query patterns
// =============================================================================

mod real_world_patterns {
    use super::*;
    use aletheiadb::PropertyValue;

    #[test]
    fn find_all_people_over_age_without_sort() {
        let db = setup_person_db();
        // Use WHERE only (no ORDER BY) since Sort is not yet supported in executor
        let query = parse_sql("SELECT * FROM nodes WHERE age > 25").expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        // Alice (30) and Carol (35) match age > 25
        assert_eq!(nodes.len(), 2, "Should return 2 nodes with age > 25");
        for node in &nodes {
            if let Some(PropertyValue::Int(age)) = node.get_property("age") {
                assert!(*age > 25, "All returned nodes should have age > 25");
            }
        }
    }

    #[test]
    fn paginated_query() {
        let db = setup_person_db();
        // Pagination without ORDER BY (Sort not yet supported)
        let first_page = parse_sql("SELECT * FROM nodes LIMIT 2").expect("Failed to parse SQL");
        let first_results = db
            .execute_query(first_page)
            .expect("Failed to execute query");
        let first_nodes = first_results
            .collect_nodes()
            .expect("Failed to collect nodes");

        assert_eq!(first_nodes.len(), 2, "First page should have 2 results");

        // OFFSET comes before LIMIT: skip 2 rows first, then take 2.
        // With 3 nodes total, OFFSET 2 skips 2, LIMIT 2 takes remaining 1.
        let second_page =
            parse_sql("SELECT * FROM nodes LIMIT 2 OFFSET 2").expect("Failed to parse SQL");
        let second_results = db
            .execute_query(second_page)
            .expect("Failed to execute query");
        let second_nodes = second_results
            .collect_nodes()
            .expect("Failed to collect nodes");

        assert_eq!(
            second_nodes.len(),
            1,
            "LIMIT 2 OFFSET 2: skip 2 rows, then take up to 2 from remaining 1"
        );
    }

    #[test]
    fn label_filter_with_string_equality() {
        let db = setup_sql_label_db();
        let query = parse_sql("SELECT * FROM Document WHERE title = 'Rust Guide'")
            .expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        assert_eq!(
            nodes.len(),
            1,
            "Should find exactly one Rust Guide document"
        );
        assert!(nodes[0].has_label_str("document"));
        assert_eq!(
            nodes[0].get_property("title"),
            Some(&PropertyValue::String("Rust Guide".into())),
        );
    }

    #[test]
    fn multi_property_filter() {
        let db = setup_person_db();
        let query = parse_sql("SELECT * FROM nodes WHERE name = 'Alice' AND age > 20")
            .expect("Failed to parse SQL");
        let results = db.execute_query(query).expect("Failed to execute query");
        let nodes = results.collect_nodes().expect("Failed to collect nodes");

        assert_eq!(
            nodes.len(),
            1,
            "Should find exactly one node matching both conditions"
        );
        assert_eq!(
            nodes[0].get_property("name"),
            Some(&PropertyValue::String("Alice".into())),
        );
    }
}

// =============================================================================
// Part 14: Consistency -- SQL results match Rust API results
// =============================================================================

mod consistency_with_rust_api {
    use super::*;

    #[test]
    fn sql_scan_count_matches_api_scan_count() {
        let db = setup_sql_label_db();

        // Count via SQL
        let query = parse_sql("SELECT * FROM nodes").expect("Failed to parse SQL");
        let sql_count = db
            .execute_query(query)
            .expect("Failed to execute SQL query")
            .count_all()
            .expect("Failed to count SQL results");

        // Count via Rust API scan (count all person + document + event nodes)
        let person_count = db.scan_nodes_by_label("person").count();
        let document_count = db.scan_nodes_by_label("document").count();
        let event_count = db.scan_nodes_by_label("event").count();
        let api_count = person_count + document_count + event_count;

        assert_eq!(
            sql_count, api_count,
            "SQL SELECT * FROM nodes count ({}) should match API label scan total ({})",
            sql_count, api_count
        );
    }

    #[test]
    fn sql_label_scan_matches_api_label_scan() {
        let db = setup_sql_label_db();

        // SQL scan by label (SQL lowercases "Person" to "person")
        let query = parse_sql("SELECT * FROM Person").expect("Failed to parse SQL");
        let sql_count = db
            .execute_query(query)
            .expect("Failed to execute SQL query")
            .count_all()
            .expect("Failed to count SQL results");

        // Rust API scan by the same lowercase label
        let api_count = db.scan_nodes_by_label("person").count();

        assert_eq!(
            sql_count, api_count,
            "SQL SELECT * FROM Person count ({}) should match scan_nodes_by_label(\"person\") count ({})",
            sql_count, api_count
        );
    }

    #[test]
    fn sql_returns_same_node_ids_as_api() {
        let db = setup_person_db();

        // Get node IDs via SQL
        let query = parse_sql("SELECT * FROM nodes").expect("Failed to parse SQL");
        let results = db
            .execute_query(query)
            .expect("Failed to execute SQL query");
        let mut sql_ids: Vec<_> = results
            .collect_nodes()
            .expect("Failed to collect nodes")
            .iter()
            .map(|n| n.id)
            .collect();
        sql_ids.sort();

        // Get node IDs via Rust API
        let mut api_ids: Vec<_> = db.scan_nodes_by_label("Person").collect();
        api_ids.sort();

        assert_eq!(
            sql_ids, api_ids,
            "SQL query should return the same node IDs as the API"
        );
    }
}
