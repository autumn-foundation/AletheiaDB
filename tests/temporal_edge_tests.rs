//! Temporal edge lookup tests
//!
//! Tests for bi-temporal edge queries, including:
//! - Point-in-time edge lookups
//! - Edge property versioning
//! - Temporal interval closing for edges
//! - Edge version chain integrity

use gallifreydb::*;
use std::thread;
use std::time::Duration;

#[test]
fn test_temporal_edge_lookup_basic() {
    let db = GallifreyDB::with_config(storage::version::AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    });

    // Increased delay to ensure background flush thread has started (fixes CI race condition)
    // On heavily loaded CI systems, the default 10ms may not be sufficient for thread scheduling.
    // Using 50ms provides better reliability without significantly impacting test execution time.
    thread::sleep(Duration::from_millis(50));

    // Create nodes
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    // Create an edge with initial properties
    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new()
                .insert("since", 2020i64)
                .insert("strength", "strong")
                .build(),
        )
        .unwrap();
    println!(
        "Created edge {:?} with since=2020, strength='strong'",
        edge_id
    );

    // Get the timestamp of the first version from HistoricalStorage
    let t1 = {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        let version_id = hist_guard.get_current_edge_version(edge_id).unwrap();
        let version = hist_guard.get_edge_version(version_id).unwrap();
        (version.temporal.valid_time().start().wallclock() + 1).into()
    };
    println!("Using query timestamp t1={} (just after first version)", t1);

    // Update the edge
    thread::sleep(Duration::from_millis(100));
    db.write(|tx| {
        tx.update_edge(
            edge_id,
            PropertyMapBuilder::new()
                .insert("since", 2020i64)
                .insert("strength", "weak")
                .build(),
        )?;
        Ok(())
    })
    .unwrap();
    println!("Updated edge to strength='weak'");

    // Check current state
    let current = db.get_edge(edge_id).unwrap();
    assert_eq!(
        current.get_property("strength").and_then(|v| v.as_str()),
        Some("weak")
    );
    println!("Current strength: 'weak' (verified)");

    // Debug: check version chain
    println!("\n=== Dumping edge version chain ===");
    {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        if let Some(head_version_id) = hist_guard.get_current_edge_version(edge_id) {
            println!("Head version: {:?}", head_version_id);
            let mut current_id = head_version_id;
            let mut index = 0;
            loop {
                if let Some(version) = hist_guard.get_edge_version(current_id) {
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
            println!("No version head found for edge {:?}", edge_id);
        }
    }

    // Verify temporal lookup at t1
    println!("\n=== Verifying temporal edge lookup at t1={} ===", t1);
    {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        match hist_guard.find_edge_version_at_time(edge_id, t1, t1) {
            Some(version_id) => {
                let version = hist_guard.get_edge_version(version_id).unwrap();
                println!("find_edge_version_at_time returned: {:?}", version_id);
                println!("Version temporal: {}", version.temporal);
                println!(
                    "Version is_visible_at(t1={}, t1={})? {}",
                    t1,
                    t1,
                    version.temporal.is_visible_at(t1, t1)
                );

                // Reconstruct properties
                let properties = hist_guard.reconstruct_edge_properties(version_id).unwrap();
                println!("Historical properties: {:?}", properties);

                assert_eq!(
                    properties.get("strength").and_then(|v| v.as_str()),
                    Some("strong"),
                    "Should return historical value 'strong', not current value 'weak'"
                );
                println!("✓ Temporal edge lookup correctly returned historical state");
            }
            None => panic!("find_edge_version_at_time returned None for t1={}", t1),
        }
    }
}

#[test]
fn test_temporal_edge_multiple_updates() {
    let db = GallifreyDB::with_config(storage::version::AnchorConfig {
        anchor_interval: 3,
        max_delta_chain: 10,
    });

    // Increased delay to ensure background flush thread has started (fixes CI race condition)
    // On heavily loaded CI systems, the default 10ms may not be sufficient for thread scheduling.
    // Using 50ms provides better reliability without significantly impacting test execution time.
    thread::sleep(Duration::from_millis(50));

    // Create nodes
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    // Create edge with initial weight
    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("weight", 0.5f64).build(),
        )
        .unwrap();

    // Capture timestamps after each update
    let mut timestamps = vec![];

    // Get timestamp of first version
    timestamps.push({
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        let version_id = hist_guard.get_current_edge_version(edge_id).unwrap();
        let version = hist_guard.get_edge_version(version_id).unwrap();
        (version.temporal.valid_time().start().wallclock() + 1).into()
    });

    // Update multiple times
    for i in 1..=5 {
        thread::sleep(Duration::from_millis(50));
        db.write(|tx| {
            tx.update_edge(
                edge_id,
                PropertyMapBuilder::new()
                    .insert("weight", (i as f64) * 0.1 + 0.5)
                    .build(),
            )?;
            Ok(())
        })
        .unwrap();

        // Capture timestamp after each update
        timestamps.push({
            let historical = db.__test_historical_storage();
            let hist_guard = historical.read();
            let version_id = hist_guard.get_current_edge_version(edge_id).unwrap();
            let version = hist_guard.get_edge_version(version_id).unwrap();
            (version.temporal.valid_time().start().wallclock() + 1).into()
        });
    }

    println!(
        "Created {} versions with timestamps: {:?}",
        timestamps.len(),
        timestamps
    );

    // Verify we can retrieve each historical state
    let historical = db.__test_historical_storage();
    let hist_guard = historical.read();

    for (i, &timestamp) in timestamps[..timestamps.len() - 1].iter().enumerate() {
        let version_id = hist_guard
            .find_edge_version_at_time(edge_id, timestamp, timestamp)
            .unwrap_or_else(|| panic!("Should find version at timestamp {}", timestamp));

        let properties = hist_guard.reconstruct_edge_properties(version_id).unwrap();
        let expected_weight = (i as f64) * 0.1 + 0.5;
        let actual_weight = properties.get("weight").and_then(|v| v.as_float()).unwrap();

        assert!(
            (actual_weight - expected_weight).abs() < 0.001,
            "At timestamp {}, expected weight {}, got {}",
            timestamp,
            expected_weight,
            actual_weight
        );
        println!(
            "✓ Version {} at t={}: weight={}",
            i, timestamp, actual_weight
        );
    }
}

