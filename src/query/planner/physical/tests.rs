use super::*;
use crate::core::NodeId;

// ==================== PhysicalPlan Tests ====================

#[test]
fn test_physical_plan_is_temporal() {
    let plan = PhysicalPlan {
        root: PhysicalOp::Empty,
        estimated_cost: Cost::default(),
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };
    assert!(!plan.is_temporal());

    let temporal_plan = PhysicalPlan {
        root: PhysicalOp::Empty,
        estimated_cost: Cost::default(),
        temporal_context: Some(TemporalContext::as_of(1000.into(), 2000.into())),
        parallel: false,
        include_provenance: false,
    };
    assert!(temporal_plan.is_temporal());
}

#[test]
fn test_physical_plan_cpu_cost() {
    let plan = PhysicalPlan {
        root: PhysicalOp::Empty,
        estimated_cost: Cost {
            cpu: 42.0,
            io: 0.0,
            memory: 0,
            network: 0.0,
        },
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };
    assert_eq!(plan.cpu_cost(), 42.0);
}

#[test]
fn test_physical_plan_memory_cost() {
    let plan = PhysicalPlan {
        root: PhysicalOp::Empty,
        estimated_cost: Cost {
            cpu: 0.0,
            io: 0.0,
            memory: 1024,
            network: 0.0,
        },
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };
    assert_eq!(plan.memory_cost(), 1024);
}

// ==================== PhysicalOp Name Tests ====================

#[test]
fn test_physical_op_names() {
    let lookup = PhysicalOp::NodeLookup {
        node_ids: vec![NodeId::new(1).unwrap()],
    };
    assert_eq!(lookup.name(), "NodeLookup");

    let empty = PhysicalOp::Empty;
    assert_eq!(empty.name(), "Empty");
}

#[test]
fn test_physical_op_names_all_variants() {
    // Scan operators
    assert_eq!(
        PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()]
        }
        .name(),
        "NodeLookup"
    );
    assert_eq!(
        PhysicalOp::NodeScan {
            label: None,
            estimated_rows: 100
        }
        .name(),
        "NodeScan"
    );
    assert_eq!(
        PhysicalOp::HnswSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            label_filter: None,
            property_key: None,
        }
        .name(),
        "HnswSearch"
    );
    assert_eq!(
        PhysicalOp::TemporalNodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
            valid_time: 1000.into(),
            transaction_time: 2000.into(),
            use_batch: false,
        }
        .name(),
        "TemporalNodeLookup"
    );
    assert_eq!(
        PhysicalOp::TemporalVectorSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            timestamp: 1000.into(),
            property_key: None,
        }
        .name(),
        "TemporalVectorSearch"
    );

    // Traversal operators
    assert_eq!(
        PhysicalOp::IndexedTraversal {
            input: Box::new(PhysicalOp::Empty),
            direction: Direction::Outgoing,
            label: None,
            depth: 1,
            temporal_context: None,
        }
        .name(),
        "IndexedTraversal"
    );

    // Join operators
    assert_eq!(
        PhysicalOp::HashJoin {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
            left_key: "id".to_string(),
            right_key: "id".to_string()
        }
        .name(),
        "HashJoin"
    );
    assert_eq!(
        PhysicalOp::Union {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty)
        }
        .name(),
        "Union"
    );
    assert_eq!(
        PhysicalOp::Intersect {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty)
        }
        .name(),
        "Intersect"
    );
    assert_eq!(
        PhysicalOp::Except {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty)
        }
        .name(),
        "Except"
    );

    // Transform operators
    assert_eq!(
        PhysicalOp::Filter {
            input: Box::new(PhysicalOp::Empty),
            predicate: Predicate::True
        }
        .name(),
        "Filter"
    );
    assert_eq!(
        PhysicalOp::VectorRerank {
            input: Box::new(PhysicalOp::Empty),
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            property_key: None,
        }
        .name(),
        "VectorRerank"
    );
    assert_eq!(
        PhysicalOp::Sort {
            input: Box::new(PhysicalOp::Empty),
            key: SortKey::Property("name".to_string()),
            descending: false
        }
        .name(),
        "Sort"
    );
    assert_eq!(
        PhysicalOp::Limit {
            input: Box::new(PhysicalOp::Empty),
            count: 10,
            offset: 0
        }
        .name(),
        "Limit"
    );
    assert_eq!(
        PhysicalOp::Project {
            input: Box::new(PhysicalOp::Empty),
            properties: vec!["name".to_string()]
        }
        .name(),
        "Project"
    );
    assert_eq!(
        PhysicalOp::Distinct {
            input: Box::new(PhysicalOp::Empty)
        }
        .name(),
        "Distinct"
    );
    assert_eq!(
        PhysicalOp::Count {
            input: Box::new(PhysicalOp::Empty)
        }
        .name(),
        "Count"
    );
    assert_eq!(
        PhysicalOp::TemporalTrack {
            input: Box::new(PhysicalOp::Empty),
            time_range: TimeRange::new(1000.into(), 2000.into()).unwrap()
        }
        .name(),
        "TemporalTrack"
    );
    assert_eq!(
        PhysicalOp::Materialize {
            input: Box::new(PhysicalOp::Empty)
        }
        .name(),
        "Materialize"
    );
    assert_eq!(PhysicalOp::Empty.name(), "Empty");
}

