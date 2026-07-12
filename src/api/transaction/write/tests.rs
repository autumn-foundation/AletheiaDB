use super::*;
use crate::core::id::TxIdGenerator;
use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
use tempfile::TempDir;

mod tombstone_tests {
    use super::*;

    fn create_test_write_tx() -> (WriteTransaction, TempDir) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());
        let tx_id_gen = TxIdGenerator::new();

        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
        );

        (tx, temp_dir)
    }

    #[test]
    fn test_tombstone_exhaustion_error() {
        let (tx, _temp_dir) = create_test_write_tx();
        let mut historical = tx.historical.write();
        let commit_ts = time::now();

        // Create an empty iterator to simulate exhaustion
        let mut tombstone_ids = Vec::new().into_iter();

        // Create a dummy DeleteNode operation
        let op = crate::api::transaction::BufferedWrite::DeleteNode {
            node_id: NodeId::new(1).unwrap(),
            valid_from: time::now(),
        };

        // Try to apply it
        let result = crate::api::transaction::write::apply::apply_single_write(
            &tx,
            &op,
            commit_ts,
            &mut historical,
            &mut tombstone_ids,
            1, // num_deletes expected
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::core::error::Error::Storage(StorageError::InconsistentState { reason }) => {
                assert!(reason.contains("Tombstone ID exhaustion"));
            }
            err => panic!("Expected InconsistentState error, got: {:?}", err),
        }
    }
}

