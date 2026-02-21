#[cfg(test)]
mod warden_repro {
    use crate::api::transaction::write::WriteTransaction;
    use crate::api::transaction::{TransactionSnapshot, TxVisibilityManager, WriteOps};
    use crate::core::id::IdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::temporal::TemporalIndexes;
    use crate::storage::current::CurrentStorage;
    use crate::storage::historical::HistoricalStorage;
    use crate::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
    use parking_lot::RwLock;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn create_test_write_tx() -> (WriteTransaction, TempDir) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let current_timestamp = Arc::new(Mutex::new(crate::core::temporal::time::now()));
        let node_id_gen = Arc::new(IdGenerator::new());
        let edge_id_gen = Arc::new(IdGenerator::new());
        let version_id_gen = Arc::new(IdGenerator::new());
        let tx_id_gen = crate::api::transaction::TxIdGenerator::new();

        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: crate::core::temporal::time::now(),
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
    fn test_create_edge_with_deleted_node_same_tx() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        // Capture components for next tx
        let current = tx.current.clone();
        let historical = tx.historical.clone();
        let temporal_indexes = tx.temporal_indexes.clone();
        let wal = tx.wal.clone();
        let current_timestamp = tx.current_timestamp.clone();
        let visibility_manager = tx.visibility_manager.clone();
        let node_id_gen = tx.node_id_gen.clone();
        let edge_id_gen = tx.edge_id_gen.clone();
        let version_id_gen = tx.version_id_gen.clone();
        let tx_id_gen = crate::api::transaction::TxIdGenerator::new();

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = tx.create_node("Person", props.clone()).unwrap();
        let node2 = tx.create_node("Person", props.clone()).unwrap();
        tx.commit().unwrap();

        // Start new transaction
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: crate::core::temporal::time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let mut tx2 = WriteTransaction::new(
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

        // Delete node1
        tx2.delete_node(node1).unwrap();

        // Create edge from node1 (deleted) to node2
        let edge_props = PropertyMapBuilder::new().build();
        tx2.create_edge(node1, node2, "KNOWS", edge_props).unwrap();

        // Commit should fail
        let result = tx2.commit();
        assert!(
            result.is_err(),
            "Commit should fail when creating edge from deleted node"
        );

        // Check error message
        if let Err(e) = result {
            let err_str = format!("{}", e);
            assert!(
                err_str.contains("Edge source node") || err_str.contains("does not exist"),
                "Unexpected error: {}",
                err_str
            );
        }
    }
}