// ==================== is_leaf Tests ====================

#[test]
fn test_is_leaf() {
    let lookup = PhysicalOp::NodeLookup {
        node_ids: vec![NodeId::new(1).unwrap()],
    };
    assert!(lookup.is_leaf());

    let filter = PhysicalOp::Filter {
        input: Box::new(PhysicalOp::Empty),
        predicate: Predicate::True,
    };
    assert!(!filter.is_leaf());
}

#[test]
fn test_is_leaf_all_leaf_operators() {
    assert!(
        PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()]
        }
        .is_leaf()
    );
    assert!(
        PhysicalOp::NodeScan {
            label: None,
            estimated_rows: 100
        }
        .is_leaf()
    );
    assert!(
        PhysicalOp::HnswSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            label_filter: None,
            property_key: None,
        }
        .is_leaf()
    );
    assert!(
        PhysicalOp::TemporalNodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
            valid_time: 1000.into(),
            transaction_time: 2000.into(),
            use_batch: false,
        }
        .is_leaf()
    );
    assert!(
        PhysicalOp::TemporalVectorSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            timestamp: 1000.into(),
            property_key: None,
        }
        .is_leaf()
    );
    assert!(PhysicalOp::Empty.is_leaf());
}

#[test]
fn test_is_leaf_non_leaf_operators() {
    assert!(
        !PhysicalOp::IndexedTraversal {
            input: Box::new(PhysicalOp::Empty),
            direction: Direction::Outgoing,
            label: None,
            depth: 1,
            temporal_context: None,
        }
        .is_leaf()
    );
    assert!(
        !PhysicalOp::Filter {
            input: Box::new(PhysicalOp::Empty),
            predicate: Predicate::True
        }
        .is_leaf()
    );
    assert!(
        !PhysicalOp::VectorRerank {
            input: Box::new(PhysicalOp::Empty),
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            property_key: None,
        }
        .is_leaf()
    );
    assert!(
        !PhysicalOp::Limit {
            input: Box::new(PhysicalOp::Empty),
            count: 10,
            offset: 0
        }
        .is_leaf()
    );
    assert!(
        !PhysicalOp::Union {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty)
        }
        .is_leaf()
    );
}

// ==================== depth Tests ====================

#[test]
fn test_depth() {
    let lookup = PhysicalOp::NodeLookup {
        node_ids: vec![NodeId::new(1).unwrap()],
    };
    assert_eq!(lookup.depth(), 1);

    let filter = PhysicalOp::Filter {
        input: Box::new(lookup),
        predicate: Predicate::True,
    };
    assert_eq!(filter.depth(), 2);

    let limit = PhysicalOp::Limit {
        input: Box::new(filter),
        count: 10,
        offset: 0,
    };
    assert_eq!(limit.depth(), 3);
}

#[test]
fn test_depth_all_leaf_operators() {
    assert_eq!(PhysicalOp::Empty.depth(), 1);
    assert_eq!(
        PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()]
        }
        .depth(),
        1
    );
    assert_eq!(
        PhysicalOp::NodeScan {
            label: None,
            estimated_rows: 100
        }
        .depth(),
        1
    );
    assert_eq!(
        PhysicalOp::HnswSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            label_filter: None,
            property_key: None,
        }
        .depth(),
        1
    );
    assert_eq!(
        PhysicalOp::TemporalNodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
            valid_time: 1000.into(),
            transaction_time: 2000.into(),
            use_batch: false,
        }
        .depth(),
        1
    );
    assert_eq!(
        PhysicalOp::TemporalVectorSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            timestamp: 1000.into(),
            property_key: None,
        }
        .depth(),
        1
    );
}

