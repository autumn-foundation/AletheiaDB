//! Tests for the Datomic datom-stream importer (Issue #3384).

use std::io::Write;
use std::path::PathBuf;

use tempfile::TempDir;

use super::datomic::{DatomicOptions, ManyScalarPolicy};
use super::parse::parse_valid_time;
use super::*;
use crate::core::property::PropertyValue;
use crate::core::temporal::Timestamp;
use crate::test_utils::create_test_db;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/api/import/fixtures")
        .join(name)
}

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
        PropertyValue::String(s) => Some(s.to_string()),
        _ => None,
    })
}

/// Reconstruct `title` at valid time `s` using current knowledge (tx = now).
fn title_at(db: &AletheiaDB, id: NodeId, s: &str) -> Option<String> {
    db.get_node_at_valid_time(id, ts(s))
        .ok()
        .and_then(title_prop)
}

/// Reconstruct `title` at a full bi-temporal coordinate (anchoring both
/// dimensions) — required to recall a since-superseded valid segment.
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

// ---- 10: golden round-trip (committed fixture files) -----------------------

#[test]
fn datomic_basic_round_trip() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    let report = importer
        .datomic_import(
            fixture("datomic_log_basic.edn"),
            fixture("datomic_schema_basic.edn"),
            &DatomicOptions::default(),
        )
        .expect("datomic import");

    assert_eq!(report.source, "datomic");
    assert_eq!(report.entities_read, 2);
    assert_eq!(report.nodes_created, 2);
    // Company: 1 tx; Person: 2 tx.
    assert_eq!(report.node_versions_written, 3);
    assert_eq!(report.edges_created, 1);
    assert!(report.zero_loss, "unexpected loss: {report:?}");

    let person = importer.resolve_key("200").unwrap();
    let company = importer.resolve_key("100").unwrap();
    assert_eq!(db.get_node(company).unwrap().label.to_string(), "Company");
    assert_eq!(db.get_node(person).unwrap().label.to_string(), "Person");
    assert_eq!(
        db.get_node(person).unwrap().properties.get("db/id"),
        Some(&PropertyValue::Int(200))
    );
}

// ---- 11: supersession (retract + assert in one tx) -------------------------

#[test]
fn datomic_supersession() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    importer
        .datomic_import(
            fixture("datomic_log_basic.edn"),
            fixture("datomic_schema_basic.edn"),
            &DatomicOptions::default(),
        )
        .expect("import");
    let person = importer.resolve_key("200").unwrap();

    assert_eq!(db.get_node_history(person).unwrap().version_count(), 2);
    // Engineer is superseded — recall it by anchoring the tx dimension to v0.
    let v0_tx = version_tx(&db, person, 0);
    assert_eq!(
        title_at_tx(&db, person, "2021-06-01T00:00:00Z", v0_tx).as_deref(),
        Some("Engineer")
    );
    assert_eq!(
        title_at(&db, person, "2023-06-01T00:00:00Z").as_deref(),
        Some("CEO")
    );
}

// ---- 12: ref edge from :db.type/ref ----------------------------------------

#[test]
fn datomic_ref_edge() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    importer
        .datomic_import(
            fixture("datomic_log_basic.edn"),
            fixture("datomic_schema_basic.edn"),
            &DatomicOptions::default(),
        )
        .expect("import");
    let person = importer.resolve_key("200").unwrap();
    let company = importer.resolve_key("100").unwrap();

    use crate::api::transaction::ReadOps;
    let tx = db.read_transaction().unwrap();
    let out = tx.get_outgoing_edges(person).unwrap();
    assert_eq!(out.len(), 1);
    let edge = tx.get_edge(out[0]).unwrap();
    assert_eq!(edge.source, person);
    assert_eq!(edge.target, company);
    assert_eq!(edge.label.to_string(), "EMPLOYER");
}

// ---- 13: txInstant -> valid_from -------------------------------------------

#[test]
fn datomic_txinstant_valid_from() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    importer
        .datomic_import(
            fixture("datomic_log_basic.edn"),
            fixture("datomic_schema_basic.edn"),
            &DatomicOptions::default(),
        )
        .expect("import");
    let person = importer.resolve_key("200").unwrap();
    // The first version's valid_from is its txInstant (2021-03-01). Anchoring the
    // tx dimension to v0, Engineer is reconstructable at/after 2021-03-01 and not
    // one day earlier.
    let v0_tx = version_tx(&db, person, 0);
    assert!(
        db.get_node_at_time(person, ts("2021-02-28T00:00:00Z"), v0_tx)
            .is_err()
    );
    assert_eq!(
        title_at_tx(&db, person, "2021-03-01T00:00:00Z", v0_tx).as_deref(),
        Some("Engineer")
    );
}

