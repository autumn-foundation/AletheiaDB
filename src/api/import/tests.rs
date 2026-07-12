//! Tests for the bulk graph importer (Issue #3211).

use std::io::Write;
use std::path::PathBuf;

use tempfile::TempDir;

use super::*;
use crate::core::property::PropertyValue;
use crate::core::temporal::time;
use crate::test_utils::create_test_db;

/// Write `contents` to a file named `name` inside `dir`, returning its path.
fn write_file(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    let mut file = std::fs::File::create(&path).expect("create temp file");
    file.write_all(contents.as_bytes())
        .expect("write temp file");
    path
}

fn person_nodes(key_col: &str) -> NodeMapping {
    NodeMapping::new(LabelSource::fixed("Person"), key_col)
        .property("name", "name", ColumnType::String)
        .property("age", "age", ColumnType::Int)
}

// AC: happy path — nodes + edges CSV load with type coercion.
#[test]
fn happy_path_nodes_and_edges_csv() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let nodes = write_file(
        &files,
        "nodes.csv",
        "id,name,age\nalice,Alice,30\nbob,Bob,25\n",
    );
    let edges = write_file(&files, "edges.csv", "src,dst,since\nalice,bob,2020\n");

    let mut importer = db.import();
    let node_report = importer
        .nodes_from_csv(&nodes, person_nodes("id"))
        .expect("nodes import");
    assert_eq!(node_report.rows_read, 2);
    assert_eq!(node_report.nodes_imported, 2);
    assert!(node_report.skipped.is_empty());

    let edge_mapping = EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst").property(
        "since",
        "since",
        ColumnType::Int,
    );
    let edge_report = importer
        .edges_from_csv(&edges, edge_mapping)
        .expect("edges import");
    assert_eq!(edge_report.edges_imported, 1);
    assert!(edge_report.unresolved_endpoints.is_empty());

    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);

    // Type coercion: name is a String, age is an Int.
    let alice = importer.resolve_key("alice").expect("alice resolved");
    let node = db.get_node(alice).unwrap();
    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("Alice"))
    );
    assert_eq!(node.properties.get("age"), Some(&PropertyValue::Int(30)));

    // Edge endpoints resolved by business key.
    let bob = importer.resolve_key("bob").expect("bob resolved");
    let out = db.get_outgoing_edges(alice);
    assert_eq!(out.len(), 1);
    assert_eq!(db.get_edge_target(out[0]).unwrap(), bob);
}

// AC: malformed row, Abort mode -> precise `row N:` error, nothing past it committed.
#[test]
fn malformed_row_abort_reports_row_number() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // Row 2 ("bob") has a non-integer age.
    let nodes = write_file(
        &files,
        "nodes.csv",
        "id,name,age\nalice,Alice,30\nbob,Bob,notanumber\ncarol,Carol,40\n",
    );

    let mut importer = db.import().batch_size(1);
    let err = importer
        .nodes_from_csv(&nodes, person_nodes("id"))
        .expect_err("should abort on malformed row");
    let msg = err.to_string();
    assert!(msg.contains("row 2"), "message was: {msg}");
    assert!(msg.contains("Int"), "message was: {msg}");

    // Abort: alice (row 1) committed before the bad row, carol (row 3) never reached.
    assert_eq!(db.node_count(), 1);
}

// AC: malformed row, SkipAndReport mode -> good rows committed, precise errors reported.
#[test]
fn malformed_row_skip_and_report() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let nodes = write_file(
        &files,
        "nodes.csv",
        "id,name,age\nalice,Alice,30\nbob,Bob,notanumber\ncarol,Carol,40\n",
    );

    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    let report = importer
        .nodes_from_csv(&nodes, person_nodes("id"))
        .expect("skip mode should not error");

    assert_eq!(report.rows_read, 3);
    assert_eq!(report.nodes_imported, 2);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].row, 2);
    assert!(report.skipped[0].message.contains("Int"));
    assert_eq!(db.node_count(), 2);
}

