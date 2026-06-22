#![allow(unexpected_cfgs)]
#[cfg(loom)]
mod loom_test {
    use loom::sync::{Arc, RwLock};
    use loom::thread;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CircuitState {
        Closed,
        Open,
        HalfOpen,
    }

    struct CircuitBreaker {
        state: RwLock<CircuitState>,
        opened_at: RwLock<Option<()>>,
        last_failure: RwLock<Option<()>>,
    }

    impl CircuitBreaker {
        fn new() -> Self {
            Self {
                state: RwLock::new(CircuitState::Closed),
                opened_at: RwLock::new(None),
                last_failure: RwLock::new(None),
            }
        }

        fn record_failure(&self) {
            let state = *self.state.read().unwrap();

            if state == CircuitState::Closed {
                {
                    let _last = self.last_failure.read().unwrap();
                }
                let mut _last_w = self.last_failure.write().unwrap();

                let mut s = self.state.write().unwrap();
                *s = CircuitState::Open;
                let mut opened = self.opened_at.write().unwrap();
                *opened = Some(());
            } else if state == CircuitState::HalfOpen {
                let mut s = self.state.write().unwrap();
                *s = CircuitState::Open;
                let mut opened = self.opened_at.write().unwrap();
                *opened = Some(());
            } else {
                let mut opened = self.opened_at.write().unwrap();
                *opened = Some(());
            }
        }

        fn maybe_transition(&self) {
            let state = *self.state.read().unwrap();
            if state == CircuitState::Open {
                let mut should_transition = false;
                {
                    let _opened = self.opened_at.read().unwrap();
                    should_transition = true;
                }

                if should_transition {
                    let mut s = self.state.write().unwrap();
                    if *s == CircuitState::Open {
                        *s = CircuitState::HalfOpen;
                    }
                }
            }
        }
    }

    #[test]
    fn test_circuit_breaker_deadlock() {
        loom::model(|| {
            let cb = Arc::new(CircuitBreaker::new());

            let c1 = cb.clone();
            let t1 = thread::spawn(move || {
                c1.record_failure();
            });

            let c2 = cb.clone();
            let t2 = thread::spawn(move || {
                c2.maybe_transition();
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
}
