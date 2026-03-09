#[cfg(loom)]
mod visibility_rwlock_order {
    use loom::sync::{Arc, Mutex, RwLock};
    use loom::thread;

    struct OrderModel {
        active: Mutex<()>,
        committed: RwLock<()>,
    }

    impl OrderModel {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                active: Mutex::new(()),
                committed: RwLock::new(()),
            })
        }

        fn check_is_visible(&self) {
            // is_visible locks committed for read
            let _committed = self.committed.read().unwrap();
        }

        fn register_commit(&self) {
            // register_commit locks committed for write, then active
            let _committed = self.committed.write().unwrap();
            let _active = self.active.lock().unwrap();
        }

        fn register_active(&self) {
            // register_active locks active
            let _active = self.active.lock().unwrap();
        }

        fn capture_snapshot(&self) {
            // capture_snapshot locks active
            let _active = self.active.lock().unwrap();
        }
    }

    #[test]
    fn test_lock_order_deadlock() {
        loom::model(|| {
            let model = OrderModel::new();
            let m1 = model.clone();
            let m2 = model.clone();

            let t1 = thread::spawn(move || {
                m1.register_commit();
            });

            let t2 = thread::spawn(move || {
                m2.register_active();
                m2.check_is_visible();
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
}
