//! Debug test for temporal node lookup issue #306

use gallifreydb::*;
use std::thread;
use std::time::Duration;

#[test]
fn test_temporal_lookup_directly() {
    let db = GallifreyDB::with_config(storage::version::AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    });

    // Create a node
    let node_id = db
        .create_node(
            "Test",
            PropertyMapBuilder::new().insert("value", "v1").build(),
        )
        .unwrap();
    println!("Created node {:?} with value='v1'", node_id);

    // Wait and record timestamp
    thread::sleep(Duration::from_millis(100));
    let t1 = core::temporal::time::now();
    println!("Recorded t1={}", t1);

    // Update the node
    thread::sleep(Duration::from_millis(100));
    db.write(|tx| {
        tx.update_node(
            node_id,
            PropertyMapBuilder::new().insert("value", "v2").build(),
        )?;
        Ok(())
    })
    .unwrap();
    println!("Updated node to value='v2'");

    let t2 = core::temporal::time::now();
    println!("Current time t2={}", t2);

    // Check current state
    let current = db.get_node(node_id).unwrap();
    println!(
        "Current value: {:?}",
        current.get_property("value").and_then(|v| v.as_str())
    );

    // Try temporal lookup at t1
    println!("\n=== Attempting temporal lookup at t1={} ===", t1);
    let query = db.query().as_of(t1, t1).start(node_id).build();

    match db.execute_query(query) {
        Ok(results) => {
            let rows: Vec<_> = results.collect_all().unwrap();
            println!("Got {} results", rows.len());
            if !rows.is_empty() {
                let node = rows[0].entity.as_node().unwrap();
                println!(
                    "Historical value: {:?}",
                    node.get_property("value").and_then(|v| v.as_str())
                );
                assert_eq!(
                    node.get_property("value").and_then(|v| v.as_str()),
                    Some("v1"),
                    "Should return historical value 'v1', not current value 'v2'"
                );
            }
        }
        Err(e) => {
            panic!("Query failed: {:?}", e);
        }
    }
}
