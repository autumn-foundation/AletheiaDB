use aletheiadb::prelude::*;
use aletheiadb::time;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Setup: Create a node
    let db = AletheiaDB::new().unwrap();
    let alice_id = db.create_node("Person", properties! { "name" => "Alice", "age" => 30 })?;

    // Get current time
    let now = time::now();

    // Get node at a specific point in time
    let historical_alice = db.get_node_at_time(
        alice_id,
        now,  // valid time
        now,  // transaction time
    )?;

    // Track how properties changed
    println!("Alice's age was: {:?}", historical_alice.properties.get("age"));

    Ok(())
}
