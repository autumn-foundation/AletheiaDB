//! AletheiaDB 60-second guided demo (Issue #3380).
//!
//! A single command that seeds a small, story-driven **bi-temporal** dataset
//! and walks you through a guided sequence of queries — including a time-travel
//! (`AS OF`) query whose answer visibly differs from the current state.
//!
//! Run it (nothing is written to disk — the database is ephemeral):
//!
//! ```bash
//! cargo run --example demo
//! ```
//!
//! This is the Rust-native quickstart path. It needs no server and no API key.
//! For the MCP (agent-issued) and HTTP (authenticated) paths, see
//! `docs/guides/quickstart.md`.
//!
//! The demo is intentionally deterministic: every query printed below succeeds
//! verbatim against the seeded data, and the flow is exercised in CI
//! (`tests/quickstart_demo.rs`) so it can never silently drift.

use std::time::Instant;

use aletheiadb::prelude::*;
use aletheiadb::time;

/// Convenience alias for the example's fallible functions. We spell out
/// `std::result::Result` because `aletheiadb::prelude` re-exports its own
/// `Result`/`Error`, which would otherwise shadow the std trait object.
type DemoResult = std::result::Result<(), Box<dyn std::error::Error>>;

/// Named core cast so the guided queries are legible and deterministic. The
/// wider org (a few hundred filler nodes/edges) is generated around them so the
/// dataset is "on the order of hundreds of nodes/edges", per the issue.
const FILLER_ENGINEERS: usize = 200;

fn main() -> DemoResult {
    let started = Instant::now();

    banner();

    // ── Seed a story-driven, bi-temporal dataset ────────────────────────────
    // Ephemeral: AletheiaDB::new() is a tempdir-backed database that leaves
    // nothing in your working directory. Use AletheiaDB::open("./path") when
    // you want durability.
    let db = AletheiaDB::new()?;

    // t_founding: the moment the company is founded and Alice is hired as an
    // Engineer. We capture this timestamp so we can time-travel back to it.
    let company = db.create_node("Company", properties! { "name" => "Aletheia Labs" })?;

    let alice = db.create_node(
        "Person",
        properties! { "name" => "Alice", "title" => "Engineer" },
    )?;
    let bob = db.create_node(
        "Person",
        properties! { "name" => "Bob", "title" => "Engineer" },
    )?;
    let carol = db.create_node(
        "Person",
        properties! { "name" => "Carol", "title" => "Designer" },
    )?;

    db.create_edge(alice, company, "WORKS_AT", properties! {})?;
    db.create_edge(bob, company, "WORKS_AT", properties! {})?;
    db.create_edge(carol, company, "WORKS_AT", properties! {})?;
    db.create_edge(alice, bob, "KNOWS", properties! {})?;
    db.create_edge(bob, carol, "KNOWS", properties! {})?;

    // The wider org: a few hundred colleagues, so temporal/graph queries run
    // against a realistically sized graph rather than a toy of three nodes.
    seed_wider_org(&db, company)?;

    // Snapshot the founding moment *after* the initial hire so a query "as of
    // founding" sees Alice as an Engineer.
    let t_founding = time::now();

    // ── The story unfolds: facts change over time ───────────────────────────
    // Alice is promoted twice. Each update creates a new bi-temporal version;
    // the old versions are preserved and remain queryable forever.
    settle(); // guarantee a distinct transaction timestamp for the next version
    db.write(|tx| {
        tx.update_node(
            alice,
            properties! { "name" => "Alice", "title" => "Staff Engineer" },
        )
    })?;
    settle();
    db.write(|tx| tx.update_node(alice, properties! { "name" => "Alice", "title" => "CTO" }))?;

    println!(
        "Seeded {} nodes and {} edges with genuine version history.\n",
        db.node_count(),
        db.edge_count()
    );

    // ── Guided query 1: current-state lookup ────────────────────────────────
    section(
        1,
        "Current-state lookup",
        "let alice = db.get_node(alice_id)?;",
    );
    let alice_now = db.get_node(alice)?;
    let title_now = prop_str(&alice_now, "title");
    println!("  → Alice is currently: {title_now}");
    println!("  what you just saw: the *latest* fact — sub-microsecond, no temporal overhead.\n");

    // This is the "time to first query" mark the issue defines.
    let ttfq = started.elapsed();

    // ── Guided query 2: a traversal ─────────────────────────────────────────
    section(
        2,
        "Traversal (who does Alice know?)",
        "for e in db.get_outgoing_edges_with_label(alice_id, \"KNOWS\") { .. }",
    );
    for edge_id in db.get_outgoing_edges_with_label(alice, "KNOWS") {
        let target = db.get_edge_target(edge_id)?;
        let person = db.get_node(target)?;
        println!(
            "  → Alice KNOWS {} ({})",
            prop_str(&person, "name"),
            prop_str(&person, "title")
        );
    }
    println!("  what you just saw: a single-hop graph traversal over the current graph.\n");

    // ── Guided query 3: AS OF time-travel (the differentiator) ──────────────
    section(
        3,
        "Time-travel — AS OF the founding day",
        "let past = db.get_node_at_time(alice_id, t_founding, t_founding)?;",
    );
    let alice_founding = db.get_node_at_time(alice, t_founding, t_founding)?;
    let title_founding = prop_str(&alice_founding, "title");
    println!("  → On founding day, Alice was: {title_founding}");
    println!(
        "  what you just saw: the SAME node, a different answer — \"{title_founding}\" then vs \"{title_now}\" now."
    );
    assert_ne!(
        title_founding, title_now,
        "the time-travel query must differ from current state"
    );
    println!("  (No incumbent graph DB can show you this in its 60-second demo.)\n");

    // ── Guided query 4: full history / provenance ───────────────────────────
    section(
        4,
        "History — every version of a fact",
        "for v in db.get_node_history(alice_id)?.versions { .. }",
    );
    let history = db.get_node_history(alice)?;
    for v in &history.versions {
        println!(
            "  → v{}: title = {}",
            v.version_number,
            v.properties
                .get("title")
                .map(display_value)
                .unwrap_or_else(|| "<none>".to_string())
        );
    }
    println!(
        "  what you just saw: {} preserved versions of one node — the audit trail is free.\n",
        history.versions.len()
    );

    // ── Timing summary (honest: run-time only; build is one-time) ───────────
    let total = started.elapsed();
    println!("────────────────────────────────────────────────────────");
    println!(
        "Time to first query (seed + current-state lookup): {} ms",
        ttfq.as_millis()
    );
    println!(
        "Full guided sequence (4 queries):                  {} ms",
        total.as_millis()
    );
    println!("Target: < 60_000 ms. (One-time `cargo build` is separate and not counted.)");
    println!("────────────────────────────────────────────────────────");
    println!("\nNext: open a durable database with `AletheiaDB::open(\"./mydb\")`,");
    println!("or point an MCP client at AletheiaDB — see docs/guides/quickstart.md.");

    Ok(())
}

