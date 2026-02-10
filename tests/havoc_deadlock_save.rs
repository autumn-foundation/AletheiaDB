use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

#[test]
fn havoc_deadlock_save_vs_add() {
    // Watchdog to detect deadlock
    // If the test takes longer than 5 seconds, we assume it deadlocked.
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(5));
        // If we're still here, it's a deadlock
        eprintln!("Test timed out - assuming deadlock!");
        std::process::exit(101); // Exit with failure
    });

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deadlock.index");

    // Create index
    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    // Populate with some data
    let node1 = NodeId::new(1).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let barrier = Arc::new(Barrier::new(2));

    // Thread A: search_with_filter calls save()
    let index_clone1 = Arc::clone(&index);
    let barrier_clone1 = Arc::clone(&barrier);
    let path_clone = path.clone();
    let handle1 = thread::spawn(move || {
        barrier_clone1.wait();
        // Run repeatedly to hit the race
        for _ in 0..100 {
            let _ = index_clone1.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 10, |_| {
                // This is the trigger: calling save() from within the filter
                // We assert that save() fails with the expected error to prevent deadlock
                let result = index_clone1.save(&path_clone);

                if let Err(e) = &result {
                    let err_msg = e.to_string();
                    if !err_msg
                        .contains("Cannot save index from within a callback")
                    {
                        // Print error but try not to panic inside FFI if possible?
                        // But for test, panic is the way to signal failure.
                        panic!("Unexpected error message: {}", err_msg);
                    }
                } else {
                    panic!("save() should have failed when called from filter");
                }

                true
            });
        }
    });

    // Thread B: add() (Occupied path)
    let index_clone2 = Arc::clone(&index);
    let barrier_clone2 = Arc::clone(&barrier);
    let handle2 = thread::spawn(move || {
        barrier_clone2.wait();
        for i in 0..1000 {
            // Update same node repeatedly to trigger Occupied path
            let val = (i % 100) as f32 / 100.0;
            // This acquires id_mapping lock, then inner lock.
            let _ = index_clone2.add(node1, &[val, 1.0 - val, 0.0, 0.0]);
            // Small sleep to allow context switches and increase race window
            if i % 10 == 0 {
                thread::yield_now();
            }
        }
    });

    handle1.join().unwrap();
    handle2.join().unwrap();
}
