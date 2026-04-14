use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::{AletheiaDB, ReadOps, WriteOps};

#[test]
fn incoming_edges_multiple_sources_same_target() {
    let db = AletheiaDB::new().unwrap();
    let empty_props = || PropertyMapBuilder::new().build();

    // Create 3 nodes: parent, child1, child2
    let parent = db
        .write(|tx| tx.create_node("Task", empty_props()))
        .unwrap();
    let child1 = db
        .write(|tx| tx.create_node("Task", empty_props()))
        .unwrap();
    let child2 = db
        .write(|tx| tx.create_node("Task", empty_props()))
        .unwrap();

    // Create 2 edges pointing AT the same target (parent)
    // child1 --SUBTASK_OF--> parent
    // child2 --SUBTASK_OF--> parent
    let edge1 = db
        .write(|tx| tx.create_edge(child1, parent, "SUBTASK_OF", empty_props()))
        .unwrap();

    let edge2 = db
        .write(|tx| tx.create_edge(child2, parent, "SUBTASK_OF", empty_props()))
        .unwrap();

    // Verify outgoing edges work
    let child1_out = db
        .read(|tx| Ok::<_, aletheiadb::Error>(tx.get_outgoing_edges(child1)))
        .unwrap();
    assert_eq!(child1_out.len(), 1, "child1 should have 1 outgoing edge");

    let child2_out = db
        .read(|tx| Ok::<_, aletheiadb::Error>(tx.get_outgoing_edges(child2)))
        .unwrap();
    assert_eq!(child2_out.len(), 1, "child2 should have 1 outgoing edge");

    // This should return 2 edges
    let parent_in = db
        .read(|tx| Ok::<_, aletheiadb::Error>(tx.get_incoming_edges(parent)))
        .unwrap();

    assert!(
        parent_in.contains(&edge1),
        "should contain edge from child1"
    );
    assert!(
        parent_in.contains(&edge2),
        "should contain edge from child2"
    );
    assert_eq!(
        parent_in.len(),
        2,
        "parent should have 2 incoming edges, but got {}. Edge IDs: {:?}",
        parent_in.len(),
        parent_in
    );
}

/// Variant: all edges created in a single transaction
#[test]
fn incoming_edges_multiple_sources_single_transaction() {
    let db = AletheiaDB::new().unwrap();
    let empty_props = || PropertyMapBuilder::new().build();

    let (parent, _child1, _child2, edge1, edge2) = db
        .write(|tx| {
            let parent = tx.create_node("Task", empty_props())?;
            let child1 = tx.create_node("Task", empty_props())?;
            let child2 = tx.create_node("Task", empty_props())?;
            let e1 = tx.create_edge(child1, parent, "SUBTASK_OF", empty_props())?;
            let e2 = tx.create_edge(child2, parent, "SUBTASK_OF", empty_props())?;
            Ok::<_, aletheiadb::Error>((parent, child1, child2, e1, e2))
        })
        .unwrap();

    let parent_in = db
        .read(|tx| Ok::<_, aletheiadb::Error>(tx.get_incoming_edges(parent)))
        .unwrap();
    assert_eq!(
        parent_in.len(),
        2,
        "parent should have 2 incoming SUBTASK_OF edges, got {}. IDs: {:?}",
        parent_in.len(),
        parent_in
    );
    assert!(parent_in.contains(&edge1));
    assert!(parent_in.contains(&edge2));
}

/// Variant: 5 edges to the same target
#[test]
fn incoming_edges_fan_in_pattern() {
    let db = AletheiaDB::new().unwrap();
    let empty_props = || PropertyMapBuilder::new().build();

    let target = db.write(|tx| tx.create_node("Hub", empty_props())).unwrap();

    let mut edge_ids = Vec::new();
    for _i in 0..5 {
        let source = db
            .write(|tx| tx.create_node("Spoke", empty_props()))
            .unwrap();
        let eid = db
            .write(|tx| tx.create_edge(source, target, "CONNECTS_TO", empty_props()))
            .unwrap();
        edge_ids.push(eid);
    }

    let incoming = db
        .read(|tx| Ok::<_, aletheiadb::Error>(tx.get_incoming_edges(target)))
        .unwrap();
    assert_eq!(
        incoming.len(),
        5,
        "hub should have 5 incoming edges, got {}. IDs: {:?}",
        incoming.len(),
        incoming
    );
}

/// Test using direct CurrentStorage API (bypassing transactions) to isolate the issue
#[test]
fn incoming_edges_direct_storage_api() {
    let db = AletheiaDB::new().unwrap();
    let empty_props = || PropertyMapBuilder::new().build();

    let (parent, _child1, _child2, edge1, edge2) = db
        .write(|tx| {
            let parent = tx.create_node("Task", empty_props())?;
            let child1 = tx.create_node("Task", empty_props())?;
            let child2 = tx.create_node("Task", empty_props())?;
            let e1 = tx.create_edge(child1, parent, "SUBTASK_OF", empty_props())?;
            let e2 = tx.create_edge(child2, parent, "SUBTASK_OF", empty_props())?;
            Ok::<_, aletheiadb::Error>((parent, child1, child2, e1, e2))
        })
        .unwrap();

    // Use the direct non-transactional API
    let parent_in = db.get_incoming_edges(parent);
    assert_eq!(
        parent_in.len(),
        2,
        "direct API: parent should have 2 incoming edges, got {}. IDs: {:?}",
        parent_in.len(),
        parent_in
    );
    assert!(parent_in.contains(&edge1));
    assert!(parent_in.contains(&edge2));
}
