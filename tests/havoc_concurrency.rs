use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn test_concurrency_stress() {
    let index = Arc::new(
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap(),
    );

    let num_threads = 10;
    let iterations = 100;
    let barrier = Arc::new(Barrier::new(num_threads));

    let mut handles = vec![];

    for i in 0..num_threads {
        let index = index.clone();
        let barrier = barrier.clone();

        handles.push(thread::spawn(move || {
            barrier.wait();

            for j in 0..iterations {
                let id = NodeId::new(((i * iterations + j) % 100) as u64 + 1).unwrap();
                let vec = vec![0.1 * (j as f32), 0.2, 0.3, 0.4];

                // Randomly choose operation
                if j % 3 == 0 {
                    let _ = index.add(id, &vec);
                } else if j % 3 == 1 {
                    let _ = index.search(&vec, 5);
                } else {
                    let _ = index.remove(id);
                }
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}
