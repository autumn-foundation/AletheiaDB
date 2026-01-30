use gallifreydb::GallifreyDB;
use gallifreydb::WriteOps;
use gallifreydb::core::graph::{Edge, Node};
use gallifreydb::core::id::{EdgeId, NodeId};
use gallifreydb::core::property::PropertyMapBuilder;

// ==================== Batch Temporal Query Tests ====================

#[test]
fn test_time_travel_query() {
    let db = GallifreyDB::new().unwrap();

    // Create a node at time T1
    let props_v1 = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let node_id = db.create_node("Person", props_v1).unwrap();

    // Capture timestamp after creation (wallclock time)
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t1 = gallifreydb::core::temporal::time::now();

    // In a real implementation, we'd create a second version here with an update_node method
    // For now, just verify we can query at T1

    // Query at time T1 (after node was created)
    let historical_node = db.get_node_at_time(node_id, t1, t1).unwrap();
    assert_eq!(
        historical_node.get_property("age").and_then(|v| v.as_int()),
        Some(30)
    );

    // Query current state
    let current_node = db.get_node(node_id).unwrap();
    assert_eq!(
        current_node.get_property("age").and_then(|v| v.as_int()),
        Some(30)
    );
}

#[test]
fn test_time_travel_after_deletion() {
    let db = GallifreyDB::new().unwrap();

    // Create a node
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();
    let node_id = db.create_node("Person", props).unwrap();

    // Record timestamp after creation (wallclock time)
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t_after_create = gallifreydb::core::temporal::time::now();

    // Delete the node
    std::thread::sleep(std::time::Duration::from_micros(100));
    db.write(|tx| {
        tx.delete_node(node_id)?;
        Ok(())
    })
    .unwrap();

    // Record timestamp after deletion (wallclock time)
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t_after_delete = gallifreydb::core::temporal::time::now();

    // Query BEFORE creation - should fail (node didn't exist)
    // Note: We can't easily test this without more control over timestamps

    // Query AFTER deletion - should fail (node was deleted)
    // This is the critical test: time-travel query after deletion should NOT
    // return the deleted node's data
    let result = db.get_node_at_time(node_id, t_after_delete, t_after_delete);
    assert!(
        result.is_err(),
        "Expected NodeNotFound after deletion, but got: {:?}",
        result
    );

    // Query BEFORE deletion - should succeed (node existed)
    let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
    assert!(
        result.is_ok(),
        "Expected to find node before deletion, but got: {:?}",
        result
    );
    let node = result.unwrap();
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

/// Test that verifies temporal index + historical storage visibility check interaction.
///
/// This is a critical integration test for Issue #194: the temporal index stores intervals
/// at insertion time, but deletions close the valid_time in historical storage. The query
/// path must verify visibility with historical storage to correctly reject deleted nodes.
#[test]
fn test_temporal_index_deletion_integration() {
    let db = GallifreyDB::new().unwrap();

    // Create a node
    let props = PropertyMapBuilder::new().insert("name", "TestNode").build();
    let node_id = db.create_node("TestLabel", props).unwrap();

    // Record timestamp after creation
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t_after_create = gallifreydb::core::temporal::time::now();

    // Verify node is queryable after creation
    let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
    assert!(
        result.is_ok(),
        "Node should be queryable after creation: {:?}",
        result
    );

    // Verify temporal index has the version
    let version_ids = db.__test_temporal_indexes().find_node_version_at_point(
        node_id,
        t_after_create,
        t_after_create,
    );
    assert!(
        !version_ids.is_empty(),
        "Temporal index should return candidates for existing node"
    );

    // Delete the node
    std::thread::sleep(std::time::Duration::from_micros(100));
    db.write(|tx| {
        tx.delete_node(node_id)?;
        Ok(())
    })
    .unwrap();

    // Record timestamp after deletion
    std::thread::sleep(std::time::Duration::from_micros(100));
    let t_after_delete = gallifreydb::core::temporal::time::now();

    // CRITICAL: Temporal index may still return the version (it stores insertion-time intervals)
    // but the query should correctly reject it via historical storage visibility check
    let version_ids_after = db.__test_temporal_indexes().find_node_version_at_point(
        node_id,
        t_after_delete,
        t_after_delete,
    );
    // Note: Temporal index might still return candidates - that's expected
    // The visibility check in get_node_at_time should filter them out

    // Query after deletion should fail
    let result = db.get_node_at_time(node_id, t_after_delete, t_after_delete);
    assert!(
        result.is_err(),
        "Query after deletion should fail. Temporal index returned {:?} candidates, \
         but historical visibility check should reject them. Got: {:?}",
        version_ids_after.len(),
        result
    );

    // Query at time BEFORE deletion should still work
    let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
    assert!(
        result.is_ok(),
        "Query before deletion should succeed: {:?}",
        result
    );
}

#[test]
fn test_get_nodes_at_time_basic() {
    let db = GallifreyDB::new().unwrap();

    // Create multiple nodes at time T1
    let props1 = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();
    let props2 = PropertyMapBuilder::new()
        .insert("name", "Bob")
        .insert("age", 25i64)
        .build();
    let props3 = PropertyMapBuilder::new()
        .insert("name", "Charlie")
        .insert("age", 35i64)
        .build();

    let node1 = db.create_node("Person", props1).unwrap();
    let node2 = db.create_node("Person", props2).unwrap();
    let node3 = db.create_node("Person", props3).unwrap();

    let t1 = db.__test_current_timestamp();

    // Query all three nodes at time T1 using batch API
    let node_ids = vec![node1, node2, node3];
    let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

    // Should return all three nodes
    assert_eq!(results.len(), 3);

    // Convert results to HashMap for easier verification
    let results_map: std::collections::HashMap<NodeId, Node> = results
        .into_iter()
        .map(|(id, node_opt)| (id, node_opt.expect("Node should exist")))
        .collect();

    assert_eq!(results_map.len(), 3);

    // Verify node1
    let n1 = results_map.get(&node1).unwrap();
    assert_eq!(n1.id, node1);
    assert_eq!(
        n1.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );

    // Verify node2
    let n2 = results_map.get(&node2).unwrap();
    assert_eq!(n2.id, node2);
    assert_eq!(
        n2.get_property("name").and_then(|v| v.as_str()),
        Some("Bob")
    );

    // Verify node3
    let n3 = results_map.get(&node3).unwrap();
    assert_eq!(n3.id, node3);
    assert_eq!(
        n3.get_property("name").and_then(|v| v.as_str()),
        Some("Charlie")
    );
}

#[test]
fn test_get_nodes_at_time_mixed_results() {
    let db = GallifreyDB::new().unwrap();

    // Create two nodes
    let node1 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let node2 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    let t1 = db.__test_current_timestamp();

    // Query including a non-existent node
    let non_existent = NodeId::new(9999).unwrap();
    let node_ids = vec![node1, non_existent, node2];
    let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

    // Should return 3 results, with one being None
    assert_eq!(results.len(), 3);

    // First result should be Some
    assert!(results[0].1.is_some());
    assert_eq!(results[0].0, node1);

    // Second result should be None (non-existent node)
    assert!(results[1].1.is_none());
    assert_eq!(results[1].0, non_existent);

    // Third result should be Some
    assert!(results[2].1.is_some());
    assert_eq!(results[2].0, node2);
}

#[test]
fn test_get_nodes_at_time_empty_batch() {
    let db = GallifreyDB::new().unwrap();

    let t1 = db.__test_current_timestamp();

    // Query with empty node list
    let results = db.get_nodes_at_time(&[], t1, t1).unwrap();

    // Should return empty results
    assert_eq!(results.len(), 0);
}

#[test]
fn test_get_nodes_at_time_after_deletion() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes
    let node1 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let node2 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    let t_after_create = db.__test_current_timestamp();

    // Delete node1
    db.write(|tx| {
        tx.delete_node(node1)?;
        Ok(())
    })
    .unwrap();

    let t_after_delete = db.__test_current_timestamp();

    // Query at time after deletion - node1 should not be found
    let node_ids = vec![node1, node2];
    let results = db
        .get_nodes_at_time(&node_ids, t_after_delete, t_after_delete)
        .unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].1.is_none()); // node1 was deleted
    assert!(results[1].1.is_some()); // node2 still exists

    // Query at time before deletion - both should exist
    let results = db
        .get_nodes_at_time(&node_ids, t_after_create, t_after_create)
        .unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].1.is_some()); // node1 existed
    assert!(results[1].1.is_some()); // node2 existed
}

