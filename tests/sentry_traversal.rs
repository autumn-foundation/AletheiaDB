use std::collections::HashSet;
use std::sync::Arc;
use parking_lot::RwLock;
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::version::AnchorConfig;
use aletheiadb::query::executor::ResultIterator;
use aletheiadb::query::executor::{NodeLookupIterator, TraversalIterator};
use aletheiadb::query::ir::Direction;
use aletheiadb::storage::current::CurrentStorage;
use aletheiadb::storage::historical::HistoricalStorage;

fn create_test_env() -> (Arc<CurrentStorage>, Arc<RwLock<HistoricalStorage>>) {
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
        AnchorConfig::default(),
    )));
    (current, historical)
}

/// Helper to create a simple connected graph
/// A -> B -> C
/// |    ^
/// v    |
/// D -> E
fn setup_graph(current: &Arc<CurrentStorage>) -> (NodeId, NodeId, NodeId, NodeId, NodeId) {
    let props = PropertyMapBuilder::new().build();
    let a = current.create_node("Node", props.clone()).unwrap();
    let b = current.create_node("Node", props.clone()).unwrap();
    let c = current.create_node("Node", props.clone()).unwrap();
    let d = current.create_node("Node", props.clone()).unwrap();
    let e = current.create_node("Node", props.clone()).unwrap();

    // A -> B
    current.create_edge(a, b, "NEXT", props.clone()).unwrap();
    // B -> C
    current.create_edge(b, c, "NEXT", props.clone()).unwrap();
    // A -> D
    current.create_edge(a, d, "NEXT", props.clone()).unwrap();
    // D -> E
    current.create_edge(d, e, "NEXT", props.clone()).unwrap();
    // E -> B (cycle potential if undirected, or distinct path)
    current.create_edge(e, b, "NEXT", props.clone()).unwrap();

    (a, b, c, d, e)
}

#[test]
fn test_traversal_cycle_detection() {
    let (current, historical) = create_test_env();
    let props = PropertyMapBuilder::new().build();

    // Create cycle: A -> B -> A
    let a = current.create_node("Node", props.clone()).unwrap();
    let b = current.create_node("Node", props.clone()).unwrap();

    current.create_edge(a, b, "NEXT", props.clone()).unwrap();
    current.create_edge(b, a, "NEXT", props.clone()).unwrap();

    // Traversal from A with depth 5
    // Should go: A -> B -> (stop, A visited)
    // Result should be B (depth 1)
    // Wait, the iterator yields nodes at ANY depth or just target depth?
    // Reading code: "if current_depth >= self.depth { yield ... }"
    // So it only yields nodes AT the target depth.

    // Case 1: Depth 1. Should yield B.
    let input = Box::new(NodeLookupIterator::new(vec![a], current.clone()));
    let mut iter = TraversalIterator::new(
        input,
        Direction::Outgoing,
        None,
        1,
        current.clone(),
        historical.clone(),
        None
    );

    let res = iter.next().unwrap().unwrap();
    assert_eq!(res.entity.node_id(), Some(b));
    assert!(iter.next().is_none());

    // Case 2: Depth 2. Should yield nothing (A is visited).
    let input = Box::new(NodeLookupIterator::new(vec![a], current.clone()));
    let mut iter = TraversalIterator::new(
        input,
        Direction::Outgoing,
        None,
        2,
        current.clone(),
        historical.clone(),
        None
    );

    // A -> B -> A. But A is in visited set of the path?
    // Implementation uses `visited` set per input node.
    // Start: frontier=[(A, path, 0)], visited={A}
    // Pop A. Depth 0 < 2. Neighbors: B.
    // B not in visited. visited={A, B}. Push (B, path+B, 1).
    // Pop B. Depth 1 < 2. Neighbors: A.
    // A in visited. Skip.
    // Frontier empty.
    assert!(iter.next().is_none());
}

#[test]
fn test_traversal_depth_limit() {
    let (current, historical) = create_test_env();
    let (a, _b, c, _d, e) = setup_graph(&current);

    // A -> B -> C
    // A -> D -> E -> B

    // Depth 2 from A
    // Expect: C (via B), E (via D)
    // B (via E) is depth 3, so not included

    let input = Box::new(NodeLookupIterator::new(vec![a], current.clone()));
    let mut iter = TraversalIterator::new(
        input,
        Direction::Outgoing,
        None,
        2,
        current.clone(),
        historical.clone(),
        None
    );

    let mut results = HashSet::new();
    while let Some(res) = iter.next() {
        if let Some(id) = res.unwrap().entity.node_id() {
            results.insert(id);
        }
    }

    assert_eq!(results.len(), 2);
    assert!(results.contains(&c));
    assert!(results.contains(&e));
}