// ---- 14: valid_time_attr redirect ------------------------------------------

#[test]
fn datomic_valid_time_attr_redirect() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // txInstant is 2025, but the domain effective-date is 2020.
    let datoms = "[{:e 1 :a :person/name :v \"Alice\" :tx 1 \
        :tx-instant #inst \"2025-01-01T00:00:00Z\" :op true}\n \
        {:e 1 :a :person/effective-date :v #inst \"2020-01-01T00:00:00Z\" :tx 1 \
        :tx-instant #inst \"2025-01-01T00:00:00Z\" :op true}]";
    let schema = "{:person/name {:db/valueType :db.type/string :db/cardinality :db.cardinality/one}\n \
        :person/effective-date {:db/valueType :db.type/instant :db/cardinality :db.cardinality/one}}";
    let dpath = write_file(&files, "d.edn", datoms);
    let spath = write_file(&files, "s.edn", schema);

    let opts = DatomicOptions::default().valid_time_attr("person/effective-date");
    let mut importer = db.import();
    importer
        .datomic_import(&dpath, &spath, &opts)
        .expect("import");
    let alice = importer.resolve_key("1").unwrap();

    // Valid from the domain date (2020), not the txInstant (2025).
    assert!(
        db.get_node_at_valid_time(alice, ts("2020-06-01T00:00:00Z"))
            .is_ok()
    );
    // The redirect attr is consumed, not stored as a property.
    assert!(
        db.get_node(alice)
            .unwrap()
            .properties
            .get("effective-date")
            .is_none()
    );
}

// ---- 15: scalar retract -> explicit null (reported) ------------------------

#[test]
fn datomic_attr_retract_as_null() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let datoms = "[{:e 1 :a :person/name :v \"Alice\" :tx 1 \
        :tx-instant #inst \"2021-01-01T00:00:00Z\" :op true}\n \
        {:e 1 :a :person/title :v \"Engineer\" :tx 1 \
        :tx-instant #inst \"2021-01-01T00:00:00Z\" :op true}\n \
        {:e 1 :a :person/title :v \"Engineer\" :tx 2 \
        :tx-instant #inst \"2022-01-01T00:00:00Z\" :op false}]";
    let schema = "{:person/name {:db/valueType :db.type/string :db/cardinality :db.cardinality/one}\n \
        :person/title {:db/valueType :db.type/string :db/cardinality :db.cardinality/one}}";
    let dpath = write_file(&files, "d.edn", datoms);
    let spath = write_file(&files, "s.edn", schema);

    let mut importer = db.import();
    let report = importer
        .datomic_import(&dpath, &spath, &DatomicOptions::default())
        .expect("import");
    let alice = importer.resolve_key("1").unwrap();

    assert!(
        report
            .unsupported
            .iter()
            .any(|u| u.kind == "attr_retract_as_null")
    );
    assert!(!report.zero_loss);
    assert_eq!(
        db.get_node(alice).unwrap().properties.get("title"),
        Some(&PropertyValue::Null)
    );
}

// ---- 16: unknown attribute reported (scalar, not a ref) --------------------

#[test]
fn datomic_unknown_attribute_reported() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let datoms = "[{:e 1 :a :person/name :v \"Alice\" :tx 1 \
        :tx-instant #inst \"2021-01-01T00:00:00Z\" :op true}\n \
        {:e 1 :a :person/nickname :v \"Al\" :tx 1 \
        :tx-instant #inst \"2021-01-01T00:00:00Z\" :op true}]";
    let schema =
        "{:person/name {:db/valueType :db.type/string :db/cardinality :db.cardinality/one}}";
    let dpath = write_file(&files, "d.edn", datoms);
    let spath = write_file(&files, "s.edn", schema);

    let mut importer = db.import();
    let report = importer
        .datomic_import(&dpath, &spath, &DatomicOptions::default())
        .expect("import");
    let alice = importer.resolve_key("1").unwrap();

    assert!(
        report
            .unsupported
            .iter()
            .any(|u| u.kind == "unknown_attribute")
    );
    // Still stored as a scalar string property (never silently guessed a ref).
    assert_eq!(
        db.get_node(alice).unwrap().properties.get("nickname"),
        Some(&PropertyValue::string("Al"))
    );
}

// ---- 17: unsupported reported (schema history / db/fn / excise) ------------