#[test]
fn test_get_edges_at_time_basic() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let charlie = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edges
    let edge1 = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();
    let edge2 = db
        .create_edge(
            bob,
            charlie,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2021i64).build(),
        )
        .unwrap();
    let edge3 = db
        .create_edge(
            alice,
            charlie,
            "WORKS_WITH",
            PropertyMapBuilder::new().insert("since", 2022i64).build(),
        )
        .unwrap();

    let t1 = db.__test_current_timestamp();

    // Query all three edges at time T1 using batch API
    let edge_ids = vec![edge1, edge2, edge3];
    let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

    // Should return all three edges
    assert_eq!(results.len(), 3);

    // Convert results to HashMap for easier verification
    let results_map: std::collections::HashMap<EdgeId, Edge> = results
        .into_iter()
        .map(|(id, edge_opt)| (id, edge_opt.expect("Edge should exist")))
        .collect();

    assert_eq!(results_map.len(), 3);

    // Verify edge1
    let e1 = results_map.get(&edge1).unwrap();
    assert_eq!(e1.id, edge1);
    assert_eq!(e1.source, alice);
    assert_eq!(e1.target, bob);
    assert_eq!(
        e1.get_property("since").and_then(|v| v.as_int()),
        Some(2020)
    );

    // Verify edge2
    let e2 = results_map.get(&edge2).unwrap();
    assert_eq!(e2.id, edge2);
    assert_eq!(e2.source, bob);
    assert_eq!(e2.target, charlie);
    assert_eq!(
        e2.get_property("since").and_then(|v| v.as_int()),
        Some(2021)
    );

    // Verify edge3
    let e3 = results_map.get(&edge3).unwrap();
    assert_eq!(e3.id, edge3);
    assert_eq!(e3.source, alice);
    assert_eq!(e3.target, charlie);
    assert_eq!(
        e3.get_property("since").and_then(|v| v.as_int()),
        Some(2022)
    );
}

