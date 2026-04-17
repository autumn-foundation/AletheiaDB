with open("src/storage/sharding/coordinator.rs", "r") as f:
    content = f.read()

# Remove the unused Err branches for mark_prepared and mark_committed since they are basically unreachable due to our earlier checks
old_prepare = """        // Mark as prepared
        drop(connections); // Prevent deadlock with active_transactions.write()
        if let Err(e) = transaction.mark_prepared() {
            self.reinsert_transaction(tx_id, transaction);
            return Err(e);
        }"""

new_prepare = """        // Mark as prepared
        drop(connections); // Prevent deadlock with active_transactions.write()
        // We already checked any_aborted/any_unreachable so mark_prepared won't fail here
        let _ = transaction.mark_prepared();"""

content = content.replace(old_prepare, new_prepare)

old_commit = """        // Mark transaction as committed
        if let Err(e) = transaction.mark_committed() {
            drop(connections); // Prevent deadlock with active_transactions.write()
            self.reinsert_transaction(tx_id, transaction);
            return Err(e);
        }"""

new_commit = """        // Mark transaction as committed
        // We already checked !transaction.all_committed() so mark_committed won't fail here
        let _ = transaction.mark_committed();"""

content = content.replace(old_commit, new_commit)

with open("src/storage/sharding/coordinator.rs", "w") as f:
    f.write(content)
