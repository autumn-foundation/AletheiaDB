#[cfg(test)]
mod tests {
    use aletheiadb::storage::wal::entry::LSN;
    use aletheiadb::storage::wal::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig};
    use aletheiadb::storage::wal::ring_buffer::PendingEntry;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn fuzz_flush_coordinator() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = FlushCoordinatorConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            segment_size: 1024 * 1024,
            segments_to_retain: 10,
            flush_interval_ms: 100,
            sync_on_flush: true,
            write_buffer_size: 1024,
            wal_cipher: None,
        };
        let fc = Arc::new(FlushCoordinator::new(config).unwrap());
        let mut handles = vec![];
        for thread_idx in 0..5 {
            let fc_clone = fc.clone();
            handles.push(thread::spawn(move || {
                let pending = PendingEntry {
                    data: vec![0; 24],
                    lsn: LSN(thread_idx as u64),
                    completion: None,
                };
                let _ = fc_clone.flush(vec![pending], false);
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    }
}