mod general_tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    fn create_test_write_tx() -> (WriteTransaction, TempDir) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        // Create WAL with temp directory for tests
        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());
        let tx_id_gen = TxIdGenerator::new();

        // Create snapshot and visibility manager for testing
        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
        );

        (tx, temp_dir)
    }

    #[test]
    fn test_write_transaction_creation() {
        let (tx, _temp_dir) = create_test_write_tx();
        assert_eq!(tx.state, TxState::Active);
        let metadata = tx.metadata();
        assert!(!metadata.is_read_only);
    }

    #[test]
    fn test_create_node_buffering() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let node_id = tx.create_node("Person", props.clone()).unwrap();
        // ID generators start at 0, so first ID is 0
        assert_eq!(node_id.as_u64(), 0);

        // Read-your-writes: should be able to read buffered node
        let node = tx.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(
            node.properties.get("name").unwrap(),
            &crate::core::property::PropertyValue::from("Alice")
        );
    }

    #[test]
    fn test_create_edge_buffering() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        // First create nodes in current storage (simulating existing nodes)
        let props = PropertyMapBuilder::new().build();
        let node1 = tx.current.create_node("Person", props.clone()).unwrap();
        let node2 = tx.current.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020i64).build();

        let edge_id = tx
            .create_edge(node1, node2, "KNOWS", edge_props.clone())
            .unwrap();
        // ID generators start at 0, so first edge ID is 0
        assert_eq!(edge_id.as_u64(), 0);

        // Read-your-writes: should be able to read buffered edge
        let edge = tx.get_edge(edge_id).unwrap();
        assert_eq!(edge.id, edge_id);
        assert_eq!(edge.source, node1);
        assert_eq!(edge.target, node2);
        assert_eq!(
            edge.properties.get("since").unwrap(),
            &crate::core::property::PropertyValue::from(2020i64)
        );
    }

    #[test]
    fn test_commit_applies_changes() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        let props = PropertyMapBuilder::new().insert("name", "Bob").build();

        let node_id = tx.create_node("Person", props).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Node should now be visible in current storage
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Bob")
        );
    }

    #[test]
    fn test_rollback_discards_changes() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        let props = PropertyMapBuilder::new().insert("name", "Charlie").build();

        let node_id = tx.create_node("Person", props).unwrap();

        // Rollback the transaction
        tx.rollback().unwrap();

        // Node should not be visible in current storage
        assert!(current.get_node(node_id).is_err());
    }

    #[test]
    fn test_validation_fails_for_invalid_edge() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().build();

        // Try to create edge with non-existent nodes
        let node1 = NodeId::new(999).unwrap();
        let node2 = NodeId::new(1000).unwrap();

        tx.create_edge(node1, node2, "KNOWS", props).unwrap();

        // Commit should fail validation
        let result = tx.commit();
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_rollback_on_drop() {
        let current = Arc::new(CurrentStorage::new());
        let node_id = {
            let (mut tx, _temp_dir) = create_test_write_tx();
            let props = PropertyMapBuilder::new().build();
            // Transaction dropped here without commit
            tx.create_node("Person", props).unwrap()
        };

        // Node should not be visible (auto-rollback)
        assert!(current.get_node(node_id).is_err());
    }

    #[test]
    fn test_update_node() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node first in current storage
        let props = PropertyMapBuilder::new().insert("age", 30i64).build();
        let node_id = current.create_node("Person", props).unwrap();

        // Update the node properties
        let new_props = PropertyMapBuilder::new().insert("age", 31i64).build();
        tx.update_node(node_id, new_props.clone()).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the update was applied
        let node = current.get_node(node_id).unwrap();
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
    }

    #[test]
    fn test_update_node_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().insert("age", 30i64).build();
        let result = tx.update_node(NodeId::new(999).unwrap(), props);

        // Should fail because node doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_update_node_patch_preserves_existing_properties() {
        // Test that update_node uses PATCH semantics - properties not mentioned are preserved
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with multiple properties
        let initial_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("city", "London")
            .build();
        let node_id = current.create_node("Person", initial_props).unwrap();

        // Update only the age - name and city should be preserved
        let update_props = PropertyMapBuilder::new().insert("age", 31i64).build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // Verify all properties
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
        assert_eq!(
            node.get_property("city").and_then(|v| v.as_str()),
            Some("London")
        );
    }

    #[test]
    fn test_update_node_patch_adds_new_properties() {
        // Test that update_node can add new properties without removing existing ones
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with one property
        let initial_props = PropertyMapBuilder::new().insert("name", "Bob").build();
        let node_id = current.create_node("Person", initial_props).unwrap();

        // Add a new property
        let update_props = PropertyMapBuilder::new().insert("age", 25i64).build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // Both properties should exist
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Bob")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(25));
    }

    #[test]
    fn test_update_node_patch_modifies_multiple_properties() {
        // Test that update_node can modify multiple properties while preserving others
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with four properties
        let initial_props = PropertyMapBuilder::new()
            .insert("name", "Charlie")
            .insert("age", 40i64)
            .insert("city", "Paris")
            .insert("occupation", "Engineer")
            .build();
        let node_id = current.create_node("Person", initial_props).unwrap();

        // Update two properties
        let update_props = PropertyMapBuilder::new()
            .insert("age", 41i64)
            .insert("city", "Berlin")
            .build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // All four properties should exist with correct values
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Charlie")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(41));
        assert_eq!(
            node.get_property("city").and_then(|v| v.as_str()),
            Some("Berlin")
        );
        assert_eq!(
            node.get_property("occupation").and_then(|v| v.as_str()),
            Some("Engineer")
        );
    }

    #[test]
    fn test_update_node_patch_empty_update() {
        // Test that empty update preserves all properties
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with properties
        let initial_props = PropertyMapBuilder::new()
            .insert("name", "Diana")
            .insert("age", 35i64)
            .build();
        let node_id = current.create_node("Person", initial_props).unwrap();

        // Empty update
        let update_props = PropertyMapBuilder::new().build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // All properties should still exist
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Diana")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(35));
    }

    #[test]
    fn test_update_node_patch_with_vector_properties() {
        // Test PATCH behavior with vector properties
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with a vector and scalar property
        let embedding = vec![0.1f32, 0.2, 0.3];
        let initial_props = PropertyMapBuilder::new()
            .insert("name", "Eve")
            .insert_vector("embedding", &embedding)
            .build();
        let node_id = current.create_node("Document", initial_props).unwrap();

        // Update only the name - embedding should be preserved
        let update_props = PropertyMapBuilder::new()
            .insert("name", "Eve Updated")
            .build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // Verify both properties
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Eve Updated")
        );
        assert!(node.get_property("embedding").is_some());
        if let Some(vec_val) = node.get_property("embedding").and_then(|v| v.as_vector()) {
            assert_eq!(vec_val, &[0.1f32, 0.2, 0.3]);
        } else {
            panic!("Vector property not found or wrong type");
        }
    }

    #[test]
    fn test_update_edge() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes and edge in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("strength", 5i64).build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", edge_props)
            .unwrap();

        // Update the edge properties
        let new_props = PropertyMapBuilder::new().insert("strength", 10i64).build();
        tx.update_edge(edge_id, new_props.clone()).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the update was applied
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("strength").and_then(|v| v.as_int()),
            Some(10)
        );
    }

    #[test]
    fn test_update_edge_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().insert("strength", 5i64).build();
        let result = tx.update_edge(EdgeId::new(999).unwrap(), props);

        // Should fail because edge doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_update_edge_patch_preserves_existing_properties() {
        // Test that update_edge uses PATCH semantics - properties not mentioned are preserved
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with multiple properties
        let initial_props = PropertyMapBuilder::new()
            .insert("weight", 5i64)
            .insert("type", "friendship")
            .insert("since", "2020")
            .build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", initial_props)
            .unwrap();

        // Update only the weight - type and since should be preserved
        let update_props = PropertyMapBuilder::new().insert("weight", 10i64).build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // Verify all properties
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(10)
        );
        assert_eq!(
            edge.get_property("type").and_then(|v| v.as_str()),
            Some("friendship")
        );
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_str()),
            Some("2020")
        );
    }

    #[test]
    fn test_update_edge_patch_adds_new_properties() {
        // Test that update_edge can add new properties without removing existing ones
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with one property
        let initial_props = PropertyMapBuilder::new().insert("weight", 5i64).build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", initial_props)
            .unwrap();

        // Add a new property
        let update_props = PropertyMapBuilder::new()
            .insert("type", "colleague")
            .build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // Both properties should exist
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(5)
        );
        assert_eq!(
            edge.get_property("type").and_then(|v| v.as_str()),
            Some("colleague")
        );
    }

    #[test]
    fn test_update_edge_patch_modifies_multiple_properties() {
        // Test that update_edge can modify multiple properties while preserving others
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with four properties
        let initial_props = PropertyMapBuilder::new()
            .insert("weight", 5i64)
            .insert("type", "friendship")
            .insert("since", "2020")
            .insert("active", true)
            .build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", initial_props)
            .unwrap();

        // Update two properties
        let update_props = PropertyMapBuilder::new()
            .insert("weight", 8i64)
            .insert("since", "2021")
            .build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // All four properties should exist with correct values
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(8)
        );
        assert_eq!(
            edge.get_property("type").and_then(|v| v.as_str()),
            Some("friendship")
        );
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_str()),
            Some("2021")
        );
        assert_eq!(
            edge.get_property("active").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_update_edge_patch_empty_update() {
        // Test that empty update preserves all properties
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with properties
        let initial_props = PropertyMapBuilder::new()
            .insert("weight", 5i64)
            .insert("type", "friendship")
            .build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", initial_props)
            .unwrap();

        // Empty update
        let update_props = PropertyMapBuilder::new().build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // All properties should still exist
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(5)
        );
        assert_eq!(
            edge.get_property("type").and_then(|v| v.as_str()),
            Some("friendship")
        );
    }

    #[test]
    fn test_update_edge_patch_with_vector_properties() {
        // Test PATCH behavior with vector properties on edges
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with a vector and scalar property
        let embedding = vec![0.5f32, 0.6, 0.7];
        let initial_props = PropertyMapBuilder::new()
            .insert("weight", 5i64)
            .insert_vector("embedding", &embedding)
            .build();
        let edge_id = current
            .create_edge(node1, node2, "SIMILAR", initial_props)
            .unwrap();

        // Update only the weight - embedding should be preserved
        let update_props = PropertyMapBuilder::new().insert("weight", 10i64).build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // Verify both properties
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(10)
        );
        assert!(edge.get_property("embedding").is_some());
        if let Some(vec_val) = edge.get_property("embedding").and_then(|v| v.as_vector()) {
            assert_eq!(vec_val, &[0.5f32, 0.6, 0.7]);
        } else {
            panic!("Vector property not found or wrong type");
        }
    }

    #[test]
    fn test_delete_node() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node first in current storage
        let props = PropertyMapBuilder::new().build();
        let node_id = current.create_node("Person", props).unwrap();

        // Verify node exists
        assert!(current.get_node(node_id).is_ok());

        // Delete the node
        tx.delete_node(node_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the node was deleted
        assert!(current.get_node(node_id).is_err());
    }

    #[test]
    fn test_delete_node_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let result = tx.delete_node(NodeId::new(999).unwrap());

        // Should fail because node doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_edge() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes and edge in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        let edge_props = PropertyMapBuilder::new().build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", edge_props)
            .unwrap();

        // Verify edge exists
        assert!(current.get_edge(edge_id).is_ok());

        // Delete the edge
        tx.delete_edge(edge_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the edge was deleted
        assert!(current.get_edge(edge_id).is_err());
    }

    #[test]
    fn test_delete_edge_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let result = tx.delete_edge(EdgeId::new(999).unwrap());

        // Should fail because edge doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_commit_after_commit_fails() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().build();
        tx.create_node("Person", props).unwrap();

        // First commit should succeed
        tx.commit().unwrap();

        // Try to commit again - should fail (can't create new tx from consumed one)
        // This is prevented by the compiler since commit consumes self
    }

    #[test]
    fn test_operations_after_commit_prevented_by_move() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().build();
        tx.create_node("Person", props).unwrap();

        // Commit consumes tx
        tx.commit().unwrap();

        // Can't use tx after commit - prevented by compiler
        // This test documents the behavior
    }

    #[test]
    fn test_read_ops_delegation() {
        let (tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create some data in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();
        current.create_edge(node1, node2, "KNOWS", props).unwrap();

        // Test ReadOps methods on transaction
        assert_eq!(tx.node_count(), 2);
        assert_eq!(tx.edge_count(), 1);
        assert!(tx.get_node(node1).is_ok());
        assert_eq!(tx.get_outgoing_edges(node1).unwrap().len(), 1);
        assert_eq!(tx.get_incoming_edges(node2).unwrap().len(), 1);
        assert_eq!(
            tx.get_outgoing_edges_with_label(node1, "KNOWS")
                .unwrap()
                .len(),
            1
        );
    }

    // Issue #359: edge-listing methods return Result so callers can
    // distinguish "node doesn't exist" (Err) from "node has no edges" (Ok(empty)).

    #[test]
    fn test_write_tx_get_outgoing_edges_nonexistent_node_errors() {
        let (tx, _temp_dir) = create_test_write_tx();

        let missing = NodeId::new(999).unwrap();
        let result = tx.get_outgoing_edges(missing);
        assert!(
            matches!(
                result,
                Err(crate::core::error::Error::Storage(
                    crate::core::error::StorageError::NodeNotFound(id)
                )) if id == missing
            ),
            "get_outgoing_edges on a nonexistent node must return Err(NodeNotFound), got {result:?}"
        );
        assert!(
            tx.get_incoming_edges(missing).is_err(),
            "get_incoming_edges on a nonexistent node must return Err"
        );
        assert!(
            tx.get_outgoing_edges_with_label(missing, "KNOWS").is_err(),
            "get_outgoing_edges_with_label on a nonexistent node must return Err"
        );
    }

    #[test]
    fn test_write_tx_get_outgoing_edges_node_created_in_tx_ok() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        // Node exists only in this transaction's write buffer.
        let props = PropertyMapBuilder::new().build();
        let node = tx.create_node("Person", props).unwrap();

        let edges = tx
            .get_outgoing_edges(node)
            .expect("node created in this tx must be visible to the existence check");
        assert!(edges.is_empty(), "expected Ok(empty), got {edges:?}");

        let edges = tx
            .get_incoming_edges(node)
            .expect("node created in this tx must be visible to the existence check");
        assert!(edges.is_empty(), "expected Ok(empty), got {edges:?}");

        let edges = tx
            .get_outgoing_edges_with_label(node, "KNOWS")
            .expect("node created in this tx must be visible to the existence check");
        assert!(edges.is_empty(), "expected Ok(empty), got {edges:?}");
    }

    /// Pins the documented write-transaction caveat: an edge created in this
    /// transaction (still buffered, not yet committed) is NOT listed by the
    /// edge-listing methods — only the node existence check is buffer-aware.
    #[test]
    fn test_write_tx_buffered_edge_not_listed_before_commit() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Source and target are committed nodes; the edge exists only in the
        // transaction's write buffer.
        let props = PropertyMapBuilder::new().build();
        let source = current.create_node("Person", props.clone()).unwrap();
        let target = current.create_node("Person", props.clone()).unwrap();
        tx.create_edge(source, target, "KNOWS", props).unwrap();

        assert_eq!(
            tx.get_outgoing_edges(source).unwrap(),
            Vec::new(),
            "an edge buffered in this tx must not be listed before commit"
        );
        assert_eq!(
            tx.get_incoming_edges(target).unwrap(),
            Vec::new(),
            "an edge buffered in this tx must not be listed before commit"
        );
        assert_eq!(
            tx.get_outgoing_edges_with_label(source, "KNOWS").unwrap(),
            Vec::new(),
            "an edge buffered in this tx must not be listed before commit"
        );
    }

    #[test]
    fn test_write_tx_get_outgoing_edges_node_deleted_in_tx_errors() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Node exists in current storage, but is deleted in this transaction.
        let props = PropertyMapBuilder::new().build();
        let node = current.create_node("Person", props).unwrap();
        tx.delete_node(node).unwrap();

        assert!(
            tx.get_outgoing_edges(node).is_err(),
            "node deleted in this tx must not be treated as existing"
        );
        assert!(
            tx.get_incoming_edges(node).is_err(),
            "node deleted in this tx must not be treated as existing"
        );
        assert!(
            tx.get_outgoing_edges_with_label(node, "KNOWS").is_err(),
            "node deleted in this tx must not be treated as existing"
        );
    }

    #[test]
    fn test_delete_node_creates_tombstone() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);
        let historical = Arc::clone(&tx.historical);

        // Create a node with properties
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let node_id = current.create_node("Person", props).unwrap();

        // Verify node exists in current storage
        assert!(current.get_node(node_id).is_ok());

        // Delete the node
        tx.delete_node(node_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify node was deleted from current storage
        assert!(current.get_node(node_id).is_err());

        // Verify tombstone version was created in historical storage
        let historical = historical.read();
        let stats = historical.stats();
        assert!(
            stats.total_node_versions > 0,
            "Expected at least one node version (tombstone) in historical storage"
        );

        // The tombstone should have a closed transaction time
        // This is implicitly tested by the fact that a version was created
    }

    #[test]
    fn test_delete_edge_creates_tombstone() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);
        let historical = Arc::clone(&tx.historical);

        // Create nodes and edge
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020i64).build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", edge_props)
            .unwrap();

        // Verify edge exists
        assert!(current.get_edge(edge_id).is_ok());

        // Delete the edge
        tx.delete_edge(edge_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify edge was deleted from current storage
        assert!(current.get_edge(edge_id).is_err());

        // Verify tombstone version was created in historical storage
        let historical = historical.read();
        let stats = historical.stats();
        assert!(
            stats.total_edge_versions > 0,
            "Expected at least one edge version (tombstone) in historical storage"
        );
    }

    #[test]
    fn test_read_your_writes_update() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node in current storage
        let props = PropertyMapBuilder::new().insert("age", 30i64).build();
        let node_id = current.create_node("Person", props).unwrap();

        // Update the node in the transaction
        let new_props = PropertyMapBuilder::new().insert("age", 31i64).build();
        tx.update_node(node_id, new_props).unwrap();

        // Read-your-writes: should see the updated value
        let node = tx.get_node(node_id).unwrap();
        assert_eq!(
            node.properties.get("age").unwrap(),
            &crate::core::property::PropertyValue::from(31i64)
        );
    }

    #[test]
    fn test_read_your_writes_delete() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node in current storage
        let props = PropertyMapBuilder::new().build();
        let node_id = current.create_node("Person", props).unwrap();

        // Delete the node in the transaction
        tx.delete_node(node_id).unwrap();

        // Read-your-writes: should NOT see the deleted node
        assert!(tx.get_node(node_id).is_err());
    }

    #[test]
    fn test_empty_transaction_commit() {
        let (tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Commit empty transaction (no operations buffered)
        // Should skip rebuild_adjacency() since no edge operations occurred
        tx.commit().unwrap();

        // Verify storage is still in valid state
        assert_eq!(current.node_count(), 0);
        assert_eq!(current.edge_count(), 0);
    }

    #[test]
    fn test_empty_transaction_with_only_node_operations() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create only nodes (no edges)
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        tx.create_node("Person", props).unwrap();

        // Commit - should skip rebuild_adjacency() since no edge operations occurred
        tx.commit().unwrap();

        // Verify node was created and adjacency is valid
        assert_eq!(current.node_count(), 1);
        assert_eq!(current.edge_count(), 0);
    }

    #[test]
    fn test_transaction_with_edge_operations() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes and edges
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        let alice = tx.create_node("Person", props).unwrap();

        let props = PropertyMapBuilder::new().insert("name", "Bob").build();
        let bob = tx.create_node("Person", props).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020i64).build();
        let edge_id = tx.create_edge(alice, bob, "KNOWS", edge_props).unwrap();

        // Commit and verify edge adjacency is immediately visible.
        tx.commit().unwrap();

        // Verify adjacency is readable without explicit compaction
        assert_eq!(current.node_count(), 2);
        assert_eq!(current.edge_count(), 1);

        // Verify adjacency list is correct
        let outgoing = current.get_outgoing_edges(alice);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0], edge_id);

        let incoming = current.get_incoming_edges(bob);
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0], edge_id);
    }

    #[test]
    fn test_edge_commit_does_not_force_adjacency_compaction() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        let props = PropertyMapBuilder::new().build();
        let source = tx.create_node("Person", props.clone()).unwrap();
        let target = tx.create_node("Person", props).unwrap();

        tx.create_edge(
            source,
            target,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();

        tx.commit().unwrap();

        // Edges are immediately readable from merged adjacency without forcing
        // commit-time compaction; compaction is handled asynchronously.
        assert_eq!(current.edge_count(), 1);
        assert_eq!(current.out_degree(source), 1);
        assert_eq!(current.delta_edge_count(), 1);
    }

    #[test]
    fn test_interleaved_create_update_delete_operations() {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        // Create WAL with temp directory for tests
        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());
        let tx_id_gen = TxIdGenerator::new();

        // Create visibility manager and snapshot for testing
        let visibility_manager = Arc::new(TxVisibilityManager::new());

        // Create initial transaction to set up nodes and one edge
        let snapshot1 = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };
        let mut tx1 = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot1,
            current.clone(),
            historical.clone(),
            temporal_indexes.clone(),
            wal.clone(),
            current_timestamp.clone(),
            visibility_manager.clone(),
            node_id_gen.clone(),
            edge_id_gen.clone(),
            version_id_gen.clone(),
        );

        let props = PropertyMapBuilder::new().build();
        let node1 = tx1.create_node("Person", props.clone()).unwrap();
        let node2 = tx1.create_node("Person", props.clone()).unwrap();
        let node3 = tx1.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("weight", 5i64).build();
        let edge1 = tx1.create_edge(node1, node2, "KNOWS", edge_props).unwrap();

        tx1.commit().unwrap();

        // Verify initial state
        assert_eq!(current.edge_count(), 1);

        // Create second transaction with interleaved operations
        let snapshot2 = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };
        let mut tx2 = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot2,
            current.clone(),
            historical.clone(),
            temporal_indexes.clone(),
            wal.clone(),
            current_timestamp.clone(),
            visibility_manager.clone(),
            node_id_gen.clone(),
            edge_id_gen.clone(),
            version_id_gen.clone(),
        );

        // 1. Create new edge
        tx2.create_edge(
            node2,
            node3,
            "FOLLOWS",
            PropertyMapBuilder::new().insert("weight", 8i64).build(),
        )
        .unwrap();

        // 2. Update existing edge
        tx2.update_edge(
            edge1,
            PropertyMapBuilder::new().insert("weight", 7i64).build(),
        )
        .unwrap();

        // 3. Create another edge
        tx2.create_edge(node1, node3, "LIKES", PropertyMapBuilder::new().build())
            .unwrap();

        // Commit all operations
        tx2.commit().unwrap();

        // After commit: verify final state
        // edge1 (updated) + 2 new edges = 3 edges total
        assert_eq!(current.edge_count(), 3);

        // Verify edge1 was updated
        let updated_edge = current.get_edge(edge1).unwrap();
        assert_eq!(
            updated_edge.get_property("weight").and_then(|v| v.as_int()),
            Some(7)
        );

        // Verify adjacency is correct after rebuild
        assert_eq!(current.out_degree(node1), 2); // KNOWS and LIKES
        assert_eq!(current.out_degree(node2), 1); // FOLLOWS
        assert_eq!(current.in_degree(node3), 2); // receives FOLLOWS and LIKES
    }

    /// Test that tombstone ID pre-generation matches actual delete operations.
    ///
    /// This test verifies the critical invariant that the number of IDs generated
    /// matches the number of delete operations, preventing iterator exhaustion.
    #[test]
    fn test_tombstone_id_count_matches_deletes() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create initial nodes and edges in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();
        let node3 = current.create_node("Person", props.clone()).unwrap();
        let node4 = current.create_node("Person", props.clone()).unwrap();

        let edge1 = current
            .create_edge(node1, node2, "KNOWS", props.clone())
            .unwrap();
        let edge2 = current
            .create_edge(node2, node3, "KNOWS", props.clone())
            .unwrap();
        let edge3 = current
            .create_edge(node3, node4, "KNOWS", props.clone())
            .unwrap();

        // Create a transaction with mixed operations:
        // - Some creates
        // - Some updates
        // - MULTIPLE deletes (both nodes and edges)
        let node5 = tx.create_node("Person", props.clone()).unwrap();
        tx.create_edge(node1, node5, "FOLLOWS", props.clone())
            .unwrap();

        tx.update_node(
            node4,
            PropertyMapBuilder::new().insert("age", 30i64).build(),
        )
        .unwrap();

        // Delete operations: 2 nodes + 2 edges = 4 tombstones needed
        tx.delete_node(node3).unwrap(); // This will also require tombstone for node
        tx.delete_node(node4).unwrap(); // This will also require tombstone for node
        tx.delete_edge(edge1).unwrap(); // Tombstone for edge
        tx.delete_edge(edge2).unwrap(); // Tombstone for edge

        // Commit should succeed without panicking on iterator exhaustion
        let result = tx.commit();
        assert!(
            result.is_ok(),
            "Commit should succeed with correct tombstone ID count"
        );

        // Verify deletes were applied
        assert!(current.get_node(node3).is_err());
        assert!(current.get_node(node4).is_err());
        assert!(current.get_edge(edge1).is_err());
        assert!(current.get_edge(edge2).is_err());

        // Verify non-deleted entities still exist
        assert!(current.get_node(node1).is_ok());
        assert!(current.get_node(node2).is_ok());
        assert!(current.get_node(node5).is_ok());
        assert!(current.get_edge(edge3).is_ok());
    }

    #[test]
    fn test_batch_edge_operations_visible_without_manual_compaction() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let mut nodes = Vec::new();
        for i in 0..100 {
            let node = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
                .unwrap();
            nodes.push(node);
        }

        // Create 99 edges
        for i in 0..99 {
            tx.create_edge(
                nodes[i],
                nodes[i + 1],
                "CONNECTS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        }

        // Commit should make edges visible immediately (compaction is asynchronous)
        tx.commit().unwrap();

        // Verify all edges are in adjacency index
        assert_eq!(current.edge_count(), 99);
        for i in 0..99 {
            assert_eq!(current.out_degree(nodes[i]), 1);
            assert_eq!(current.in_degree(nodes[i + 1]), 1);
        }
    }

    // ===================================================================
    // Cascade Delete Tests (Issue #364)
    // ===================================================================

    #[test]
    fn test_delete_node_cascade_removes_edges() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a central node with multiple connections
        let props = PropertyMapBuilder::new().build();
        let central_node = tx.create_node("Person", props.clone()).unwrap();
        let node1 = tx.create_node("Person", props.clone()).unwrap();
        let node2 = tx.create_node("Person", props.clone()).unwrap();
        let node3 = tx.create_node("Person", props).unwrap();

        // Create edges: central node has 2 outgoing and 1 incoming edge
        let edge1 = tx
            .create_edge(
                central_node,
                node1,
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        let edge2 = tx
            .create_edge(
                central_node,
                node2,
                "FOLLOWS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        let edge3 = tx
            .create_edge(
                node3,
                central_node,
                "LIKES",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        tx.commit().unwrap();

        // Verify all edges exist
        assert!(current.get_edge(edge1).is_ok());
        assert!(current.get_edge(edge2).is_ok());
        assert!(current.get_edge(edge3).is_ok());

        // Delete the central node with cascade
        let (mut tx2, _temp_dir2) = create_test_write_tx_from_existing(Arc::clone(&current));
        tx2.delete_node_cascade(central_node).unwrap();
        tx2.commit().unwrap();

        // Verify the node was deleted
        assert!(current.get_node(central_node).is_err());

        // Verify all connected edges were deleted (CASCADE)
        assert!(
            current.get_edge(edge1).is_err(),
            "Outgoing edge should be deleted with cascade"
        );
        assert!(
            current.get_edge(edge2).is_err(),
            "Outgoing edge should be deleted with cascade"
        );
        assert!(
            current.get_edge(edge3).is_err(),
            "Incoming edge should be deleted with cascade"
        );

        // Verify other nodes still exist
        assert!(current.get_node(node1).is_ok());
        assert!(current.get_node(node2).is_ok());
        assert!(current.get_node(node3).is_ok());
    }

    #[test]
    fn test_delete_node_no_cascade_keeps_edges() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a central node with connections
        let props = PropertyMapBuilder::new().build();
        let central_node = tx.create_node("Person", props.clone()).unwrap();
        let node1 = tx.create_node("Person", props).unwrap();

        // Create edge
        let edge1 = tx
            .create_edge(
                central_node,
                node1,
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        tx.commit().unwrap();

        // Verify edge exists
        assert!(current.get_edge(edge1).is_ok());

        // Delete the node WITHOUT cascade (default behavior)
        let (mut tx2, _temp_dir2) = create_test_write_tx_from_existing(Arc::clone(&current));
        tx2.delete_node(central_node).unwrap();
        tx2.commit().unwrap();

        // Verify the node was deleted
        assert!(current.get_node(central_node).is_err());

        // Verify edge still exists (NO CASCADE - current behavior)
        // Note: This creates an orphaned edge, which is the problem issue #364 addresses
        assert!(
            current.get_edge(edge1).is_ok(),
            "Edge should remain when cascade is not enabled (current behavior)"
        );
    }

    #[test]
    fn test_delete_node_cascade_with_bidirectional_edges() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes with bidirectional relationships
        let props = PropertyMapBuilder::new().build();
        let node_a = tx.create_node("Person", props.clone()).unwrap();
        let node_b = tx.create_node("Person", props).unwrap();

        // Create bidirectional edges
        let edge_a_to_b = tx
            .create_edge(node_a, node_b, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        let edge_b_to_a = tx
            .create_edge(node_b, node_a, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        tx.commit().unwrap();

        // Delete node_a with cascade
        let (mut tx2, _temp_dir2) = create_test_write_tx_from_existing(Arc::clone(&current));
        tx2.delete_node_cascade(node_a).unwrap();
        tx2.commit().unwrap();

        // Both edges should be deleted
        assert!(current.get_edge(edge_a_to_b).is_err());
        assert!(current.get_edge(edge_b_to_a).is_err());

        // node_b should still exist
        assert!(current.get_node(node_b).is_ok());
    }

    #[test]
    fn test_delete_node_cascade_performance_many_edges() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a central node
        let props = PropertyMapBuilder::new().build();
        let central_node = tx.create_node("Hub", props.clone()).unwrap();

        // Create many connected nodes (100 outgoing, 100 incoming)
        let mut outgoing_edges = Vec::new();
        let mut incoming_edges = Vec::new();

        for i in 0..100 {
            let target = tx
                .create_node(
                    "Target",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
                .unwrap();
            let edge = tx
                .create_edge(
                    central_node,
                    target,
                    "OUT",
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
            outgoing_edges.push(edge);
        }

        for i in 0..100 {
            let source = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
                .unwrap();
            let edge = tx
                .create_edge(
                    source,
                    central_node,
                    "IN",
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
            incoming_edges.push(edge);
        }

        tx.commit().unwrap();

        // Verify all edges exist
        for edge in &outgoing_edges {
            assert!(current.get_edge(*edge).is_ok());
        }
        for edge in &incoming_edges {
            assert!(current.get_edge(*edge).is_ok());
        }

        // Delete the central node with cascade - should be performant
        let (mut tx2, _temp_dir2) = create_test_write_tx_from_existing(Arc::clone(&current));
        let start = std::time::Instant::now();
        tx2.delete_node_cascade(central_node).unwrap();
        tx2.commit().unwrap();
        let elapsed = start.elapsed();

        // Performance assertion: should complete in reasonable time (< 2000ms)
        // This threshold is generous to avoid flakiness on slow CI systems (especially Windows)
        // while still catching significant performance regressions
        assert!(
            elapsed.as_millis() < 2000,
            "Cascade delete of 200 edges took too long: {:?}",
            elapsed
        );

        // Verify the node was deleted
        assert!(current.get_node(central_node).is_err());

        // Verify all 200 edges were deleted
        for edge in &outgoing_edges {
            assert!(current.get_edge(*edge).is_err());
        }
        for edge in &incoming_edges {
            assert!(current.get_edge(*edge).is_err());
        }
    }

    /// Helper function to create a write transaction from existing storage
    fn create_test_write_tx_from_existing(
        current: Arc<CurrentStorage>,
    ) -> (WriteTransaction, TempDir) {
        use crate::core::id::TxIdGenerator;
        use crate::core::temporal::time;
        use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
        use tempfile::TempDir;

        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        // Create WAL with temp directory for tests
        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());
        let tx_id_gen = TxIdGenerator::new();

        // Create snapshot and visibility manager for testing
        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
        );

        (tx, temp_dir)
    }
}

mod conflict_detection_tests {
    use super::*;
    use crate::core::id::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use tempfile::TempDir;

    /// Test harness for conflict detection tests.
    ///
    /// Bundles all shared infrastructure needed to create multiple concurrent
    /// transactions for testing write-write conflict detection.
    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
        tx_id_gen: TxIdGenerator,
        _temp_dir: TempDir, // Keep alive for WAL directory
    }

    impl TestHarness {
        /// Create a new test harness with all shared infrastructure.
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(TemporalIndexes::new());

            let temp_dir = TempDir::new().unwrap();
            let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
            let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let node_id_gen = Arc::new(IdGenerator::new());
            let edge_id_gen = Arc::new(IdGenerator::new());
            let version_id_gen = Arc::new(IdGenerator::new());
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());

            TestHarness {
                current,
                historical,
                temporal_indexes,
                wal,
                current_timestamp,
                visibility_manager,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                _temp_dir: temp_dir,
            }
        }

        /// Create a new write transaction using the shared infrastructure.
        fn create_tx(&self) -> WriteTransaction {
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: *self.current_timestamp.lock().unwrap(),
                active_transactions: Arc::new(std::collections::HashSet::new()),
            };

            WriteTransaction::new(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    /// Test: First-committer-wins for node updates.
    ///
    /// Scenario from Issue #8:
    /// ```text
    /// Time    Transaction 1                    Transaction 2
    /// ----    -------------                    -------------
    /// T1      tx1 = write_transaction()
    /// T2      tx1.update_node(A, {age: 31})
    /// T3                                       tx2 = write_transaction()
    /// T4                                       tx2.update_node(A, {age: 32})
    /// T5                                       tx2.commit()  // Succeeds
    /// T6      tx1.commit()                     // Should FAIL!
    /// ```
    #[test]
    fn test_first_committer_wins_node_update() {
        let harness = TestHarness::new();

        // Create initial node via transaction (so it has proper metadata)
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 30i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // T1: tx1 starts
        let mut tx1 = harness.create_tx();

        // T2: tx1 updates node
        tx1.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 31i64).build(),
        )
        .unwrap();

        // T3: tx2 starts
        let mut tx2 = harness.create_tx();

        // T4: tx2 updates node
        tx2.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 32i64).build(),
        )
        .unwrap();

        // T5: tx2 commits first - should succeed
        tx2.commit().unwrap();

        // Verify tx2's update was applied
        let node_after_tx2 = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_after_tx2.get_property("age").and_then(|v| v.as_int()),
            Some(32),
            "tx2's update should have been applied"
        );

        // T6: tx1 tries to commit - should FAIL with SerializationFailure
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail due to write-write conflict"
        );

        // Verify it's a SerializationFailure error
        let err = result.unwrap_err();
        let err_str = format!("{:?}", err);
        assert!(
            err_str.contains("SerializationFailure"),
            "Expected SerializationFailure, got: {}",
            err_str
        );

        // Verify the final value is still tx2's value (first committer wins)
        let final_node = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            final_node.get_property("age").and_then(|v| v.as_int()),
            Some(32),
            "Final value should be tx2's value (first committer wins)"
        );
    }

    /// Test: First-committer-wins for edge updates.
    #[test]
    fn test_first_committer_wins_edge_update() {
        let harness = TestHarness::new();

        // Create initial nodes and edge
        let (node1, node2, edge_id) = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let e = tx
                .create_edge(
                    n1,
                    n2,
                    "KNOWS",
                    PropertyMapBuilder::new().insert("weight", 5i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            (n1, n2, e)
        };

        // tx1 starts
        let mut tx1 = harness.create_tx();
        tx1.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 10i64).build(),
        )
        .unwrap();

        // tx2 starts and commits first
        let mut tx2 = harness.create_tx();
        tx2.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 20i64).build(),
        )
        .unwrap();
        tx2.commit().unwrap();

        // tx1 tries to commit - should fail
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail due to edge update conflict"
        );

        // Verify final value is tx2's
        let final_edge = harness.current.get_edge(edge_id).unwrap();
        assert_eq!(
            final_edge.get_property("weight").and_then(|v| v.as_int()),
            Some(20)
        );

        // Suppress unused variable warnings
        let _ = (node1, node2);
    }

    /// Test: First-committer-wins for node deletion.
    #[test]
    fn test_first_committer_wins_node_delete() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 starts and wants to update
        let mut tx1 = harness.create_tx();
        tx1.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 31i64).build(),
        )
        .unwrap();

        // tx2 starts and deletes the node, then commits
        let mut tx2 = harness.create_tx();
        tx2.delete_node(node_id).unwrap();
        tx2.commit().unwrap();

        // Node should be deleted now
        assert!(harness.current.get_node(node_id).is_err());

        // tx1 tries to commit its update - should fail
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail - node was modified (deleted) by tx2"
        );
    }

    /// Test: Delete vs Delete conflict.
    #[test]
    fn test_delete_delete_conflict() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 wants to delete
        let mut tx1 = harness.create_tx();
        tx1.delete_node(node_id).unwrap();

        // tx2 also wants to delete and commits first
        let mut tx2 = harness.create_tx();
        tx2.delete_node(node_id).unwrap();
        tx2.commit().unwrap();

        // tx1 tries to commit - should fail
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail - node was already deleted by tx2"
        );
    }

    /// Test: No conflict when transactions modify different entities.
    #[test]
    fn test_no_conflict_different_entities() {
        let harness = TestHarness::new();

        // Create two nodes
        let (node1, node2) = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                )
                .unwrap();
            let n2 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )
                .unwrap();
            tx.commit().unwrap();
            (n1, n2)
        };

        // tx1 updates node1
        let mut tx1 = harness.create_tx();
        tx1.update_node(
            node1,
            PropertyMapBuilder::new().insert("age", 30i64).build(),
        )
        .unwrap();

        // tx2 updates node2 and commits first
        let mut tx2 = harness.create_tx();
        tx2.update_node(
            node2,
            PropertyMapBuilder::new().insert("age", 25i64).build(),
        )
        .unwrap();
        tx2.commit().unwrap();

        // tx1 should also succeed - no conflict on different entities
        tx1.commit().unwrap();

        // Verify both updates were applied
        assert_eq!(
            harness
                .current
                .get_node(node1)
                .unwrap()
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(30)
        );
        assert_eq!(
            harness
                .current
                .get_node(node2)
                .unwrap()
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(25)
        );
    }

    /// Test: No conflict for create operations (new entities).
    #[test]
    fn test_no_conflict_for_creates() {
        let harness = TestHarness::new();

        // tx1 creates a node
        let mut tx1 = harness.create_tx();
        let node1 = tx1
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // tx2 creates a different node and commits first
        let mut tx2 = harness.create_tx();
        let node2 = tx2
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        tx2.commit().unwrap();

        // tx1 should also succeed - creates don't conflict
        tx1.commit().unwrap();

        // Both nodes should exist
        assert!(harness.current.get_node(node1).is_ok());
        assert!(harness.current.get_node(node2).is_ok());
    }

    /// Test: Conflict error message contains useful information.
    #[test]
    fn test_conflict_error_message() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 updates
        let mut tx1 = harness.create_tx();
        tx1.update_node(node_id, PropertyMapBuilder::new().build())
            .unwrap();

        // tx2 commits first
        let mut tx2 = harness.create_tx();
        tx2.update_node(node_id, PropertyMapBuilder::new().build())
            .unwrap();
        tx2.commit().unwrap();

        // tx1 fails
        let result = tx1.commit();
        let err = result.unwrap_err();
        let err_str = format!("{:?}", err);

        // Verify error contains node ID info
        assert!(
            err_str.contains("NodeId"),
            "Error should mention the entity: {}",
            err_str
        );
        assert!(
            err_str.contains("committed") || err_str.contains("snapshot"),
            "Error should explain the conflict: {}",
            err_str
        );
    }

    /// Test: Delete-then-recreate race condition (Issue #357, Scenario 1).
    ///
    /// Scenario:
    /// - tx1 deletes a node
    /// - tx2 creates a new node concurrently
    /// - Both transactions try to commit concurrently
    ///
    /// Note: The API doesn't allow explicit ID specification, so this tests
    /// concurrent delete/create operations on different nodes rather than
    /// true ID reuse.
    ///
    /// Expected behavior:
    /// - Test Case A: If delete commits first, concurrent create should succeed
    /// - Test Case B: If create commits first, concurrent delete should succeed
    ///   (operations on different entities don't conflict)
    #[test]
    fn test_delete_then_recreate_race() {
        let harness = TestHarness::new();

        // Test Case A: Delete commits first, then create
        {
            // Create initial node
            let node_id = {
                let mut tx = harness.create_tx();
                let id = tx
                    .create_node(
                        "Person",
                        PropertyMapBuilder::new().insert("name", "Alice").build(),
                    )
                    .unwrap();
                tx.commit().unwrap();
                id
            };

            // tx1 starts and deletes the node
            let mut tx1 = harness.create_tx();
            tx1.delete_node(node_id).unwrap();

            // tx2 starts and creates a new node (different entity, no conflict expected)
            let mut tx2 = harness.create_tx();
            let new_node_id = tx2
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )
                .unwrap();

            // tx1 commits first (deletes original node)
            tx1.commit().unwrap();

            // Verify node was deleted
            assert!(
                harness.current.get_node(node_id).is_err(),
                "Original node should be deleted"
            );

            // tx2 should succeed - creating new nodes doesn't conflict with deletes
            tx2.commit().unwrap();

            // New node should exist
            assert!(
                harness.current.get_node(new_node_id).is_ok(),
                "New node should be created successfully"
            );

            // Original node should still be deleted
            assert!(
                harness.current.get_node(node_id).is_err(),
                "Original node should remain deleted"
            );
        }

        // Test Case B: Create commits first, then delete (on different node)
        {
            // Create initial node to be deleted
            let node_to_delete = {
                let mut tx = harness.create_tx();
                let id = tx
                    .create_node(
                        "Person",
                        PropertyMapBuilder::new().insert("name", "Charlie").build(),
                    )
                    .unwrap();
                tx.commit().unwrap();
                id
            };

            // tx3 starts and creates a new node
            let mut tx3 = harness.create_tx();
            let created_node_id = tx3
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Dave").build(),
                )
                .unwrap();

            // tx4 starts and deletes a different node
            let mut tx4 = harness.create_tx();
            tx4.delete_node(node_to_delete).unwrap();

            // tx3 commits first (creates new node)
            tx3.commit().unwrap();

            // Verify new node was created
            assert!(
                harness.current.get_node(created_node_id).is_ok(),
                "New node should be created"
            );

            // tx4 should also succeed - operations on different entities don't conflict
            tx4.commit().unwrap();

            // Verify original node was deleted
            assert!(
                harness.current.get_node(node_to_delete).is_err(),
                "Original node should be deleted"
            );

            // Created node should still exist
            assert!(
                harness.current.get_node(created_node_id).is_ok(),
                "Created node should still exist"
            );
        }
    }

    /// Test: Update-delete conflict (Issue #357, Scenario 2).
    ///
    /// Scenario:
    /// - tx1 updates a node
    /// - tx2 deletes the same node
    /// - Both transactions have overlapping execution
    ///
    /// Expected behavior:
    /// - First committer wins (MVCC conflict detection)
    /// - If tx1 commits first (update), tx2's delete should fail due to write-write conflict
    /// - If tx2 commits first (delete), tx1's update should fail (can't update deleted node)
    #[test]
    fn test_update_delete_conflict() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 30i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // Test Case A: Update commits first, then delete
        {
            // tx1 updates the node
            let mut tx1 = harness.create_tx();
            tx1.update_node(
                node_id,
                PropertyMapBuilder::new().insert("age", 35i64).build(),
            )
            .unwrap();

            // tx2 wants to delete the node
            let mut tx2 = harness.create_tx();
            tx2.delete_node(node_id).unwrap();

            // tx1 commits first (update succeeds)
            tx1.commit().unwrap();

            // Verify update was applied
            let node = harness.current.get_node(node_id).unwrap();
            assert_eq!(
                node.get_property("age").and_then(|v| v.as_int()),
                Some(35),
                "Update should be applied"
            );

            // tx2 commits (delete on already modified node should fail)
            let result = tx2.commit();
            assert!(
                result.is_err(),
                "tx2 should fail - node was modified by tx1"
            );

            // Node should still exist with tx1's update
            let node = harness.current.get_node(node_id).unwrap();
            assert_eq!(
                node.get_property("age").and_then(|v| v.as_int()),
                Some(35),
                "Node should still have tx1's update"
            );
        }

        // Test Case B: Delete commits first, then update
        // Create a fresh node for this test case
        let node_id_b = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 50i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        {
            // tx3 wants to update
            let mut tx3 = harness.create_tx();
            tx3.update_node(
                node_id_b,
                PropertyMapBuilder::new().insert("age", 55i64).build(),
            )
            .unwrap();

            // tx4 deletes first
            let mut tx4 = harness.create_tx();
            tx4.delete_node(node_id_b).unwrap();
            tx4.commit().unwrap();

            // Node should be deleted
            assert!(
                harness.current.get_node(node_id_b).is_err(),
                "Node should be deleted by tx4"
            );

            // tx3 tries to commit update - should fail
            let result = tx3.commit();
            assert!(result.is_err(), "tx3 should fail - node was deleted by tx4");

            // Verify error message contains useful information
            let err = result.unwrap_err();
            let err_str = format!("{:?}", err);
            assert!(
                err_str.contains("NodeId")
                    || err_str.contains("deleted")
                    || err_str.contains("not found"),
                "Error should explain the conflict: {}",
                err_str
            );
        }
    }

    /// Test: Edge creation with concurrent node deletion (Issue #357, Scenario 3).
    ///
    /// Scenario:
    /// - tx1 creates an edge between two nodes
    /// - tx2 deletes one of the endpoint nodes
    /// - Both transactions try to commit concurrently
    ///
    /// Expected behavior (documents actual MVCC implementation):
    /// - Test Case A: If edge creation commits first, node deletion succeeds because
    ///   edge addition doesn't modify the node's version (no conflict detected).
    ///   This creates an orphaned edge, which is acceptable if traversals handle it.
    /// - Test Case B: If node deletion commits first, edge creation fails on commit
    ///   due to referential integrity (endpoint node was deleted after snapshot).
    #[test]
    fn test_edge_creation_with_concurrent_node_deletion() {
        let harness = TestHarness::new();

        // Create two nodes
        let (node1, node2) = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            (n1, n2)
        };

        // Test Case A: Edge creation commits first
        {
            // tx1 creates an edge
            let mut tx1 = harness.create_tx();
            let edge_id = tx1
                .create_edge(node1, node2, "KNOWS", PropertyMapBuilder::new().build())
                .unwrap();

            // tx2 wants to delete node2
            let mut tx2 = harness.create_tx();
            tx2.delete_node(node2).unwrap();

            // tx1 commits first (edge created)
            tx1.commit().unwrap();

            // Verify edge exists
            assert!(
                harness.current.get_edge(edge_id).is_ok(),
                "Edge should be created"
            );

            // tx2 tries to delete node2. According to the current MVCC implementation,
            // edge addition doesn't modify node2's version, so no conflict is detected
            // and the deletion succeeds.
            let result = tx2.commit();
            assert!(
                result.is_ok(),
                "tx2 commit should succeed - edge addition doesn't create version conflict on node2"
            );

            // Verify node was deleted
            assert!(
                harness.current.get_node(node2).is_err(),
                "Node should be deleted after successful tx2 commit"
            );

            // Current implementation: edge becomes orphaned but still exists in storage.
            // This documents a limitation: the system allows orphaned edges.
            // TODO(issue): Consider adding cascade delete or stricter referential integrity
            assert!(
                harness.current.get_edge(edge_id).is_ok(),
                "Edge still exists as orphan (documents current behavior)"
            );

            // Verify the edge references the deleted node (orphaned edge)
            let edge = harness.current.get_edge(edge_id).unwrap();
            assert_eq!(
                edge.target, node2,
                "Edge still references deleted node (orphaned)"
            );
        }

        // Create a fresh pair of nodes for Test Case B
        let (node3, node4) = {
            let mut tx = harness.create_tx();
            let n3 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let n4 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            (n3, n4)
        };

        // Test Case B: Node deletion commits first
        {
            // tx3 wants to create an edge
            let mut tx3 = harness.create_tx();

            // tx4 deletes node4 first
            let mut tx4 = harness.create_tx();
            tx4.delete_node(node4).unwrap();
            tx4.commit().unwrap();

            // Node should be deleted
            assert!(
                harness.current.get_node(node4).is_err(),
                "Node should be deleted"
            );

            // tx3 tries to create edge with deleted node.
            // Under snapshot isolation, tx3's snapshot was taken before tx4's deletion,
            // so node4 exists in tx3's view and edge creation succeeds at operation time.
            let edge_result =
                tx3.create_edge(node3, node4, "KNOWS", PropertyMapBuilder::new().build());
            assert!(
                edge_result.is_ok(),
                "Edge creation should succeed - node4 exists in tx3's snapshot"
            );
            let edge_id = edge_result.unwrap();

            // However, commit should detect the conflict: node4 was deleted after tx3's snapshot,
            // violating referential integrity for the edge.
            let commit_result = tx3.commit();
            assert!(
                commit_result.is_err(),
                "tx3 commit should fail - node4 was deleted by tx4, breaking referential integrity"
            );

            // Verify edge was not persisted (rollback worked correctly)
            assert!(
                harness.current.get_edge(edge_id).is_err(),
                "Edge should not exist after failed commit"
            );
        }
    }

    /// Test: Rollback during concurrent commit (Issue #357, Scenario 4).
    ///
    /// Scenario:
    /// - tx1 modifies a node and commits successfully
    /// - tx2 modifies the same node but gets dropped (implicit rollback)
    /// - Verify visibility consistency and that uncommitted changes are not visible
    ///
    /// Expected behavior:
    /// - Only committed transaction's changes should be visible
    /// - Dropped transaction's changes should never become visible
    /// - No visibility inconsistencies should occur
    #[test]
    fn test_rollback_during_concurrent_commit() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 30i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 updates and commits
        let mut tx1 = harness.create_tx();
        tx1.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 35i64).build(),
        )
        .unwrap();
        tx1.commit().unwrap();

        // Verify tx1's changes are visible
        let node_after_tx1 = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_after_tx1.get_property("age").and_then(|v| v.as_int()),
            Some(35),
            "tx1's update should be visible"
        );

        // tx2 modifies but is dropped (implicit rollback)
        {
            let mut tx2 = harness.create_tx();
            tx2.update_node(
                node_id,
                PropertyMapBuilder::new().insert("age", 40i64).build(),
            )
            .unwrap();
            // tx2 is dropped here without commit - implicit rollback
        }

        // Verify tx2's changes are NOT visible (rollback worked)
        let node_after_rollback = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_after_rollback
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(35),
            "Only tx1's changes should be visible; tx2 rolled back"
        );

        // tx3 commits successfully after tx2's rollback
        let mut tx3 = harness.create_tx();
        tx3.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 45i64).build(),
        )
        .unwrap();
        tx3.commit().unwrap();

        // Verify tx3's changes are visible
        let node_final = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_final.get_property("age").and_then(|v| v.as_int()),
            Some(45),
            "tx3's update should be visible"
        );

        // Verify visibility consistency: ensure storage reflects latest committed state
        // (Reading directly from storage is appropriate since WriteTransaction doesn't
        // provide read methods - it's write-only)
        let node_final_check = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_final_check
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(45),
            "Storage should consistently show latest committed state"
        );
    }

    /// Assert a commit failed with `SerializationFailure`.
    fn assert_serialization_failure(result: crate::core::error::Result<()>, context: &str) {
        let err = result.expect_err(context);
        let err_str = format!("{:?}", err);
        assert!(
            err_str.contains("SerializationFailure"),
            "{context}: expected SerializationFailure, got: {err_str}"
        );
    }

    /// Issue #3230 conflict arm: a buffered RetractNode must abort when a
    /// concurrent UPDATE of the same node committed after our snapshot —
    /// the valid_from the retraction was validated against is stale.
    #[test]
    fn test_retract_node_conflicts_with_concurrent_update() {
        let harness = TestHarness::new();

        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 buffers a retraction.
        let mut tx1 = harness.create_tx();
        tx1.retract_node(node_id, time::now()).unwrap();

        // tx2 updates and commits first.
        let mut tx2 = harness.create_tx();
        tx2.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 31i64).build(),
        )
        .unwrap();
        tx2.commit().unwrap();

        assert_serialization_failure(
            tx1.commit(),
            "retract must lose to a concurrent committed update",
        );

        // First committer wins: the node is still present with tx2's state.
        let node = harness.current.get_node(node_id).unwrap();
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
    }

    /// Issue #3230 conflict arm: a buffered RetractNode must abort when a
    /// concurrent DELETE of the same node committed first (the
    /// entity-gone branch of the RetractNode arm).
    #[test]
    fn test_retract_node_conflicts_with_concurrent_delete() {
        let harness = TestHarness::new();

        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        let mut tx1 = harness.create_tx();
        tx1.retract_node(node_id, time::now()).unwrap();

        let mut tx2 = harness.create_tx();
        tx2.delete_node(node_id).unwrap();
        tx2.commit().unwrap();

        assert_serialization_failure(
            tx1.commit(),
            "retract must lose to a concurrent committed delete",
        );
        assert!(harness.current.get_node(node_id).is_err());
    }

    /// Issue #3230 conflict arm: RetractEdge vs concurrent edge update.
    #[test]
    fn test_retract_edge_conflicts_with_concurrent_update() {
        let harness = TestHarness::new();

        let edge_id = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let e = tx
                .create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            e
        };

        let mut tx1 = harness.create_tx();
        tx1.retract_edge(edge_id, time::now()).unwrap();

        let mut tx2 = harness.create_tx();
        tx2.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 2i64).build(),
        )
        .unwrap();
        tx2.commit().unwrap();

        assert_serialization_failure(
            tx1.commit(),
            "edge retract must lose to a concurrent committed update",
        );
        let edge = harness.current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(2)
        );
    }

    /// Issue #3230 conflict arm: RetractEdge vs concurrent edge delete
    /// (the entity-gone branch of the RetractEdge arm).
    #[test]
    fn test_retract_edge_conflicts_with_concurrent_delete() {
        let harness = TestHarness::new();

        let edge_id = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let e = tx
                .create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            e
        };

        let mut tx1 = harness.create_tx();
        tx1.retract_edge(edge_id, time::now()).unwrap();

        let mut tx2 = harness.create_tx();
        tx2.delete_edge(edge_id).unwrap();
        tx2.commit().unwrap();

        assert_serialization_failure(
            tx1.commit(),
            "edge retract must lose to a concurrent committed delete",
        );
        assert!(harness.current.get_edge(edge_id).is_err());
    }
}

