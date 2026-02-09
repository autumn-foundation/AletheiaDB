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

        // Thread B: add (Occupied path)
        // 1. Acquire mapping lock (via entry API)
        // 2. Acquire inner write lock (to update vector)
        let t2 = {
            let model = model.clone();
            thread::spawn(move || {
                // Simulate add (Occupied)
                let _map_guard = model.mapping.lock().unwrap();
                // Try to update inner index
                let _guard = model.inner.write().unwrap();
            })
        };

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
