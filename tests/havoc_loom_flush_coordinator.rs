#![cfg(loom)]

//! Loom model for `FlushCoordinator` lock ordering.
//!
//! `FlushCoordinator` (`src/storage/wal/flush_coordinator.rs`) holds two mutexes:
//!
//!   * `writer`      – the buffered segment writer
//!   * `sync_handle` – the file handle used for `fsync`
//!
//! Both `flush()` and the helpers it calls while holding `writer`
//! (`ensure_segment_open`, `maybe_rotate_segment`) acquire `sync_handle`
//! from *inside* the `writer` critical section.  The invariant is therefore:
//!
//!   writer → sync_handle   (always)
//!
//! No path acquires `sync_handle` and then tries to acquire `writer`, so there
//! is no inversion.  This model verifies that under all loom-explored
//! interleavings no deadlock arises when two flush threads race.
//!
//! Note: the production `FlushCoordinator` is documented as single-threaded
//! (one flush thread), but the lock ordering must still be correct in case a
//! caller accidentally invokes it concurrently.

use loom::sync::{Arc, Mutex};
use loom::thread;

struct FlushCoordinatorModel {
    writer: Mutex<Option<u64>>,      // None = segment not yet open
    sync_handle: Mutex<Option<u64>>, // None = file not yet open
}

impl FlushCoordinatorModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            writer: Mutex::new(None),
            sync_handle: Mutex::new(None),
        })
    }

    /// Mirrors `flush()` → `ensure_segment_open()` → (optionally) `maybe_rotate_segment()`.
    ///
    /// Acquires `writer` first, then `sync_handle` from inside the critical section.
    fn flush(&self, entries: &[u64]) {
        let mut writer_guard = self.writer.lock().unwrap();

        // ensure_segment_open: opens file handles while still holding writer
        if writer_guard.is_none() {
            let mut sync_guard = self.sync_handle.lock().unwrap();
            *sync_guard = Some(0); // open sync file
            *writer_guard = Some(0); // open writer
        }

        // Write entries using the open writer
        if let Some(ref mut w) = *writer_guard {
            *w += entries.len() as u64;
        }

        // maybe_rotate_segment: also acquires sync_handle inside writer lock
        {
            let mut sync_guard = self.sync_handle.lock().unwrap();
            if let Some(ref mut s) = *sync_guard {
                *s += 1; // simulate fsync
            }
        }
    }

    /// Mirrors the `sync = true` branch of `flush()`: acquires sync_handle
    /// while writer is held for the fsync step.
    fn flush_sync(&self) {
        let mut writer_guard = self.writer.lock().unwrap();

        if writer_guard.is_none() {
            let mut sync_guard = self.sync_handle.lock().unwrap();
            *sync_guard = Some(0);
            *writer_guard = Some(0);
        }

        // Explicit fsync: still inside writer lock
        let _sync_guard = self.sync_handle.lock().unwrap();
    }
}

/// Two concurrent flush() calls must not deadlock.
///
/// In practice `FlushCoordinator` uses a single flush thread, but the
/// ordering invariant must still hold if two callers race.
#[test]
fn test_concurrent_flush_no_deadlock() {
    loom::model(|| {
        let coord = FlushCoordinatorModel::new();
        let c1 = coord.clone();
        let c2 = coord.clone();

        let t1 = thread::spawn(move || {
            c1.flush(&[1, 2, 3]);
        });

        let t2 = thread::spawn(move || {
            c2.flush(&[4, 5, 6]);
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}

/// flush() vs flush_sync() must not deadlock.
#[test]
fn test_flush_vs_flush_sync_no_deadlock() {
    loom::model(|| {
        let coord = FlushCoordinatorModel::new();
        let c1 = coord.clone();
        let c2 = coord.clone();

        let t1 = thread::spawn(move || {
            c1.flush(&[1]);
        });

        let t2 = thread::spawn(move || {
            c2.flush_sync();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
