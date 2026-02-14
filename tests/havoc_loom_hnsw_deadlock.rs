use loom::sync::{Arc, Mutex, RwLock};
use loom::thread;

// Simplified model of HnswIndex components
struct MockHnswIndex {
    // Represents inner RwLock<Index>
    inner: Arc<RwLock<()>>,
    // Represents id_mapping DashMap (simplified as Mutex)
    // In reality, DashMap has shard locks, but a single Mutex is sufficient to prove deadlock cycle
    // if threads contend for the same shard.
    id_mapping: Arc<Mutex<()>>,
}

impl MockHnswIndex {
    fn new() -> Self {
        MockHnswIndex {
            inner: Arc::new(RwLock::new(())),
            id_mapping: Arc::new(Mutex::new(())),
        }
    }

    // Models add() Vacant path
    fn add_vacant(&self) {
        // 1. Check mapping (simplified: lock then unlock)
        {
            let _guard = self.id_mapping.lock().unwrap();
        } // unlock

        // 2. Lock Inner
        let _inner_guard = self.inner.write().unwrap();

        // 3. Lock Map again (to insert)
        let _map_guard = self.id_mapping.lock().unwrap();

        // Critical section...
    }

    // Models add() Occupied path
    fn add_occupied(&self) {
        // 1. Lock Map (to check existence and get value)
        // CRITICAL: Hold lock continuously
        let _map_guard = self.id_mapping.lock().unwrap();

        // 2. Lock Inner (to update vector)
        let _inner_guard = self.inner.write().unwrap();

        // Critical section...
    }

    // Models add() Occupied path with FIX
    fn add_occupied_fixed(&self) {
        // 1. Lock Map (to check existence and get value)
        {
            let _map_guard = self.id_mapping.lock().unwrap();
        } // Drop lock

        // 2. Lock Inner (to update vector)
        let _inner_guard = self.inner.write().unwrap();

        // 3. Re-lock Map (to verify)
        let _map_guard = self.id_mapping.lock().unwrap();

        // Critical section...
    }
}

#[test]
#[should_panic]
#[ignore] // Ignored because it crashes the test runner with SIGABRT
fn test_deadlock_reproduction() {
    loom::model(|| {
        let index = Arc::new(MockHnswIndex::new());

        let index1 = index.clone();
        let t1 = thread::spawn(move || {
            index1.add_vacant();
        });

        let index2 = index.clone();
        let t2 = thread::spawn(move || {
            index2.add_occupied();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}

#[test]
fn test_deadlock_fix_simulation() {
    loom::model(|| {
        let index = Arc::new(MockHnswIndex::new());

        let index1 = index.clone();
        let t1 = thread::spawn(move || {
            index1.add_vacant();
        });

        let index2 = index.clone();
        let t2 = thread::spawn(move || {
            index2.add_occupied_fixed();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
