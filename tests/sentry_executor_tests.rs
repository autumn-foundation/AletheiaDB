use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::query::executor::QueryExecutor;
use aletheiadb::query::planner::physical::{PhysicalOp, PhysicalPlan};
use aletheiadb::storage::current::CurrentStorage;
use aletheiadb::storage::historical::HistoricalStorage;
use aletheiadb::index::vector::hnsw::HnswConfig;
use aletheiadb::index::vector::DistanceMetric;
use aletheiadb::core::version::AnchorConfig;
use std::sync::Arc;
use parking_lot::RwLock;

#[test]
fn test_similar_to_node_multi_index_bug() {
    // 🎯 Target: PhysicalOp::SimilarToNode execution logic
    // 💣 Risk: Incorrectly rejects valid queries in multi-index setup because it compares against the "default" index
    // 🧪 Strategy: Enable two vector indexes, query the one that is NOT the default (alphabetically second)

    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
        AnchorConfig::default(),
    )));

    // Enable "a_index" (will be default because 'a' < 'b')
    current
        .enable_vector_index("a_index", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Enable "b_index" (valid, but not default)
    current
        .enable_vector_index("b_index", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    let executor = QueryExecutor::new(current.clone(), historical);

    // Create a node with both vectors
    let props = PropertyMapBuilder::new()
        .insert_vector("a_index", &[1.0, 0.0, 0.0, 0.0])
        .insert_vector("b_index", &[0.0, 1.0, 0.0, 0.0])
        .build();

    let node_id = current.create_node("Node", props).unwrap();

    // Query using "b_index"
    // This should work, but currently fails because it checks against "a_index"
    let plan = PhysicalPlan {
        root: PhysicalOp::SimilarToNode {
            source_node: node_id,
            property_key: "b_index".to_string(),
            k: 10,
            label_filter: None,
        },
        estimated_cost: Default::default(),
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };

    let result = executor.execute(plan);

    if let Err(e) = &result {
        println!("Error: {}", e);
    }

    assert!(result.is_ok(), "Query on non-default index should succeed");
}
