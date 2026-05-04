#![allow(unexpected_cfgs)]
#[cfg(loom)]
mod loom_test {
    use loom::sync::{Arc, RwLock};
    use loom::thread;

    struct Coordinator {
        connections: Arc<RwLock<()>>,
        commit_log: Arc<RwLock<()>>,
    }

    impl Coordinator {
        fn new() -> Self {
            Coordinator {
                connections: Arc::new(RwLock::new(())),
                commit_log: Arc::new(RwLock::new(())),
            }
        }

        fn commit_distributed_transaction(&self) {
            let connections = self.connections.read().unwrap();

            // Drop connections BEFORE acquiring commit_log to match the canonical ordering
            // (connections always released before commit_log write) and prevent the
            // connections.read + commit_log.write / commit_log.write + connections.write
            // cycle that arises in the full coordinator under write-preferring RwLock.
            drop(connections);

            let _log = self.commit_log.read().unwrap();
        }

        fn abort_distributed_transaction(&self) {
            {
                let _log = self.commit_log.write().unwrap();
            }

            let _connections = self.connections.read().unwrap();
        }

        fn add_shard(&self) {
            let _connections = self.connections.write().unwrap();
        }
    }

    #[test]
    fn test_commit_log_deadlock() {
        loom::model(|| {
            let coord = Arc::new(Coordinator::new());

            let c1 = coord.clone();
            let t1 = thread::spawn(move || {
                c1.commit_distributed_transaction();
            });

            let c2 = coord.clone();
            let t2 = thread::spawn(move || {
                c2.abort_distributed_transaction();
            });

            let c3 = coord.clone();
            let t3 = thread::spawn(move || {
                c3.add_shard();
            });

            t1.join().unwrap();
            t2.join().unwrap();
            t3.join().unwrap();
        });
    }
}