#[test]
fn test_depth_binary_operators() {
    // Symmetric depths
    let union = PhysicalOp::Union {
        left: Box::new(PhysicalOp::Empty),
        right: Box::new(PhysicalOp::Empty),
    };
    assert_eq!(union.depth(), 2);

    // Asymmetric depths - should take max
    let union_asymmetric = PhysicalOp::Union {
        left: Box::new(PhysicalOp::Filter {
            input: Box::new(PhysicalOp::Empty),
            predicate: Predicate::True,
        }),
        right: Box::new(PhysicalOp::Empty),
    };
    assert_eq!(union_asymmetric.depth(), 3);

    // HashJoin
    let hash_join = PhysicalOp::HashJoin {
        left: Box::new(PhysicalOp::Empty),
        right: Box::new(PhysicalOp::Filter {
            input: Box::new(PhysicalOp::Filter {
                input: Box::new(PhysicalOp::Empty),
                predicate: Predicate::True,
            }),
            predicate: Predicate::True,
        }),
        left_key: "id".to_string(),
        right_key: "id".to_string(),
    };
    assert_eq!(hash_join.depth(), 4); // 1 + max(1, 3)

    // Intersect
    let intersect = PhysicalOp::Intersect {
        left: Box::new(PhysicalOp::Empty),
        right: Box::new(PhysicalOp::Empty),
    };
    assert_eq!(intersect.depth(), 2);

    // Except
    let except = PhysicalOp::Except {
        left: Box::new(PhysicalOp::Empty),
        right: Box::new(PhysicalOp::Empty),
    };
    assert_eq!(except.depth(), 2);
}

#[test]
fn test_depth_unary_operators() {
    let base = PhysicalOp::Empty;

    // All unary operators add 1 to depth
    assert_eq!(
        PhysicalOp::Sort {
            input: Box::new(base.clone()),
            key: SortKey::Property("name".to_string()),
            descending: false
        }
        .depth(),
        2
    );
    assert_eq!(
        PhysicalOp::Project {
            input: Box::new(base.clone()),
            properties: vec![]
        }
        .depth(),
        2
    );
    assert_eq!(
        PhysicalOp::Distinct {
            input: Box::new(base.clone())
        }
        .depth(),
        2
    );
    assert_eq!(
        PhysicalOp::Count {
            input: Box::new(base.clone())
        }
        .depth(),
        2
    );
    assert_eq!(
        PhysicalOp::TemporalTrack {
            input: Box::new(base.clone()),
            time_range: TimeRange::new(1000.into(), 2000.into()).unwrap()
        }
        .depth(),
        2
    );
    assert_eq!(
        PhysicalOp::Materialize {
            input: Box::new(base)
        }
        .depth(),
        2
    );
}

// ==================== explain Tests ====================

#[test]
fn test_explain() {
    let plan = PhysicalOp::Limit {
        input: Box::new(PhysicalOp::Filter {
            input: Box::new(PhysicalOp::NodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
            }),
            predicate: Predicate::True,
        }),
        count: 10,
        offset: 0,
    };

    let explain = plan.explain();
    assert!(explain.contains("Limit"));
    assert!(explain.contains("Filter"));
    assert!(explain.contains("NodeLookup"));
}

#[test]
fn test_explain_node_scan() {
    let plan = PhysicalOp::NodeScan {
        label: Some("Person".to_string()),
        estimated_rows: 1000,
    };

    let explain = plan.explain();
    assert!(explain.contains("NodeScan"));
    assert!(explain.contains("Person"));
    assert!(explain.contains("1000"));
}

#[test]
fn test_explain_hnsw_search() {
    let plan = PhysicalOp::HnswSearch {
        embedding: Arc::from([0.1f32; 4].as_slice()),
        k: 10,
        label_filter: Some("Document".to_string()),
        property_key: None,
    };

    let explain = plan.explain();
    assert!(explain.contains("HnswSearch"));
    assert!(explain.contains("k: 10"));
    assert!(explain.contains("Document"));
}