// AC: edges referencing missing endpoints are reported, not silently dropped.
#[test]
fn missing_endpoint_edges_reported() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let nodes = write_file(&files, "nodes.csv", "id,name,age\nalice,Alice,30\n");
    // bob does not exist as a node.
    let edges = write_file(&files, "edges.csv", "src,dst\nalice,bob\n");

    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    importer
        .nodes_from_csv(&nodes, person_nodes("id"))
        .expect("nodes");
    let report = importer
        .edges_from_csv(
            &edges,
            EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst"),
        )
        .expect("edges");

    assert_eq!(report.edges_imported, 0);
    assert_eq!(report.unresolved_endpoints.len(), 1);
    let unresolved = &report.unresolved_endpoints[0];
    assert_eq!(unresolved.row, 1);
    assert_eq!(unresolved.key, "bob");
    assert_eq!(unresolved.side, Endpoint::Target);
    assert_eq!(db.edge_count(), 0);

    // Same situation aborts in Abort mode.
    let (_tmp2, db2) = create_test_db().unwrap();
    let mut importer2 = db2.import();
    importer2
        .nodes_from_csv(&nodes, person_nodes("id"))
        .unwrap();
    let err = importer2
        .edges_from_csv(
            &edges,
            EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst"),
        )
        .expect_err("abort on unresolved endpoint");
    assert!(err.to_string().contains("unresolved"));
}

// AC: optional valid_time column -> data is queryable with AS OF VALID_TIME.
#[test]
fn valid_time_backfill_round_trips_with_as_of() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // Alice's fact is valid from 2021-01-01.
    let nodes = write_file(
        &files,
        "nodes.csv",
        "id,name,age,known_since\nalice,Alice,30,2021-01-01\n",
    );

    let mapping = person_nodes("id").valid_time_column("known_since");
    let mut importer = db.import();
    importer.nodes_from_csv(&nodes, mapping).unwrap();
    let alice = importer.resolve_key("alice").unwrap();

    let now = time::now();
    // 2021-06-01 (after valid_from): node is visible.
    let after = time::from_secs(1_622_505_600);
    assert!(
        db.get_node_at_time(alice, after, now).is_ok(),
        "node should be valid after its valid_from"
    );

    // 2020-06-01 (before valid_from): node is NOT yet valid.
    let before = time::from_secs(1_590_969_600);
    assert!(
        db.get_node_at_time(alice, before, now).is_err(),
        "node should not be valid before its valid_from"
    );
}

// AC: when valid_time is absent, rows default to import (transaction) time.
#[test]
fn without_valid_time_defaults_to_now() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", "id,name,age\nalice,Alice,30\n");

    let mut importer = db.import();
    importer.nodes_from_csv(&nodes, person_nodes("id")).unwrap();
    let alice = importer.resolve_key("alice").unwrap();

    // Visible now in current state.
    assert!(db.get_node(alice).is_ok());
    let now = time::now();
    assert!(db.get_node_at_time(alice, now, now).is_ok());
}

// AC: JSONL variant parses + imports equivalently.
#[test]
fn jsonl_nodes_and_edges() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let nodes = write_file(
        &files,
        "nodes.jsonl",
        "{\"id\":\"alice\",\"name\":\"Alice\",\"age\":30}\n\n{\"id\":\"bob\",\"name\":\"Bob\",\"age\":25}\n",
    );
    let edges = write_file(
        &files,
        "edges.jsonl",
        "{\"src\":\"alice\",\"dst\":\"bob\"}\n",
    );

    let mut importer = db.import();
    let node_report = importer
        .nodes_from_jsonl(&nodes, person_nodes("id"))
        .unwrap();
    // Blank line is skipped, not counted.
    assert_eq!(node_report.rows_read, 2);
    assert_eq!(node_report.nodes_imported, 2);

    let edge_report = importer
        .edges_from_jsonl(
            &edges,
            EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst"),
        )
        .unwrap();
    assert_eq!(edge_report.edges_imported, 1);

    let alice = importer.resolve_key("alice").unwrap();
    let node = db.get_node(alice).unwrap();
    assert_eq!(node.properties.get("age"), Some(&PropertyValue::Int(30)));
}

// AC: label-from-column works (in addition to fixed labels).
#[test]
fn label_from_column() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        "id,kind,name\nn1,Person,Alice\nn2,Company,Acme\n",
    );

    let mapping = NodeMapping::new(LabelSource::column("kind"), "id").property(
        "name",
        "name",
        ColumnType::String,
    );
    let mut importer = db.import();
    importer.nodes_from_csv(&nodes, mapping).unwrap();

    assert_eq!(db.scan_nodes_by_label("Person").count(), 1);
    assert_eq!(db.scan_nodes_by_label("Company").count(), 1);
}

// Batching: a chunk larger than one batch still imports every row.
#[test]
fn batching_across_multiple_chunks() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let mut csv = String::from("id,name,age\n");
    for i in 0..25 {
        csv.push_str(&format!("n{i},Name{i},{i}\n"));
    }
    let nodes = write_file(&files, "nodes.csv", &csv);

    let mut importer = db.import().batch_size(10);
    let report = importer.nodes_from_csv(&nodes, person_nodes("id")).unwrap();
    assert_eq!(report.nodes_imported, 25);
    assert_eq!(db.node_count(), 25);
}

