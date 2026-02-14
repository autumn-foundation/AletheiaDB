use loom::sync::{Mutex, RwLock};
use loom::thread;
use std::sync::Arc;

// Mock of HnswIndex internal structure
struct MockIndex {
    // inner (usearch index)
    inner: RwLock<()>,
    // id_mapping (DashMap) - we simulate one shard with a Mutex
    // In reality, DashMap has multiple shards, but if two threads hit the same shard, it behaves like a Mutex.
    id_mapping_shard: Mutex<()>,
}

impl MockIndex {
    fn new() -> Self {
        MockIndex {
            inner: RwLock::new(()),
            id_mapping_shard: Mutex::new(()),
        }
    }

    // Simulates add() - Vacant path
    // 1. Lock Map (entry)
    // 2. Unlock Map
    // 3. Lock Inner (write)
    // 4. Lock Map (insert) -> SAFE now because Occupied path doesn't hold Map while waiting for Inner
    fn add_vacant(&self) {
        // Step 1: entry()
        {
            let _guard = self.id_mapping_shard.lock().unwrap();
            // Check... vacant
        } // Step 2: drop(entry)

        // Step 3: inner.write()
        let _inner_guard = self.inner.write().unwrap();

        // Step 4: insert to map
        // This used to deadlock if another thread held map lock and wanted inner
        // But now, nobody holds map lock and wants inner.
        let _map_guard = self.id_mapping_shard.lock().unwrap();
    }

    // Simulates add() - Occupied path (FIXED)
    // 1. Lock Map (entry)
    // 2. Drop Map (entry)
    // 3. Lock Inner (write)
    // 4. Lock Map (read/verify)
    fn add_occupied(&self) {
        // Step 1: entry()
        {
            let _map_guard = self.id_mapping_shard.lock().unwrap();
            // Step 2: Drop map lock
        }

        // Step 3: inner.write()
        let _inner_guard = self.inner.write().unwrap();

        // Step 4: Verify mapping (requires map lock again)
        let _map_guard = self.id_mapping_shard.lock().unwrap();
    }
}

#[test]
fn test_hnsw_deadlock_repro() {
    loom::model(|| {
        let index = Arc::new(MockIndex::new());

        let index1 = index.clone();
        let t1 = thread::spawn(move || {
            // Thread 1 does Add Vacant (Inner -> Map)
            index1.add_vacant();
        });

        let index2 = index.clone();
        let t2 = thread::spawn(move || {
            // Thread 2 does Add Occupied (Map -> Drop -> Inner -> Map)
            // It no longer holds Map while waiting for Inner.
            index2.add_occupied();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