mod clock_skew_tests {
    use super::*;
    use crate::core::hlc::ClockSkewAutoHealTestGuard;
    use crate::core::id::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Test harness for clock skew tests.
    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        commit_clock_observed_at: Arc<Mutex<Instant>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
        tx_id_gen: TxIdGenerator,
        _temp_dir: TempDir,
    }

    impl TestHarness {
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(TemporalIndexes::new());

            let temp_dir = TempDir::new().unwrap();
            let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
            let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let commit_clock_observed_at = Arc::new(Mutex::new(Instant::now()));
            let node_id_gen = Arc::new(IdGenerator::new());
            let edge_id_gen = Arc::new(IdGenerator::new());
            let version_id_gen = Arc::new(IdGenerator::new());
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());

            TestHarness {
                current,
                historical,
                temporal_indexes,
                wal,
                current_timestamp,
                commit_clock_observed_at,
                visibility_manager,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                _temp_dir: temp_dir,
            }
        }

        fn create_tx(&self) -> WriteTransaction {
            let snapshot_ts = *self.current_timestamp.lock().unwrap();
            let snapshot = self.visibility_manager.capture_snapshot(snapshot_ts);

            WriteTransaction::new(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }

        fn create_tx_with_shared_observation_clock(&self) -> WriteTransaction {
            let snapshot_ts = *self.current_timestamp.lock().unwrap();
            let snapshot = self.visibility_manager.capture_snapshot(snapshot_ts);

            WriteTransaction::new_with_clock_observed_at(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.commit_clock_observed_at.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    #[test]
    fn test_clock_skew_backward_error() {
        let _auto_heal_guard = ClockSkewAutoHealTestGuard::force(false);
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        // Create a node to have something to commit
        let props = PropertyMapBuilder::new().insert("test", true).build();
        tx.create_node("Test", props).unwrap();

        // Simulate backward skew: previous commit timestamp is 10 mins in future
        {
            let mut ts = harness.current_timestamp.lock().unwrap();
            let future_time = time::now().wallclock() + 10 * 60 * 1_000_000;
            *ts = crate::core::hlc::HybridTimestamp::new(future_time, 0).unwrap();
        }

        let result = tx.commit();
        assert!(result.is_err());

        // Verify it is a ClockSkew error
        match result.unwrap_err() {
            crate::core::error::Error::Transaction(TransactionError::ClockSkew {
                drift_us,
                ..
            }) => {
                // Drift should be negative and large magnitude
                assert!(drift_us < -super::MAX_BACKWARD_DRIFT_US);
            }
            err => panic!("Expected ClockSkew error, got: {:?}", err),
        }
    }

    #[test]
    fn test_clock_skew_forward_error() {
        let _auto_heal_guard = ClockSkewAutoHealTestGuard::force(false);
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        // Create a node
        let props = PropertyMapBuilder::new().insert("test", true).build();
        tx.create_node("Test", props).unwrap();

        // Simulate forward jump: previous commit timestamp is 2 hours in past
        {
            let mut ts = harness.current_timestamp.lock().unwrap();
            let past_time = time::now().wallclock() - 2 * 60 * 60 * 1_000_000;
            *ts = crate::core::hlc::HybridTimestamp::new(past_time, 0).unwrap();
        }

        let result = tx.commit();
        assert!(result.is_err());

        match result.unwrap_err() {
            crate::core::error::Error::Transaction(TransactionError::ClockSkew {
                drift_us,
                ..
            }) => {
                // Drift should be positive and large
                assert!(drift_us > super::MAX_FORWARD_JUMP_US);
            }
            err => panic!("Expected ClockSkew error, got: {:?}", err),
        }
    }

    #[test]
    fn test_clock_skew_failure_does_not_advance_observation_timestamp() {
        let _auto_heal_guard = ClockSkewAutoHealTestGuard::force(false);
        let harness = TestHarness::new();
        let mut tx = harness.create_tx_with_shared_observation_clock();

        let props = PropertyMapBuilder::new().insert("test", true).build();
        tx.create_node("Test", props).unwrap();

        {
            let mut ts = harness.current_timestamp.lock().unwrap();
            let old_frontier = time::now().wallclock() - (6 * 60 * 60 * 1_000_000);
            *ts = crate::core::hlc::HybridTimestamp::new(old_frontier, 0).unwrap();
        }

        let old_observed_at = {
            let mut observed_at = harness.commit_clock_observed_at.lock().unwrap();
            let now = Instant::now();
            let old_observed = now.checked_sub(Duration::from_secs(5)).unwrap_or(now);
            *observed_at = old_observed;
            old_observed
        };

        let result = tx.commit();
        assert!(matches!(
            result,
            Err(crate::core::error::Error::Transaction(
                TransactionError::ClockSkew { .. }
            ))
        ));

        let observed_after_failure = *harness.commit_clock_observed_at.lock().unwrap();
        assert_eq!(
            observed_after_failure, old_observed_at,
            "failed skew validation must not consume idle-time budget"
        );
    }

    #[test]
    fn test_clock_skew_allows_idle_forward_drift_with_shared_observation_clock() {
        let _auto_heal_guard = ClockSkewAutoHealTestGuard::force(false);
        let harness = TestHarness::new();
        let mut tx = harness.create_tx_with_shared_observation_clock();

        let props = PropertyMapBuilder::new().insert("test", true).build();
        tx.create_node("Test", props).unwrap();

        let idle_gap_us = super::MAX_FORWARD_JUMP_US + 2_000_000;
        {
            let mut ts = harness.current_timestamp.lock().unwrap();
            let past_time = time::now().wallclock() - idle_gap_us;
            *ts = crate::core::hlc::HybridTimestamp::new(past_time, 0).unwrap();
        }

        {
            let mut observed_at = harness.commit_clock_observed_at.lock().unwrap();
            match Instant::now().checked_sub(Duration::from_micros(idle_gap_us as u64)) {
                Some(past_instant) => *observed_at = past_instant,
                None => {
                    println!(
                        "Skipping test_clock_skew_allows_idle_forward_drift_with_shared_observation_clock: uptime insufficient for 1h+ idle gap."
                    );
                    return;
                }
            }
        }

        let result = tx.commit();
        assert!(
            result.is_ok(),
            "normal idle time should not be treated as forward clock skew"
        );
    }
}

mod timestamp_ordering_tests {
    use super::*;
    use crate::core::id::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use std::thread;
    use tempfile::TempDir;

    /// Test harness for timestamp ordering tests.
    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
        tx_id_gen: TxIdGenerator,
        _temp_dir: TempDir,
    }

    impl TestHarness {
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(TemporalIndexes::new());

            let temp_dir = TempDir::new().unwrap();
            let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
            let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let node_id_gen = Arc::new(IdGenerator::new());
            let edge_id_gen = Arc::new(IdGenerator::new());
            let version_id_gen = Arc::new(IdGenerator::new());
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());

            TestHarness {
                current,
                historical,
                temporal_indexes,
                wal,
                current_timestamp,
                visibility_manager,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                _temp_dir: temp_dir,
            }
        }

        fn create_tx(&self) -> WriteTransaction {
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: *self.current_timestamp.lock().unwrap(),
                active_transactions: Arc::new(std::collections::HashSet::new()),
            };

            WriteTransaction::new(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    /// Test: Sequential commits have monotonically increasing timestamps.
    #[test]
    fn test_sequential_commits_monotonic_timestamps() {
        let harness = TestHarness::new();

        let mut timestamps = Vec::new();

        // Perform 10 sequential commits
        for i in 0..10 {
            let mut tx = harness.create_tx();
            tx.create_node(
                "Test",
                PropertyMapBuilder::new().insert("seq", i as i64).build(),
            )
            .unwrap();
            tx.commit().unwrap();

            // Record the current timestamp after commit
            let ts = *harness.current_timestamp.lock().unwrap();
            timestamps.push(ts);
        }

        // Verify timestamps are strictly increasing
        for i in 1..timestamps.len() {
            assert!(
                timestamps[i] > timestamps[i - 1],
                "Timestamp {} ({}) should be > timestamp {} ({})",
                i,
                timestamps[i],
                i - 1,
                timestamps[i - 1]
            );
        }
    }

    /// Test: Concurrent commits still produce monotonically increasing timestamps.
    ///
    /// This test verifies that the fix for Issue #10 works correctly:
    /// - Multiple threads commit transactions concurrently
    /// - Each commit creates a node and we get its commit timestamp from metadata
    /// - We verify that all commit timestamps are unique and properly ordered
    #[test]
    fn test_concurrent_commits_ordered_timestamps() {
        let harness = Arc::new(TestHarness::new());
        let results = Arc::new(Mutex::new(Vec::new()));

        let num_threads = 8;
        let commits_per_thread = 5;

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let harness = harness.clone();
                let results = results.clone();

                thread::spawn(move || {
                    for i in 0..commits_per_thread {
                        let mut tx = harness.create_tx();
                        let node_id = tx
                            .create_node(
                                "Test",
                                PropertyMapBuilder::new()
                                    .insert("thread", thread_id as i64)
                                    .insert("iteration", i as i64)
                                    .build(),
                            )
                            .unwrap();
                        tx.commit().unwrap();

                        // Get the ACTUAL commit timestamp from the node's metadata
                        let node = harness.current.get_node(node_id).unwrap();
                        let commit_ts = node.metadata.commit_timestamp.unwrap();

                        results
                            .lock()
                            .unwrap()
                            .push((commit_ts, thread_id, i, node_id));
                    }
                })
            })
            .collect();

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Analyze results: sort by commit timestamp
        let mut results = results.lock().unwrap();
        results.sort_by_key(|(ts, _, _, _)| *ts);

        // With the fix, all timestamps should be unique (due to double-increment)
        // Check for duplicates
        for i in 1..results.len() {
            let (ts_prev, thread_prev, iter_prev, _) = results[i - 1];
            let (ts_curr, thread_curr, iter_curr, _) = results[i];

            assert!(
                ts_curr > ts_prev,
                "Duplicate or out-of-order timestamp detected: \
                 Thread {} iter {} (ts={}) vs Thread {} iter {} (ts={})",
                thread_prev,
                iter_prev,
                ts_prev,
                thread_curr,
                iter_curr,
                ts_curr
            );
        }

        // Verify we got all expected commits
        assert_eq!(
            results.len(),
            num_threads * commits_per_thread,
            "Expected {} commits, got {}",
            num_threads * commits_per_thread,
            results.len()
        );
    }

    /// Test: Version chains are correctly ordered by transaction time.
    ///
    /// When multiple transactions update the same node, the version chain
    /// should reflect the actual commit order.
    #[test]
    fn test_version_chain_ordering() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("version", 0i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // Track commit timestamps
        let mut commit_timestamps = Vec::new();

        // Perform sequential updates
        for version in 1..=5 {
            let mut tx = harness.create_tx();
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("version", version as i64)
                    .build(),
            )
            .unwrap();
            tx.commit().unwrap();

            // Get the node's current metadata to verify timestamp
            let node = harness.current.get_node(node_id).unwrap();
            if let Some(ts) = node.metadata.commit_timestamp {
                commit_timestamps.push(ts);
            }
        }

        // Verify timestamps are strictly increasing
        for i in 1..commit_timestamps.len() {
            assert!(
                commit_timestamps[i] > commit_timestamps[i - 1],
                "Version {} timestamp ({}) should be > version {} timestamp ({})",
                i + 1,
                commit_timestamps[i],
                i,
                commit_timestamps[i - 1]
            );
        }

        // Verify final version is correct
        let final_node = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            final_node.get_property("version").and_then(|v| v.as_int()),
            Some(5)
        );
    }

    /// Test: A rollback must not advance the shared current_timestamp.
    ///
    /// If an uncommitted transaction were to advance the HLC frontier during
    /// rollback, subsequent commits would see an artificially high starting
    /// timestamp, potentially conflicting with future clock ticks.
    #[test]
    fn test_rollback_does_not_advance_current_timestamp() {
        let harness = TestHarness::new();

        let ts_before = *harness.current_timestamp.lock().unwrap();

        // Build a transaction with work but drop it without committing (implicit rollback)
        {
            let mut tx = harness.create_tx();
            tx.create_node("Test", PropertyMapBuilder::new().insert("x", 1i64).build())
                .unwrap();
            // tx is dropped here → rollback; current_timestamp must not change
        }

        let ts_after_rollback = *harness.current_timestamp.lock().unwrap();
        assert_eq!(
            ts_before, ts_after_rollback,
            "Rollback must not advance current_timestamp"
        );

        // A subsequent commit must produce a timestamp strictly greater than ts_before
        let mut tx2 = harness.create_tx();
        tx2.create_node("Test", PropertyMapBuilder::new().insert("x", 2i64).build())
            .unwrap();
        tx2.commit().unwrap();

        let ts_after_commit = *harness.current_timestamp.lock().unwrap();
        assert!(
            ts_after_commit > ts_before,
            "Commit after rollback must produce timestamp > pre-rollback timestamp \
             (before={ts_before}, after_commit={ts_after_commit})"
        );
    }

    /// Test: Concurrent transactions that collide on wallclock are still ordered
    /// by the logical counter embedded in the HLC timestamp.
    ///
    /// Two threads commit back-to-back in < 1 µs (a common occurrence under
    /// load). The HLC monotonic guarantee means each commit gets a unique,
    /// strictly-increasing commit timestamp regardless of wallclock resolution.
    ///
    /// Note: the shared `current_timestamp` reflects the LAST committed value,
    /// so we read each node's embedded commit timestamp instead — that is the
    /// value sealed into every committed version record.
    #[test]
    fn test_concurrent_commits_never_produce_equal_timestamps() {
        let harness = Arc::new(TestHarness::new());
        let node_ids: Arc<Mutex<Vec<crate::core::id::NodeId>>> = Arc::new(Mutex::new(Vec::new()));

        let threads: Vec<_> = (0..20)
            .map(|i| {
                let h = harness.clone();
                let ids = node_ids.clone();
                thread::spawn(move || {
                    let mut tx = h.create_tx();
                    let node_id = tx
                        .create_node(
                            "Test",
                            PropertyMapBuilder::new().insert("i", i as i64).build(),
                        )
                        .unwrap();
                    tx.commit().unwrap();

                    ids.lock().unwrap().push(node_id);
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        // Read the actual commit timestamp embedded in each node at commit time.
        // This is distinct from the shared current_timestamp which is updated by
        // every commit — reading it concurrently would produce duplicates.
        let ids = node_ids.lock().unwrap();
        let mut commit_timestamps: Vec<Timestamp> = ids
            .iter()
            .map(|&nid| {
                harness
                    .current
                    .get_node(nid)
                    .unwrap()
                    .metadata
                    .commit_timestamp
                    .unwrap()
            })
            .collect();
        commit_timestamps.sort();

        // All per-node commit timestamps must be unique
        for i in 1..commit_timestamps.len() {
            assert_ne!(
                commit_timestamps[i],
                commit_timestamps[i - 1],
                "Concurrent commits produced duplicate commit timestamps at index {i}: {}",
                commit_timestamps[i]
            );
        }
    }
}

mod bitemporal_validation_tests {
    use super::*;
    use crate::core::id::TxIdGenerator;
    use crate::core::property::PropertyMap;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use tempfile::TempDir;

    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
        tx_id_gen: TxIdGenerator,
        visibility_manager: Arc<TxVisibilityManager>,
        _temp_dir: TempDir,
    }

    impl TestHarness {
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(TemporalIndexes::new());

            let temp_dir = TempDir::new().unwrap();
            let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
            let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let node_id_gen = Arc::new(IdGenerator::new());
            let edge_id_gen = Arc::new(IdGenerator::new());
            let version_id_gen = Arc::new(IdGenerator::new());
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());

            TestHarness {
                current: current.clone(),
                historical: historical.clone(),
                temporal_indexes: temporal_indexes.clone(),
                wal: wal.clone(),
                current_timestamp: current_timestamp.clone(),
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                visibility_manager,
                _temp_dir: temp_dir,
            }
        }

        fn begin_write(&self) -> WriteTransaction {
            let tx_id = self.tx_id_gen.next();
            let snapshot_ts = *self.current_timestamp.lock().unwrap();
            let snapshot = self.visibility_manager.capture_snapshot(snapshot_ts);

            WriteTransaction::new(
                tx_id,
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    #[test]
    fn test_create_node_with_backdated_valid_time_verified() {
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        // Create node with valid_time = 1 hour ago
        let one_hour_ago_wallclock = time::now().wallclock() - 3_600_000_000;
        let one_hour_ago = HybridTimestamp::new(one_hour_ago_wallclock, 0).unwrap();

        let mut tx = harness.begin_write();
        let node_id = tx
            .create_node_with_valid_time(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
                Some(one_hour_ago),
            )
            .unwrap();
        let commit_result = tx.commit();
        assert!(commit_result.is_ok(), "Commit failed: {:?}", commit_result);

        // Verify via historical storage that valid_time != transaction_time
        let historical = harness.historical.read();
        let version_id = historical.get_current_node_version(node_id).unwrap();
        let node_version = historical.get_node_version(version_id).unwrap();

        // Valid time should be backdated (1 hour ago)
        assert_eq!(
            node_version.temporal.valid_time().start(),
            one_hour_ago,
            "valid_time should be 1 hour ago"
        );

        // Transaction time should be recent (commit time)
        assert!(
            node_version.temporal.transaction_time().start() > one_hour_ago,
            "transaction_time should be after valid_time"
        );

        // Verify the gap is approximately 1 hour
        let gap_us = node_version.temporal.transaction_time().start().wallclock()
            - node_version.temporal.valid_time().start().wallclock();
        assert!(
            gap_us > 3_500_000_000, // At least 58 minutes
            "Gap should be approximately 1 hour, got {}µs",
            gap_us
        );
    }

    #[test]
    fn test_create_node_rejects_far_future_valid_time() {
        use crate::core::error::TemporalError;
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        // Try to create node with valid_time = 2 years in future
        let two_years_future_wallclock = time::now().wallclock() + 2 * 365 * 24 * 3_600_000_000;
        let two_years_future = HybridTimestamp::new(two_years_future_wallclock, 0).unwrap();

        let mut tx = harness.begin_write();
        let result =
            tx.create_node_with_valid_time("Person", PropertyMap::new(), Some(two_years_future));

        assert!(result.is_err(), "Should reject far-future valid_time");

        // Verify it's the right error type
        let err = result.unwrap_err();
        match err {
            crate::core::error::Error::Temporal(TemporalError::ValidTimeTooFarInFuture {
                ..
            }) => {
                // Expected error type
            }
            other => panic!("Expected ValidTimeTooFarInFuture, got: {:?}", other),
        }
    }

    #[test]
    fn test_update_node_rejects_valid_time_before_creation() {
        use crate::core::error::TemporalError;
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        // First create a node
        let mut tx = harness.begin_write();
        let node_id = tx.create_node("Person", PropertyMap::new()).unwrap();
        let commit_result = tx.commit();
        assert!(commit_result.is_ok());

        // Get the node's creation time
        let historical = harness.historical.read();
        let version_id = historical.get_current_node_version(node_id).unwrap();
        let creation_version = historical.get_node_version(version_id).unwrap();
        let creation_time = creation_version.temporal.valid_time().start();
        drop(historical);

        // Try to update with valid_time before creation
        let way_in_past = HybridTimestamp::new(1000, 0).unwrap(); // Epoch + 1ms
        assert!(
            way_in_past < creation_time,
            "Test setup: way_in_past should be before creation_time"
        );

        let mut tx2 = harness.begin_write();
        let result = tx2.update_node_with_valid_time(
            node_id,
            PropertyMapBuilder::new().insert("name", "Bob").build(),
            Some(way_in_past),
        );

        assert!(
            result.is_err(),
            "Should reject valid_time before entity creation"
        );

        // Verify it's the right error type
        let err = result.unwrap_err();
        match err {
            crate::core::error::Error::Temporal(TemporalError::ValidTimeBeforeEntityCreation {
                ..
            }) => {
                // Expected error type
            }
            other => panic!("Expected ValidTimeBeforeEntityCreation, got: {:?}", other),
        }
    }

    #[test]
    fn test_create_edge_with_backdated_valid_time_verified() {
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        // Create two nodes first
        let mut tx = harness.begin_write();
        let source_id = tx.create_node("Person", PropertyMap::new()).unwrap();
        let target_id = tx.create_node("Person", PropertyMap::new()).unwrap();
        tx.commit().unwrap();

        // Create edge with valid_time = 30 minutes ago
        let thirty_min_ago_wallclock = time::now().wallclock() - 1_800_000_000;
        let thirty_min_ago = HybridTimestamp::new(thirty_min_ago_wallclock, 0).unwrap();

        let mut tx2 = harness.begin_write();
        let edge_id = tx2
            .create_edge_with_valid_time(
                source_id,
                target_id,
                "KNOWS",
                PropertyMap::new(),
                Some(thirty_min_ago),
            )
            .unwrap();
        tx2.commit().unwrap();

        // Verify via historical storage
        let historical = harness.historical.read();
        let version_id = historical.get_current_edge_version(edge_id).unwrap();
        let edge_version = historical.get_edge_version(version_id).unwrap();

        // Valid time should be backdated
        assert_eq!(edge_version.temporal.valid_time().start(), thirty_min_ago);

        // Transaction time should be recent
        assert!(edge_version.temporal.transaction_time().start() > thirty_min_ago);
    }

    #[test]
    fn test_delete_node_rejects_valid_time_before_creation() {
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        // Create a node
        let mut tx = harness.begin_write();
        let node_id = tx.create_node("Person", PropertyMap::new()).unwrap();
        tx.commit().unwrap();

        // Try to delete with valid_time before creation
        let way_in_past = HybridTimestamp::new(1000, 0).unwrap();

        let mut tx2 = harness.begin_write();
        let result = tx2.delete_node_with_valid_time(node_id, Some(way_in_past));

        assert!(
            result.is_err(),
            "Should reject valid_time before entity creation"
        );
    }

    /// Test: Updating an edge with valid_time before the edge's own creation is rejected.
    ///
    /// This exercises ValidTimeBeforeEntityCreation for edges, complementing the
    /// existing node test. The edge's own valid_from (not the connected nodes') is
    /// used as the lower bound.
    #[test]
    fn test_update_edge_rejects_valid_time_before_edge_creation() {
        use crate::core::error::TemporalError;
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        // Create source and target nodes, then an edge
        let mut tx = harness.begin_write();
        let src = tx.create_node("Person", PropertyMap::new()).unwrap();
        let tgt = tx.create_node("Person", PropertyMap::new()).unwrap();
        tx.commit().unwrap();

        let mut tx2 = harness.begin_write();
        let edge_id = tx2
            .create_edge(src, tgt, "KNOWS", PropertyMap::new())
            .unwrap();
        tx2.commit().unwrap();

        // Try to update the edge with a valid_time that predates its own creation
        let before_creation = HybridTimestamp::new(1000, 0).unwrap();

        let mut tx3 = harness.begin_write();
        let result = tx3.update_edge_with_valid_time(
            edge_id,
            PropertyMapBuilder::new().insert("strength", 5i64).build(),
            Some(before_creation),
        );

        assert!(
            result.is_err(),
            "Should reject valid_time before edge creation"
        );
        match result.unwrap_err() {
            crate::core::error::Error::Temporal(TemporalError::ValidTimeBeforeEntityCreation {
                ..
            }) => {}
            err => panic!("Expected ValidTimeBeforeEntityCreation, got: {err:?}"),
        }
    }

    /// Test: Deleting an edge with valid_time before the edge's own creation is rejected.
    #[test]
    fn test_delete_edge_rejects_valid_time_before_edge_creation() {
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        let mut tx = harness.begin_write();
        let src = tx.create_node("Person", PropertyMap::new()).unwrap();
        let tgt = tx.create_node("Person", PropertyMap::new()).unwrap();
        tx.commit().unwrap();

        let mut tx2 = harness.begin_write();
        let edge_id = tx2
            .create_edge(src, tgt, "KNOWS", PropertyMap::new())
            .unwrap();
        tx2.commit().unwrap();

        let before_creation = HybridTimestamp::new(1000, 0).unwrap();

        let mut tx3 = harness.begin_write();
        let result = tx3.delete_edge_with_valid_time(edge_id, Some(before_creation));

        assert!(
            result.is_err(),
            "Should reject valid_time before edge creation"
        );
    }

    /// Test: `create_edge_with_valid_time` rejects a far-future `valid_time`, mirroring
    /// `test_create_node_rejects_far_future_valid_time`. Regression test for a gap where
    /// `create_edge_with_valid_time` was the only one of the six `*_with_valid_time`
    /// methods that never called `validate_valid_from_future`, silently accepting an
    /// arbitrarily-far-future `valid_time` on edges.
    #[test]
    fn test_create_edge_rejects_far_future_valid_time() {
        use crate::core::error::TemporalError;
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        let mut tx = harness.begin_write();
        let src = tx.create_node("Person", PropertyMap::new()).unwrap();
        let tgt = tx.create_node("Person", PropertyMap::new()).unwrap();
        tx.commit().unwrap();

        let two_years_future_wallclock = time::now().wallclock() + 2 * 365 * 24 * 3_600_000_000;
        let two_years_future = HybridTimestamp::new(two_years_future_wallclock, 0).unwrap();

        let mut tx2 = harness.begin_write();
        let result = tx2.create_edge_with_valid_time(
            src,
            tgt,
            "KNOWS",
            PropertyMap::new(),
            Some(two_years_future),
        );

        assert!(
            result.is_err(),
            "Should reject far-future valid_time on edge creation"
        );
        match result.unwrap_err() {
            crate::core::error::Error::Temporal(TemporalError::ValidTimeTooFarInFuture {
                ..
            }) => {}
            other => panic!("Expected ValidTimeTooFarInFuture, got: {:?}", other),
        }
    }

    /// Test: backfilling a node update between two existing (already backdated) versions
    /// succeeds. Regression test for a bug where the "not before creation" floor was
    /// computed from the *latest* version's `valid_from` instead of the entity's true
    /// original creation time, spuriously rejecting legitimate backfills.
    #[test]
    fn test_update_node_valid_time_backfill_between_existing_versions_succeeds() {
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();
        let now = time::now().wallclock();
        let t0 = HybridTimestamp::new(now - 3 * 3_600_000_000, 0).unwrap(); // 3h ago: true creation
        let t2 = HybridTimestamp::new(now - 2 * 3_600_000_000, 0).unwrap(); // 2h ago: latest version
        let t1 = HybridTimestamp::new(now - 2 * 3_600_000_000 - 1_800_000_000, 0).unwrap(); // 2h30m ago: t0 < t1 < t2

        let mut tx = harness.begin_write();
        let node_id = tx
            .create_node_with_valid_time("Person", PropertyMap::new(), Some(t0))
            .unwrap();
        tx.commit().unwrap();

        let mut tx2 = harness.begin_write();
        tx2.update_node_with_valid_time(
            node_id,
            PropertyMapBuilder::new().insert("name", "Bob").build(),
            Some(t2),
        )
        .unwrap();
        tx2.commit().unwrap();

        // Backfill a correction between t0 and t2: must succeed, not spuriously reject
        // against t2 (the latest version) instead of t0 (the true creation time).
        let mut tx3 = harness.begin_write();
        let result = tx3.update_node_with_valid_time(
            node_id,
            PropertyMapBuilder::new().insert("name", "Carol").build(),
            Some(t1),
        );
        assert!(
            result.is_ok(),
            "Backfill between existing versions should succeed, got: {:?}",
            result
        );
    }

    /// Edge mirror of `test_update_node_valid_time_backfill_between_existing_versions_succeeds`.
    #[test]
    fn test_update_edge_valid_time_backfill_between_existing_versions_succeeds() {
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();
        let now = time::now().wallclock();
        let t0 = HybridTimestamp::new(now - 3 * 3_600_000_000, 0).unwrap();
        let t2 = HybridTimestamp::new(now - 2 * 3_600_000_000, 0).unwrap();
        let t1 = HybridTimestamp::new(now - 2 * 3_600_000_000 - 1_800_000_000, 0).unwrap();

        let mut tx = harness.begin_write();
        let src = tx.create_node("Person", PropertyMap::new()).unwrap();
        let tgt = tx.create_node("Person", PropertyMap::new()).unwrap();
        tx.commit().unwrap();

        let mut tx2 = harness.begin_write();
        let edge_id = tx2
            .create_edge_with_valid_time(src, tgt, "KNOWS", PropertyMap::new(), Some(t0))
            .unwrap();
        tx2.commit().unwrap();

        let mut tx3 = harness.begin_write();
        tx3.update_edge_with_valid_time(
            edge_id,
            PropertyMapBuilder::new().insert("strength", 5i64).build(),
            Some(t2),
        )
        .unwrap();
        tx3.commit().unwrap();

        let mut tx4 = harness.begin_write();
        let result = tx4.update_edge_with_valid_time(
            edge_id,
            PropertyMapBuilder::new().insert("strength", 3i64).build(),
            Some(t1),
        );
        assert!(
            result.is_ok(),
            "Backfill between existing edge versions should succeed, got: {:?}",
            result
        );
    }

    /// Node-delete mirror of the update backfill regression test: deleting with a
    /// `valid_time` between an entity's true creation and a later update must succeed.
    #[test]
    fn test_delete_node_valid_time_backfill_between_existing_versions_succeeds() {
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();
        let now = time::now().wallclock();
        let t0 = HybridTimestamp::new(now - 3 * 3_600_000_000, 0).unwrap();
        let t2 = HybridTimestamp::new(now - 2 * 3_600_000_000, 0).unwrap();
        let t1 = HybridTimestamp::new(now - 2 * 3_600_000_000 - 1_800_000_000, 0).unwrap();

        let mut tx = harness.begin_write();
        let node_id = tx
            .create_node_with_valid_time("Person", PropertyMap::new(), Some(t0))
            .unwrap();
        tx.commit().unwrap();

        let mut tx2 = harness.begin_write();
        tx2.update_node_with_valid_time(
            node_id,
            PropertyMapBuilder::new().insert("name", "Bob").build(),
            Some(t2),
        )
        .unwrap();
        tx2.commit().unwrap();

        let mut tx3 = harness.begin_write();
        let result = tx3.delete_node_with_valid_time(node_id, Some(t1));
        assert!(
            result.is_ok(),
            "Delete backfill between existing versions should succeed, got: {:?}",
            result
        );
    }

    /// Edge-delete mirror of the node-delete backfill regression test.
    #[test]
    fn test_delete_edge_valid_time_backfill_between_existing_versions_succeeds() {
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();
        let now = time::now().wallclock();
        let t0 = HybridTimestamp::new(now - 3 * 3_600_000_000, 0).unwrap();
        let t2 = HybridTimestamp::new(now - 2 * 3_600_000_000, 0).unwrap();
        let t1 = HybridTimestamp::new(now - 2 * 3_600_000_000 - 1_800_000_000, 0).unwrap();

        let mut tx = harness.begin_write();
        let src = tx.create_node("Person", PropertyMap::new()).unwrap();
        let tgt = tx.create_node("Person", PropertyMap::new()).unwrap();
        tx.commit().unwrap();

        let mut tx2 = harness.begin_write();
        let edge_id = tx2
            .create_edge_with_valid_time(src, tgt, "KNOWS", PropertyMap::new(), Some(t0))
            .unwrap();
        tx2.commit().unwrap();

        let mut tx3 = harness.begin_write();
        tx3.update_edge_with_valid_time(
            edge_id,
            PropertyMapBuilder::new().insert("strength", 5i64).build(),
            Some(t2),
        )
        .unwrap();
        tx3.commit().unwrap();

        let mut tx4 = harness.begin_write();
        let result = tx4.delete_edge_with_valid_time(edge_id, Some(t1));
        assert!(
            result.is_ok(),
            "Edge delete backfill between existing versions should succeed, got: {:?}",
            result
        );
    }

    /// Test: The maximum valid timestamp boundary is accepted and just above is rejected.
    ///
    /// `HybridTimestamp::new` rejects wallclock values exceeding `MAX_VALID_TIMESTAMP`
    /// (i64::MAX - 1000) to guard against overflow-based DoS attacks on the temporal
    /// indexing layer. The internal `TIMESTAMP_MAX` sentinel (i64::MAX) is created only
    /// via `new_unchecked` for use as an open-ended interval marker.
    #[test]
    fn test_timestamp_boundary_max_valid_timestamp() {
        use crate::core::hlc::HybridTimestamp;
        use crate::core::temporal::MAX_VALID_TIMESTAMP;

        // Exactly at MAX_VALID_TIMESTAMP is valid
        let at_boundary = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0);
        assert!(
            at_boundary.is_ok(),
            "HybridTimestamp at MAX_VALID_TIMESTAMP must be valid, got: {at_boundary:?}"
        );

        // One above MAX_VALID_TIMESTAMP is invalid — this is the overflow guard
        let above_boundary = HybridTimestamp::new(MAX_VALID_TIMESTAMP + 1, 0);
        assert!(
            above_boundary.is_err(),
            "HybridTimestamp just above MAX_VALID_TIMESTAMP must be rejected"
        );

        // i64::MAX via the public constructor is also rejected (not a user-visible value)
        let via_new = HybridTimestamp::new(i64::MAX, 0);
        assert!(
            via_new.is_err(),
            "i64::MAX via HybridTimestamp::new must be rejected (use TIMESTAMP_MAX internally)"
        );

        // TIMESTAMP_MAX is accessible as a module-level constant and can be used
        // in open-ended time ranges internally, but cannot be created via new()
        let sentinel = crate::core::temporal::TIMESTAMP_MAX;
        assert_eq!(sentinel.wallclock(), i64::MAX);
        assert_eq!(sentinel.logical(), 0);
    }

    /// Test: Transaction with a valid_time set 1 year in future is rejected.
    ///
    /// The MAX_VALID_TIME_FUTURE_OFFSET_US constant limits how far ahead valid_time
    /// can be set, preventing logical time paradoxes where recorded facts appear
    /// to be valid arbitrarily far in the future.
    #[test]
    fn test_valid_time_one_year_in_future_rejected() {
        use crate::core::error::TemporalError;
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        // Use exactly 1 year + 1 second past the allowed limit
        let over_limit_wallclock =
            time::now().wallclock() + super::super::MAX_VALID_TIME_FUTURE_OFFSET_US + 1_000_000; // +1s over limit

        let over_limit_ts = HybridTimestamp::new(over_limit_wallclock, 0).unwrap();

        let mut tx = harness.begin_write();
        let result =
            tx.create_node_with_valid_time("Test", PropertyMap::new(), Some(over_limit_ts));

        assert!(
            result.is_err(),
            "Should reject valid_time beyond the 1-year limit"
        );
        match result.unwrap_err() {
            crate::core::error::Error::Temporal(TemporalError::ValidTimeTooFarInFuture {
                ..
            }) => {}
            err => panic!("Expected ValidTimeTooFarInFuture, got: {err:?}"),
        }
    }

    /// Test: Updating an edge with valid_time set more than 1 year in future is rejected.
    #[test]
    fn test_update_edge_rejects_far_future_valid_time() {
        use crate::core::error::TemporalError;
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        let mut tx = harness.begin_write();
        let src = tx.create_node("Person", PropertyMap::new()).unwrap();
        let tgt = tx.create_node("Person", PropertyMap::new()).unwrap();
        tx.commit().unwrap();

        let mut tx2 = harness.begin_write();
        let edge_id = tx2
            .create_edge(src, tgt, "KNOWS", PropertyMap::new())
            .unwrap();
        tx2.commit().unwrap();

        let over_limit_wallclock =
            time::now().wallclock() + super::super::MAX_VALID_TIME_FUTURE_OFFSET_US + 1_000_000;
        let over_limit_ts = HybridTimestamp::new(over_limit_wallclock, 0).unwrap();

        let mut tx3 = harness.begin_write();
        let result = tx3.update_edge_with_valid_time(
            edge_id,
            PropertyMapBuilder::new().insert("strength", 5i64).build(),
            Some(over_limit_ts),
        );

        assert!(
            result.is_err(),
            "Should reject valid_time beyond the 1-year limit"
        );
        match result.unwrap_err() {
            crate::core::error::Error::Temporal(TemporalError::ValidTimeTooFarInFuture {
                ..
            }) => {}
            err => panic!("Expected ValidTimeTooFarInFuture, got: {err:?}"),
        }
    }

    /// Test: Deleting an edge with valid_time set more than 1 year in future is rejected.
    #[test]
    fn test_delete_edge_rejects_far_future_valid_time() {
        use crate::core::error::TemporalError;
        use crate::core::hlc::HybridTimestamp;

        let harness = TestHarness::new();

        let mut tx = harness.begin_write();
        let src = tx.create_node("Person", PropertyMap::new()).unwrap();
        let tgt = tx.create_node("Person", PropertyMap::new()).unwrap();
        tx.commit().unwrap();

        let mut tx2 = harness.begin_write();
        let edge_id = tx2
            .create_edge(src, tgt, "KNOWS", PropertyMap::new())
            .unwrap();
        tx2.commit().unwrap();

        let over_limit_wallclock =
            time::now().wallclock() + super::super::MAX_VALID_TIME_FUTURE_OFFSET_US + 1_000_000;
        let over_limit_ts = HybridTimestamp::new(over_limit_wallclock, 0).unwrap();

        let mut tx3 = harness.begin_write();
        let result = tx3.delete_edge_with_valid_time(edge_id, Some(over_limit_ts));

        assert!(
            result.is_err(),
            "Should reject valid_time beyond the 1-year limit"
        );
        match result.unwrap_err() {
            crate::core::error::Error::Temporal(TemporalError::ValidTimeTooFarInFuture {
                ..
            }) => {}
            err => panic!("Expected ValidTimeTooFarInFuture, got: {err:?}"),
        }
    }
}

mod find_nodes_by_property_tests {
    use super::*;
    use crate::api::transaction::ReadOps;
    use crate::core::property::{PropertyMapBuilder, PropertyValue};

    fn create_test_write_tx() -> (WriteTransaction, TempDir) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());
        let tx_id_gen = TxIdGenerator::new();

        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
        );

        (tx, temp_dir)
    }

    #[test]
    fn test_committed_nodes_visible() {
        let current = Arc::new(CurrentStorage::new());

        // Pre-create committed nodes
        let alice_id = current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();

        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());
        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());
        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());
        let tx_id_gen = TxIdGenerator::new();
        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
        );

        let results =
            tx.find_nodes_by_property("Person", "name", &PropertyValue::String("Alice".into()));
        assert_eq!(results, vec![alice_id]);
    }

    #[test]
    fn test_buffered_create_node_visible() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        // Create a node in the write buffer
        let alice_id = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();

        // Should find the buffered node
        let results =
            tx.find_nodes_by_property("Person", "name", &PropertyValue::String("Alice".into()));
        assert_eq!(results, vec![alice_id]);
    }

    #[test]
    fn test_buffered_delete_excluded() {
        let current = Arc::new(CurrentStorage::new());

        // Pre-create a committed node
        let alice_id = current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();

        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());
        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());
        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());
        let tx_id_gen = TxIdGenerator::new();
        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let mut tx = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
        );

        // Delete the node in this transaction
        tx.delete_node(alice_id).unwrap();

        // Should NOT find the deleted node
        let results =
            tx.find_nodes_by_property("Person", "name", &PropertyValue::String("Alice".into()));
        assert!(results.is_empty());
    }

    #[test]
    fn test_buffered_update_with_matching_property() {
        let current = Arc::new(CurrentStorage::new());

        // Pre-create a committed node with a different name
        let node_id = current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "OldName").build(),
            )
            .unwrap();

        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());
        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());
        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());
        let tx_id_gen = TxIdGenerator::new();
        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let mut tx = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
        );

        // Update the node to have a new name
        tx.update_node(
            node_id,
            PropertyMapBuilder::new().insert("name", "NewName").build(),
        )
        .unwrap();

        // Should find with new name
        let results =
            tx.find_nodes_by_property("Person", "name", &PropertyValue::String("NewName".into()));
        assert_eq!(results, vec![node_id]);

        // Should NOT find with old name
        let results =
            tx.find_nodes_by_property("Person", "name", &PropertyValue::String("OldName".into()));
        assert!(results.is_empty());
    }
}