#[test]
fn test_temporal_edge_interval_closing() {
    let db = GallifreyDB::with_config(storage::version::AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    });

    // Increased delay to ensure background flush thread has started (fixes CI race condition)
    // On heavily loaded CI systems, the default 10ms may not be sufficient for thread scheduling.
    // Using 50ms provides better reliability without significantly impacting test execution time.
    thread::sleep(Duration::from_millis(50));

    // Create nodes
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edge
    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("active", true).build(),
        )
        .unwrap();

    // Get first version
    let (v1_id, v1_start) = {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        let version_id = hist_guard.get_current_edge_version(edge_id).unwrap();
        let version = hist_guard.get_edge_version(version_id).unwrap();
        (version_id, version.temporal.valid_time().start())
    };

    // Update edge
    thread::sleep(Duration::from_millis(100));
    db.write(|tx| {
        tx.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("active", false).build(),
        )?;
        Ok(())
    })
    .unwrap();

    // Get second version
    let v2_start = {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        let version_id = hist_guard.get_current_edge_version(edge_id).unwrap();
        let version = hist_guard.get_edge_version(version_id).unwrap();
        version.temporal.valid_time().start()
    };

    // Verify first version interval was closed
    {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        let v1 = hist_guard.get_edge_version(v1_id).unwrap();

        assert!(
            !v1.temporal.is_currently_valid(),
            "First version should have closed valid_time interval"
        );
        assert_eq!(
            v1.temporal.valid_time().end(),
            v2_start,
            "First version's valid_time end should equal second version's start"
        );
        println!("✓ Edge version interval properly closed");
        println!(
            "  V1 interval: {} (closed at v2_start={})",
            v1.temporal, v2_start
        );
    }

    // Verify temporal queries work correctly for both periods
    let historical = db.__test_historical_storage();
    let hist_guard = historical.read();

    // Query at t1 (during v1)
    let t1 = v1_start + 1;
    let v1_lookup = hist_guard
        .find_edge_version_at_time(edge_id, t1, t1)
        .unwrap();
    assert_eq!(
        v1_lookup, v1_id,
        "Should find v1 at timestamp within v1's interval"
    );

    let props_v1 = hist_guard.reconstruct_edge_properties(v1_lookup).unwrap();
    assert_eq!(
        props_v1.get("active").and_then(|v| v.as_bool()),
        Some(true),
        "Should return v1's property value"
    );

    // Query at t2 (during v2)
    let t2 = v2_start + 1;
    let v2_lookup = hist_guard
        .find_edge_version_at_time(edge_id, t2, t2)
        .unwrap();
    let props_v2 = hist_guard.reconstruct_edge_properties(v2_lookup).unwrap();
    assert_eq!(
        props_v2.get("active").and_then(|v| v.as_bool()),
        Some(false),
        "Should return v2's property value"
    );

    println!("✓ Temporal queries work correctly across closed intervals");
}

