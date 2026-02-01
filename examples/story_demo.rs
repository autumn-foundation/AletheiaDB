use gallifreydb::GallifreyDB;
use gallifreydb::experimental::temporal_narrative::NarrativeGenerator;
use gallifreydb::PropertyMapBuilder;
use gallifreydb::WriteOps;

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