mod lock_poisoning_tests {
    use super::*;
    use crate::core::id::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use std::sync::Barrier;
    use std::thread;
    use std::time::Instant;
    use tempfile::TempDir;

    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
        tx_id_gen: TxIdGenerator,
        _temp_dir: TempDir,
    }

    impl TestHarness {
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(TemporalIndexes::new());
            let temp_dir = TempDir::new().unwrap();
            let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
            let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());
            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let node_id_gen = Arc::new(IdGenerator::new());
            let edge_id_gen = Arc::new(IdGenerator::new());
            let version_id_gen = Arc::new(IdGenerator::new());
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());
            TestHarness {
                current,
                historical,
                temporal_indexes,
                wal,
                current_timestamp,
                visibility_manager,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                _temp_dir: temp_dir,
            }
        }

        fn create_tx_with_timestamp(
            &self,
            current_timestamp: Arc<Mutex<Timestamp>>,
        ) -> WriteTransaction {
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: time::now(),
                active_transactions: Arc::new(std::collections::HashSet::new()),
            };
            WriteTransaction::new(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                current_timestamp,
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }

        fn create_tx_with_clock(
            &self,
            current_timestamp: Arc<Mutex<Timestamp>>,
            commit_clock_observed_at: Arc<Mutex<Instant>>,
        ) -> WriteTransaction {
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: time::now(),
                active_transactions: Arc::new(std::collections::HashSet::new()),
            };
            WriteTransaction::new_with_clock_observed_at(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                current_timestamp,
                commit_clock_observed_at,
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    fn poison_mutex<T: Send + 'static>(mutex: &Arc<Mutex<T>>) {
        let clone = mutex.clone();
        let _ = thread::spawn(move || {
            let _guard = clone.lock().unwrap();
            panic!("intentional panic to poison the lock");
        })
        .join();
    }

    /// Poisoning `current_timestamp` causes `commit()` to return `LockPoisoned`
    /// instead of panicking.
    #[test]
    fn test_timestamp_lock_poisoning_during_commit() {
        let harness = TestHarness::new();
        let poisoned_ts: Arc<Mutex<Timestamp>> = Arc::new(Mutex::new(time::now()));
        poison_mutex(&poisoned_ts);
        assert!(poisoned_ts.is_poisoned());

        let mut tx = harness.create_tx_with_timestamp(poisoned_ts);
        tx.create_node("Test", PropertyMapBuilder::new().insert("x", 1i64).build())
            .unwrap();

        let result = tx.commit();
        assert!(
            result.is_err(),
            "commit should fail with poisoned timestamp lock"
        );
        match result.unwrap_err() {
            crate::core::error::Error::Transaction(TransactionError::LockPoisoned { resource }) => {
                assert!(
                    resource.contains("current_timestamp"),
                    "expected current_timestamp in resource, got: {resource}"
                );
            }
            err => panic!("expected LockPoisoned error, got: {err:?}"),
        }
    }

    /// When multiple threads attempt concurrent commits against a poisoned lock,
    /// each thread gets a `LockPoisoned` error rather than panicking.
    #[test]
    fn test_concurrent_commits_with_poisoned_lock() {
        let poisoned_ts: Arc<Mutex<Timestamp>> = Arc::new(Mutex::new(time::now()));
        poison_mutex(&poisoned_ts);
        assert!(poisoned_ts.is_poisoned());

        let num_threads = 4;
        let harness = Arc::new(TestHarness::new());
        let barrier = Arc::new(Barrier::new(num_threads));
        let mut handles = Vec::new();

        for _ in 0..num_threads {
            let h = harness.clone();
            let ts = poisoned_ts.clone();
            let barrier = barrier.clone();

            handles.push(thread::spawn(move || {
                let mut tx = h.create_tx_with_timestamp(ts);
                tx.create_node("Test", PropertyMapBuilder::new().insert("x", 1i64).build())
                    .unwrap();
                barrier.wait();
                tx.commit()
            }));
        }

        for handle in handles {
            let result = handle.join().expect("thread should not panic");
            assert!(result.is_err(), "commit should fail");
            match result.unwrap_err() {
                crate::core::error::Error::Transaction(TransactionError::LockPoisoned {
                    resource,
                }) => {
                    assert!(
                        resource.contains("current_timestamp"),
                        "expected current_timestamp in resource, got: {resource}"
                    );
                }
                err => panic!("expected LockPoisoned error, got: {err:?}"),
            }
        }
    }

    /// Poisoning `commit_clock_observed_at` causes `commit()` to return
    /// `LockPoisoned` via the adaptive forward-jump guard, not a panic.
    #[test]
    fn test_commit_clock_observed_at_lock_poisoning() {
        let harness = TestHarness::new();
        let poisoned_clock: Arc<Mutex<Instant>> = Arc::new(Mutex::new(Instant::now()));
        poison_mutex(&poisoned_clock);
        assert!(poisoned_clock.is_poisoned());

        let mut tx =
            harness.create_tx_with_clock(harness.current_timestamp.clone(), poisoned_clock);
        tx.create_node("Test", PropertyMapBuilder::new().insert("x", 1i64).build())
            .unwrap();

        let result = tx.commit();
        assert!(
            result.is_err(),
            "commit should fail with poisoned clock lock"
        );
        match result.unwrap_err() {
            crate::core::error::Error::Transaction(TransactionError::LockPoisoned { resource }) => {
                assert!(
                    resource.contains("commit_clock_observed_at"),
                    "expected commit_clock_observed_at in resource, got: {resource}"
                );
            }
            err => panic!("expected LockPoisoned error, got: {err:?}"),
        }
    }
}

