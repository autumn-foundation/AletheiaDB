use aletheiadb::storage::wal::ring_buffer::{PendingEntry, WalRingBuffer};
use loom::sync::Arc;

#[test]
fn test_concurrent_drain_underflow() {
    loom::model(|| {
        let rb = Arc::new(WalRingBuffer::new(4));

        // Setup state: one entry
        rb.try_append(PendingEntry {
            lsn: aletheiadb::storage::wal::entry::LSN(1),
            data: vec![1, 2, 3],
            completion: None,
        })
        .unwrap();

        let rb1 = rb.clone();
        let t1 = loom::thread::spawn(move || {
            rb1.drain();
        });

        let rb2 = rb.clone();
        let t2 = loom::thread::spawn(move || {
            // Append and drain concurrently
            rb2.try_append(PendingEntry {
                lsn: aletheiadb::storage::wal::entry::LSN(2),
                data: vec![4, 5, 6],
                completion: None,
            })
            .unwrap();
            rb2.drain();
        });

        t1.join().unwrap();
        t2.join().unwrap();
    });
}
