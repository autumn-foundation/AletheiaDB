//! Narrative Generation Example (Story Demo)
//!
//! This example demonstrates how to use the experimental Narrative Generation feature
//! to create natural language histories of graph nodes.
//!
//! # Prerequisites
//!
//! This feature is experimental and requires the `nova` feature flag.
//!
//! ## Running this example
//!
//! ```bash
//! cargo run --features nova --example story_demo
//! ```
//!
//! ## Using in your project
//!
//! Add the `nova` feature to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! gallifreydb = { version = "0.1", features = ["nova"] }
//! ```

use gallifreydb::GallifreyDB;
use gallifreydb::PropertyMapBuilder;
use gallifreydb::WriteOps;
use gallifreydb::experimental::temporal_narrative::NarrativeGenerator;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = GallifreyDB::new()?;

    // 1. Create Node
    let props1 = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();
    let node_id = db.create_node("Person", props1)?;

    // 2. Update Node
    db.write(|tx| {
        let props2 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 31i64)
            .insert("city", "London")
            .build();
        tx.update_node(node_id, props2)
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
