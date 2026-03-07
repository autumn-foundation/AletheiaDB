// ⚠️ REQUIRES FEATURE: nova
// [dependencies]
// aletheiadb = { version = "0.1", features = ["nova"] }

use aletheiadb::prelude::*;
use aletheiadb::experimental::temporal_narrative::NarrativeGenerator;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 1. Setup database and node (for self-contained example)
    let db = AletheiaDB::new().unwrap();
    let node_id = db.write(|tx| {
        tx.create_node("Person", properties! {
            "name" => "Alice"
        })
    })?;

    // 2. Generate natural language history of a node
    let generator = NarrativeGenerator::new(&db);
    let narrative = generator.generate_node_narrative(node_id)?;

    for event in narrative {
        println!("Version {}: {}", event.version_number, event.description);
        // Output: "Version 1: Node created with label 'Person'."

        for change in event.changes {
            println!("  - {}", change);
            // Output: "  - Initial property 'name': '"Alice"'"
        }
    }

    Ok(())
}
