use aletheiadb::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Create a new database
    let db = AletheiaDB::new().unwrap();

    // Create nodes
    let alice_id = db.create_node("Person", properties! {
        "name" => "Alice",
        "age" => 30,
    })?;

    let bob_id = db.create_node("Person", properties! {
        "name" => "Bob",
    })?;

    // Create relationship
    db.create_edge(alice_id, bob_id, "KNOWS", properties! {})?;

    // Read current state
    let alice = db.get_node(alice_id)?;
    println!("Created Alice: {:?}", alice);
    println!("Label: {}", alice.label); // "Person"

    Ok(())
}
