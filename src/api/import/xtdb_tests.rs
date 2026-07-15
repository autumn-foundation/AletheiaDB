//! Tests for the XTDB entity-history importer (Issue #3384).

use std::io::Write;
use std::path::PathBuf;

use tempfile::TempDir;

use super::parse::parse_valid_time;
use super::xtdb::XtdbOptions;
use super::*;
use crate::core::property::PropertyValue;
use crate::core::temporal::Timestamp;
use crate::test_utils::create_test_db;

/// Path to a committed golden fixture under `src/api/import/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/api/import/fixtures")
        .join(name)
}

/// Write `contents` to `name` inside `dir`, returning its path.
fn write_file(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    let mut file = std::fs::File::create(&path).expect("create temp file");
    file.write_all(contents.as_bytes())
        .expect("write temp file");
    path
}

fn ts(s: &str) -> Timestamp {
    parse_valid_time(s).unwrap()
}

fn title_prop(n: crate::core::graph::Node) -> Option<String> {
    n.properties.get("title").and_then(|v| match v {
        crate::core::property::PropertyValue::String(s) => Some(s.to_string()),
        _ => None,
    })
}

/// Reconstruct a node's `title` at valid time `s`, using **current knowledge**
/// (transaction time = now).
fn title_at(db: &AletheiaDB, id: NodeId, s: &str) -> Option<String> {
    db.get_node_at_valid_time(id, ts(s))
        .ok()
        .and_then(title_prop)
}

/// Reconstruct a node's `title` at a full bi-temporal coordinate — anchoring
/// **both** dimensions, which is required to recall a valid segment that has
/// since been superseded by a later-recorded version (the documented XTDB
/// migration caveat).
fn title_at_tx(db: &AletheiaDB, id: NodeId, valid: &str, tx: Timestamp) -> Option<String> {
    db.get_node_at_time(id, ts(valid), tx)
        .ok()
        .and_then(title_prop)
}

/// Transaction time at which version `i` (0-based, oldest first) was recorded.
fn version_tx(db: &AletheiaDB, id: NodeId, i: usize) -> Timestamp {
    db.get_node_history(id).unwrap().versions[i]
        .temporal
        .transaction_time()
        .start()
}

// ---- 1: golden round-trip (committed fixture file) -------------------------

#[test]
fn xtdb_basic_round_trip() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    let report = importer
        .xtdb_import(fixture("xtdb_history_basic.edn"), &XtdbOptions::default())
        .expect("xtdb import");

    assert_eq!(report.source, "xtdb");
    assert_eq!(report.entities_read, 2);
    assert_eq!(report.nodes_created, 2);
    // Alice: create + update (2 live) ; Acme: 1.
    assert_eq!(report.node_versions_written, 3);
    assert_eq!(report.edges_created, 1);
    assert!(report.zero_loss, "unexpected loss: {report:?}");
}

// ---- 2: supersession -------------------------------------------------------

#[test]
fn xtdb_supersession() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    importer
        .xtdb_import(fixture("xtdb_history_basic.edn"), &XtdbOptions::default())
        .expect("xtdb import");
    let alice = importer.resolve_key("alice").expect("alice resolves");

    let history = db.get_node_history(alice).unwrap();
    // create(Engineer) + update(CEO) + delete tombstone.
    assert_eq!(history.version_count(), 3);

    // The Engineer segment is superseded — recall it by anchoring the tx time to
    // when it was recorded (v0). The CEO segment is current knowledge.
    let v0_tx = version_tx(&db, alice, 0);
    assert_eq!(
        title_at_tx(&db, alice, "2022-06-01T00:00:00Z", v0_tx).as_deref(),
        Some("Engineer")
    );
    assert_eq!(
        title_at(&db, alice, "2023-06-01T00:00:00Z").as_deref(),
        Some("CEO")
    );
}

// ---- 3: delete retracts (+ edge co-retraction) -----------------------------

#[test]
fn xtdb_delete_retracts() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    let report = importer
        .xtdb_import(fixture("xtdb_history_basic.edn"), &XtdbOptions::default())
        .expect("xtdb import");
    let alice = importer.resolve_key("alice").expect("alice resolves");

    // Before delete: still valid.
    assert_eq!(
        title_at(&db, alice, "2024-05-01T00:00:00Z").as_deref(),
        Some("CEO")
    );
    // After delete (2024-06-01): no longer valid.
    assert!(
        db.get_node_at_valid_time(alice, ts("2024-07-01T00:00:00Z"))
            .is_err()
    );

    // Node retraction + co-retracted EMPLOYER edge counted.
    assert!(
        report.retractions >= 2,
        "retractions: {}",
        report.retractions
    );
}