// kills: `report.edges_imported += count` -> `= count` (mod.rs commit_edge_chunk).
// Edges spanning multiple batch chunks must ACCUMULATE, not overwrite with the last
// chunk's count. batch_size(2) over 5 edges -> chunks of 2,2,1; the sum is 5, but the
// `= count` mutant would leave only the final chunk's 1.
#[test]
fn edge_count_accumulates_across_multiple_chunks() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // Six nodes so we can draw five distinct edges between them.
    let mut node_csv = String::from("id,name,age\n");
    for i in 0..6 {
        node_csv.push_str(&format!("n{i},Name{i},{i}\n"));
    }
    let nodes = write_file(&files, "nodes.csv", &node_csv);

    // Five edges => with batch_size(2) they flush as chunks of 2, 2, 1.
    let edges = write_file(
        &files,
        "edges.csv",
        "src,dst\nn0,n1\nn1,n2\nn2,n3\nn3,n4\nn4,n5\n",
    );

    let mut importer = db.import().batch_size(2);
    importer.nodes_from_csv(&nodes, person_nodes("id")).unwrap();
    let report = importer
        .edges_from_csv(
            &edges,
            EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst"),
        )
        .expect("edges import");

    // Exact total across all chunks (kills `= count`).
    assert_eq!(report.edges_imported, 5);
    // And every edge is actually persisted in the DB.
    assert_eq!(db.edge_count(), 5);
}

// pins: the Io-fatality of the OPEN path only. Opening a nonexistent file is fatal even
// under SkipAndReport: the importer must return `Err`, never a "success" report with the
// file recorded as a skipped row. NOTE: this does NOT reach the `handle_row_failure`
// `ImportError::Io(_) => return Err(...)` arm — the open fails before any row is read, so
// that arm's mutation is killed separately by `handle_row_failure_io_is_fatal_in_skip_mode`.
#[test]
fn io_error_is_fatal_even_in_skip_mode() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let missing = files.path().join("does_not_exist.csv");

    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    let result = importer.nodes_from_csv(&missing, person_nodes("id"));

    assert!(
        result.is_err(),
        "an I/O error must be fatal even in SkipAndReport mode, got: {result:?}"
    );
    // Nothing was imported and nothing was silently downgraded to a skipped row.
    assert_eq!(db.node_count(), 0);
}

// kills: the `ImportError::Io(_) => return Err(err.into())` arm in the PRIVATE
// `handle_row_failure`. That arm is unreachable via the public API (the reader opens the
// whole file before yielding rows, so a mid-stream reader Io error cannot occur today), so
// only a direct call to the private method — legal from this in-crate `#[cfg(test)]` module
// — can exercise it. A synthetic `ImportError::Io` under SkipAndReport must return `Err`
// (fatal), never be downgraded to a skipped row; the arm mutation (return Ok / push to
// skipped) is thereby killed.
#[test]
fn handle_row_failure_io_is_fatal_in_skip_mode() {
    let (_tmp, db) = create_test_db().unwrap();
    let importer = db.import().failure_mode(FailureMode::SkipAndReport);

    // Build a synthetic Io error by wrapping a std::io::Error (the variant holds its string).
    let io_err = std::io::Error::other("synthetic mid-stream I/O");
    let mut report = ImportReport::default();
    let result = importer.handle_row_failure(ImportError::Io(io_err.to_string()), &mut report);

    assert!(
        result.is_err(),
        "a synthetic mid-stream Io error must be fatal in skip mode, got: {result:?}"
    );
    // The Io error was NOT silently downgraded to a skipped row or unresolved endpoint.
    assert!(
        report.skipped.is_empty(),
        "Io must not be recorded as a skipped row"
    );
    assert!(
        report.unresolved_endpoints.is_empty(),
        "Io must not be recorded as an unresolved endpoint"
    );
}

