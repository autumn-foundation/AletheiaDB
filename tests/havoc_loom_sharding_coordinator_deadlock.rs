#![cfg(loom)]

use loom::sync::{Arc, RwLock};
use loom::thread;

struct ShardCoordinatorModel {
    connections: RwLock<()>,
}

impl ShardCoordinatorModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            connections: RwLock::new(()),
        })
    }

    fn prepare_distributed_transaction(&self) {
        let unavailable_shards;
        {
            let _c = self.connections.read().unwrap();
            // simulate ParticipantUnavailable -> collect
            unavailable_shards = true;
        }

        if unavailable_shards {
            self.mark_shard_unavailable();
        }
    }

    fn mark_shard_unavailable(&self) {
        // This will no longer deadlock because the read lock was dropped
        let _c = self.connections.write().unwrap();
    }
}

#[test]
fn test_shard_unavailable_self_deadlock() {
    loom::model(|| {
        let model = ShardCoordinatorModel::new();
        let m1 = model.clone();

        let t1 = thread::spawn(move || {
            m1.prepare_distributed_transaction();
        });

        t1.join().unwrap();
    });
}
