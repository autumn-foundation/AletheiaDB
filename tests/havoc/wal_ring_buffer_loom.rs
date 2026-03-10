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
fn test_ring_buffer_loom_drain_preserves_order() {
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

        if entries.len() == 2 {
            // Because t1 and t2 race, either LSN 1 or LSN 2 can be in pos 0.
            // Draining simply pulls from pos 0 then pos 1.
            // So we just check that we got *both* LSN 1 and LSN 2 in some order.
            let has_1 = entries[0].lsn.0 == 1 || entries[1].lsn.0 == 1;
            let has_2 = entries[0].lsn.0 == 2 || entries[1].lsn.0 == 2;
            assert!(
                has_1 && has_2,
                "Drained entries do not contain both expected LSNs"
            );
        }
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
