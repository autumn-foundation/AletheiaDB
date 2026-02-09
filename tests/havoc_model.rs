use loom::sync::{Arc, Mutex, RwLock};
use loom::thread;

// Simplified Model of HnswIndex
struct Model {
    // inner: RwLock<Index>
    inner: RwLock<()>,
    // id_mapping: DashMap (Simulated by Mutex for simplicity, representing a shard lock)
    // In reality, DashMap has multiple shards, but a single Mutex is enough to prove the cycle exists.
    mapping: Mutex<()>,
}

#[test]
fn havoc_model_deadlock() {
    loom::model(|| {
        let model = Arc::new(Model {
            inner: RwLock::new(()),
            mapping: Mutex::new(()),
        });

        // Thread A: search_with_filter
        // 1. Acquire inner read lock
        // 2. Access mapping (acquire mapping lock)
        let t1 = {
            let model = model.clone();
            thread::spawn(move || {
                // Simulate search_with_filter
                let _guard = model.inner.read().unwrap();
                // Inside callback: access mapping
                let _map_guard = model.mapping.lock().unwrap();
            })
        };

        // Thread B: add (Occupied path) - FIXED
        // 1. Acquire mapping lock (via entry API), then RELEASE it
        // 2. Acquire inner write lock (to update vector)
        // 3. Re-acquire mapping lock (to verify)
        let t2 = {
            let model = model.clone();
            thread::spawn(move || {
                // Simulate add (Occupied) with Optimistic Concurrency Fix

                // 1. Get initial mapping state
                {
                    let _map_guard = model.mapping.lock().unwrap();
                    // Release lock immediately
                }

                // 2. Acquire Inner Write Lock
                let _inner_guard = model.inner.write().unwrap();

                // 3. Re-verify mapping (matches Inner -> Mapping order)
                let _map_guard = model.mapping.lock().unwrap();

                // (In real code, if verification fails, we release inner and retry, but here we model the lock acquisition success path)
            })
        };

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