#[test]
fn test_traversal_input_isolation() {
    let (current, historical) = create_test_env();
    let props = PropertyMapBuilder::new().build();

    // X -> Z
    // Y -> Z
    let x = current.create_node("Node", props.clone()).unwrap();
    let y = current.create_node("Node", props.clone()).unwrap();
    let z = current.create_node("Node", props.clone()).unwrap();

    current.create_edge(x, z, "NEXT", props.clone()).unwrap();
    current.create_edge(y, z, "NEXT", props.clone()).unwrap();

    // Input: [X, Y]. Depth 1.
    // Expect Z twice (once from X, once from Y).
    // This verifies that `visited` is cleared between inputs.

    let input = Box::new(NodeLookupIterator::new(vec![x, y], current.clone()));
    let mut iter = TraversalIterator::new(
        input,
        Direction::Outgoing,
        None,
        1,
        current.clone(),
        historical.clone(),
        None
    );

    let mut count = 0;
    while let Some(res) = iter.next() {
        let id = res.unwrap().entity.node_id().unwrap();
        assert_eq!(id, z);
        count += 1;
    }

    assert_eq!(count, 2, "Should find Z twice, once for each input");
}

#[test]
fn test_traversal_temporal_visibility() {
    let (current, historical) = create_test_env();
    let props = PropertyMapBuilder::new().build();
    let label = GLOBAL_INTERNER.intern("NEXT").unwrap();

    let a = current.create_node("Node", props.clone()).unwrap();
    let b = current.create_node("Node", props.clone()).unwrap();

    // Create edge A->B at t=100
    let t100 = 100.into();
    let edge_id = current.create_edge(a, b, "NEXT", props.clone()).unwrap();

    // Manually insert into history to simulate past existence
    // We need to ensure the edge existed at t=100
    {
        let mut hist = historical.write();
        hist.add_edge_version(
            edge_id,
            VersionId::new(1).unwrap(),
            t100,
            t100,
            label,
            a,
            b,
            props.clone(),
            false
        ).unwrap();
    }

    // Query at t=50 (Before edge existed). Should yield nothing.
    let t50 = 50.into();
    let input = Box::new(NodeLookupIterator::new(vec![a], current.clone()));
    let mut iter = TraversalIterator::new(
        input,
        Direction::Outgoing,
        None,
        1,
        current.clone(),
        historical.clone(),
        Some((t50, t50))
    );
    assert!(iter.next().is_none());

    // Query at t=150 (After edge existed). Should yield B.
    let t150 = 150.into();
    let input = Box::new(NodeLookupIterator::new(vec![a], current.clone()));
    let mut iter = TraversalIterator::new(
        input,
        Direction::Outgoing,
        None,
        1,
        current.clone(),
        historical.clone(),
        Some((t150, t150))
    );

    let res = iter.next().unwrap().unwrap();
    assert_eq!(res.entity.node_id(), Some(b));
}

#[test]
fn test_traversal_label_filter() {
    let (current, historical) = create_test_env();
    let props = PropertyMapBuilder::new().build();

    let a = current.create_node("Node", props.clone()).unwrap();
    let b = current.create_node("Node", props.clone()).unwrap();
    let c = current.create_node("Node", props.clone()).unwrap();

    // A -[FRIEND]-> B
    current.create_edge(a, b, "FRIEND", props.clone()).unwrap();
    // A -[ENEMY]-> C
    current.create_edge(a, c, "ENEMY", props.clone()).unwrap();

    // Traverse "FRIEND" only
    let input = Box::new(NodeLookupIterator::new(vec![a], current.clone()));
    let mut iter = TraversalIterator::new(
        input,
        Direction::Outgoing,
        Some("FRIEND".to_string()),
        1,
        current.clone(),
        historical.clone(),
        None
    );

    let res = iter.next().unwrap().unwrap();
    assert_eq!(res.entity.node_id(), Some(b));
    assert!(iter.next().is_none());
}
