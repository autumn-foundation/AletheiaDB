//! Chronoscope Demo
//!
//! This example demonstrates the Chronoscope TUI for visualizing temporal history.
//!
//! Run with:
//! cargo run --example chronoscope_demo --features nova

#[cfg(feature = "nova")]
use gallifreydb::core::temporal::time;
#[cfg(feature = "nova")]
use gallifreydb::experimental::chronoscope::ChronoscopeApp;
#[cfg(feature = "nova")]
use gallifreydb::{GallifreyDB, PropertyMapBuilder, WriteOps};
#[cfg(feature = "nova")]
use std::thread;
#[cfg(feature = "nova")]
use std::time::Duration;

#[cfg(feature = "nova")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Initializing GallifreyDB...");
    let db = GallifreyDB::new()?;

    // Generate some history
    println!("Generating synthetic history...");

    // Create a node
    let node_id = db.write(|tx| {
        tx.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("status", "Created")
                .build(),
        )
    })?;

    // Update it multiple times
    for i in 1..=5 {
        // Sleep to get different transaction times
        thread::sleep(Duration::from_millis(10));

        db.write(|tx| {
            let props = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("status", format!("Update {}", i))
                .insert("step", i as i64)
                .build();
            tx.update_node(node_id, props)
        })?;
    }

    // Create another node
    let node_id2 = db.write(|tx| {
        tx.create_node(
            "Project",
            PropertyMapBuilder::new()
                .insert("name", "Secret Nova")
                .insert("priority", "High")
                .build(),
        )
    })?;

    // Update it
    db.write(|tx| {
        tx.update_node(
            node_id2,
            PropertyMapBuilder::new()
                .insert("name", "Secret Nova")
                .insert("priority", "Critical")
                .build(),
        )
    })?;

    println!("Starting Chronoscope TUI...");
    println!("Controls: Up/Down to select, Q to quit.");

    // Run the TUI
    // Note: In a real terminal this works. In CI/Sandbox it might fail or exit immediately
    // if no TTY.
    // We catch errors to avoid crashing the test runner if run there.
    if let Err(e) = ChronoscopeApp::new(&db)?.run() {
        eprintln!("TUI Error: {}", e);
        eprintln!("(This is expected if running in a non-interactive environment)");
    }

    Ok(())
}

#[cfg(not(feature = "nova"))]
fn main() {
    println!("This example requires the 'nova' feature.");
    println!("Run with: cargo run --example chronoscope_demo --features nova");
}
