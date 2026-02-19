use crate::index::vector::{HnswConfig, DistanceMetric, Quantization, HnswIndexBuilder, HnswIndex};
use crate::core::id::NodeId;
use crate::index::VectorIndex;
use proptest::prelude::*;
use std::io::Cursor;
use tempfile::tempdir;
use std::sync::{Arc, Mutex, Weak};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn fuzz_hnsw_config_deserialization(data in prop::collection::vec(any::<u8>(), 0..1024)) {
        let mut cursor = Cursor::new(data);
        // We expect errors, but no panics
        let _ = HnswConfig::deserialize_from(&mut cursor);
    }

    #[test]
    fn fuzz_load_mappings_with_integrity(
        file_content in prop::collection::vec(any::<u8>(), 0..2048),
    ) {
        let dir = tempdir().unwrap();
        let mappings_path = dir.path().join("test.usearch.mappings");
        std::fs::write(&mappings_path, &file_content).unwrap();

        // This is a private function, so we need to be in the module hierarchy
        let _ = super::load_mappings_with_integrity(&mappings_path);
    }

    #[test]
    fn fuzz_hnsw_index_ops(
        // Generated vectors must match dimension (e.g., 4)
        vectors in prop::collection::vec(prop::collection::vec(any::<f32>(), 4), 1..50),
        ops in prop::collection::vec(prop::bool::ANY, 1..100)
    ) {
        // Build an index
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();

        // Add vectors
        for (i, vector) in vectors.iter().enumerate() {
            let id = NodeId::new((i + 1) as u64).unwrap();
            // This might fail if vector has NaNs, but shouldn't panic
            let _ = index.add(id, vector);
        }

        // Perform random operations
        for (i, should_remove) in ops.iter().enumerate() {
             let id_val = (i % vectors.len()) + 1;
             let id = NodeId::new(id_val as u64).unwrap();
             if *should_remove {
                 let _ = index.remove(id);
             } else {
                 let _ = index.add(id, &vectors[id_val - 1]);
             }

             // Verify search doesn't panic
             let _ = index.search(&[0.1, 0.2, 0.3, 0.4], 5);
        }
    }
}

#[test]
fn test_deadlock_custom_metric_reentrancy() {
    // Shared state to give access to index inside metric
    struct Shared {
        index: Mutex<Option<Weak<HnswIndex>>>,
    }
    let shared = Arc::new(Shared { index: Mutex::new(None) });
    let shared_clone = shared.clone();

    // Define metric that tries to add to index
    let metric_fn = move |_a: &[f32], _b: &[f32]| -> f32 {
        // Try to get index
        if let Some(weak) = shared_clone.index.lock().unwrap().as_ref() {
            if let Some(index) = weak.upgrade() {
                // Try to add - THIS SHOULD DEADLOCK if inner lock is held
                // We use a different ID to avoid other logic
                // Using a vector that definitely exists to ensure we don't just exit early
                let _ = index.add(NodeId::new(999).unwrap(), &[0.1, 0.2, 0.3, 0.4]);
            }
        }
        0.0
    };

    let mut builder = HnswIndexBuilder::new(4, DistanceMetric::Cosine);
    // We need F32 for custom metric
    builder = builder.quantization(Quantization::F32);
    builder = builder.with_custom_metric("deadlock", metric_fn);

    let index = Arc::new(builder.build().unwrap());

    // Populate shared state
    *shared.index.lock().unwrap() = Some(Arc::downgrade(&index));

    // Add some data to enable search
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0]).unwrap();

    // Trigger search which calls metric
    // Use a timeout to detect deadlock
    let (tx, rx) = std::sync::mpsc::channel();
    let index_clone = index.clone();
    std::thread::spawn(move || {
        // Search will call metric_fn, which calls index.add(), which tries to write-lock inner.
        // search holds read-lock on inner.
        // Deadlock.
        let _ = index_clone.search(&[1.0, 0.0, 0.0, 0.0], 1);
        tx.send(()).unwrap();
    });

    if rx.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
        panic!("Deadlock detected! Custom metric re-entrancy caused deadlock.");
    }
}