#[test]
fn test_get_edges_at_time_mixed_results() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes and edges
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge1 = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();

    let t1 = db.__test_current_timestamp();

    // Query including a non-existent edge
    let non_existent = EdgeId::new(9999).unwrap();
    let edge_ids = vec![edge1, non_existent];
    let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

    // Should return 2 results, with one being None
    assert_eq!(results.len(), 2);

    // First result should be Some
    assert!(results[0].1.is_some());
    assert_eq!(results[0].0, edge1);

    // Second result should be None (non-existent edge)
    assert!(results[1].1.is_none());
    assert_eq!(results[1].0, non_existent);
}

#[test]
fn test_get_edges_at_time_empty_batch() {
    let db = GallifreyDB::new().unwrap();

    let t1 = db.__test_current_timestamp();

    // Query with empty edge list
    let results = db.get_edges_at_time(&[], t1, t1).unwrap();

    // Should return empty results
    assert_eq!(results.len(), 0);
}

#[test]
fn test_get_edges_at_time_after_deletion() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes and edges
    let alice = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = db
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge1 = db
        .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    let edge2 = db
        .create_edge(bob, alice, "WORKS_WITH", PropertyMapBuilder::new().build())
        .unwrap();

    let t_after_create = db.__test_current_timestamp();

    // Delete edge1
    db.write(|tx| {
        tx.delete_edge(edge1)?;
        Ok(())
    })
    .unwrap();

    let t_after_delete = db.__test_current_timestamp();

    // Query at time after deletion - edge1 should not be found
    let edge_ids = vec![edge1, edge2];
    let results = db
        .get_edges_at_time(&edge_ids, t_after_delete, t_after_delete)
        .unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].1.is_none()); // edge1 was deleted
    assert!(results[1].1.is_some()); // edge2 still exists

    // Query at time before deletion - both should exist
    let results = db
        .get_edges_at_time(&edge_ids, t_after_create, t_after_create)
        .unwrap();

    assert_eq!(results.len(), 2);
    assert!(results[0].1.is_some()); // edge1 existed
    assert!(results[1].1.is_some()); // edge2 existed
}

