#![cfg(loom)]
#![allow(unexpected_cfgs)]

use loom::sync::{Arc, Mutex, RwLock};
use loom::thread;

// Mock HnswIndex structure for concurrency model verification
struct Model {
    entry_locks: Vec<Mutex<()>>,
    save_lock: RwLock<()>,
    inner: RwLock<()>,
    id_mapping: RwLock<()>,      // Represents DashMap
    reverse_mapping: RwLock<()>, // Represents DashMap
}

impl Model {
    fn new() -> Arc<Self> {
        let entry_locks = (0..2).map(|_| Mutex::new(())).collect();
        Arc::new(Self {
            entry_locks,
            save_lock: RwLock::new(()),
            inner: RwLock::new(()),
            id_mapping: RwLock::new(()),
            reverse_mapping: RwLock::new(()),
        })
    }

    // Mimics HnswIndex::add
    fn add(&self, id: usize) {
        let lock_idx = id % self.entry_locks.len();
        // 1. Acquire entry lock
        let _key_guard = self.entry_locks[lock_idx].lock().unwrap();

        // 2. Acquire save_lock (shared)
        let _save_guard = self.save_lock.read().unwrap();

        // 3. Simulate DashMap entry() - acquire lock then drop
        {
            // Acquire map write lock (simulating entry())
            let _map_guard = self.id_mapping.write().unwrap();
            // Critical section for map lookup
        } // Drop map lock

        // 4. Acquire inner write lock
        let _inner_guard = self.inner.write().unwrap();

        // 5. Re-acquire map lock (insert/update)
        let _map_guard = self.id_mapping.write().unwrap();

        // 6. Acquire reverse map lock
        let _rev_guard = self.reverse_mapping.write().unwrap();
    }

    // Mimics HnswIndex::save
    fn save(&self) {
        // 1. Acquire save_lock (exclusive)
        let _save_guard = self.save_lock.write().unwrap();

        // 2. Acquire inner read lock
        let _inner_guard = self.inner.read().unwrap();

        // 3. Iterate map (acquires map lock)
        let _map_guard = self.id_mapping.read().unwrap();
    }

    // Mimics HnswIndex::search
    fn search(&self) {
        // 1. Acquire inner read lock
        // (Note: search skips save_lock!)
        let _inner_guard = self.inner.read().unwrap();

        // 2. Convert matches (reads reverse mapping)
        let _rev_guard = self.reverse_mapping.read().unwrap();
    }

    // Mimics HnswIndex::remove
    fn remove(&self, id: usize) {
        let lock_idx = id % self.entry_locks.len();
        // 1. Acquire entry lock
        let _key_guard = self.entry_locks[lock_idx].lock().unwrap();

        // 2. Acquire save_lock (shared)
        let _save_guard = self.save_lock.read().unwrap();

        // 3. Remove from map (acquires map lock then drops)
        {
            let _map_guard = self.id_mapping.write().unwrap();
            let _rev_guard = self.reverse_mapping.write().unwrap();
        } // Drop map locks

        // 4. Acquire inner write lock
        let _inner_guard = self.inner.write().unwrap();
    }
}

#[test]
fn test_concurrency_deadlock_check() {
    loom::model(|| {
        let model = Model::new();
        let m1 = model.clone();
        let m2 = model.clone();
        let m3 = model.clone();

        // Thread 1: Add
        let t1 = thread::spawn(move || {
            m1.add(0);
        });

        // Thread 2: Save
        let t2 = thread::spawn(move || {
            m2.save();
        });

        // Thread 3: Search
        let t3 = thread::spawn(move || {
            m3.search();
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();
    });
}

#[test]
fn test_add_remove_concurrency() {
    loom::model(|| {
        let model = Model::new();
        let m1 = model.clone();
        let m2 = model.clone();

        let t1 = thread::spawn(move || {
            m1.add(1);
        });

        let t2 = thread::spawn(move || {
            m2.remove(1);
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
