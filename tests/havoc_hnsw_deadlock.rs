use gallifreydb::core::id::NodeId;
use gallifreydb::index::VectorIndex;
use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

#[test]
fn test_hnsw_save_add_deadlock() {
    // 👺 HAVOC: Triggering AB-BA Deadlock between save() and add()
    //
    // save(): Locks inner(Read) -> Locks id_mapping(Shard) (via iter)
    // add():  Locks id_mapping(Shard) -> Locks inner(Write)
    //
    // If they interleave, they deadlock.

    let dim = 4;
    let index = Arc::new(
        HnswIndexBuilder::new(dim, DistanceMetric::Cosine)
            .m(16)
            .ef_construction(100)
            .build()
            .unwrap(),
    );

    // Pre-populate with enough nodes to hit multiple shards
    let num_nodes = 1000;
    for i in 0..num_nodes {
        let id = NodeId::new(i as u64 + 1).unwrap();
        let vec = vec![0.1; dim];
        index.add(id, &vec).unwrap();
    }

    let iterations = 100;
    let barrier = Arc::new(Barrier::new(2));

    // Thread A: Continuously save
    let index_clone_a = index.clone();
    let barrier_clone_a = barrier.clone();
    let handle_a = thread::spawn(move || {
        barrier_clone_a.wait();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.index");

        for _ in 0..iterations {
            // This takes the inner Read lock, then iterates the map
            if let Err(e) = index_clone_a.save(&path) {
                println!("Save error: {}", e);
            }
        }
    });

    // Thread B: Continuously update existing nodes
    let index_clone_b = index.clone();
    let barrier_clone_b = barrier.clone();
    let handle_b = thread::spawn(move || {
        barrier_clone_b.wait();
        for i in 0..iterations * 100 {
            // Pick a random node to update (Occupied path)
            // We use simple modulo to cycle through nodes
            let id = NodeId::new((i % num_nodes) as u64 + 1).unwrap();
            let vec = vec![0.2; dim];

            // This takes the map Shard lock, then tries to take inner Write lock
            index_clone_b.add(id, &vec).unwrap();

            // Small sleep to allow interleaving, but not too much
            if i % 10 == 0 {
                thread::yield_now();
            }
        }
    });

    // Join threads
    // If deadlock occurs, these will never return.
    // We can't easily enforce a timeout in a standard test without a wrapper,
    // but the test runner has a default timeout.
    println!("👺 Havoc: Starting deadlock torture...");
    handle_a.join().unwrap();
    handle_b.join().unwrap();
    println!("👺 Havoc: Survived! (Deadlock fixed?)");
}
