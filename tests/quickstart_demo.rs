//! CI guard for the 60-second quickstart (Issue #3380).
//!
//! This test mirrors the guided sequence in `examples/demo.rs` and the
//! copy-paste blocks in `docs/guides/quickstart.md`, asserting that:
//!
//! 1. every guided query succeeds against the seeded data (zero-error path),
//! 2. the `AS OF` time-travel answer visibly differs from current state,
//! 3. the whole seed-plus-query flow completes well under the 60-second
//!    time-to-first-query (TTFQ) budget the project targets.
//!
//! `examples/demo.rs` itself is compiled by CI's `clippy --all-targets` step
//! and run end-to-end by `.github/workflows/quickstart.yml`; this test locks
//! the *behavior* those surfaces promise so the funnel's first page can never
//! silently drift.

use std::time::Instant;

use aletheiadb::prelude::*;
use aletheiadb::time;

/// The TTFQ budget from the project brief and Issue #3380.
const TTFQ_BUDGET_MS: u128 = 60_000;

/// Mirror of the demo's wider-org filler count so the graph is realistically
/// sized ("on the order of hundreds of nodes/edges").
const FILLER_ENGINEERS: usize = 200;

/// Extracts a string-valued property, or `<none>` if absent.
fn prop_str(node: &aletheiadb::Node, key: &str) -> String {
    match node.properties.get(key) {
        Some(PropertyValue::String(s)) => s.to_string(),
        Some(other) => format!("{other:?}"),
        None => "<none>".to_string(),
    }
}

#[test]
fn quickstart_guided_sequence_succeeds_within_budget() {
    let started = Instant::now();

    // ── Seed the same story-driven, bi-temporal dataset as the demo ─────────
    let db = AletheiaDB::new().expect("ephemeral database should open");

    let company = db
        .create_node("Company", properties! { "name" => "Aletheia Labs" })
        .expect("create company");
    let alice = db
        .create_node(
            "Person",
            properties! { "name" => "Alice", "title" => "Engineer" },
        )
        .expect("create Alice");
    let bob = db
        .create_node(
            "Person",
            properties! { "name" => "Bob", "title" => "Engineer" },
        )
        .expect("create Bob");
    let carol = db
        .create_node(
            "Person",
            properties! { "name" => "Carol", "title" => "Designer" },
        )
        .expect("create Carol");

    db.create_edge(alice, company, "WORKS_AT", properties! {})
        .expect("Alice WORKS_AT");
    db.create_edge(bob, company, "WORKS_AT", properties! {})
        .expect("Bob WORKS_AT");
    db.create_edge(carol, company, "WORKS_AT", properties! {})
        .expect("Carol WORKS_AT");
    db.create_edge(alice, bob, "KNOWS", properties! {})
        .expect("Alice KNOWS Bob");
    db.create_edge(bob, carol, "KNOWS", properties! {})
        .expect("Bob KNOWS Carol");

    let mut previous: Option<NodeId> = None;
    for i in 0..FILLER_ENGINEERS {
        let person = db
            .create_node(
                "Person",
                properties! { "name" => format!("Engineer {i}"), "title" => "Engineer" },
            )
            .expect("create filler engineer");
        db.create_edge(person, company, "WORKS_AT", properties! {})
            .expect("filler WORKS_AT");
        if let Some(prev) = previous {
            db.create_edge(prev, person, "KNOWS", properties! {})
                .expect("filler KNOWS");
        }
        previous = Some(person);
    }

    // Capture the founding moment, then let facts change over time.
    let t_founding = time::now();
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.write(|tx| {
        tx.update_node(
            alice,
            properties! { "name" => "Alice", "title" => "Staff Engineer" },
        )
    })
    .expect("promote Alice to Staff Engineer");
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.write(|tx| tx.update_node(alice, properties! { "name" => "Alice", "title" => "CTO" }))
        .expect("promote Alice to CTO");

    // Dataset is realistically sized.
    assert!(
        db.node_count() >= 200,
        "expected hundreds of nodes, got {}",
        db.node_count()
    );
    assert!(
        db.edge_count() >= 200,
        "expected hundreds of edges, got {}",
        db.edge_count()
    );

    // ── Query 1: current-state lookup ───────────────────────────────────────
    let alice_now = db.get_node(alice).expect("get current Alice");
    let title_now = prop_str(&alice_now, "title");
    assert_eq!(title_now, "CTO", "current title should be the latest fact");

    // ── Query 2: traversal ──────────────────────────────────────────────────
    let mut known: Vec<String> = Vec::new();
    for edge_id in db.get_outgoing_edges_with_label(alice, "KNOWS") {
        let target = db.get_edge_target(edge_id).expect("resolve KNOWS target");
        let person = db.get_node(target).expect("load known person");
        known.push(prop_str(&person, "name"));
    }
    assert!(
        known.contains(&"Bob".to_string()),
        "Alice should KNOW Bob, got {known:?}"
    );

    // ── Query 3: AS OF time-travel — must differ from current state ──────────
    let alice_founding = db
        .get_node_at_time(alice, t_founding, t_founding)
        .expect("time-travel to founding day");
    let title_founding = prop_str(&alice_founding, "title");
    assert_eq!(
        title_founding, "Engineer",
        "on founding day Alice was an Engineer"
    );
    assert_ne!(
        title_founding, title_now,
        "the AS OF answer must visibly differ from current state"
    );

    // ── Query 4: full version history ───────────────────────────────────────
    let history = db.get_node_history(alice).expect("load Alice history");
    let titles: Vec<String> = history
        .versions
        .iter()
        .map(|v| match v.properties.get("title") {
            Some(PropertyValue::String(s)) => s.to_string(),
            _ => "<none>".to_string(),
        })
        .collect();
    assert_eq!(
        titles,
        vec![
            "Engineer".to_string(),
            "Staff Engineer".to_string(),
            "CTO".to_string()
        ],
        "history should preserve every version of the title"
    );

    // ── TTFQ budget gate ────────────────────────────────────────────────────
    let elapsed_ms = started.elapsed().as_millis();
    assert!(
        elapsed_ms < TTFQ_BUDGET_MS,
        "quickstart flow took {elapsed_ms} ms, exceeding the {TTFQ_BUDGET_MS} ms TTFQ budget"
    );
}
