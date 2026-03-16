use super::*;
use crate::core::NodeId;
use crate::query::builder::QueryBuilder;
use crate::query::ir::{Direction, Predicate, TraversalDepth};
use crate::query::plan::QueryHints;

fn test_planner() -> QueryPlanner {
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::hnsw::HnswConfig;
    use crate::storage::CurrentStorage;

    // Create storage with vector index enabled for most tests
    let storage = Arc::new(CurrentStorage::new());
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    storage.enable_vector_index("embedding", config).unwrap();

    QueryPlanner::new(Arc::new(Statistics::default()), storage)
}

// ==================== Basic Planner Tests ====================

#[test]
fn test_planner_new() {
    use crate::storage::CurrentStorage;

    let stats = Arc::new(Statistics::default());
    let storage = Arc::new(CurrentStorage::new());
    let planner = QueryPlanner::new(Arc::clone(&stats), storage);
    // Verify the planner was created (no public fields to check)
    let _ = planner;
}

#[test]
fn test_planner_with_cost_model() {
    use crate::storage::CurrentStorage;

    let stats = Arc::new(Statistics::default());
    let storage = Arc::new(CurrentStorage::new());
    let custom_cost = CostModel::default();
    let planner = QueryPlanner::new(stats, storage).with_cost_model(custom_cost);
    let _ = planner;
}

#[test]
fn test_planner_with_rules() {
    use crate::storage::CurrentStorage;

    let stats = Arc::new(Statistics::default());
    let storage = Arc::new(CurrentStorage::new());
    let custom_rules: Vec<Box<dyn OptimizationRule>> = vec![];
    let planner = QueryPlanner::new(stats, storage).with_rules(custom_rules);
    let _ = planner;
}

#[test]
fn test_simple_node_lookup() {
    let planner = test_planner();
    let query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::NodeLookup { .. }));
}

#[test]
fn test_multiple_node_lookup() {
    let planner = test_planner();
    let ids = vec![
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        NodeId::new(3).unwrap(),
    ];
    let query = QueryBuilder::new().start_from(ids.clone()).build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::NodeLookup { node_ids } => {
            assert_eq!(node_ids.len(), 3);
        }
        _ => panic!("Expected NodeLookup"),
    }
}

// ==================== Node Scan Tests ====================

#[test]
fn test_node_scan_all() {
    let planner = test_planner();
    let query = QueryBuilder::new().scan(None).build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::NodeScan { label, .. } => {
            assert!(label.is_none());
        }
        _ => panic!("Expected NodeScan"),
    }
}

#[test]
fn test_node_scan_with_label() {
    let planner = test_planner();
    let query = QueryBuilder::new().scan_label("Person").build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::NodeScan { label, .. } => {
            assert_eq!(label.as_ref().unwrap(), "Person");
        }
        _ => panic!("Expected NodeScan"),
    }
}

// ==================== Traverse Tests ====================

#[test]
fn test_traverse_planning() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .traverse("KNOWS")
        .build();

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::IndexedTraversal { .. }));
}

#[test]
fn test_traverse_outgoing() {
    let planner = test_planner();
    // Use traverse() which defaults to outgoing
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .traverse("KNOWS")
        .build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::IndexedTraversal { direction, .. } => {
            assert_eq!(*direction, Direction::Outgoing);
        }
        _ => panic!("Expected IndexedTraversal"),
    }
}

#[test]
fn test_traverse_incoming() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .traverse_in("KNOWS")
        .build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::IndexedTraversal { direction, .. } => {
            assert_eq!(*direction, Direction::Incoming);
        }
        _ => panic!("Expected IndexedTraversal"),
    }
}

#[test]
fn test_traverse_both() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .traverse_both("KNOWS")
        .build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::IndexedTraversal { direction, .. } => {
            assert_eq!(*direction, Direction::Both);
        }
        _ => panic!("Expected IndexedTraversal"),
    }
}