/// Prints the demo banner. Loudly labels the data as an ephemeral demo.
fn banner() {
    println!("════════════════════════════════════════════════════════");
    println!("  AletheiaDB — 60-second bi-temporal demo");
    println!("  Ephemeral demo data (in-memory; nothing written to disk)");
    println!("════════════════════════════════════════════════════════\n");
}

/// Prints a guided-query section header plus the copy-paste snippet.
fn section(n: u8, title: &str, snippet: &str) {
    println!("── Query {n} of 4: {title} ──");
    println!("  {snippet}");
}

/// Generates a few hundred colleague nodes/edges around the named cast so the
/// dataset is realistically sized. Kept deterministic (no randomness).
fn seed_wider_org(db: &AletheiaDB, company: NodeId) -> DemoResult {
    let mut previous: Option<NodeId> = None;
    for i in 0..FILLER_ENGINEERS {
        let person = db.create_node(
            "Person",
            properties! { "name" => format!("Engineer {i}"), "title" => "Engineer" },
        )?;
        db.create_edge(person, company, "WORKS_AT", properties! {})?;
        if let Some(prev) = previous {
            db.create_edge(prev, person, "KNOWS", properties! {})?;
        }
        previous = Some(person);
    }
    Ok(())
}

/// Ensures the next write lands on a strictly later transaction timestamp than
/// the previous one, so point-in-time reads resolve to distinct versions even
/// on very fast machines.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(2));
}

/// Extracts a string-valued property, or a placeholder if missing.
fn prop_str(node: &aletheiadb::Node, key: &str) -> String {
    node.properties
        .get(key)
        .map(display_value)
        .unwrap_or_else(|| "<none>".to_string())
}

/// Renders a `PropertyValue` for display without leaking Debug formatting.
fn display_value(value: &PropertyValue) -> String {
    match value {
        PropertyValue::String(s) => s.to_string(),
        PropertyValue::Int(i) => i.to_string(),
        PropertyValue::Float(f) => f.to_string(),
        PropertyValue::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}