// ---- 4: ref becomes edge ---------------------------------------------------

#[test]
fn xtdb_ref_becomes_edge() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // A live (non-deleted) alice → acme employer ref, so the edge is current.
    let edn = "[{:xt/id :alice :history [{:xtdb.api/valid-time #inst \"2021-01-01T00:00:00Z\" \
                :xtdb.api/tx-id 1 :xtdb.api/doc {:xt/id :alice :type \"Person\" :employer :acme}}]}\n \
                {:xt/id :acme :history [{:xtdb.api/valid-time #inst \"2020-01-01T00:00:00Z\" \
                :xtdb.api/tx-id 2 :xtdb.api/doc {:xt/id :acme :type \"Company\"}}]}]";
    let path = write_file(&files, "ref.edn", edn);
    let mut importer = db.import();
    importer
        .xtdb_import(&path, &XtdbOptions::default())
        .expect("xtdb import");
    let alice = importer.resolve_key("alice").expect("alice");
    let acme = importer.resolve_key("acme").expect("acme");

    use crate::api::transaction::ReadOps;
    let tx = db.read_transaction().unwrap();
    let out = tx.get_outgoing_edges(alice).unwrap();
    assert_eq!(out.len(), 1, "alice should have one EMPLOYER edge");
    let edge = tx.get_edge(out[0]).unwrap();
    assert_eq!(edge.source, alice);
    assert_eq!(edge.target, acme);
    assert_eq!(edge.label.to_string(), "EMPLOYER");
}

// ---- 5: vector-of-refs fan-out ---------------------------------------------

#[test]
fn xtdb_vector_of_refs_fanout() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let edn = "[{:xt/id :a :history [{:xtdb.api/valid-time #inst \"2021-01-01T00:00:00Z\" \
                :xtdb.api/tx-id 1 :xtdb.api/doc {:xt/id :a :type \"P\" :knows [:b :c]}}]}\n \
                {:xt/id :b :history [{:xtdb.api/valid-time #inst \"2021-01-01T00:00:00Z\" \
                :xtdb.api/tx-id 2 :xtdb.api/doc {:xt/id :b :type \"P\"}}]}\n \
                {:xt/id :c :history [{:xtdb.api/valid-time #inst \"2021-01-01T00:00:00Z\" \
                :xtdb.api/tx-id 3 :xtdb.api/doc {:xt/id :c :type \"P\"}}]}]";
    let path = write_file(&files, "fanout.edn", edn);
    let mut importer = db.import();
    let report = importer
        .xtdb_import(&path, &XtdbOptions::default())
        .expect("import");
    assert_eq!(
        report.edges_created, 2,
        "knows [:b :c] should fan out to 2 edges"
    );
}

// ---- disjoint valid intervals: honor the close, report reincarnation --------

#[test]
fn xtdb_disjoint_intervals_close_and_report() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // Live A [2021-01, 2022-01), GAP, live B [2023-01, ...). The engine cannot
    // reincarnate a retracted node (retraction removes it from current state),
    // so A's close is honored (in-gap coordinate correctly ABSENT, not a stale
    // fact) and B is reported as disjoint_reincarnation — never silently dropped.
    let edn = "[{:xt/id :a :history [\
        {:xtdb.api/valid-time #inst \"2021-01-01T00:00:00Z\" \
         :xtdb.api/valid-to #inst \"2022-01-01T00:00:00Z\" \
         :xtdb.api/tx-id 1 :xtdb.api/doc {:xt/id :a :type \"P\" :title \"A\"}}\
        {:xtdb.api/valid-time #inst \"2023-01-01T00:00:00Z\" \
         :xtdb.api/tx-id 2 :xtdb.api/doc {:xt/id :a :type \"P\" :title \"B\"}}]}]";
    let path = write_file(&files, "disjoint.edn", edn);
    let mut importer = db.import();
    let report = importer
        .xtdb_import(&path, &XtdbOptions::default())
        .expect("import");
    let a = importer.resolve_key("a").unwrap();

    // Pre-gap window returns A at current knowledge (after the close).
    assert_eq!(
        title_at(&db, a, "2021-06-01T00:00:00Z").as_deref(),
        Some("A")
    );
    // In-gap coordinate is ABSENT — the intermediate close is honored.
    assert!(
        db.get_node_at_valid_time(a, ts("2022-06-01T00:00:00Z"))
            .is_err(),
        "in-gap coordinate must be absent, not a stale fact"
    );
    // The un-revivable later segment is reported, not silently dropped.
    assert!(
        report
            .unsupported
            .iter()
            .any(|u| u.kind == "disjoint_reincarnation"),
        "reincarnation must be reported: {report:?}"
    );
    assert!(!report.zero_loss);
}