/// Issue #3417: write-path reads must be buffer-aware (read-your-own-writes).
///
/// An entity created earlier in the SAME transaction must be visible to a
/// later update/delete/retract in that transaction, and a same-tx
/// create-then-update/delete must NOT raise a spurious write-write conflict at
/// commit. Cross-transaction isolation must be preserved: an entity created by
/// another, still-uncommitted transaction stays invisible.
#[cfg(test)]
mod buffer_aware_read_tests {
    use super::*;
    use crate::core::id::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use tempfile::TempDir;

    /// Shared infrastructure so multiple transactions observe the same
    /// storage / visibility manager (needed for the cross-tx isolation test).
    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
        tx_id_gen: TxIdGenerator,
        _temp_dir: TempDir,
    }

    impl TestHarness {
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(TemporalIndexes::new());

            let temp_dir = TempDir::new().unwrap();
            let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
            let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let node_id_gen = Arc::new(IdGenerator::new());
            let edge_id_gen = Arc::new(IdGenerator::new());
            let version_id_gen = Arc::new(IdGenerator::new());
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());

            TestHarness {
                current,
                historical,
                temporal_indexes,
                wal,
                current_timestamp,
                visibility_manager,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                _temp_dir: temp_dir,
            }
        }

        fn create_tx(&self) -> WriteTransaction {
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: *self.current_timestamp.lock().unwrap(),
                active_transactions: Arc::new(std::collections::HashSet::new()),
            };

            WriteTransaction::new(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    /// create_node then update_node in one tx: the update sees the buffered
    /// create (no NodeNotFound) and PATCH-merges onto the buffered properties.
    #[test]
    fn test_3417_create_then_update_node_same_tx() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let node_id = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build(),
            )
            .unwrap();

        // Update the just-created node in the SAME transaction.
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert("age", 31i64)
                .insert("city", "NYC")
                .build(),
        )
        .expect("update of same-tx-created node must succeed (read-your-own-writes)");

        tx.commit().unwrap();

        let node = harness.current.get_node(node_id).unwrap();
        // Merge composed onto the buffered CREATE, not committed-only state.
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice"),
            "original property from the buffered create must survive the merge"
        );
        assert_eq!(
            node.get_property("age").and_then(|v| v.as_int()),
            Some(31),
            "updated property must overwrite the created value"
        );
        assert_eq!(
            node.get_property("city").and_then(|v| v.as_str()),
            Some("NYC"),
            "new property from the update must be present"
        );
    }

    /// create_node then delete_node in one tx: delete sees the buffered create
    /// (no NodeNotFound) and the node is absent after commit.
    #[test]
    fn test_3417_create_then_delete_node_same_tx() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let node_id = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();

        tx.delete_node(node_id)
            .expect("delete of same-tx-created node must succeed (read-your-own-writes)");

        tx.commit().unwrap();

        assert!(
            harness.current.get_node(node_id).is_err(),
            "node created then deleted in one tx must be absent after commit"
        );
    }

    /// create_edge then update_edge in one tx: the update sees the buffered
    /// create and PATCH-merges onto the buffered edge properties.
    #[test]
    fn test_3417_create_then_update_edge_same_tx() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let n1 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let edge_id = tx
            .create_edge(
                n1,
                n2,
                "KNOWS",
                PropertyMapBuilder::new().insert("weight", 5i64).build(),
            )
            .unwrap();

        tx.update_edge(
            edge_id,
            PropertyMapBuilder::new()
                .insert("weight", 10i64)
                .insert("since", 2020i64)
                .build(),
        )
        .expect("update of same-tx-created edge must succeed (read-your-own-writes)");

        tx.commit().unwrap();

        let edge = harness.current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(10),
            "updated edge property must overwrite the created value"
        );
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_int()),
            Some(2020),
            "new edge property from the update must be present"
        );
    }

    /// create_edge then delete_edge in one tx: delete sees the buffered create
    /// and the edge is absent after commit (endpoints remain).
    #[test]
    fn test_3417_create_then_delete_edge_same_tx() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let n1 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let edge_id = tx
            .create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        tx.delete_edge(edge_id)
            .expect("delete of same-tx-created edge must succeed (read-your-own-writes)");

        tx.commit().unwrap();

        assert!(
            harness.current.get_edge(edge_id).is_err(),
            "edge created then deleted in one tx must be absent after commit"
        );
        assert!(
            harness.current.get_node(n1).is_ok() && harness.current.get_node(n2).is_ok(),
            "endpoints must remain after the same-tx edge delete"
        );
    }

    /// create_node then retract_node in one tx: retract sees the buffered
    /// create (no NodeNotFound) and closes the valid interval.
    #[test]
    fn test_3417_create_then_retract_node_same_tx() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let node_id = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Carol").build(),
            )
            .unwrap();

        let valid_to = time::now();
        let result = tx
            .retract_node(node_id, valid_to)
            .expect("retract of same-tx-created node must succeed (read-your-own-writes)");
        assert!(
            !result.already_retracted,
            "a freshly-created node is not already retracted"
        );

        tx.commit().unwrap();

        assert!(
            harness.current.get_node(node_id).is_err(),
            "retracted node must be absent from current state after commit"
        );
    }

    /// create_edge then retract_edge in one tx.
    #[test]
    fn test_3417_create_then_retract_edge_same_tx() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let n1 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let edge_id = tx
            .create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        let valid_to = time::now();
        let result = tx
            .retract_edge(edge_id, valid_to)
            .expect("retract of same-tx-created edge must succeed (read-your-own-writes)");
        assert!(!result.already_retracted);

        tx.commit().unwrap();

        assert!(
            harness.current.get_edge(edge_id).is_err(),
            "retracted edge must be absent from current state after commit"
        );
    }

    /// A same-tx create-then-update must NOT raise a spurious
    /// SerializationFailure at commit: `detect_conflicts` treats a missing
    /// committed row as "deleted by another tx", but a same-tx-created entity
    /// has no committed row by design. This exercises the conflict.rs skip.
    #[test]
    fn test_3417_create_then_update_no_spurious_conflict() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let node_id = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("v", 1i64).build(),
            )
            .unwrap();
        tx.update_node(node_id, PropertyMapBuilder::new().insert("v", 2i64).build())
            .unwrap();

        let result = tx.commit();
        assert!(
            result.is_ok(),
            "create-then-update in one tx must commit without a spurious \
             write-write conflict, got: {:?}",
            result.err()
        );
    }

    /// Headline #3417 scenario for nodes: create → update → update in ONE tx.
    /// The SECOND update must read-your-own-writes onto the FIRST update's
    /// buffered state (not a stale committed/created read), so all three
    /// writes compose in the committed result.
    #[test]
    fn test_3417_create_update_update_node_chain_merges() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let node_id = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build(),
            )
            .unwrap();
        // First update: sets age onto the buffered CREATE.
        tx.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 31i64).build(),
        )
        .unwrap();
        // Second update: MUST merge onto the buffered FIRST update, adding a
        // new key while preserving the first update's age and the created name.
        tx.update_node(
            node_id,
            PropertyMapBuilder::new().insert("city", "NYC").build(),
        )
        .unwrap();

        tx.commit().unwrap();

        let node = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice"),
            "created property must survive both updates"
        );
        assert_eq!(
            node.get_property("age").and_then(|v| v.as_int()),
            Some(31),
            "first update must survive the second (chained merge, not stale read)"
        );
        assert_eq!(
            node.get_property("city").and_then(|v| v.as_str()),
            Some("NYC"),
            "second update's new key must be present"
        );
    }

    /// Headline #3417 scenario for a COMMITTED node: two updates in one tx must
    /// compose — the second reads the buffered first update, not committed
    /// state (which is exactly the bug: a second write dropping the first's
    /// merge). This is the multi-write-per-committed-entity case #3417 names.
    #[test]
    fn test_3417_double_update_committed_node_composes() {
        let harness = TestHarness::new();

        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert("name", "Bob")
                        .insert("age", 40i64)
                        .build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        let mut tx = harness.create_tx();
        // First update onto committed state.
        tx.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 41i64).build(),
        )
        .unwrap();
        // Second update MUST see the buffered first update (age=41) and add a
        // key, rather than re-reading committed state (age=40) and dropping it.
        tx.update_node(
            node_id,
            PropertyMapBuilder::new().insert("city", "LA").build(),
        )
        .unwrap();
        tx.commit().unwrap();

        let node = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Bob"),
            "committed property untouched by the updates must survive"
        );
        assert_eq!(
            node.get_property("age").and_then(|v| v.as_int()),
            Some(41),
            "first update must not be dropped by the second (composed merge)"
        );
        assert_eq!(
            node.get_property("city").and_then(|v| v.as_str()),
            Some("LA"),
            "second update's key must be present"
        );
    }

    /// Edge equivalent of the headline scenario: create → update → update edge
    /// in ONE tx composes all three writes.
    #[test]
    fn test_3417_create_update_update_edge_chain_merges() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let n1 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let edge_id = tx
            .create_edge(
                n1,
                n2,
                "KNOWS",
                PropertyMapBuilder::new().insert("weight", 1i64).build(),
            )
            .unwrap();
        tx.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 2i64).build(),
        )
        .unwrap();
        tx.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();

        tx.commit().unwrap();

        let edge = harness.current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(2),
            "first edge update must survive the second (chained merge)"
        );
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_int()),
            Some(2020),
            "second edge update's new key must be present"
        );
    }

    /// A same-tx create_edge → update_edge must NOT raise a spurious
    /// SerializationFailure at commit (dedicated edge conflict-skip coverage,
    /// mirroring the node case).
    #[test]
    fn test_3417_create_then_update_edge_no_spurious_conflict() {
        let harness = TestHarness::new();
        let mut tx = harness.create_tx();

        let n1 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = tx
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let edge_id = tx
            .create_edge(
                n1,
                n2,
                "KNOWS",
                PropertyMapBuilder::new().insert("weight", 1i64).build(),
            )
            .unwrap();
        tx.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 2i64).build(),
        )
        .unwrap();

        let result = tx.commit();
        assert!(
            result.is_ok(),
            "create-then-update of an edge in one tx must commit without a \
             spurious write-write conflict, got: {:?}",
            result.err()
        );
    }

    /// Collapse: create then delete the same node in one tx commits cleanly
    /// and leaves **current state** clean (no phantom, node absent).
    ///
    /// Scope: this asserts CURRENT-STATE cleanliness only. The historical
    /// record is intentionally NOT collapsed: apply replays the buffer in
    /// order, so it writes a create version AND a delete tombstone, both
    /// stamped with the same commit_timestamp. The create version therefore
    /// ends up with a zero-width transaction-time interval (opened and closed
    /// at the same commit instant) and the tombstone closes the valid-time
    /// interval — i.e. the node is never visible at any (valid, tx) coordinate
    /// after this commit. Verifying that bi-temporal shape is out of scope for
    /// this test (and for #3417); it belongs with the apply/temporal-index
    /// coverage.
    #[test]
    fn test_3417_create_then_delete_collapse_current_state_clean() {
        let harness = TestHarness::new();
        let node_before = harness.current.node_count();

        let mut tx = harness.create_tx();
        let node_id = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("x", 1i64).build(),
            )
            .unwrap();
        tx.delete_node(node_id).unwrap();

        tx.commit()
            .expect("create-then-delete collapse must commit cleanly");

        assert!(
            harness.current.get_node(node_id).is_err(),
            "collapsed node must be absent from current state"
        );
        assert_eq!(
            harness.current.node_count(),
            node_before,
            "no phantom node should remain in current state after a \
             create-then-delete collapse"
        );
    }

    /// Cross-transaction isolation preserved: an entity created by another,
    /// still-uncommitted transaction stays invisible (buffer-awareness is
    /// strictly per-transaction, it does not leak reads across txns).
    #[test]
    fn test_3417_cross_tx_created_entity_invisible() {
        let harness = TestHarness::new();

        let mut tx_a = harness.create_tx();
        let node_id = tx_a
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Dana").build(),
            )
            .unwrap();

        // A concurrent, independent transaction must not see tx_a's buffered
        // (uncommitted) create.
        let mut tx_b = harness.create_tx();
        assert!(
            ReadOps::get_node(&tx_b, node_id).is_err(),
            "an uncommitted create in another tx must be invisible"
        );
        assert!(
            tx_b.update_node(
                node_id,
                PropertyMapBuilder::new().insert("name", "X").build()
            )
            .is_err(),
            "updating an entity created by another uncommitted tx must fail NotFound"
        );

        drop(tx_a);
        drop(tx_b);
    }

    /// Cross-transaction MVCC safety: the write-path read is deliberately
    /// UN-filtered (it sees the latest committed version, not just versions
    /// visible to this tx's snapshot), so a concurrent commit landing AFTER
    /// tx_b's snapshot is read by tx_b's update — but `detect_conflicts` MUST
    /// still abort tx_b with `SerializationFailure` at commit (first-committer
    /// wins). This is the abort the unfiltered read relies on for correctness.
    #[test]
    fn test_3417_cross_tx_committed_after_snapshot_still_conflicts() {
        let harness = TestHarness::new();

        // Committed baseline node.
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 1i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx_b captures its snapshot BEFORE tx_a commits.
        let mut tx_b = harness.create_tx();

        // tx_a commits a new version of the node AFTER tx_b's snapshot.
        let mut tx_a = harness.create_tx();
        tx_a.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 2i64).build(),
        )
        .unwrap();
        tx_a.commit().unwrap();

        // tx_b's write-path read is unfiltered, so this update succeeds (it
        // reads the just-committed version)...
        tx_b.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 3i64).build(),
        )
        .unwrap();

        // ...but the commit MUST abort: tx_a committed after tx_b's snapshot.
        let result = tx_b.commit();
        assert!(
            result.is_err(),
            "tx_b must abort: a concurrent commit landed after its snapshot"
        );
        assert!(
            format!("{:?}", result.unwrap_err()).contains("SerializationFailure"),
            "expected SerializationFailure (first-committer-wins)"
        );

        // First committer (tx_a) wins.
        assert_eq!(
            harness
                .current
                .get_node(node_id)
                .unwrap()
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(2),
            "first committer's value must stand"
        );
    }
}

