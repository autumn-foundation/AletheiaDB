#![cfg(loom)]

use loom::sync::{Arc, RwLock};
use loom::thread;

struct ShardCoordinatorModel {
    connections: RwLock<()>,
    shard_states: RwLock<()>,
}

impl ShardCoordinatorModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            connections: RwLock::new(()),
            shard_states: RwLock::new(()),
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
        // This simulates the actual ShardCoordinator which locks shard_states
        // instead of connections, avoiding the self-deadlock.
        let _s = self.shard_states.write().unwrap();
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