// ---- durable re-import refusal (AC5) ---------------------------------------

#[test]
fn xtdb_reimport_refused_across_fresh_session() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let edn = "[{:xt/id :x :history [{:xtdb.api/valid-time #inst \"2021-01-01T00:00:00Z\" \
                :xtdb.api/tx-id 1 :xtdb.api/doc {:xt/id :x :type \"P\" :name \"X\"}}]}]";
    let path = write_file(&files, "x.edn", edn);

    // First import via one importer session.
    {
        let mut importer = db.import();
        importer
            .xtdb_import(&path, &XtdbOptions::default())
            .expect("first import");
    }
    // A brand-new importer (empty in-memory key map) simulates a fresh process
    // against the now-populated database — the DURABLE guard must still refuse.
    let mut importer2 = db.import();
    let err = importer2
        .xtdb_import(&path, &XtdbOptions::default())
        .expect_err("durable refusal across a fresh session");
    assert!(err.to_string().contains("AlreadyImported"), "got: {err}");
}

// ---- 6: provenance recorded ------------------------------------------------

#[test]
fn xtdb_provenance_recorded() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    importer
        .xtdb_import(fixture("xtdb_history_basic.edn"), &XtdbOptions::default())
        .expect("import");
    let alice = importer.resolve_key("alice").unwrap();
    let history = db.get_node_history(alice).unwrap();
    let first = &history.versions[0];
    let prov = first
        .provenance
        .as_ref()
        .expect("first version has provenance");
    assert!(prov.source().unwrap().starts_with("xtdb-import::"));
    assert!(prov.note().unwrap().contains("tx-id=42"));
    assert_eq!(prov.correlation_id(), Some("xt:tx:42"));
}

// ---- 7: default label ------------------------------------------------------

#[test]
fn xtdb_default_label() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let edn = "[{:xt/id :x :history [{:xtdb.api/valid-time #inst \"2021-01-01T00:00:00Z\" \
                :xtdb.api/tx-id 1 :xtdb.api/doc {:xt/id :x :name \"X\"}}]}]";
    let path = write_file(&files, "nolabel.edn", edn);
    let mut importer = db.import();
    let report = importer
        .xtdb_import(&path, &XtdbOptions::default())
        .expect("import");
    let x = importer.resolve_key("x").unwrap();
    assert_eq!(db.get_node(x).unwrap().label.to_string(), "Entity");
    assert!(report.coerced.iter().any(|c| c.kind == "default_label"));
}

// ---- 8: re-import refused (idempotency) ------------------------------------

#[test]
fn xtdb_reimport_refused() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    importer
        .xtdb_import(fixture("xtdb_history_basic.edn"), &XtdbOptions::default())
        .expect("first import");
    let err = importer
        .xtdb_import(fixture("xtdb_history_basic.edn"), &XtdbOptions::default())
        .expect_err("second import must be refused");
    assert!(err.to_string().contains("AlreadyImported"), "got: {err}");
}

// ---- 9: version order enforced (driver sorts) ------------------------------

#[test]
fn xtdb_version_order_enforced() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // CEO segment written BEFORE the Engineer segment; the driver must sort by
    // (valid-time, tx-id) so the replay applies Engineer then CEO.
    let edn = "[{:xt/id :a :history [\
        {:xtdb.api/valid-time #inst \"2023-01-01T00:00:00Z\" :xtdb.api/tx-id 2 \
         :xtdb.api/doc {:xt/id :a :type \"P\" :title \"CEO\"}}\
        {:xtdb.api/valid-time #inst \"2021-01-01T00:00:00Z\" :xtdb.api/tx-id 1 \
         :xtdb.api/doc {:xt/id :a :type \"P\" :title \"Engineer\"}}]}]";
    let path = write_file(&files, "unordered.edn", edn);
    let mut importer = db.import();
    importer
        .xtdb_import(&path, &XtdbOptions::default())
        .expect("import");
    let a = importer.resolve_key("a").unwrap();
    // Despite the source order, v0 must be Engineer (valid 2021), v1 CEO (2023).
    let hist = db.get_node_history(a).unwrap();
    assert_eq!(
        hist.versions[0].properties.get("title"),
        Some(&PropertyValue::string("Engineer"))
    );
    assert_eq!(
        hist.versions[1].properties.get("title"),
        Some(&PropertyValue::string("CEO"))
    );
    // Reconstruct the (superseded) Engineer era by anchoring both dimensions.
    let v0_tx = version_tx(&db, a, 0);
    assert_eq!(
        title_at_tx(&db, a, "2021-06-01T00:00:00Z", v0_tx).as_deref(),
        Some("Engineer")
    );
    assert_eq!(
        title_at(&db, a, "2023-06-01T00:00:00Z").as_deref(),
        Some("CEO")
    );
}

