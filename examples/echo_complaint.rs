//! 🗣️ Echo Complaint: The "InternedString" Friction
//!
//! This example demonstrates a friction point for new users:
//! Printing a node's label requires understanding the internal `GLOBAL_INTERNER`.
//!
//! Run with: cargo run --example echo_complaint

use gallifreydb::{GallifreyDB, PropertyMapBuilder};
// Users have to find and import this to make sense of their data?
use gallifreydb::{GLOBAL_INTERNER, WriteOps};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🗣️ Echo is trying to debug a node...");

    let db = GallifreyDB::new()?;

    // Create a node
    let node_id = db.write(|tx| {
        tx.create_node(
            "Supercalifragilisticexpialidocious",
            PropertyMapBuilder::new()
                .insert("meaning", "something to say when you have nothing to say")
                .build(),
        )
    })?;

    // Read it back
    let node = db.get_node(node_id)?;

    // 1. The Naive User Approach (What I expect to work)
    println!("\nAttempt 1: println!(\"{{}}\", node.label)");
    println!("Result: {}", node.label); // Expectation: "Supercalifragilisticexpialidocious", Reality: "Interned(10)"

    // 2. The "Read the Docs" Approach (The Friction)
    println!("\nAttempt 2: The Boilerplate Dance");
    let resolved = GLOBAL_INTERNER
        .resolve_with(node.label, |s| s.to_string())
        .expect("Why would it be None if I just got it from the DB?");

    println!("Result: {}", resolved);

    println!("\n📢 Echo says: 'Why do I care that it is interned? I just want the string!'");

    Ok(())
}