#[test]
fn test_explain_temporal_node_lookup() {
    let plan = PhysicalOp::TemporalNodeLookup {
        node_ids: vec![NodeId::new(42).unwrap()],
        valid_time: 1000.into(),
        transaction_time: 2000.into(),
        use_batch: false,
    };

    let explain = plan.explain();
    assert!(explain.contains("TemporalNodeLookup"));
    assert!(explain.contains("vt: 1000"));
    assert!(explain.contains("tt: 2000"));
    assert!(explain.contains("batch: false"));
}

#[test]
fn test_explain_indexed_traversal() {
    let plan = PhysicalOp::IndexedTraversal {
        input: Box::new(PhysicalOp::Empty),
        direction: Direction::Outgoing,
        label: Some("KNOWS".to_string()),
        depth: 2,
        temporal_context: None,
    };

    let explain = plan.explain();
    assert!(explain.contains("IndexedTraversal"));
    assert!(explain.contains("Outgoing"));
    assert!(explain.contains("KNOWS"));
    assert!(explain.contains("depth: 2"));
}

#[test]
fn test_explain_vector_rerank() {
    let plan = PhysicalOp::VectorRerank {
        input: Box::new(PhysicalOp::Empty),
        embedding: Arc::from([0.1f32; 4].as_slice()),
        k: 5,
        property_key: None,
    };

    let explain = plan.explain();
    assert!(explain.contains("VectorRerank"));
    assert!(explain.contains("k: 5"));
}

#[test]
fn test_explain_hash_join() {
    let plan = PhysicalOp::HashJoin {
        left: Box::new(PhysicalOp::Empty),
        right: Box::new(PhysicalOp::Empty),
        left_key: "user_id".to_string(),
        right_key: "id".to_string(),
    };

    let explain = plan.explain();
    assert!(explain.contains("HashJoin"));
    assert!(explain.contains("user_id"));
    assert!(explain.contains("id"));
}

#[test]
fn test_explain_union() {
    let plan = PhysicalOp::Union {
        left: Box::new(PhysicalOp::Empty),
        right: Box::new(PhysicalOp::Empty),
    };

    let explain = plan.explain();
    assert!(explain.contains("Union"));
    assert!(explain.contains("Empty"));
}

#[test]
fn test_explain_simple_operators() {
    // Sort
    let sort = PhysicalOp::Sort {
        input: Box::new(PhysicalOp::Empty),
        key: SortKey::Property("name".to_string()),
        descending: false,
    };
    let explain = sort.explain();
    assert!(explain.contains("Sort"));
    assert!(explain.contains("Empty"));

    // Project
    let project = PhysicalOp::Project {
        input: Box::new(PhysicalOp::Empty),
        properties: vec!["name".to_string()],
    };
    let explain = project.explain();
    assert!(explain.contains("Project"));

    // Distinct
    let distinct = PhysicalOp::Distinct {
        input: Box::new(PhysicalOp::Empty),
    };
    let explain = distinct.explain();
    assert!(explain.contains("Distinct"));

    // Count
    let count = PhysicalOp::Count {
        input: Box::new(PhysicalOp::Empty),
    };
    let explain = count.explain();
    assert!(explain.contains("Count"));

    // Materialize
    let materialize = PhysicalOp::Materialize {
        input: Box::new(PhysicalOp::Empty),
    };
    let explain = materialize.explain();
    assert!(explain.contains("Materialize"));
}

// ==================== get_input Tests ====================

#[test]
fn test_get_input_returns_none_for_leaf() {
    let lookup = PhysicalOp::NodeLookup {
        node_ids: vec![NodeId::new(1).unwrap()],
    };
    assert!(lookup.get_input().is_none());
    assert!(PhysicalOp::Empty.get_input().is_none());
}

#[test]
fn test_get_input_returns_some_for_unary() {
    let filter = PhysicalOp::Filter {
        input: Box::new(PhysicalOp::Empty),
        predicate: Predicate::True,
    };
    assert!(filter.get_input().is_some());
    assert!(matches!(filter.get_input(), Some(PhysicalOp::Empty)));
}