// kills: removing the `if matches!(value, PropertyValue::Null) { continue; }` in
// build_properties. A blank cell for a non-string mapped column coerces to Null and must
// become an ABSENT property, not a stored null. Removing the `continue` would either
// store the property as null (making get() return Some) or fail the insert.
#[test]
fn blank_cell_becomes_absent_property_not_null() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // alice's `age` cell is blank -> coerces to Null -> should be skipped entirely.
    let nodes = write_file(&files, "nodes.csv", "id,name,age\nalice,Alice,\n");

    let mut importer = db.import();
    importer.nodes_from_csv(&nodes, person_nodes("id")).unwrap();
    let alice = importer.resolve_key("alice").unwrap();
    let node = db.get_node(alice).unwrap();

    // The blank Int cell must be absent, never present-as-null.
    assert_eq!(node.properties.get("age"), None);
    // The non-blank String property is still stored.
    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("Alice"))
    );
}

// kills: `chunk.len() >= self.config.batch_size` -> `>` on the NODE flush (import_nodes).
// Under Abort, batch_size(2) with [good1, good2, bad3]: the correct `>=` flushes the full
// chunk [good1, good2] as one committed transaction BEFORE bad3 aborts, so exactly 2
// nodes persist. The `>` mutant never reaches the boundary, so nothing is flushed before
// the abort and 0 nodes persist.
#[test]
fn node_flush_boundary_partial_commit_on_abort() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // Row 3 has a non-integer age -> malformed under Abort.
    let nodes = write_file(
        &files,
        "nodes.csv",
        "id,name,age\ng1,G1,1\ng2,G2,2\nbad,Bad,notanumber\n",
    );

    let mut importer = db.import().batch_size(2);
    let err = importer
        .nodes_from_csv(&nodes, person_nodes("id"))
        .expect_err("malformed row 3 must abort");
    assert!(err.to_string().contains("row 3"), "message: {err}");

    // The first full chunk flushed at the `>=` boundary before the abort.
    assert_eq!(db.node_count(), 2);
}

// kills: `chunk.len() >= self.config.batch_size` -> `>` on the EDGE flush (import_edges).
// Same partial-commit reasoning as the node side, exercised on the edge path.
#[test]
fn edge_flush_boundary_partial_commit_on_abort() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let nodes = write_file(&files, "nodes.csv", "id,name,age\na,A,1\nb,B,2\nc,C,3\n");
    // Edge row 3 has a non-integer `weight` -> a Row error AFTER endpoint resolution.
    let edges = write_file(
        &files,
        "edges.csv",
        "src,dst,weight\na,b,10\nb,c,20\nc,a,notanumber\n",
    );

    let mut importer = db.import().batch_size(2);
    importer.nodes_from_csv(&nodes, person_nodes("id")).unwrap();

    let edge_mapping = EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst").property(
        "weight",
        "weight",
        ColumnType::Int,
    );
    let err = importer
        .edges_from_csv(&edges, edge_mapping)
        .expect_err("malformed edge row 3 must abort");
    assert!(err.to_string().contains("row 3"), "message: {err}");

    // The first full edge chunk flushed at the `>=` boundary before the abort.
    assert_eq!(db.edge_count(), 2);
}

// kills: swapping the Source/Target endpoint side in prepare_edge. Existing tests only
// cover an unresolved TARGET; this pins the SOURCE side. src `ghost` is unresolved while
// the target resolves, so resolution must fail on the Source endpoint first.
#[test]
fn unresolved_source_endpoint_reported() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let nodes = write_file(&files, "nodes.csv", "id,name,age\nalice,Alice,30\n");
    // src `ghost` does not exist; dst `alice` does.
    let edges = write_file(&files, "edges.csv", "src,dst\nghost,alice\n");

    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    importer.nodes_from_csv(&nodes, person_nodes("id")).unwrap();
    let report = importer
        .edges_from_csv(
            &edges,
            EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst"),
        )
        .expect("skip mode");

    assert_eq!(report.edges_imported, 0);
    assert_eq!(report.unresolved_endpoints.len(), 1);
    let unresolved = &report.unresolved_endpoints[0];
    assert_eq!(unresolved.side, Endpoint::Source);
    assert_eq!(unresolved.key, "ghost");
    assert_eq!(unresolved.row, 1);
}

// kills: the empty-key guard in prepare_node (the `key column '...' is empty` branch and
// its precise row number).
#[test]
fn empty_key_column_errors_with_row_number() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // Row 1 has a blank `id` (key) cell.
    let nodes = write_file(&files, "nodes.csv", "id,name,age\n,Alice,30\n");

    let mut importer = db.import();
    let err = importer
        .nodes_from_csv(&nodes, person_nodes("id"))
        .expect_err("blank key must error");
    let msg = err.to_string();
    assert!(msg.contains("row 1"), "message: {msg}");
    assert!(msg.contains("key column 'id' is empty"), "message: {msg}");
    assert_eq!(db.node_count(), 0);
}