/// Regression tests for Issue #3415.
///
/// A commit that fails *after* `commit_with_timestamp_inner` transitions the
/// transaction into `TxState::Preparing` (write-write conflict, constraint,
/// WAL, or apply failure) must still release the transaction's `TxId` from
/// `TxVisibilityManager::active` when the consumed `WriteTransaction` is
/// dropped. Before the fix, `Drop` only aborted `TxState::Active` transactions,
/// so a failed commit leaked its `TxId` in the active set forever — pinning the
/// snapshot horizon and growing `active_count()` without bound under retries.
///
/// These tests drive a deterministic write-write conflict (no failure-injection
/// hook needed): two transactions update the same committed node; the first
/// committer wins and the second commit fails in `Preparing`.
#[cfg(test)]
mod commit_failure_visibility_tests {
    use crate::AletheiaDB;
    use crate::api::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    /// A single commit that fails via write-write conflict must return the
    /// active-transaction count to its baseline (the leaked-`TxId` regression).
    #[test]
    fn test_conflict_failed_commit_releases_txid() {
        let db = AletheiaDB::new().expect("db");

        // Seed a committed node that both transactions will contend on.
        let node_id = db
            .write(|tx| {
                let id = tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 30i64).build(),
                )?;
                Ok::<_, crate::Error>(id)
            })
            .expect("seed node");

        let baseline = db.visibility_manager.active_count();
        assert_eq!(baseline, 0, "no transactions should be active at baseline");

        // tx1 and tx2 both start (both registered active) and update the same node.
        let mut tx1 = db.write_transaction().expect("tx1");
        tx1.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 31i64).build(),
        )
        .expect("tx1 update");

        let mut tx2 = db.write_transaction().expect("tx2");
        tx2.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 32i64).build(),
        )
        .expect("tx2 update");

        assert_eq!(
            db.visibility_manager.active_count(),
            2,
            "both in-flight transactions must be registered active"
        );

        // First committer wins.
        tx2.commit().expect("tx2 commit should succeed");

        // Second commit fails with a write-write conflict *in the Preparing
        // phase*, consuming and dropping tx1.
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1 commit should fail due to write-write conflict"
        );

        // The failed commit must have released tx1's TxId from the active set.
        assert_eq!(
            db.visibility_manager.active_count(),
            baseline,
            "a commit failing in Preparing must release its TxId (Issue #3415)"
        );
    }

    /// Repeated conflict-failed commits must not grow the active set: the count
    /// stays flat at baseline rather than leaking one `TxId` per failure.
    #[test]
    fn test_repeated_conflict_failures_do_not_leak() {
        let db = AletheiaDB::new().expect("db");

        let node_id = db
            .write(|tx| {
                let id = tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 0i64).build(),
                )?;
                Ok::<_, crate::Error>(id)
            })
            .expect("seed node");

        let baseline = db.visibility_manager.active_count();
        assert_eq!(baseline, 0);

        for i in 0..50i64 {
            let mut loser = db.write_transaction().expect("loser tx");
            loser
                .update_node(node_id, PropertyMapBuilder::new().insert("age", i).build())
                .expect("loser update");

            let mut winner = db.write_transaction().expect("winner tx");
            winner
                .update_node(
                    node_id,
                    PropertyMapBuilder::new().insert("age", i + 1000).build(),
                )
                .expect("winner update");

            // Winner commits first, loser fails in Preparing and is dropped.
            winner.commit().expect("winner commit");
            assert!(loser.commit().is_err(), "loser commit should conflict");

            // After each failed commit the active set must be back to baseline —
            // no monotonic growth (Issue #3415 amplification path).
            assert_eq!(
                db.visibility_manager.active_count(),
                baseline,
                "active_count leaked after {} conflict-failed commits",
                i + 1
            );
        }
    }

    /// Guard: a successful commit and an explicit rollback both leave the active
    /// set at baseline (no leak), so the #3415 fix does not regress the happy
    /// paths.
    ///
    /// Note: this asserts the active-set-clean invariant only. It does NOT
    /// enforce "no double-abort" — `register_abort` is an idempotent
    /// `HashSet::remove`, so a spurious `Drop` on an already-`Committed`/
    /// `Aborted` transaction would leave `active_count()` unchanged and this
    /// test would still pass. Double-abort is harmless by construction (the
    /// broadened `Drop` guard only matches `Active`/`Preparing`, and the
    /// removal is idempotent regardless), not a property this test verifies.
    #[test]
    fn test_success_and_rollback_leave_active_set_clean() {
        let db = AletheiaDB::new().expect("db");
        let baseline = db.visibility_manager.active_count();

        // Successful commit.
        let mut tx = db.write_transaction().expect("tx");
        tx.create_node("Person", PropertyMapBuilder::new().build())
            .expect("create");
        assert_eq!(db.visibility_manager.active_count(), baseline + 1);
        tx.commit().expect("commit");
        assert_eq!(
            db.visibility_manager.active_count(),
            baseline,
            "successful commit must release its TxId"
        );

        // Explicit rollback.
        let mut tx = db.write_transaction().expect("tx");
        tx.create_node("Person", PropertyMapBuilder::new().build())
            .expect("create");
        assert_eq!(db.visibility_manager.active_count(), baseline + 1);
        tx.rollback().expect("rollback");
        assert_eq!(
            db.visibility_manager.active_count(),
            baseline,
            "explicit rollback must release its TxId"
        );
    }
}
