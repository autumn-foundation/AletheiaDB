use loom::sync::RwLock;
use std::sync::Arc;

#[test]
fn test_coordinator_real_deadlock() {
    loom::model(|| {
        let active_transactions = Arc::new(RwLock::new(()));
        let connections = Arc::new(RwLock::new(()));

        let tx1 = active_transactions.clone();
        let conn1 = connections.clone();

        let t1 = loom::thread::spawn(move || {
            // prepare_distributed_transaction
            let _t = tx1.write().unwrap();
            drop(_t);

            let _c = conn1.read().unwrap();
            drop(_c);

            let _cw = conn1.write().unwrap(); // mark_shard_unavailable
            drop(_cw);

            let _t2 = tx1.write().unwrap();
            drop(_t2);
        });

        let conn2 = connections.clone();

        let t2 = loom::thread::spawn(move || {
            // health_check_all
            let _c = conn2.write().unwrap();
            drop(_c);
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