#[test]
fn test_traverse_without_source_error() {
    let planner = test_planner();
    let query = Query {
        ops: vec![QueryOp::TraverseOut {
            label: Some("KNOWS".to_string()),
            depth: TraversalDepth::Exact(1),
        }],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    assert!(planner.plan(query).is_err());
}

// ==================== Filter Tests ====================

#[test]
fn test_filter_planning() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .filter(Predicate::eq("name", "Alice"))
        .build();

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::Filter { .. }));
}

#[test]
fn test_filter_without_source_error() {
    let planner = test_planner();
    let query = Query {
        ops: vec![QueryOp::Filter(Predicate::True)],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    assert!(planner.plan(query).is_err());
}

#[test]
fn test_filter_label_planning() {
    let planner = test_planner();
    let query = QueryBuilder::new().scan(None).with_label("Person").build();

    let plan = planner.plan(query).unwrap();
    // with_label gets converted to Filter with _label predicate
    assert!(matches!(plan.root, PhysicalOp::Filter { .. }));
}

// ==================== Limit/Skip Tests ====================

#[test]
fn test_limit_planning() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .limit(10)
        .build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::Limit { count, offset, .. } => {
            assert_eq!(*count, 10);
            assert_eq!(*offset, 0);
        }
        _ => panic!("Expected Limit"),
    }
}

#[test]
fn test_skip_planning() {
    let planner = test_planner();
    let query = QueryBuilder::new().scan(None).skip(5).build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::Limit { offset, .. } => {
            assert_eq!(*offset, 5);
        }
        _ => panic!("Expected Limit with offset (Skip)"),
    }
}

#[test]
fn test_limit_without_source_error() {
    let planner = test_planner();
    let query = Query {
        ops: vec![QueryOp::Limit(10)],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    assert!(planner.plan(query).is_err());
}

// ==================== Vector Search Tests ====================

#[test]
fn test_vector_search_planning() {
    let planner = test_planner();
    let embedding = [0.1f32; 4];
    let query = QueryBuilder::new().find_similar(&embedding, 10).build();

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::HnswSearch { .. }));
}

#[test]
fn test_vector_rerank_planning() {
    let planner = test_planner();
    let embedding = [0.1f32; 4];
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .rank_by_similarity(&embedding, 10)
        .build();

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::VectorRerank { .. }));
}

// ==================== Temporal Tests ====================

#[test]
fn test_temporal_planning() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .as_of(1000.into(), 2000.into())
        .start(NodeId::new(1).unwrap())
        .build();

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::TemporalNodeLookup { .. }));
    assert!(plan.temporal_context.is_some());
}

#[test]
fn test_temporal_vector_search() {
    let planner = test_planner();
    let embedding = [0.1f32; 4];
    let query = QueryBuilder::new()
        .as_of(1000.into(), 2000.into())
        .find_similar(&embedding, 10)
        .build();

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::TemporalVectorSearch { .. }));
}

// ==================== Aggregation Tests ====================

#[test]
fn test_count_planning() {
    let planner = test_planner();
    // Use raw Query since count() is not on QueryBuilder
    let query = Query {
        ops: vec![QueryOp::ScanNodes { label: None }, QueryOp::Count],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::Count { .. }));
}

#[test]
fn test_count_without_source_error() {
    let planner = test_planner();
    let query = Query {
        ops: vec![QueryOp::Count],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    assert!(planner.plan(query).is_err());
}

#[test]
fn test_distinct_planning() {
    let planner = test_planner();
    // Use raw Query since distinct() is not on QueryBuilder
    let query = Query {
        ops: vec![QueryOp::ScanNodes { label: None }, QueryOp::Distinct],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::Distinct { .. }));
}

#[test]
fn test_project_planning() {
    let planner = test_planner();
    // Use raw Query since project() is not on QueryBuilder
    let query = Query {
        ops: vec![
            QueryOp::StartNode(NodeId::new(1).unwrap()),
            QueryOp::Project(vec!["name".to_string(), "age".to_string()]),
        ],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::Project { properties, .. } => {
            assert_eq!(properties.len(), 2);
            assert!(properties.contains(&"name".to_string()));
            assert!(properties.contains(&"age".to_string()));
        }
        _ => panic!("Expected Project"),
    }
}