#[test]
fn test_get_input_returns_none_for_binary() {
    let union = PhysicalOp::Union {
        left: Box::new(PhysicalOp::Empty),
        right: Box::new(PhysicalOp::Empty),
    };
    // Binary operators don't have a single input
    assert!(union.get_input().is_none());
}

// ==================== Additional Explain Tests ====================

#[test]
fn test_explain_temporal_vector_search() {
    let plan = PhysicalOp::TemporalVectorSearch {
        embedding: Arc::from([0.1f32; 4].as_slice()),
        k: 10,
        timestamp: 42000.into(),
        property_key: None,
    };

    let explain = plan.explain();
    assert!(explain.contains("TemporalVectorSearch"));
}

#[test]
fn test_explain_intersect() {
    let plan = PhysicalOp::Intersect {
        left: Box::new(PhysicalOp::Empty),
        right: Box::new(PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
        }),
    };

    let explain = plan.explain();
    assert!(explain.contains("Intersect"));
    assert!(explain.contains("Empty"));
    assert!(explain.contains("NodeLookup"));
}

#[test]
fn test_explain_except() {
    let plan = PhysicalOp::Except {
        left: Box::new(PhysicalOp::NodeScan {
            label: Some("Person".to_string()),
            estimated_rows: 100,
        }),
        right: Box::new(PhysicalOp::Empty),
    };

    let explain = plan.explain();
    assert!(explain.contains("Except"));
    assert!(explain.contains("NodeScan"));
    assert!(explain.contains("Empty"));
}

#[test]
fn test_explain_temporal_track() {
    let plan = PhysicalOp::TemporalTrack {
        input: Box::new(PhysicalOp::Empty),
        time_range: TimeRange::new(1000.into(), 2000.into()).unwrap(),
    };

    let explain = plan.explain();
    assert!(explain.contains("TemporalTrack"));
    assert!(explain.contains("Empty"));
}

// ==================== Additional Name Tests ====================

#[test]
fn test_all_operator_names() {
    // Test all operator variants have correct names
    assert_eq!(
        PhysicalOp::TemporalVectorSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            timestamp: 1000.into(),
            property_key: None,
        }
        .name(),
        "TemporalVectorSearch"
    );

    assert_eq!(
        PhysicalOp::Intersect {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
        }
        .name(),
        "Intersect"
    );

    assert_eq!(
        PhysicalOp::Except {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
        }
        .name(),
        "Except"
    );

    assert_eq!(
        PhysicalOp::TemporalTrack {
            input: Box::new(PhysicalOp::Empty),
            time_range: TimeRange::new(1000.into(), 2000.into()).unwrap(),
        }
        .name(),
        "TemporalTrack"
    );

    assert_eq!(
        PhysicalOp::Materialize {
            input: Box::new(PhysicalOp::Empty),
        }
        .name(),
        "Materialize"
    );
}

// ==================== Additional Depth Tests ====================

#[test]
fn test_depth_additional_binary_operators() {
    let intersect = PhysicalOp::Intersect {
        left: Box::new(PhysicalOp::Filter {
            input: Box::new(PhysicalOp::Empty),
            predicate: Predicate::True,
        }),
        right: Box::new(PhysicalOp::Empty),
    };
    assert_eq!(intersect.depth(), 3); // 1 + max(2, 1)

    let except = PhysicalOp::Except {
        left: Box::new(PhysicalOp::Empty),
        right: Box::new(PhysicalOp::Limit {
            input: Box::new(PhysicalOp::Empty),
            count: 10,
            offset: 0,
        }),
    };
    assert_eq!(except.depth(), 3); // 1 + max(1, 2)
}

#[test]
fn test_depth_temporal_operators() {
    let temporal_track = PhysicalOp::TemporalTrack {
        input: Box::new(PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
        }),
        time_range: TimeRange::new(1000.into(), 2000.into()).unwrap(),
    };
    assert_eq!(temporal_track.depth(), 2); // 1 + 1
}

// ==================== Additional is_leaf Tests ====================

