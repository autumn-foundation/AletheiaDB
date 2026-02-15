use aletheiadb::core::id::NodeId;
use aletheiadb::index::VectorIndex;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

const VECTOR_DIM: usize = 4;

#[test]
fn havoc_callback_deadlock_repro() {
    let (tx, rx) = std::sync::mpsc::channel();

    thread::spawn(move || {
        run_deadlock_repro();
        tx.send(()).unwrap();
    });

    // Timeout after 5 seconds (should be instant)
    if rx.recv_timeout(Duration::from_secs(5)).is_err() {
        panic!("Deadlock detected: search_with_filter callback waiting for add() timed out");
    }
}

fn run_deadlock_repro() {
    let index = HnswIndexBuilder::new(VECTOR_DIM, DistanceMetric::Cosine)
        .build()
        .expect("Failed to build index");
    let index = Arc::new(index);

    // Add some initial data
    for i in 1..=10 {
        let id = NodeId::new(i).unwrap();
        let vec = vec![0.1f32; VECTOR_DIM];
        index.add(id, &vec).unwrap();
    }

    let barrier = Arc::new(Barrier::new(2));
    let index_clone = index.clone();
    let barrier_clone = barrier.clone();

    // Ensure we only synchronize on the FIRST callback invocation
    let triggered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let triggered_clone = triggered.clone();

    let query = vec![0.1f32; VECTOR_DIM];

    let handle = thread::spawn(move || {
        println!("Thread A: Starting search");
        let _ = index_clone.search_with_filter(&query, 5, |id| {
            // Only sync once
            if !triggered_clone.swap(true, std::sync::atomic::Ordering::SeqCst) {
                println!("Thread A: Inside filter callback for node {:?} - Triggering barrier", id);

                // Signal Thread B to start 'add'
                barrier_clone.wait();

                // Wait for Thread B to finish 'add'
                // If 'add' blocks on 'search', Thread B never reaches here.
                barrier_clone.wait();
            }
            true
        });
        println!("Thread A: Search finished");
    });

    println!("Thread B: Waiting for filter callback signal");
    barrier.wait(); // Wait for callback to start

    println!("Thread B: Starting add");
    let id = NodeId::new(100).unwrap();
    let vec = vec![0.2f32; VECTOR_DIM];

    // This add requires a lock.
    // Before fix: Required Write lock. Search held Read lock. Deadlock.
    // After fix: Requires Read lock. Search holds Read lock. Compatible.
    index.add(id, &vec).unwrap();

    println!("Thread B: Add finished");
    barrier.wait(); // Unblock Thread A

    handle.join().unwrap();
}