#[test]
fn datomic_unsupported_reported() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    let report = importer
        .datomic_import(
            fixture("datomic_unsupported.edn"),
            fixture("datomic_schema_basic.edn"),
            &DatomicOptions::default(),
        )
        .expect("import");

    assert!(
        report
            .unsupported
            .iter()
            .any(|u| u.kind == "schema_history")
    );
    assert!(report.unsupported.iter().any(|u| u.kind == "db_fn"));
    assert!(report.unsupported.iter().any(|u| u.kind == "excision"));
    assert!(!report.zero_loss);
    // Only the one domain entity (company 100) is created.
    assert_eq!(report.nodes_created, 1);
}

// ---- 18: cardinality-many scalar policy ------------------------------------

#[test]
fn datomic_cardinality_many_scalar_policy() {
    let files = TempDir::new().unwrap();
    let datoms = "[{:e 1 :a :person/name :v \"Alice\" :tx 1 \
        :tx-instant #inst \"2021-01-01T00:00:00Z\" :op true}\n \
        {:e 1 :a :person/alias :v \"Al\" :tx 1 \
        :tx-instant #inst \"2021-01-01T00:00:00Z\" :op true}\n \
        {:e 1 :a :person/alias :v \"Ali\" :tx 1 \
        :tx-instant #inst \"2021-01-01T00:00:00Z\" :op true}]";
    let schema = "{:person/name {:db/valueType :db.type/string :db/cardinality :db.cardinality/one}\n \
        :person/alias {:db/valueType :db.type/string :db/cardinality :db.cardinality/many}}";

    // LastValue policy → last asserted alias wins.
    {
        let (_tmp, db) = create_test_db().unwrap();
        let dpath = write_file(&files, "d1.edn", datoms);
        let spath = write_file(&files, "s1.edn", schema);
        let mut importer = db.import();
        importer
            .datomic_import(&dpath, &spath, &DatomicOptions::default())
            .expect("import");
        let alice = importer.resolve_key("1").unwrap();
        assert_eq!(
            db.get_node(alice).unwrap().properties.get("alias"),
            Some(&PropertyValue::string("Ali"))
        );
    }
    // List policy → both aliases accumulate into an array.
    {
        let (_tmp, db) = create_test_db().unwrap();
        let dpath = write_file(&files, "d2.edn", datoms);
        let spath = write_file(&files, "s2.edn", schema);
        let opts = DatomicOptions::default().cardinality_many_scalar(ManyScalarPolicy::List);
        let mut importer = db.import();
        importer
            .datomic_import(&dpath, &spath, &opts)
            .expect("import");
        let alice = importer.resolve_key("1").unwrap();
        let node = db.get_node(alice).unwrap();
        match node.properties.get("alias") {
            Some(PropertyValue::Array(a)) => assert_eq!(a.len(), 2),
            other => panic!("expected array of 2 aliases, got {other:?}"),
        }
    }
}

// ---- 28: unresolved ref endpoint -------------------------------------------

#[test]
fn datomic_ref_unresolved_endpoint() {
    let files = TempDir::new().unwrap();
    let datoms = "[{:e 1 :a :person/name :v \"Alice\" :tx 1 \
        :tx-instant #inst \"2021-01-01T00:00:00Z\" :op true}\n \
        {:e 1 :a :person/employer :v 999 :tx 1 \
        :tx-instant #inst \"2021-01-01T00:00:00Z\" :op true}]";
    let schema = "{:person/name {:db/valueType :db.type/string :db/cardinality :db.cardinality/one}\n \
        :person/employer {:db/valueType :db.type/ref :aletheia/edge-label \"EMPLOYER\"}}";

    // Abort (default): unresolved endpoint fails the import.
    {
        let (_tmp, db) = create_test_db().unwrap();
        let dpath = write_file(&files, "d1.edn", datoms);
        let spath = write_file(&files, "s1.edn", schema);
        let mut importer = db.import();
        assert!(
            importer
                .datomic_import(&dpath, &spath, &DatomicOptions::default())
                .is_err()
        );
    }
    // Skip-and-report: collected in the report, node still imported.
    {
        let (_tmp, db) = create_test_db().unwrap();
        let dpath = write_file(&files, "d2.edn", datoms);
        let spath = write_file(&files, "s2.edn", schema);
        let opts = DatomicOptions::default().failure_mode(FailureMode::SkipAndReport);
        let mut importer = db.import();
        let report = importer
            .datomic_import(&dpath, &spath, &opts)
            .expect("import");
        assert_eq!(report.unresolved_endpoints.len(), 1);
        assert_eq!(report.nodes_created, 1);
    }
}

// ---- 19b: AS-OF equivalence grid (>= 20 coordinates) -----------------------