#[test]
fn test_get_nodes_at_time_large_batch() {
    let db = GallifreyDB::new().unwrap();

    // Create 100 nodes
    let node_ids: Vec<_> = (0..100)
        .map(|i| {
            db.create_node(
                "Test",
                PropertyMapBuilder::new().insert("index", i as i64).build(),
            )
            .unwrap()
        })
        .collect();

    let t1 = db.__test_current_timestamp();

    // Query all 100 at once
    let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

    // All should exist
    assert_eq!(results.len(), 100);
    assert!(results.iter().all(|(_, node)| node.is_some()));

    // Verify order is preserved
    for (i, (id, _)) in results.iter().enumerate() {
        assert_eq!(*id, node_ids[i]);
    }
}

#[test]
fn test_get_nodes_at_time_duplicate_ids() {
    let db = GallifreyDB::new().unwrap();

    let node1 = db
        .create_node(
            "Test",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();

    let t1 = db.__test_current_timestamp();

    // Query with duplicates
    let node_ids = vec![node1, node1, node1];
    let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

    // Should return 3 results (one per input, even if duplicate)
    assert_eq!(results.len(), 3);
    assert!(
        results
            .iter()
            .all(|(id, node)| { *id == node1 && node.is_some() })
    );
}

#[test]
fn test_get_edges_at_time_large_batch() {
    let db = GallifreyDB::new().unwrap();

    // Create nodes
    let source = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();
    let target = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();

    // Create 100 edges
    let edge_ids: Vec<_> = (0..100)
        .map(|i| {
            db.create_edge(
                source,
                target,
                "LINK",
                PropertyMapBuilder::new().insert("index", i as i64).build(),
            )
            .unwrap()
        })
        .collect();

    let t1 = db.__test_current_timestamp();

    // Query all 100 at once
    let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

    // All should exist
    assert_eq!(results.len(), 100);
    assert!(results.iter().all(|(_, edge)| edge.is_some()));

    // Verify order is preserved
    for (i, (id, _)) in results.iter().enumerate() {
        assert_eq!(*id, edge_ids[i]);
    }
}

#[test]
fn test_get_edges_at_time_duplicate_ids() {
    let db = GallifreyDB::new().unwrap();

    let source = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();
    let target = db
        .create_node("Node", PropertyMapBuilder::new().build())
        .unwrap();

    let edge1 = db
        .create_edge(source, target, "LINK", PropertyMapBuilder::new().build())
        .unwrap();

    let t1 = db.__test_current_timestamp();

    // Query with duplicates
    let edge_ids = vec![edge1, edge1, edge1];
    let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

    // Should return 3 results (one per input, even if duplicate)
    assert_eq!(results.len(), 3);
    assert!(
        results
            .iter()
            .all(|(id, edge)| { *id == edge1 && edge.is_some() })
    );
}
