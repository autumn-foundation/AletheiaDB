//! Narrative Generation Example (Story Demo)
//!
//! This example demonstrates how to use the experimental Narrative Generation feature
//! to create natural language histories of graph nodes.
//!
//! # Prerequisites
//!
//! This feature is experimental and requires the `semantic-temporal` feature flag.
//!
//! ## Running this example
//!
//! ```bash
//! cargo run --features semantic-temporal --example story_demo
//! ```
//!
//! ## Using in your project
//!
//! Add the `semantic-temporal` feature to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! aletheiadb = { version = "0.1", features = ["semantic-temporal"] }
//! ```

use aletheiadb::AletheiaDB;
use aletheiadb::WriteOps;
// ⚠️ REQUIRES FEATURE 'SEMANTIC-TEMPORAL' IN CARGO.TOML
// [dependencies]
// aletheiadb = { version = "0.1", features = ["semantic-temporal"] }
use aletheiadb::experimental::temporal_narrative::NarrativeGenerator;
use aletheiadb::properties;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = AletheiaDB::new()?;

    // 1. Create Node
    let node_id = db.create_node(
        "Person",
        properties! {
            "name" => "Alice",
            "age" => 30,
        },
    )?;

    // 2. Update Node
    db.write(|tx| {
        tx.update_node(
            node_id,
            properties! {
                "name" => "Alice",
                "age" => 31,
                "city" => "London",
            },
        )
    })?;

    // 3. Generate Narrative
    let generator = NarrativeGenerator::new(&db);
    let narrative = generator.generate_node_narrative(node_id)?;

    for event in narrative {
        println!("Version {}: {}", event.version_number, event.description);
        for change in event.changes {
            println!("  - {}", change);
        }
    }

    Ok(())
}