#[test]
fn datomic_as_of_grid() {
    let (_tmp, db) = create_test_db().unwrap();
    let mut importer = db.import();
    importer
        .datomic_import(
            fixture("datomic_log_basic.edn"),
            fixture("datomic_schema_basic.edn"),
            &DatomicOptions::default(),
        )
        .expect("import");
    let person = importer.resolve_key("200").unwrap();
    let v0_tx = version_tx(&db, person, 0); // Engineer recorded
    let now = crate::core::temporal::time::now();

    // (valid_time, tx anchor, expected title | None). The Engineer era is
    // superseded, so anchor it to v0's tx; the CEO era is current knowledge.
    let coords: &[(&str, Timestamp, Option<&str>)] = &[
        ("2020-06-01T00:00:00Z", v0_tx, None),
        ("2021-02-28T00:00:00Z", v0_tx, None),
        ("2021-03-01T00:00:00Z", v0_tx, Some("Engineer")),
        ("2021-04-01T00:00:00Z", v0_tx, Some("Engineer")),
        ("2021-06-01T00:00:00Z", v0_tx, Some("Engineer")),
        ("2021-09-01T00:00:00Z", v0_tx, Some("Engineer")),
        ("2021-12-31T00:00:00Z", v0_tx, Some("Engineer")),
        ("2022-06-01T00:00:00Z", v0_tx, Some("Engineer")),
        ("2022-12-31T00:00:00Z", v0_tx, Some("Engineer")),
        // At v0's tx the Engineer fact is still open-ended into the future.
        ("2030-01-01T00:00:00Z", v0_tx, Some("Engineer")),
        // CEO era at current knowledge.
        ("2023-01-01T00:00:00Z", now, Some("CEO")),
        ("2023-02-01T00:00:00Z", now, Some("CEO")),
        ("2023-06-01T00:00:00Z", now, Some("CEO")),
        ("2023-12-31T00:00:00Z", now, Some("CEO")),
        ("2024-01-01T00:00:00Z", now, Some("CEO")),
        ("2024-06-01T00:00:00Z", now, Some("CEO")),
        ("2025-01-01T00:00:00Z", now, Some("CEO")),
        ("2030-01-01T00:00:00Z", now, Some("CEO")),
        // Before creation / superseded segment at current knowledge.
        ("2019-01-01T00:00:00Z", now, None),
        ("2021-06-01T00:00:00Z", now, None), // Engineer era not current
        ("2020-01-01T00:00:00Z", v0_tx, None),
    ];
    assert!(coords.len() >= 20);
    for (when, tx, expected) in coords {
        match expected {
            Some(title) => assert_eq!(
                title_at_tx(&db, person, when, *tx).as_deref(),
                Some(*title),
                "at valid {when} expected {title}"
            ),
            None => assert!(
                db.get_node_at_time(person, ts(when), *tx).is_err(),
                "at valid {when} person should not be visible"
            ),
        }
    }
}

// ---- scale: 1_000 entities in a flat datom stream --------------------------

#[test]
fn datomic_scale_smoke_1k_entities() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let mut buf = String::from("[");
    for e in 0..1_000 {
        for v in 0..10 {
            buf.push_str(&format!(
                "{{:e {} :a :person/title :v \"T{v}\" :tx {} \
                   :tx-instant #inst \"20{:02}-01-01T00:00:00Z\" :op true}}\n",
                1000 + e,
                (e * 10 + v) + 100_000,
                10 + v
            ));
        }
    }
    buf.push(']');
    let schema =
        "{:person/title {:db/valueType :db.type/string :db/cardinality :db.cardinality/one}}";
    let dpath = write_file(&files, "scale.edn", &buf);
    let spath = write_file(&files, "schema.edn", schema);

    let mut importer = db.import();
    let report = importer
        .datomic_import(&dpath, &spath, &DatomicOptions::default())
        .expect("scale import");
    assert_eq!(report.entities_read, 1_000);
    assert_eq!(report.nodes_created, 1_000);
    assert_eq!(report.node_versions_written, 10_000);
    assert!(report.zero_loss);

    let mut sampled = 0;
    for e in (0..1_000).step_by(100) {
        let id = importer.resolve_key(&(1000 + e).to_string()).unwrap();
        for v in [0usize, 5, 9] {
            let vtx = version_tx(&db, id, v);
            let when = format!("20{:02}-06-01T00:00:00Z", 10 + v);
            assert_eq!(
                title_at_tx(&db, id, &when, vtx).as_deref(),
                Some(format!("T{v}").as_str())
            );
            sampled += 1;
        }
        // Latest era is current knowledge.
        assert_eq!(
            title_at(&db, id, "2019-06-01T00:00:00Z").as_deref(),
            Some("T9")
        );
    }
    assert!(sampled >= 20);
}