// ==================== Hybrid Query Tests ====================

#[test]
fn test_hybrid_planning() {
    let planner = test_planner();
    let embedding = [0.1f32; 4];
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .traverse("KNOWS")
        .rank_by_similarity(&embedding, 10)
        .build();

    let plan = planner.plan(query).unwrap();
    // Should be VectorRerank(IndexedTraversal(NodeLookup))
    assert!(matches!(plan.root, PhysicalOp::VectorRerank { .. }));
}

#[test]
fn test_complex_query_chain() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .scan_label("Person")
        .filter(Predicate::gt("age", 21i64))
        .limit(100)
        .build();

    let plan = planner.plan(query).unwrap();
    // Should be Limit(Filter(NodeScan))
    assert!(matches!(plan.root, PhysicalOp::Limit { .. }));
}

// ==================== Error Cases ====================

#[test]
fn test_empty_query_error() {
    let planner = test_planner();
    let query = Query {
        ops: vec![],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    assert!(planner.plan(query).is_err());
}

#[test]
fn test_rank_without_source_error() {
    let planner = test_planner();
    let embedding = [0.1f32; 4];
    let query = Query {
        ops: vec![QueryOp::RankBySimilarity {
            embedding: Arc::from(embedding.as_slice()),
            top_k: Some(10),
            property_key: None,
        }],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    assert!(planner.plan(query).is_err());
}

#[test]
fn test_distinct_without_source_error() {
    let planner = test_planner();
    let query = Query {
        ops: vec![QueryOp::Distinct],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    assert!(planner.plan(query).is_err());
}

#[test]
fn test_project_without_source_error() {
    let planner = test_planner();
    let query = Query {
        ops: vec![QueryOp::Project(vec!["name".to_string()])],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    assert!(planner.plan(query).is_err());
}

// ==================== Plan Properties Tests ====================

#[test]
fn test_plan_has_estimated_cost() {
    let planner = test_planner();
    let query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();

    let plan = planner.plan(query).unwrap();
    // Cost should be non-zero
    assert!(
        plan.estimated_cost.cpu > 0.0
            || plan.estimated_cost.io > 0.0
            || plan.estimated_cost.memory > 0
    );
}

#[test]
fn test_plan_parallel_hint() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .parallel()
        .build();

    let plan = planner.plan(query).unwrap();
    assert!(plan.parallel);
}

#[test]
fn test_plan_default_not_parallel() {
    let planner = test_planner();
    let query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();

    let plan = planner.plan(query).unwrap();
    assert!(!plan.parallel);
}

// ==================== Additional Operation Tests ====================

#[test]
fn test_traverse_in_direction() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .traverse_in("KNOWS")
        .build();

    let plan = planner.plan(query).unwrap();
    // Should be IndexedTraversal with Incoming direction
    if let PhysicalOp::IndexedTraversal { direction, .. } = plan.root {
        assert_eq!(direction, crate::query::ir::Direction::Incoming);
    } else {
        panic!("Expected IndexedTraversal");
    }
}

#[test]
fn test_traverse_both_directions() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .traverse_both("KNOWS")
        .build();

    let plan = planner.plan(query).unwrap();
    // Should be IndexedTraversal with Both direction
    if let PhysicalOp::IndexedTraversal { direction, .. } = plan.root {
        assert_eq!(direction, crate::query::ir::Direction::Both);
    } else {
        panic!("Expected IndexedTraversal");
    }
}

#[test]
fn test_filter_label_operation() {
    let planner = test_planner();
    let query = Query {
        ops: vec![
            QueryOp::ScanNodes {
                label: Some("Person".to_string()),
            },
            QueryOp::FilterLabel("Admin".to_string()),
        ],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let plan = planner.plan(query).unwrap();
    // Should be Filter(NodeScan)
    assert!(matches!(plan.root, PhysicalOp::Filter { .. }));
}

#[test]
fn test_skip_operation() {
    let planner = test_planner();
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .skip(10)
        .build();

    let plan = planner.plan(query).unwrap();
    // Skip is converted to Limit with offset
    if let PhysicalOp::Limit { offset, .. } = plan.root {
        assert_eq!(offset, 10);
    } else {
        panic!("Expected Limit with offset");
    }
}

#[test]
fn test_count_operation() {
    let planner = test_planner();
    let query = Query {
        ops: vec![
            QueryOp::ScanNodes {
                label: Some("Person".to_string()),
            },
            QueryOp::Count,
        ],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let plan = planner.plan(query).unwrap();
    // Should be Count(NodeScan)
    assert!(matches!(plan.root, PhysicalOp::Count { .. }));
}

#[test]
fn test_get_edges_requires_source() {
    let planner = test_planner();
    let query = Query {
        ops: vec![QueryOp::GetEdges {
            direction: crate::query::ir::Direction::Outgoing,
        }],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let result = planner.plan(query);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("requires a source"));
}

#[test]
fn test_temporal_as_of_without_source() {
    let planner = test_planner();
    let now = crate::core::temporal::time::now();
    let query = Query {
        ops: vec![QueryOp::AsOf {
            valid_time: now,
            transaction_time: now,
        }],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let result = planner.plan(query);
    assert!(result.is_err());
}

#[test]
fn test_temporal_between_without_source() {
    let planner = test_planner();
    let now = crate::core::temporal::time::now();
    let query = Query {
        ops: vec![QueryOp::Between {
            time_range: crate::core::temporal::TimeRange::new(now, now).unwrap(),
        }],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let result = planner.plan(query);
    assert!(result.is_err());
}

#[test]
fn test_track_changes_without_source() {
    let planner = test_planner();
    let now = crate::core::temporal::time::now();
    let query = Query {
        ops: vec![QueryOp::TrackChanges {
            time_range: crate::core::temporal::TimeRange::new(now, now).unwrap(),
        }],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let result = planner.plan(query);
    assert!(result.is_err());
}

#[test]
fn test_temporal_node_lookup_with_context() {
    let planner = test_planner();
    let now = crate::core::temporal::time::now();
    let mut query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();

    // Add temporal context
    query.temporal_context = Some(TemporalContext::as_of(now, now));

    let plan = planner.plan(query).unwrap();
    // Should be TemporalNodeLookup instead of NodeLookup
    assert!(matches!(plan.root, PhysicalOp::TemporalNodeLookup { .. }));
}

#[test]
fn test_temporal_vector_search_with_context() {
    let planner = test_planner();
    let embedding = [0.1f32; 4];
    let now = crate::core::temporal::time::now();

    let mut query = Query {
        ops: vec![QueryOp::VectorSearch {
            embedding: Arc::from(embedding.as_slice()),
            k: 10,
            metric: crate::index::vector::DistanceMetric::Cosine,
            property_key: None,
        }],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    // Add temporal context
    query.temporal_context = Some(TemporalContext::as_of(now, now));

    let plan = planner.plan(query).unwrap();
    // Should be TemporalVectorSearch instead of HnswSearch
    assert!(matches!(plan.root, PhysicalOp::TemporalVectorSearch { .. }));
}

#[test]
fn test_filter_label_without_source_error() {
    let planner = test_planner();
    let query = Query {
        ops: vec![QueryOp::FilterLabel("Person".to_string())],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let result = planner.plan(query);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("requires a source"));
}

#[test]
fn test_skip_without_source_error() {
    let planner = test_planner();
    let query = Query {
        ops: vec![QueryOp::Skip(10)],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let result = planner.plan(query);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("requires a source"));
}

// ==================== SimilarTo Tests ====================

#[test]
fn test_similar_to_planning() {
    let planner = test_planner();
    let source_node = NodeId::new(1).unwrap();
    let query = QueryBuilder::new()
        .start(source_node)
        .similar_to(source_node, 10)
        .build();

    let plan = planner.plan(query).unwrap();
    assert!(matches!(plan.root, PhysicalOp::SimilarToNode { .. }));
}

#[test]
fn test_similar_to_node_parameters() {
    let planner = test_planner();
    let source_node = NodeId::new(42).unwrap();
    let k = 15;
    let query = Query {
        ops: vec![QueryOp::SimilarTo {
            source_node,
            k,
            property_key: None,
            label_filter: None,
        }],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::SimilarToNode {
            source_node: sn,
            k: result_k,
            ..
        } => {
            assert_eq!(*sn, source_node);
            assert_eq!(*result_k, k);
        }
        _ => panic!("Expected SimilarToNode, got {:?}", plan.root.name()),
    }
}

#[test]
fn test_similar_to_with_property_key() {
    let planner = test_planner();
    let source_node = NodeId::new(1).unwrap();
    let query = Query {
        ops: vec![QueryOp::SimilarTo {
            source_node,
            k: 10,
            property_key: None,
            label_filter: None,
        }],
        temporal_context: None,
        hints: QueryHints::default(),
    };

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::SimilarToNode { property_key, .. } => {
            // Default property key should be "embedding"
            assert_eq!(property_key, "embedding");
        }
        _ => panic!("Expected SimilarToNode"),
    }
}

// ==================== Index Validation Tests (Issue #309) ====================

#[test]
fn test_vector_search_without_index_error() {
    use crate::storage::CurrentStorage;

    // Create planner with storage (no vector index enabled)
    let storage = Arc::new(CurrentStorage::new());
    let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

    let embedding = [0.1f32; 4];
    let query = QueryBuilder::new().find_similar(&embedding, 10).build();

    // Should fail during planning with IndexNotFound error
    let result = planner.plan(query);
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("index"));
    assert!(err_msg.contains("embedding"));
    assert!(
        err_msg.contains("vector_index(\"embedding\").hnsw"),
        "Error message should provide hint to enable index: {}",
        err_msg
    );
}

#[test]
fn test_vector_rerank_without_index_error() {
    use crate::storage::CurrentStorage;

    let storage = Arc::new(CurrentStorage::new());
    let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

    let embedding = [0.1f32; 4];
    let query = QueryBuilder::new()
        .start(NodeId::new(1).unwrap())
        .rank_by_similarity(&embedding, 10)
        .build();

    let result = planner.plan(query);
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = format!("{}", err).to_lowercase();
    assert!(err_msg.contains("vector"));
    assert!(err_msg.contains("index"));
    assert!(err_msg.contains("embedding"));
}

#[test]
fn test_similar_to_without_index_error() {
    use crate::storage::CurrentStorage;

    let storage = Arc::new(CurrentStorage::new());
    let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

    let source_node = NodeId::new(1).unwrap();
    let query = QueryBuilder::new()
        .start(source_node)
        .similar_to(source_node, 10)
        .build();

    let result = planner.plan(query);
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("index"));
}

#[test]
fn test_temporal_vector_search_without_index_error() {
    use crate::storage::CurrentStorage;

    let storage = Arc::new(CurrentStorage::new());
    let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

    let embedding = [0.1f32; 4];
    let query = QueryBuilder::new()
        .as_of(1000.into(), 2000.into())
        .find_similar(&embedding, 10)
        .build();

    let result = planner.plan(query);
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(matches!(err, crate::core::error::Error::Query(_)));
}

// ==================== Multi-Property Temporal Vector Search Tests (Issue #411) ====================

#[test]
fn test_scan_op_temporal_vector_search_with_property_key() {
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::hnsw::HnswConfig;
    use crate::storage::CurrentStorage;

    // Create planner with multi-property vector index
    let storage = Arc::new(CurrentStorage::new());
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    storage
        .enable_vector_index("embedding", config.clone())
        .unwrap();
    storage
        .enable_vector_index("title_embedding", config)
        .unwrap();
    let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

    // Create a logical plan with ScanOp::TemporalVectorSearch directly
    let embedding: Arc<[f32]> = Arc::from([0.1f32; 4].as_slice());
    let logical_plan = LogicalPlan::new(LogicalOp::Scan(ScanOp::TemporalVectorSearch {
        embedding,
        k: 10,
        timestamp: 1000.into(),
        property_key: Some("title_embedding".to_string()),
    }));

    let physical_plan = planner.to_physical_plan(&logical_plan).unwrap();
    match &physical_plan.root {
        PhysicalOp::TemporalVectorSearch { property_key, .. } => {
            assert_eq!(
                property_key.as_deref(),
                Some("title_embedding"),
                "property_key should be extracted from ScanOp::TemporalVectorSearch"
            );
        }
        _ => panic!(
            "Expected TemporalVectorSearch, got {:?}",
            physical_plan.root.name()
        ),
    }
}

#[test]
fn test_vector_search_with_temporal_context_preserves_property_key() {
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::hnsw::HnswConfig;
    use crate::storage::CurrentStorage;

    // This tests the existing path: VectorSearch + temporal_context -> TemporalVectorSearch
    let storage = Arc::new(CurrentStorage::new());
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    storage
        .enable_vector_index("embedding", config.clone())
        .unwrap();
    storage
        .enable_vector_index("title_embedding", config)
        .unwrap();
    let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

    let embedding = [0.1f32; 4];
    let query = QueryBuilder::new()
        .as_of(1000.into(), 2000.into())
        .find_similar_builder(&embedding, 10)
        .property("title_embedding")
        .finish()
        .build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::TemporalVectorSearch { property_key, .. } => {
            assert_eq!(
                property_key.as_deref(),
                Some("title_embedding"),
                "property_key should be preserved through VectorSearch->TemporalVectorSearch conversion"
            );
        }
        _ => panic!("Expected TemporalVectorSearch, got {:?}", plan.root.name()),
    }
}

#[test]
fn test_temporal_vector_search_default_property() {
    let planner = test_planner();
    let embedding = [0.1f32; 4];
    let query = QueryBuilder::new()
        .as_of(1000.into(), 2000.into())
        .find_similar(&embedding, 10)
        .build();

    let plan = planner.plan(query).unwrap();
    match &plan.root {
        PhysicalOp::TemporalVectorSearch { property_key, .. } => {
            assert_eq!(
                property_key, &None,
                "property_key should be None when using default property"
            );
        }
        _ => panic!("Expected TemporalVectorSearch, got {:?}", plan.root.name()),
    }
}

#[test]
fn test_temporal_vector_search_invalid_property_error() {
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::hnsw::HnswConfig;
    use crate::storage::CurrentStorage;

    // Create planner with only "embedding" property enabled
    let storage = Arc::new(CurrentStorage::new());
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    storage.enable_vector_index("embedding", config).unwrap();
    let planner = QueryPlanner::new(Arc::new(Statistics::default()), storage);

    // Try to use a non-existent property in temporal search
    let embedding: Arc<[f32]> = Arc::from([0.1f32; 4].as_slice());
    let logical_plan = LogicalPlan::new(LogicalOp::Scan(ScanOp::TemporalVectorSearch {
        embedding,
        k: 10,
        timestamp: 1000.into(),
        property_key: Some("nonexistent_property".to_string()),
    }));

    let result = planner.to_physical_plan(&logical_plan);
    assert!(result.is_err(), "Should reject invalid property name");

    let err = result.unwrap_err();
    match err {
        Error::Query(QueryError::IndexNotFound {
            index_type,
            property_name,
            ..
        }) => {
            assert_eq!(index_type, "vector");
            assert_eq!(property_name, "nonexistent_property");
        }
        _ => panic!("Expected IndexNotFound error, got {:?}", err),
    }
}