// kills: the empty-label guard in resolve_label (the `label column '...' is empty` branch
// and its precise row number), used when the label comes from a column.
#[test]
fn empty_label_column_errors_with_row_number() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // Row 1 has a blank `kind` (label) cell.
    let nodes = write_file(&files, "nodes.csv", "id,kind,name\nn1,,Alice\n");

    let mapping = NodeMapping::new(LabelSource::column("kind"), "id").property(
        "name",
        "name",
        ColumnType::String,
    );
    let mut importer = db.import();
    let err = importer
        .nodes_from_csv(&nodes, mapping)
        .expect_err("blank label must error");
    let msg = err.to_string();
    assert!(msg.contains("row 1"), "message: {msg}");
    assert!(
        msg.contains("label column 'kind' is empty"),
        "message: {msg}"
    );
    assert_eq!(db.node_count(), 0);
}

// kills: dropping the edge-side `extract_valid_time` in prepare_edge. Only node valid_time
// is currently pinned; this confirms an edge's per-row valid_time backfills and round-trips
// with a point-in-time edge read.
#[test]
fn edge_valid_time_backfill_round_trips_with_as_of() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let nodes = write_file(
        &files,
        "nodes.csv",
        "id,name,age\nalice,Alice,30\nbob,Bob,25\n",
    );
    // The KNOWS edge is valid from 2021-01-01.
    let edges = write_file(&files, "edges.csv", "src,dst,since\nalice,bob,2021-01-01\n");

    let mut importer = db.import();
    importer.nodes_from_csv(&nodes, person_nodes("id")).unwrap();
    let alice = importer.resolve_key("alice").unwrap();

    let edge_mapping =
        EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst").valid_time_column("since");
    importer.edges_from_csv(&edges, edge_mapping).unwrap();

    let out = db.get_outgoing_edges(alice);
    assert_eq!(out.len(), 1);
    let edge_id = out[0];

    let now = time::now();
    // 2021-06-01 (after valid_from): edge is visible.
    let after = time::from_secs(1_622_505_600);
    assert!(
        db.get_edge_at_time(edge_id, after, now).is_ok(),
        "edge should be valid after its valid_from"
    );
    // 2020-06-01 (before valid_from): edge is NOT yet valid.
    let before = time::from_secs(1_590_969_600);
    assert!(
        db.get_edge_at_time(edge_id, before, now).is_err(),
        "edge should not be valid before its valid_from"
    );
}

// DEFENSIVE (not a kill): documents that batch_size(0) still imports every row. This does
// NOT kill the `batch_size.max(1)` mutant: under the `chunk.len() >= batch_size` flush
// guard a batch_size of 0 flushes after every row regardless of whether `.max(1)` clamps to
// 1 or lets 0 through, so `.max(1)` is behaviorally unobservable here. Kept as a regression
// guard for the observable end-state (all rows imported).
#[test]
fn batch_size_zero_clamps_and_imports_all() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let mut csv = String::from("id,name,age\n");
    for i in 0..5 {
        csv.push_str(&format!("n{i},Name{i},{i}\n"));
    }
    let nodes = write_file(&files, "nodes.csv", &csv);

    let mut importer = db.import().batch_size(0);
    let report = importer.nodes_from_csv(&nodes, person_nodes("id")).unwrap();
    assert_eq!(report.nodes_imported, 5);
    assert_eq!(db.node_count(), 5);
}

// kills: the `if raw.trim().is_empty() { return Ok(None) }` blank-cell guard in
// extract_valid_time. A node row whose valid_time column is BLANK must fall back to
// transaction time (None), so the row still imports cleanly and is visible in current
// state — never rejected as a malformed timestamp.
#[test]
fn blank_valid_time_column_defaults_to_now_and_imports() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // alice's `known_since` cell is blank -> should default to import (tx) time.
    let nodes = write_file(
        &files,
        "nodes.csv",
        "id,name,age,known_since\nalice,Alice,30,\n",
    );

    let mapping = person_nodes("id").valid_time_column("known_since");
    let mut importer = db.import();
    let report = importer.nodes_from_csv(&nodes, mapping).unwrap();
    assert_eq!(report.nodes_imported, 1);
    assert!(report.skipped.is_empty());

    let alice = importer.resolve_key("alice").unwrap();
    // Visible in current state, exactly like a row with no valid_time column at all.
    assert!(db.get_node(alice).is_ok());
    let now = time::now();
    assert!(db.get_node_at_time(alice, now, now).is_ok());
}
