#[cfg(loom)]
mod visibility_rwlock_deadlock {
    use loom::sync::{Arc, Mutex, RwLock};
    use loom::thread;

    struct BugModel {
        active: Mutex<()>,
        committed: RwLock<()>,
    }

    impl BugModel {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                active: Mutex::new(()),
                committed: RwLock::new(()),
            })
        }

        fn check_is_visible(&self) {
            let _committed = self.committed.read().unwrap();
        }

        fn register_commit(&self) {
            // Fix lock order inversion!
            let _active = self.active.lock().unwrap();
            let _committed = self.committed.write().unwrap();
        }

        fn do_something_active_then_commit(&self) {
            let _active = self.active.lock().unwrap();
            let _committed = self.committed.read().unwrap();
        }
    }

    #[test]
    fn test_lock_order_deadlock() {
        loom::model(|| {
            let model = BugModel::new();
            let m1 = model.clone();
            let m2 = model.clone();

            let t1 = thread::spawn(move || {
                m1.register_commit();
            });

            let t2 = thread::spawn(move || {
                m2.do_something_active_then_commit();
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
}
