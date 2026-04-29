#[cfg(loom)]
mod loom_test {
    use aletheiadb::honeycomb::{BatchBuffer, Event, TransmissionOptions};
    use loom::sync::Arc;
    use loom::thread;
    use std::time::Duration;

    #[test]
    fn test_havoc_batch_buffer_concurrency() {
        loom::model(|| {
            let options = TransmissionOptions::default()
                .with_max_batch_size(2)
                .with_batch_timeout(Duration::from_millis(100));
            let buffer = Arc::new(BatchBuffer::new(options));

            let b1 = Arc::clone(&buffer);
            let t1 = thread::spawn(move || {
                let _ = b1.add(Event::new());
            });

            let b2 = Arc::clone(&buffer);
            let t2 = thread::spawn(move || {
                let _ = b2.add(Event::new());
            });

            let b3 = Arc::clone(&buffer);
            let t3 = thread::spawn(move || {
                let _ = b3.flush();
            });

            t1.join().unwrap();
            t2.join().unwrap();
            t3.join().unwrap();
        });
    }
}