// ---- 19: AS-OF equivalence grid (>= 20 coordinates) ------------------------

#[test]
fn xtdb_as_of_grid() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    importer
        .xtdb_import(fixture("xtdb_history_basic.edn"), &XtdbOptions::default())
        .expect("import");
    let alice = importer.resolve_key("alice").unwrap();

    let v0_tx = version_tx(&db, alice, 0); // Engineer recorded
    let v1_tx = version_tx(&db, alice, 1); // CEO recorded (before the delete)

    // (valid_time, tx anchor, expected title | None = not visible)
    // >= 20 bi-temporal coordinates spanning before-create / each era / the
    // supersession boundary / after-delete, anchoring the tx dimension where a
    // superseded segment must be recalled.
    let coords: &[(&str, Timestamp, Option<&str>)] = &[
        // Engineer era, recalled at v0's transaction time.
        ("2020-06-01T00:00:00Z", v0_tx, None), // before create
        ("2021-02-28T00:00:00Z", v0_tx, None), // 1 day before create
        ("2021-03-01T00:00:00Z", v0_tx, Some("Engineer")),
        ("2021-06-01T00:00:00Z", v0_tx, Some("Engineer")),
        ("2021-12-31T00:00:00Z", v0_tx, Some("Engineer")),
        ("2022-01-01T00:00:00Z", v0_tx, Some("Engineer")),
        ("2022-06-01T00:00:00Z", v0_tx, Some("Engineer")),
        ("2022-12-31T00:00:00Z", v0_tx, Some("Engineer")),
        // CEO era, recalled at v1's transaction time (before the delete).
        ("2023-01-01T00:00:00Z", v1_tx, Some("CEO")), // supersession boundary
        ("2023-02-01T00:00:00Z", v1_tx, Some("CEO")),
        ("2023-06-01T00:00:00Z", v1_tx, Some("CEO")),
        ("2023-12-31T00:00:00Z", v1_tx, Some("CEO")),
        ("2024-01-01T00:00:00Z", v1_tx, Some("CEO")),
        ("2024-05-31T00:00:00Z", v1_tx, Some("CEO")),
        // At v1's tx the delete has not been recorded yet, so CEO extends on.
        ("2030-01-01T00:00:00Z", v1_tx, Some("CEO")),
        // Current knowledge (tx = now): only the CEO era survives, and the
        // delete has closed the valid interval at 2024-06-01.
        (
            "2023-06-01T00:00:00Z",
            crate::core::temporal::time::now(),
            Some("CEO"),
        ),
        (
            "2024-05-31T00:00:00Z",
            crate::core::temporal::time::now(),
            Some("CEO"),
        ),
        (
            "2024-06-01T00:00:00Z",
            crate::core::temporal::time::now(),
            None,
        ), // delete boundary
        (
            "2024-07-01T00:00:00Z",
            crate::core::temporal::time::now(),
            None,
        ), // after delete
        (
            "2025-01-01T00:00:00Z",
            crate::core::temporal::time::now(),
            None,
        ),
        // Engineer era is NOT current knowledge (superseded).
        (
            "2022-06-01T00:00:00Z",
            crate::core::temporal::time::now(),
            None,
        ),
    ];
    assert!(coords.len() >= 20);
    for (when, tx, expected) in coords {
        match expected {
            Some(title) => assert_eq!(
                title_at_tx(&db, alice, when, *tx).as_deref(),
                Some(*title),
                "at valid {when} expected {title}"
            ),
            None => assert!(
                db.get_node_at_time(alice, ts(when), *tx).is_err(),
                "at valid {when} alice should not be visible"
            ),
        }
    }
}

// ---- 27: skip-and-report mode ----------------------------------------------

