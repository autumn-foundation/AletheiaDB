use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder};
use aletheiadb::index::VectorIndex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn test_havoc_cross_index_reentrancy_fixed() {
    // 😇 Reform: Verify that we CAN write to Index B while reading Index A.

    // Create two independent indexes
    let index_a = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    let index_b = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();

    // Populate Index A
    let node1 = NodeId::new(1).unwrap();
    index_a.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    // Track success
    let add_success = Arc::new(AtomicBool::new(false));
    let add_success_clone = add_success.clone();

    // Search Index A with a filter
    // Inside the filter, try to add to Index B
    let _ = index_a.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 1, move |_node_id| {
        let node3 = NodeId::new(3).unwrap();
        // This should SUCCEED now
        match index_b.add(node3, &[0.0, 0.0, 1.0, 0.0]) {
            Ok(_) => {
                add_success_clone.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                println!("👺 Havoc detected error (still broken): {}", e);
            }
        }
        true
    });

    if !add_success.load(Ordering::SeqCst) {
        panic!("👺 Havoc: STILL FRAGILE. Cross-index reentrancy failed.");
    }
}
