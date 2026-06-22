//! Bulk graph import with valid-time backfill (Issue #3211).
//!
//! Run with:
//!
//! ```bash
//! cargo run --features import --example bulk_import
//! ```
//!
//! This example writes a small sample dataset (a node CSV carrying a per-row
//! `valid_time` column, plus an edge CSV that references nodes by business key),
//! bulk-loads both with **fewer than 15 lines of importer code**, and then issues an
//! `AS OF VALID_TIME` query proving the historical backfill round-trips.

use std::io::Write;

use aletheiadb::AletheiaDB;
use aletheiadb::api::import::{ColumnType, EdgeMapping, LabelSource, NodeMapping};
use aletheiadb::core::temporal::time;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- Sample dataset -----------------------------------------------------
    // Two people, each with the valid time at which the fact became true, and one
    // KNOWS edge that references them by business key ("id").
    let dir = tempfile::tempdir()?;
    let nodes_csv = dir.path().join("people.csv");
    let edges_csv = dir.path().join("knows.csv");

    write(
        &nodes_csv,
        "id,name,role,known_since\n\
         alice,Alice,Engineer,2021-01-01\n\
         bob,Bob,Designer,2023-06-15\n",
    )?;
    write(&edges_csv, "src,dst,since\nalice,bob,2023-06-15\n")?;

    // --- Bulk import (the < 15 lines of caller code) ------------------------
    let db = AletheiaDB::new()?;
    let mut importer = db.import();
    importer.nodes_from_csv(
        &nodes_csv,
        NodeMapping::new(LabelSource::fixed("Person"), "id")
            .property("name", "name", ColumnType::String)
            .property("role", "role", ColumnType::String)
            .valid_time_column("known_since"),
    )?;
    importer.edges_from_csv(
        &edges_csv,
        EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst").valid_time_column("since"),
    )?;

    println!(
        "Imported {} nodes and {} edges.",
        db.node_count(),
        db.edge_count()
    );

    // --- Prove the valid-time backfill with AS OF VALID_TIME ----------------
    let alice = importer.resolve_key("alice").expect("alice imported");
    let now = time::now();

    // 2022-01-01: Alice's fact (valid since 2021-01-01) is already true.
    let in_2022 = time::from_secs(1_640_995_200);
    let alice_2022 = db.get_node_at_time(alice, in_2022, now);

    // 2020-01-01: before Alice's valid_from — she is not yet a known fact.
    let in_2020 = time::from_secs(1_577_836_800);
    let alice_2020 = db.get_node_at_time(alice, in_2020, now);

    println!("\nAS OF VALID_TIME round-trip:");
    println!(
        "  2022-01-01 -> Alice present: {} (expected true)",
        alice_2022.is_ok()
    );
    println!(
        "  2020-01-01 -> Alice present: {} (expected false)",
        alice_2020.is_ok()
    );

    assert!(alice_2022.is_ok(), "Alice should exist in 2022");
    assert!(alice_2020.is_err(), "Alice should not exist in 2020");
    println!("\nBackfill verified: the imported history is time-travelable.");

    Ok(())
}

fn write(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(contents.as_bytes())
}
