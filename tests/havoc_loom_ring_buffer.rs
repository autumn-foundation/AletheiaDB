#[cfg(test)]
mod tests {
    use loom::sync::Arc;
    use aletheiadb::storage::wal::ring_buffer::*;
    use aletheiadb::storage::wal::LSN;
    use loom::thread;

    #[test]
    fn test_loom_append_drain() {
        loom::model(|| {
            let buffer = Arc::new(WalRingBuffer::new(2));

            let buffer_clone = buffer.clone();
            let thread1 = thread::spawn(move || {
                let (entry, _) = PendingEntry::new_sync(LSN(1), vec![1]);
                let _ = buffer_clone.try_append(entry);
            });

            let buffer_clone2 = buffer.clone();
            let thread2 = thread::spawn(move || {
                let (entry, _) = PendingEntry::new_sync(LSN(2), vec![2]);
                let _ = buffer_clone2.try_append(entry);
            });

            let buffer_clone3 = buffer.clone();
            let thread3 = thread::spawn(move || {
                let _ = buffer_clone3.drain();
            });

            thread1.join().unwrap();
            thread2.join().unwrap();
            thread3.join().unwrap();

            let _ = buffer.drain();
        });
    }
}
