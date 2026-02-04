use gallifreydb::{GallifreyDB, PropertyMap, PropertyMapBuilder, WriteOps};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a new database
    let db = GallifreyDB::new().unwrap();

    // Create nodes using write transactions
    let alice_id = db.write(|tx| {
        tx.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30)
                .build(),
        )
    })?;

    let bob_id = db.write(|tx| {
        tx.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
    })?;

    // Create relationships
    db.write(|tx| tx.create_edge(alice_id, bob_id, "KNOWS", PropertyMap::new()))?;

    // Read current state
    let alice = db.get_node(alice_id)?;

    Ok(())
}
