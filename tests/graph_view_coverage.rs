use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use aletheiadb::query::traits::GraphView;
use aletheiadb::core::temporal::time;

#[test]
fn test_graph_view_trait_coverage() {
    let db = AletheiaDB::new().unwrap();

    // Create nodes
    let node1 = db
        .create_node("Person", PropertyMapBuilder::new().insert("name", "Alice").build())
        .unwrap();
    let node2 = db
        .create_node("Person", PropertyMapBuilder::new().insert("name", "Bob").build())
        .unwrap();

    // Create edge
    let edge_id = db
        .create_edge(node1, node2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Use GraphView trait methods explicitly
    let view: &dyn GraphView = &db;

    // 1. get_node (already covered usually, but good to be explicit)
    let n1 = view.get_node(node1).unwrap();
    assert_eq!(n1.id, node1);

    // 2. get_edge (Uncovered Lines 13-15)
    let e = view.get_edge(edge_id).unwrap();
    assert_eq!(e.id, edge_id);

    // 3. get_edge_target (Already covered)
    assert_eq!(view.get_edge_target(edge_id).unwrap(), node2);

    // 4. get_edge_source (Uncovered Lines 21-23)
    assert_eq!(view.get_edge_source(edge_id).unwrap(), node1);

    // 5. get_outgoing_edges (Already covered usually)
    let out_edges = view.get_outgoing_edges(node1);
    assert_eq!(out_edges.len(), 1);
    assert_eq!(out_edges[0], edge_id);

    // 6. get_incoming_edges (Uncovered Lines 29-31)
    let in_edges = view.get_incoming_edges(node2);
    assert_eq!(in_edges.len(), 1);
    assert_eq!(in_edges[0], edge_id);

    // 7. get_outgoing_edges_with_label (Already covered)
    let out_label = view.get_outgoing_edges_with_label(node1, "KNOWS");
    assert_eq!(out_label.len(), 1);

    // 8. Temporal methods
    let now = time::now();

    // get_node_at_time
    let n1_t = view.get_node_at_time(node1, now, now).unwrap();
    assert_eq!(n1_t.id, node1);

    // get_edge_at_time
    let e_t = view.get_edge_at_time(edge_id, now, now).unwrap();
    assert_eq!(e_t.id, edge_id);

    // get_outgoing_edges_at_time
    let out_t = view.get_outgoing_edges_at_time(node1, now, now);
    assert_eq!(out_t.len(), 1);

    // get_incoming_edges_at_time (Uncovered Lines 64-71)
    let in_t = view.get_incoming_edges_at_time(node2, now, now);
    assert_eq!(in_t.len(), 1);
}
