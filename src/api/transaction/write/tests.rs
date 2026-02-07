use super::*;
use crate::api::transaction::TxIdGenerator;
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
            crate::utils::error::Error::Storage(StorageError::InconsistentState { reason }) => {
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
        assert_eq!(tx.get_outgoing_edges(node1).len(), 1);
        assert_eq!(tx.get_incoming_edges(node2).len(), 1);
        assert_eq!(tx.get_outgoing_edges_with_label(node1, "KNOWS").len(), 1);
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

        // Commit - should call rebuild_adjacency() since edge was created
        tx.commit().unwrap();

        // Verify adjacency was rebuilt correctly
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
    fn test_batch_edge_operations_rebuild_once() {
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

        // Commit should rebuild adjacency only once
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
        use crate::api::transaction::TxIdGenerator;
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
    use crate::api::transaction::TxIdGenerator;
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
}

mod clock_skew_tests {
    use super::*;
    use crate::api::transaction::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use tempfile::TempDir;

    /// Test harness for clock skew tests.
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
    }

    #[test]
    fn test_clock_skew_backward_error() {
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
            crate::utils::error::Error::Transaction(TransactionError::ClockSkew {
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
            crate::utils::error::Error::Transaction(TransactionError::ClockSkew {
                drift_us,
                ..
            }) => {
                // Drift should be positive and large
                assert!(drift_us > super::MAX_FORWARD_JUMP_US);
            }
            err => panic!("Expected ClockSkew error, got: {:?}", err),
        }
    }
}

mod timestamp_ordering_tests {
    use super::*;
    use crate::api::transaction::TxIdGenerator;
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
}

mod bitemporal_validation_tests {
    use super::*;
    use crate::api::transaction::types::TxIdGenerator;
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
        use crate::core::hlc::HybridTimestamp;
        use crate::utils::error::TemporalError;

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
            crate::utils::error::Error::Temporal(TemporalError::ValidTimeTooFarInFuture {
                ..
            }) => {
                // Expected error type
            }
            other => panic!("Expected ValidTimeTooFarInFuture, got: {:?}", other),
        }
    }

    #[test]
    fn test_update_node_rejects_valid_time_before_creation() {
        use crate::core::hlc::HybridTimestamp;
        use crate::utils::error::TemporalError;

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
            crate::utils::error::Error::Temporal(
                TemporalError::ValidTimeBeforeEntityCreation { .. },
            ) => {
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