#[test]
fn xtdb_skip_and_report_mode() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // Second entity is malformed (no :history) — skipped in SkipAndReport.
    let edn = "[{:xt/id :good :history [{:xtdb.api/valid-time #inst \"2021-01-01T00:00:00Z\" \
                :xtdb.api/tx-id 1 :xtdb.api/doc {:xt/id :good :type \"P\" :name \"G\"}}]}\n \
                {:xt/id :bad}]";
    let path = write_file(&files, "mixed.edn", edn);

    // Abort (default) fails.
    {
        let mut importer = db.import();
        assert!(
            importer
                .xtdb_import(&path, &XtdbOptions::default())
                .is_err()
        );
    }
    // Skip-and-report collects the bad entity, imports the good one.
    let (_tmp2, db2) = create_test_db().unwrap();
    let mut importer = db2.import();
    let opts = XtdbOptions::default().failure_mode(FailureMode::SkipAndReport);
    let report = importer
        .xtdb_import(&path, &opts)
        .expect("skip mode imports");
    assert_eq!(report.nodes_created, 1);
    assert_eq!(report.skipped.len(), 1);
    assert!(!report.zero_loss);
}

// ---- 20 / E.5: inline scale fixture (1_000 entities x 10 versions) ----------
// AC4: >= 10K historical versions with BOTH supersessions and retractions. A
// fraction of entities (e % 100 == 0) get a terminal nil-doc delete, so the
// fixture mixes 10 superseding versions per entity with 10 terminal retractions.

#[test]
fn xtdb_scale_smoke_1k_entities_10k_versions() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let is_retracted = |e: usize| e % 100 == 0;

    let mut buf = String::from("[");
    for e in 0..1_000 {
        buf.push_str(&format!("{{:xt/id :n{e} :history ["));
        for v in 0..10 {
            buf.push_str(&format!(
                "{{:xtdb.api/valid-time #inst \"20{:02}-01-01T00:00:00Z\" \
                   :xtdb.api/tx-time #inst \"20{:02}-01-01T00:00:00Z\" :xtdb.api/tx-id {} \
                   :xtdb.api/doc {{:xt/id :n{e} :type \"Person\" :name \"N{e}\" :title \"T{v}\"}}}}",
                10 + v,
                10 + v,
                e * 10 + v
            ));
        }
        // A terminal delete (retraction) at 2019-06-01 for a fraction of entities.
        if is_retracted(e) {
            buf.push_str(&format!(
                "{{:xtdb.api/valid-time #inst \"2019-06-01T00:00:00Z\" \
                   :xtdb.api/tx-time #inst \"2019-06-01T00:00:00Z\" :xtdb.api/tx-id {} \
                   :xtdb.api/doc nil}}",
                e * 10 + 100
            ));
        }
        buf.push_str("]}");
    }
    buf.push(']');
    let path = write_file(&files, "scale.edn", &buf);

    let mut importer = db.import();
    let report = importer
        .xtdb_import(&path, &XtdbOptions::default())
        .expect("scale import");
    assert_eq!(report.entities_read, 1_000);
    assert_eq!(report.nodes_created, 1_000);
    assert_eq!(report.node_versions_written, 10_000);
    // The terminal deletes are retractions (interval closes), not fresh versions.
    assert_eq!(report.retractions, 10); // 10 retracted entities (e % 100 == 0), no edges
    assert!(report.zero_loss, "handled deletes keep the import lossless");

    // Sample >= 20 AS-OF coordinates across eras and entities. Each era is a
    // superseded segment, so anchor the tx dimension to that version's record
    // time.
    let mut sampled = 0;
    for e in (0..1_000).step_by(100) {
        let id = importer.resolve_key(&format!("n{e}")).unwrap();
        for v in [0usize, 5, 9] {
            let vtx = version_tx(&db, id, v);
            let when = format!("20{:02}-06-01T00:00:00Z", 10 + v);
            assert_eq!(
                title_at_tx(&db, id, &when, vtx).as_deref(),
                Some(format!("T{v}").as_str()),
                "entity n{e} at era {v}"
            );
            sampled += 1;
        }
        // These sampled entities (e % 100 == 0) are all RETRACTED at 2019-06-01:
        // present (T9) just before the delete, absent just after.
        assert!(is_retracted(e));
        assert_eq!(
            title_at(&db, id, "2019-03-01T00:00:00Z").as_deref(),
            Some("T9"),
            "retracted entity n{e} present before its delete"
        );
        assert!(
            db.get_node_at_valid_time(id, ts("2019-09-01T00:00:00Z"))
                .is_err(),
            "retracted entity n{e} absent after its delete"
        );
    }
    // A non-retracted entity's latest era survives at current knowledge.
    let live = importer.resolve_key("n50").unwrap();
    assert_eq!(
        title_at(&db, live, "2019-06-01T00:00:00Z").as_deref(),
        Some("T9")
    );
    assert!(sampled >= 20, "sampled {sampled} coordinates");
}
