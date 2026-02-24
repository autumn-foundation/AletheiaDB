use aletheiadb::prelude::*;
use aletheiadb::time;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Basic Graph Operations

    // Create a new database
    let db = AletheiaDB::new().unwrap();

    // Create nodes
    let alice_id = db.create_node(
        "Person",
        properties! {
            "name" => "Alice",
            "age" => 30,
        },
    )?;

    let bob_id = db.create_node(
        "Person",
        properties! {
            "name" => "Bob",
        },
    )?;

    // Create relationship
    db.create_edge(alice_id, bob_id, "KNOWS", properties! {})?;

    // Read current state
    let alice = db.get_node(alice_id)?;
    println!("Created Alice: {:?}", alice);
    println!("Label: {}", alice.label); // "Person"

    // Time-Travel Queries

    // Get current time
    let now = time::now();

    // Get node at a specific point in time
    let historical_alice = db.get_node_at_time(
        alice_id, now, // valid time
        now, // transaction time
    )?;

    // Track how properties changed
    println!(
        "Alice's age was: {:?}",
        historical_alice.properties.get("age")
    );

    Ok(())
}
