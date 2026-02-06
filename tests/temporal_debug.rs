//! Debug test for temporal node lookup issue #306

use aletheiadb::Error;
use aletheiadb::*;
use std::thread;
use std::time::Duration;

#[test]
fn test_temporal_lookup_directly() {
    let db = AletheiaDB::with_config(storage::version::AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    })
    .unwrap();

    // Create a node and get the timestamp AFTER it's committed
    let node_id = db
        .create_node(
            "Test",
            PropertyMapBuilder::new().insert("value", "v1").build(),
        )
        .unwrap();
    println!("Created node {:?} with value='v1'", node_id);

    // Get the timestamp of the first version from HistoricalStorage
    let (t1_valid, t1_tx) = {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        let version_id = hist_guard.get_current_node_version(node_id).unwrap();
        let version = hist_guard.get_node_version(version_id).unwrap();
        // Use a timestamp between this version's start and the next update for valid_time
        let valid = (version.temporal.valid_time().start().wallclock() + 1).into();
        // Use a timestamp after the version was recorded for tx_time
        let tx = (version.temporal.transaction_time().start().wallclock() + 1000).into();
        (valid, tx)
    };
    println!(
        "Using query timestamp t1_valid={}, t1_tx={} (just after first version)",
        t1_valid, t1_tx
    );

    // Update the node
    thread::sleep(Duration::from_millis(100));
    db.write(|tx| {
        tx.update_node(
            node_id,
            PropertyMapBuilder::new().insert("value", "v2").build(),
        )?;
        Ok::<_, Error>(())
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

    // Debug: check what versions exist in historical storage
    println!("\n=== Dumping version chain ===");
    {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        if let Some(head_version_id) = hist_guard.get_current_node_version(node_id) {
            println!("Head version: {:?}", head_version_id);
            let mut current_id = head_version_id;
            let mut index = 0;
            loop {
                if let Some(version) = hist_guard.get_node_version(current_id) {
                    println!(
                        "Version {}: id={:?}, temporal={}, prev={:?}",
                        index, current_id, version.temporal, version.prev_version
                    );
                    if let Some(prev) = version.prev_version {
                        current_id = prev;
                        index += 1;
                    } else {
                        break;
                    }
                } else {
                    println!("Version {:?} not found!", current_id);
                    break;
                }
            }
        } else {
            println!("No version head found for node {:?}", node_id);
        }
    }

    // Try finding version manually
    println!("\n=== Manual version lookup ===");
    {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        match hist_guard.find_node_version_at_time(node_id, t1_valid, t1_tx) {
            Some(version_id) => {
                let version = hist_guard.get_node_version(version_id).unwrap();
                println!("find_node_version_at_time returned: {:?}", version_id);
                println!("Version temporal: {}", version.temporal);
                println!(
                    "Version is_visible_at(valid={}, tx={})? {}",
                    t1_valid,
                    t1_tx,
                    version.temporal.is_visible_at(t1_valid, t1_tx)
                );
            }
            None => println!("find_node_version_at_time returned None"),
        }
    }

    // Try temporal lookup with proper bi-temporal coordinates
    println!(
        "\n=== Attempting temporal lookup at valid={}, tx={} ===",
        t1_valid, t1_tx
    );
    let query = db.query().as_of(t1_valid, t1_tx).start(node_id).build();

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
