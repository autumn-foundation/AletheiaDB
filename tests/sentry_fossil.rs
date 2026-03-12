use aletheiadb::AletheiaDB;
use aletheiadb::experimental::fossil::FossilDetector;
use aletheiadb::core::temporal::TimeRange;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::property::PropertyMapBuilder;
use std::time::Duration;

#[test]
fn test_fossil_detector_coverage() {
    let db = AletheiaDB::new().unwrap();
    let detector = FossilDetector::new(&db);

    let start = HybridTimestamp::new(100, 0).unwrap();
    let end = HybridTimestamp::new(200, 0).unwrap();
    let window = TimeRange::new(start, end).unwrap();

    let node_id = NodeId::new(1).unwrap();

    // 1. Node not found/doesn't exist
    let result = detector.detect_fossil(node_id, window, "vec");
    assert!(result.is_err());

    // 2. Node exists but no property
    let mut missing_prop_node = NodeId::new(0).unwrap();
    db.write(|tx| {
        missing_prop_node = tx.create_node("Concept", Default::default()).unwrap();
        Ok::<(), aletheiadb::core::error::Error>(())
    }).unwrap();
    let result = detector.detect_fossil(missing_prop_node, window, "vec");
    assert!(result.is_err());

    // 3. Vector dimensions mismatch over time
    let mut dim_mismatch_node = NodeId::new(0).unwrap();

    // Create node at T0
    db.write(|tx| {
        dim_mismatch_node = tx.create_node(
            "Concept",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 2.0])
                .build()
        ).unwrap();
        Ok::<(), aletheiadb::core::error::Error>(())
    }).unwrap();

    let dim_time_start = aletheiadb::core::temporal::time::now();
    std::thread::sleep(Duration::from_millis(50));

    // Update node at T1 with different dimensions
    db.write(|tx| {
        tx.update_node(
            dim_mismatch_node,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 2.0, 3.0]) // 3 dimensions instead of 2
                .build()
        ).unwrap();
        Ok::<(), aletheiadb::core::error::Error>(())
    }).unwrap();

    let dim_time_end = aletheiadb::core::temporal::time::now();
    let dim_window = TimeRange::new(dim_time_start, dim_time_end).unwrap();

    let result = detector.detect_fossil(dim_mismatch_node, dim_window, "vec");
    assert!(result.is_err());
    if let Err(e) = result {
        assert!(e.to_string().contains("Dimension mismatch"));
    }

    // 4. Neighbor missing vector property
    let mut valid_node = NodeId::new(0).unwrap();
    let mut missing_neighbor = NodeId::new(0).unwrap();

    db.write(|tx| {
        valid_node = tx.create_node(
            "Concept",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 2.0])
                .build()
        ).unwrap();

        missing_neighbor = tx.create_node("Concept", Default::default()).unwrap();
        // incoming edge
        tx.create_edge(missing_neighbor, valid_node, "LINK", Default::default()).unwrap();
        // outgoing edge
        tx.create_edge(valid_node, missing_neighbor, "LINK", Default::default()).unwrap();
        Ok::<(), aletheiadb::core::error::Error>(())
    }).unwrap();

    let neighbor_time_start = aletheiadb::core::temporal::time::now();
    std::thread::sleep(Duration::from_millis(50));

    db.write(|tx| {
        tx.update_node(
            valid_node,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[2.0, 3.0])
                .build()
        ).unwrap();
        Ok::<(), aletheiadb::core::error::Error>(())
    }).unwrap();

    let neighbor_time_end = aletheiadb::core::temporal::time::now();
    let neighbor_window = TimeRange::new(neighbor_time_start, neighbor_time_end).unwrap();

    let result = detector.detect_fossil(valid_node, neighbor_window, "vec");
    assert!(result.is_ok());
    let res = result.unwrap();
    // context_displacement should be 0 because the only neighbor failed calculation
    assert_eq!(res.context_displacement, 0.0);
}
