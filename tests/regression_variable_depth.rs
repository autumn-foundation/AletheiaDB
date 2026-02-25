use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::core::version::AnchorConfig;
use aletheiadb::query::QueryExecutor;
use aletheiadb::query::planner::cost::Cost;
use aletheiadb::query::planner::{PhysicalOp, PhysicalPlan};
use aletheiadb::storage::CurrentStorage;
use aletheiadb::storage::historical::HistoricalStorage;
use parking_lot::RwLock;
use std::sync::Arc;

#[test]
fn test_variable_depth_traversal_bug() {
    // 1. Setup storage
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
        AnchorConfig::default(),
    )));

    // 2. Create graph: A -> B -> C -> D
    let props = PropertyMapBuilder::new().insert("name", "A").build();
    let a = current.create_node("Node", props).unwrap();

    let props = PropertyMapBuilder::new().insert("name", "B").build();
    let b = current.create_node("Node", props).unwrap();

    let props = PropertyMapBuilder::new().insert("name", "C").build();
    let c = current.create_node("Node", props).unwrap();

    let props = PropertyMapBuilder::new().insert("name", "D").build();
    let d = current.create_node("Node", props).unwrap();

    current
        .create_edge(a, b, "REL", PropertyMapBuilder::new().build())
        .unwrap();
    current
        .create_edge(b, c, "REL", PropertyMapBuilder::new().build())
        .unwrap();
    current
        .create_edge(c, d, "REL", PropertyMapBuilder::new().build())
        .unwrap();

    // 3. Setup Executor
    let executor = QueryExecutor::new(current.clone(), historical);

    // 4. Construct Physical Plan Manually
    // Look up A -> Traverse 1..3
    let plan = PhysicalPlan {
        root: PhysicalOp::IndexedTraversal {
            input: Box::new(PhysicalOp::NodeLookup { node_ids: vec![a] }),
            direction: aletheiadb::query::ir::Direction::Outgoing,
            label: Some("REL".to_string()),
            min_depth: 1,
            max_depth: 3,
            temporal_context: None,
        },
        estimated_cost: Cost::default(),
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };

    // 5. Execute
    let results = executor.execute(plan).expect("Execution failed");
    let rows: Vec<_> = results.collect_all().expect("Collection failed");

    // 6. Verify results
    let names: Vec<String> = rows
        .iter()
        .map(|r| {
            r.entity
                .as_node()
                .unwrap()
                .properties
                .get("name")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();

    println!("Found nodes: {:?}", names);

    // B, C, D should be found
    assert!(
        names.contains(&"B".to_string()),
        "Should contain B (depth 1)"
    );
    assert!(
        names.contains(&"C".to_string()),
        "Should contain C (depth 2)"
    );
    assert!(
        names.contains(&"D".to_string()),
        "Should contain D (depth 3)"
    );
    assert_eq!(rows.len(), 3, "Should find exactly 3 nodes");
}

#[test]
fn test_variable_depth_min_depth_filter() {
    // 1. Setup storage
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
        AnchorConfig::default(),
    )));

    // 2. Create graph: A -> B -> C -> D
    let props = PropertyMapBuilder::new().insert("name", "A").build();
    let a = current.create_node("Node", props).unwrap();

    let props = PropertyMapBuilder::new().insert("name", "B").build();
    let b = current.create_node("Node", props).unwrap();

    let props = PropertyMapBuilder::new().insert("name", "C").build();
    let c = current.create_node("Node", props).unwrap();

    let props = PropertyMapBuilder::new().insert("name", "D").build();
    let d = current.create_node("Node", props).unwrap();

    current
        .create_edge(a, b, "REL", PropertyMapBuilder::new().build())
        .unwrap();
    current
        .create_edge(b, c, "REL", PropertyMapBuilder::new().build())
        .unwrap();
    current
        .create_edge(c, d, "REL", PropertyMapBuilder::new().build())
        .unwrap();

    // 3. Setup Executor
    let executor = QueryExecutor::new(current.clone(), historical);

    // 4. Construct Physical Plan Manually
    // Look up A -> Traverse 2..3 (Should skip B at depth 1)
    let plan = PhysicalPlan {
        root: PhysicalOp::IndexedTraversal {
            input: Box::new(PhysicalOp::NodeLookup { node_ids: vec![a] }),
            direction: aletheiadb::query::ir::Direction::Outgoing,
            label: Some("REL".to_string()),
            min_depth: 2,
            max_depth: 3,
            temporal_context: None,
        },
        estimated_cost: Cost::default(),
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };

    // 5. Execute
    let results = executor.execute(plan).expect("Execution failed");
    let rows: Vec<_> = results.collect_all().expect("Collection failed");

    // 6. Verify results
    let names: Vec<String> = rows
        .iter()
        .map(|r| {
            r.entity
                .as_node()
                .unwrap()
                .properties
                .get("name")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();

    println!("Found nodes: {:?}", names);

    assert!(
        !names.contains(&"B".to_string()),
        "Should NOT contain B (depth 1)"
    );
    assert!(
        names.contains(&"C".to_string()),
        "Should contain C (depth 2)"
    );
    assert!(
        names.contains(&"D".to_string()),
        "Should contain D (depth 3)"
    );
    assert_eq!(rows.len(), 2, "Should find exactly 2 nodes (C, D)");
}

#[test]
fn test_variable_depth_max_includes_zero_depth() {
    // 1. Setup storage
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
        AnchorConfig::default(),
    )));

    // 2. Create graph: A -> B
    let props = PropertyMapBuilder::new().insert("name", "A").build();
    let a = current.create_node("Node", props).unwrap();

    let props = PropertyMapBuilder::new().insert("name", "B").build();
    let b = current.create_node("Node", props).unwrap();

    current
        .create_edge(a, b, "REL", PropertyMapBuilder::new().build())
        .unwrap();

    // 3. Setup Executor
    let executor = QueryExecutor::new(current.clone(), historical);

    // 4. Construct Physical Plan Manually
    // Look up A -> Traverse 0..1 (Should include A at depth 0 and B at depth 1)
    let plan = PhysicalPlan {
        root: PhysicalOp::IndexedTraversal {
            input: Box::new(PhysicalOp::NodeLookup { node_ids: vec![a] }),
            direction: aletheiadb::query::ir::Direction::Outgoing,
            label: Some("REL".to_string()),
            min_depth: 0,
            max_depth: 1,
            temporal_context: None,
        },
        estimated_cost: Cost::default(),
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };

    // 5. Execute
    let results = executor.execute(plan).expect("Execution failed");
    let rows: Vec<_> = results.collect_all().expect("Collection failed");

    // 6. Verify results
    let names: Vec<String> = rows
        .iter()
        .map(|r| {
            r.entity
                .as_node()
                .unwrap()
                .properties
                .get("name")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();

    println!("Found nodes: {:?}", names);

    assert!(
        names.contains(&"A".to_string()),
        "Should contain A (depth 0)"
    );
    assert!(
        names.contains(&"B".to_string()),
        "Should contain B (depth 1)"
    );
    assert_eq!(rows.len(), 2, "Should find exactly 2 nodes (A, B)");
}
