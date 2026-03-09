#[cfg(loom)]
mod visibility_rwlock_upgrade {
    use loom::sync::{Arc, Mutex, RwLock};
    use loom::thread;

    struct VisibilityModel {
        active: Mutex<Arc<()>>,
        committed: RwLock<()>,
    }

    impl VisibilityModel {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                active: Mutex::new(Arc::new(())),
                committed: RwLock::new(()),
            })
        }

        fn register_active(&self) {
            let _active_guard = self.active.lock().unwrap();
        }

        fn is_visible(&self) {
            let _committed = self.committed.read().unwrap();
        }

        fn register_commit(&self) {
            // Write lock on committed
            let _committed = self.committed.write().unwrap();

            // Read lock on committed during snapshot capture or something else?
            // Actually, in register_commit:
            // let mut committed = self.committed.write()...
            // let mut active = self.active.lock()...
            let _active_guard = self.active.lock().unwrap();
        }

        // What if we do a visibility check while holding an active lock?
        // capture_snapshot:
        // let active = self.active.lock().unwrap();
    }

    #[test]
    fn test_rwlock_upgrade_deadlock() {
        loom::model(|| {
            let model = VisibilityModel::new();
            let m1 = model.clone();
            let m2 = model.clone();

            let t1 = thread::spawn(move || {
                // Thread 1: Check visibility (Read lock on `committed`)
                let _read_lock1 = m1.committed.read().unwrap();
                let _read_lock2 = m1.committed.read().unwrap(); // Simulate recursive read or multiple readers
            });

            let t2 = thread::spawn(move || {
                // Thread 2: Try to commit (Write lock on `committed`)
                m2.register_commit();
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
}
