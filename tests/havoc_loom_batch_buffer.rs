#![cfg(loom)]

//! Loom model for `BatchBuffer` lock ordering.
//!
//! `BatchBuffer` (`src/honeycomb/batch.rs`) documents that its two mutexes
//! must always be acquired in this order:
//!
//!   1. `events`      – the event queue
//!   2. `batch_start` – the optional batch-start timestamp
//!
//! Both `add()` and `flush()` follow this order. `is_timeout_expired()`
//! acquires only `batch_start`. This model verifies that no interleaving
//! of those three operations produces a deadlock.

use loom::sync::{Arc, Mutex};
use loom::thread;

struct BatchBufferModel {
    events: Mutex<Vec<u32>>,
    batch_start: Mutex<bool>, // true = batch in progress
    max_batch_size: usize,
}

impl BatchBufferModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            batch_start: Mutex::new(false),
            max_batch_size: 4,
        })
    }

    // Mirrors BatchBuffer::add(): events → batch_start
    fn add(&self, event: u32) -> Option<Vec<u32>> {
        let mut events = self.events.lock().unwrap();
        let mut batch_start = self.batch_start.lock().unwrap();

        if !*batch_start {
            *batch_start = true;
        }
        events.push(event);

        if events.len() >= self.max_batch_size {
            let batch = std::mem::take(&mut *events);
            *batch_start = false;
            return Some(batch);
        }
        None
    }

    // Mirrors BatchBuffer::flush(): events → batch_start
    fn flush(&self) -> Vec<u32> {
        let mut events = self.events.lock().unwrap();
        let mut batch_start = self.batch_start.lock().unwrap();
        let batch = std::mem::take(&mut *events);
        *batch_start = false;
        batch
    }

    // Mirrors BatchBuffer::is_timeout_expired(): batch_start only
    fn is_timeout_expired(&self) -> bool {
        *self.batch_start.lock().unwrap()
    }
}

/// Two threads racing add() vs flush() must not deadlock.
#[test]
fn test_add_vs_flush_no_deadlock() {
    loom::model(|| {
        let buf = BatchBufferModel::new();
        let b1 = buf.clone();
        let b2 = buf.clone();

        let t1 = thread::spawn(move || {
            b1.add(1);
        });

        let t2 = thread::spawn(move || {
            b2.flush();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}

/// Concurrent add() + add() + is_timeout_expired() must not deadlock.
/// This exercises the case where is_timeout_expired() holds batch_start
/// while add() tries to acquire events then batch_start.
#[test]
fn test_concurrent_adds_with_timeout_check_no_deadlock() {
    loom::model(|| {
        let buf = BatchBufferModel::new();
        let b1 = buf.clone();
        let b2 = buf.clone();
        let b3 = buf.clone();

        let t1 = thread::spawn(move || {
            b1.add(1);
        });

        let t2 = thread::spawn(move || {
            b2.add(2);
        });

        let t3 = thread::spawn(move || {
            b3.is_timeout_expired();
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();
    });
}
