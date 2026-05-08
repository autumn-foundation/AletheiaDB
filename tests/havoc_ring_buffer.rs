#[cfg(test)]
mod tests {
    use aletheiadb::storage::wal::entry::LSN;
    use aletheiadb::storage::wal::ring_buffer::{PendingEntry, WalRingBuffer};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn fuzz_ring_buffer_concurrency() {
        let rb = Arc::new(WalRingBuffer::new(128));
        let mut handles = vec![];

        for thread_idx in 0..10 {
            let rb_clone = rb.clone();
            handles.push(thread::spawn(move || {
                for i in 0..1000 {
                    loop {
                        let pending = PendingEntry {
                            data: vec![0; 24],
                            lsn: LSN((thread_idx * 1000 + i) as u64),
                            completion: None,
                        };
                        if rb_clone.try_append(pending).is_ok() {
                            break;
                        }
                        thread::sleep(Duration::from_nanos(10));
                    }
                }
            }));
        }

        // Simulating the flush thread
        let rb_flush = rb.clone();
        handles.push(thread::spawn(move || {
            let mut flushed = 0;
            while flushed < 10000 {
                let entries = rb_flush.drain();
                flushed += entries.len();
                if entries.is_empty() {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }));

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