#[test]
fn test_is_leaf_additional_non_leaf_operators() {
    // Temporal track
    assert!(
        !PhysicalOp::TemporalTrack {
            input: Box::new(PhysicalOp::Empty),
            time_range: TimeRange::new(1000.into(), 2000.into()).unwrap(),
        }
        .is_leaf()
    );

    // Binary operators
    assert!(
        !PhysicalOp::Intersect {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
        }
        .is_leaf()
    );

    assert!(
        !PhysicalOp::Except {
            left: Box::new(PhysicalOp::Empty),
            right: Box::new(PhysicalOp::Empty),
        }
        .is_leaf()
    );
}

// ==================== Explain Tests ====================

#[test]
fn test_explain_simple_node_lookup() {
    let plan = PhysicalPlan {
        root: PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()],
        },
        estimated_cost: Cost {
            cpu: 1.0,
            io: 0.0,
            memory: 100,
            network: 0.0,
        },
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };

    let explanation = plan.explain();

    // Should include operator name
    assert!(explanation.contains("NodeLookup"));
    // Should include cost estimate
    assert!(explanation.contains("cost"));
    // Should include cardinality (2 nodes)
    assert!(explanation.contains("rows"));
}

#[test]
fn test_explain_nested_operations() {
    let plan = PhysicalPlan {
        root: PhysicalOp::Filter {
            input: Box::new(PhysicalOp::IndexedTraversal {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![NodeId::new(1).unwrap()],
                }),
                direction: Direction::Outgoing,
                label: Some("KNOWS".to_string()),
                depth: 2,
                temporal_context: None,
            }),
            predicate: Predicate::eq("age", 25i64),
        },
        estimated_cost: Cost {
            cpu: 10.0,
            io: 5.0,
            memory: 1000,
            network: 0.0,
        },
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };

    let explanation = plan.explain();

    // Should show nested structure with indentation
    assert!(explanation.contains("Filter"));
    assert!(explanation.contains("IndexedTraversal"));
    assert!(explanation.contains("NodeLookup"));

    // Check that nested operators are indented correctly
    let lines: Vec<&str> = explanation.lines().collect();
    assert_eq!(lines.len(), 4, "Expected 4 lines for header + 3 nested ops");
    assert!(lines[1].starts_with("Filter"));
    assert!(lines[2].starts_with("  └─ IndexedTraversal"));
    assert!(lines[3].starts_with("    └─ NodeLookup"));
}

#[test]
fn test_explain_with_temporal_context() {
    let plan = PhysicalPlan {
        root: PhysicalOp::TemporalNodeLookup {
            node_ids: vec![NodeId::new(1).unwrap()],
            valid_time: 1000.into(),
            transaction_time: 2000.into(),
            use_batch: false,
        },
        estimated_cost: Cost {
            cpu: 50.0,
            io: 10.0,
            memory: 500,
            network: 0.0,
        },
        temporal_context: Some(TemporalContext::as_of(1000.into(), 2000.into())),
        parallel: false,
        include_provenance: false,
    };

    let explanation = plan.explain();

    // Should mention temporal context
    assert!(explanation.contains("Temporal") || explanation.contains("temporal"));
    assert!(explanation.contains("TemporalNodeLookup"));
}

#[test]
fn test_explain_binary_operations() {
    let plan = PhysicalPlan {
        root: PhysicalOp::HashJoin {
            left: Box::new(PhysicalOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: 100,
            }),
            right: Box::new(PhysicalOp::NodeScan {
                label: Some("Company".to_string()),
                estimated_rows: 50,
            }),
            left_key: "id".to_string(),
            right_key: "person_id".to_string(),
        },
        estimated_cost: Cost {
            cpu: 100.0,
            io: 20.0,
            memory: 5000,
            network: 0.0,
        },
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };

    let explanation = plan.explain();

    // Should show both sides of the join
    assert!(explanation.contains("HashJoin"));
    assert!(explanation.contains("Person"));
    assert!(explanation.contains("Company"));
}

#[test]
fn test_explain_includes_cost_breakdown() {
    let plan = PhysicalPlan {
        root: PhysicalOp::HnswSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            label_filter: Some("Document".to_string()),
            property_key: None,
        },
        estimated_cost: Cost {
            cpu: 15.5,
            io: 2.0,
            memory: 1024,
            network: 0.0,
        },
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };

    let explanation = plan.explain();

    // Check for exact formatted cost strings
    assert!(explanation.contains("cpu=15.5"));
    assert!(explanation.contains("io=2.0"));
    assert!(explanation.contains("mem=1.0KB"));
}

