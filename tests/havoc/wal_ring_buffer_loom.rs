use aletheiadb::storage::wal::LSN;
use aletheiadb::storage::wal::ring_buffer::{PendingEntry, WalRingBuffer};
use loom::sync::Arc;
use loom::thread;

#[test]
fn test_ring_buffer_loom_append_drain() {
    loom::model(|| {
        let buf = Arc::new(WalRingBuffer::new(2));

        let b1 = buf.clone();
        let t1 = thread::spawn(move || {
            let _ = b1.try_append(PendingEntry::new_async(LSN(1), vec![1]));
        });

        let b2 = buf.clone();
        let t2 = thread::spawn(move || {
            let _ = b2.try_append(PendingEntry::new_async(LSN(2), vec![2]));
        });

        let b3 = buf.clone();
        let t3 = thread::spawn(move || {
            let _ = b3.drain();
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        let _ = buf.drain();
    });
}

#[test]
fn test_ring_buffer_loom_drain_order() {
    loom::model(|| {
        let buf = Arc::new(WalRingBuffer::new(2));

        let b1 = buf.clone();
        let t1 = thread::spawn(move || {
            let _ = b1.try_append(PendingEntry::new_async(LSN(1), vec![1]));
        });

        let b2 = buf.clone();
        let t2 = thread::spawn(move || {
            let _ = b2.try_append(PendingEntry::new_async(LSN(2), vec![2]));
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let entries = buf.drain();
        assert!(entries.len() <= 2);
    });
}

#[test]
fn test_ring_buffer_loom_full_wrap() {
    loom::model(|| {
        let buf = Arc::new(WalRingBuffer::new(2));

        let b1 = buf.clone();
        let t1 = thread::spawn(move || {
            let _ = b1.try_append(PendingEntry::new_async(LSN(1), vec![1]));
            let _ = b1.try_append(PendingEntry::new_async(LSN(2), vec![2]));
        });

        let b2 = buf.clone();
        let t2 = thread::spawn(move || {
            let _ = b2.try_append(PendingEntry::new_async(LSN(3), vec![3]));
        });

        let b3 = buf.clone();
        let t3 = thread::spawn(move || {
            let _ = b3.drain();
            let _ = b3.drain();
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        let _ = buf.drain();
    });
}

#[test]
fn test_ring_buffer_loom_drain_concurrency() {
    loom::model(|| {
        let buf = Arc::new(WalRingBuffer::new(2));

        let b1 = buf.clone();
        let t1 = thread::spawn(move || {
            let _ = b1.try_append(PendingEntry::new_async(LSN(1), vec![1]));
        });

        let b3 = buf.clone();
        let t3 = thread::spawn(move || {
            let entries = b3.drain();
            for entry in entries {
                let _ = entry; // Move entry
            }
        });

        t1.join().unwrap();
        t3.join().unwrap();

        let _ = buf.drain();
    });
}