#[test]
fn test_temporal_edge_version_chain_integrity() {
    let db = GallifreyDB::with_config(storage::version::AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    });

    // Create nodes
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edge and update it several times to create version chain
    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("version", 0i64).build(),
        )
        .unwrap();

    for i in 1..=5 {
        thread::sleep(Duration::from_millis(50));
        db.write(|tx| {
            tx.update_edge(
                edge_id,
                PropertyMapBuilder::new().insert("version", i).build(),
            )?;
            Ok(())
        })
        .unwrap();
    }

    // Verify version chain integrity
    {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();

        let mut current_id = hist_guard.get_current_edge_version(edge_id).unwrap();
        let mut version_count = 0;
        let mut prev_timestamp = i64::MAX.into();

        loop {
            let version = hist_guard.get_edge_version(current_id).unwrap();
            version_count += 1;

            // Verify timestamp ordering (newer versions have larger timestamps)
            let current_timestamp = version.temporal.valid_time().start();
            assert!(
                current_timestamp < prev_timestamp || prev_timestamp == i64::MAX.into(),
                "Version chain should be ordered by decreasing timestamp"
            );
            prev_timestamp = current_timestamp;

            // Verify version is either anchor or delta
            assert!(
                version.is_anchor() || version.is_delta(),
                "Version must be either anchor or delta"
            );

            // Check bidirectional links
            if let Some(prev_id) = version.prev_version {
                let prev_version = hist_guard.get_edge_version(prev_id).unwrap();
                assert_eq!(
                    prev_version.next_version,
                    Some(current_id),
                    "Version chain should have correct bidirectional links"
                );
                current_id = prev_id;
            } else {
                break;
            }
        }

        assert_eq!(
            version_count, 6,
            "Should have 6 versions (1 create + 5 updates)"
        );
        println!(
            "✓ Edge version chain integrity verified ({} versions)",
            version_count
        );
    }
}

#[test]
fn test_temporal_edge_anchor_delta_pattern() {
    let db = GallifreyDB::with_config(storage::version::AnchorConfig {
        anchor_interval: 3, // Create anchor every 3 versions
        max_delta_chain: 10,
    });

    // Increased delay to ensure background flush thread has started (fixes CI race condition)
    // On heavily loaded CI systems, the default 10ms may not be sufficient for thread scheduling.
    // Using 50ms provides better reliability without significantly impacting test execution time.
    thread::sleep(Duration::from_millis(50));

    // Create nodes
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edge (v0 - anchor)
    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("v", 0i64).build(),
        )
        .unwrap();

    // Update 5 times (v1, v2, v3, v4, v5)
    for i in 1..=5 {
        thread::sleep(Duration::from_millis(50));
        db.write(|tx| {
            tx.update_edge(edge_id, PropertyMapBuilder::new().insert("v", i).build())?;
            Ok(())
        })
        .unwrap();
    }

    // Verify anchor/delta pattern: v0(A), v1(D), v2(D), v3(A), v4(D), v5(D)
    {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();

        let mut version_ids = vec![];
        let mut current_id = hist_guard.get_current_edge_version(edge_id).unwrap();

        // Collect all version IDs in reverse order (newest to oldest)
        loop {
            version_ids.push(current_id);
            let version = hist_guard.get_edge_version(current_id).unwrap();
            match version.prev_version {
                Some(prev_id) => current_id = prev_id,
                None => break,
            }
        }

        // Reverse to get oldest to newest
        version_ids.reverse();

        assert_eq!(version_ids.len(), 6, "Should have 6 versions");

        // Check pattern
        let expected_pattern = [true, false, false, true, false, false]; // A, D, D, A, D, D
        for (i, &version_id) in version_ids.iter().enumerate() {
            let version = hist_guard.get_edge_version(version_id).unwrap();
            let is_anchor = version.is_anchor();
            assert_eq!(
                is_anchor,
                expected_pattern[i],
                "Version {} should be {} but is {}",
                i,
                if expected_pattern[i] {
                    "anchor"
                } else {
                    "delta"
                },
                if is_anchor { "anchor" } else { "delta" }
            );
        }

        println!("✓ Edge anchor/delta pattern correct: A D D A D D");
    }
}

#[test]
fn test_temporal_edge_with_vector_properties() {
    let db = GallifreyDB::with_config(storage::version::AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    });

    // Create nodes
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edge with vector property
    let embedding_v1 = [1.0f32, 0.0, 0.0, 0.0];
    let edge_id = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new()
                .insert("weight", 0.9f64)
                .insert_vector("embedding", &embedding_v1)
                .build(),
        )
        .unwrap();

    // Capture timestamp
    let t1 = {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();
        let version_id = hist_guard.get_current_edge_version(edge_id).unwrap();
        let version = hist_guard.get_edge_version(version_id).unwrap();
        (version.temporal.valid_time().start().wallclock() + 1).into()
    };

    // Update with new vector
    thread::sleep(Duration::from_millis(100));
    let embedding_v2 = [0.0f32, 1.0, 0.0, 0.0];
    db.write(|tx| {
        tx.update_edge(
            edge_id,
            PropertyMapBuilder::new()
                .insert("weight", 0.95f64)
                .insert_vector("embedding", &embedding_v2)
                .build(),
        )?;
        Ok(())
    })
    .unwrap();

    // Verify temporal lookup returns correct vector
    {
        let historical = db.__test_historical_storage();
        let hist_guard = historical.read();

        let version_id = hist_guard
            .find_edge_version_at_time(edge_id, t1, t1)
            .unwrap();
        let properties = hist_guard.reconstruct_edge_properties(version_id).unwrap();

        assert_eq!(
            properties.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding_v1[..]),
            "Should return historical vector"
        );
        assert_eq!(
            properties.get("weight").and_then(|v| v.as_float()),
            Some(0.9),
            "Should return historical weight"
        );
        println!("✓ Temporal edge lookup with vectors works correctly");
    }
}
