use aletheiadb::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
let db = AletheiaDB::new().unwrap();
let alice_id = db.create_node("Person", properties! { "name" => "Alice", "age" => 30 })?;

// Explicit read transaction
let result = db.read(|tx| {
    tx.get_node(alice_id).map(|node| node.label.clone())
})?;

// Explicit write transaction with multiple operations
db.write(|tx| {
    let node1 = tx.create_node("Event", PropertyMap::new())?;
    let node2 = tx.create_node("Event", PropertyMap::new())?;
    tx.create_edge(node1, node2, "FOLLOWS", PropertyMap::new())
})?;
Ok(())
}
