with open("src/storage/sharding/coordinator.rs", "r") as f:
    content = f.read()

# Replace prepare block
old_prepare = """        // Mark as prepared
        drop(connections); // Prevent deadlock with active_transactions.write()
        if let Err(e) = transaction.mark_prepared() {
            if let Ok(mut txns) = self.active_transactions.write() {
                txns.insert(tx_id, transaction);
            }
            return Err(e);
        }

        // Re-insert the transaction
        if let Ok(mut txns) = self.active_transactions.write() {
            txns.insert(tx_id, transaction);
        }

        Ok(())"""

new_prepare = """        // Mark as prepared
        drop(connections); // Prevent deadlock with active_transactions.write()
        if let Err(e) = transaction.mark_prepared() {
            self.reinsert_transaction(tx_id, transaction);
            return Err(e);
        }

        // Re-insert the transaction
        self.reinsert_transaction(tx_id, transaction);

        Ok(())"""

content = content.replace(old_prepare, new_prepare)

# Replace commit block
old_commit = """        // Mark transaction as committed
        transaction.mark_committed()?;

        // Record metrics
        if let Ok(metrics_map) = self.metrics.read() {
            for shard_id in transaction.participant_shards() {
                if let Some(metrics) = metrics_map.get(&shard_id) {
                    metrics.record_write(true); // Distributed write
                }
            }
        }

        Ok(())"""

new_commit = """        // Mark transaction as committed
        if let Err(e) = transaction.mark_committed() {
            drop(connections); // Prevent deadlock with active_transactions.write()
            self.reinsert_transaction(tx_id, transaction);
            return Err(e);
        }

        // Record metrics
        if let Ok(metrics_map) = self.metrics.read() {
            for shard_id in transaction.participant_shards() {
                if let Some(metrics) = metrics_map.get(&shard_id) {
                    metrics.record_write(true); // Distributed write
                }
            }
        }

        drop(connections); // Prevent deadlock with active_transactions.write()

        // Re-insert as completed so caller can clean up
        self.reinsert_transaction(tx_id, transaction);

        Ok(())"""

content = content.replace(old_commit, new_commit)

with open("src/storage/sharding/coordinator.rs", "w") as f:
    f.write(content)