// ==================== Property Key Explain Tests (Issue #411) ====================

#[test]
fn test_explain_hnsw_search_with_property_key() {
    let plan = PhysicalOp::HnswSearch {
        embedding: Arc::from([0.1f32; 4].as_slice()),
        k: 10,
        label_filter: None,
        property_key: Some("title_embedding".to_string()),
    };

    let explain = plan.explain();
    assert!(explain.contains("HnswSearch"));
    assert!(explain.contains("k: 10"));
    assert!(explain.contains("prop: title_embedding"));
}

#[test]
fn test_explain_hnsw_search_without_property_key() {
    let plan = PhysicalOp::HnswSearch {
        embedding: Arc::from([0.1f32; 4].as_slice()),
        k: 10,
        label_filter: None,
        property_key: None,
    };

    let explain = plan.explain();
    assert!(explain.contains("HnswSearch"));
    assert!(explain.contains("k: 10"));
    // Should not show property key when None
    assert!(!explain.contains("prop:"));
}

#[test]
fn test_explain_temporal_vector_search_with_property_key() {
    let plan = PhysicalOp::TemporalVectorSearch {
        embedding: Arc::from([0.1f32; 4].as_slice()),
        k: 10,
        timestamp: 42000.into(),
        property_key: Some("content_embedding".to_string()),
    };

    let explain = plan.explain();
    assert!(explain.contains("TemporalVectorSearch"));
    assert!(explain.contains("k: 10"));
    assert!(explain.contains("ts: 42000"));
    assert!(explain.contains("prop: content_embedding"));
}

#[test]
fn test_explain_temporal_vector_search_without_property_key() {
    let plan = PhysicalOp::TemporalVectorSearch {
        embedding: Arc::from([0.1f32; 4].as_slice()),
        k: 10,
        timestamp: 42000.into(),
        property_key: None,
    };

    let explain = plan.explain();
    assert!(explain.contains("TemporalVectorSearch"));
    assert!(explain.contains("k: 10"));
    assert!(explain.contains("ts: 42000"));
    // Should not show property key when None
    assert!(!explain.contains("prop:"));
}

#[test]
fn test_explain_similar_to_node_with_property_key() {
    let plan = PhysicalOp::SimilarToNode {
        source_node: NodeId::new(42).unwrap(),
        property_key: "document_embedding".to_string(),
        k: 5,
        label_filter: Some("Document".to_string()),
    };

    let explain = plan.explain();
    assert!(explain.contains("SimilarToNode"));
    assert!(explain.contains("k: 5"));
    assert!(explain.contains("label: Some(\"Document\")"));
    assert!(explain.contains("prop: document_embedding"));
}

// ==================== EdgeScan Coverage Tests ====================

#[test]
fn test_edge_scan_name() {
    let op = PhysicalOp::EdgeScan {
        edge_type: Some("KNOWS".to_string()),
        estimated_rows: 500,
    };
    assert_eq!(op.name(), "EdgeScan");
}

#[test]
fn test_edge_scan_is_leaf() {
    let op = PhysicalOp::EdgeScan {
        edge_type: None,
        estimated_rows: 100,
    };
    assert!(op.is_leaf());
}

#[test]
fn test_edge_scan_depth() {
    let op = PhysicalOp::EdgeScan {
        edge_type: Some("FOLLOWS".to_string()),
        estimated_rows: 200,
    };
    assert_eq!(op.depth(), 1);
}

#[test]
fn test_edge_scan_explain_with_type() {
    let op = PhysicalOp::EdgeScan {
        edge_type: Some("KNOWS".to_string()),
        estimated_rows: 500,
    };
    let explain = op.explain();
    assert!(explain.contains("EdgeScan"));
    assert!(explain.contains("KNOWS"));
    assert!(explain.contains("500"));
}

#[test]
fn test_edge_scan_explain_without_type() {
    let op = PhysicalOp::EdgeScan {
        edge_type: None,
        estimated_rows: 1000,
    };
    let explain = op.explain();
    assert!(explain.contains("EdgeScan"));
    assert!(explain.contains("1000"));
}

#[test]
fn test_edge_scan_get_input_is_none() {
    let op = PhysicalOp::EdgeScan {
        edge_type: None,
        estimated_rows: 100,
    };
    assert!(op.get_input().is_none());
}
