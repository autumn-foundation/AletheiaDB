#[cfg(test)]
mod havoc_tests {
    use crate::api::transaction::visibility::TxVisibilityManager;
    use crate::core::id::TxId;
    use crate::core::hlc::HybridTimestamp;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_tx_visibility_race() {
        let vm = Arc::new(TxVisibilityManager::new());
        let vm_clone = Arc::clone(&vm);

        // Start transaction
        let tx = TxId::new(1);
        vm.start_transaction(tx);

        // Thread 1: commit
        let t1 = thread::spawn(move || {
            let _ = vm_clone.commit_transaction(tx, HybridTimestamp::from_system_time());
        });

        // Thread 2: abort
        let vm_clone2 = Arc::clone(&vm);
        let t2 = thread::spawn(move || {
            let _ = vm_clone2.abort_transaction(tx);
        });

        t1.join().unwrap();
        t2.join().unwrap();
    }
}
