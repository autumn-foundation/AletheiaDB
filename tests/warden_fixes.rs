use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::query::executor::QueryExecutor;
use aletheiadb::query::planner::PhysicalPlan;
use aletheiadb::query::planner::physical::PhysicalOp;
use aletheiadb::storage::current::CurrentStorage;
use aletheiadb::storage::historical::HistoricalStorage;
use parking_lot::RwLock;
use std::sync::Arc;

#[test]
fn test_schema_leak_protection() {
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));

    // Enable vector index on "secret_property"
    current
        .enable_vector_index(
            "secret_property",
            HnswConfig::new(3, DistanceMetric::Cosine),
        )
        .unwrap();

    // Create a node to search from
    let props = PropertyMapBuilder::new()
        .insert_vector("secret_property", &[1.0, 0.0, 0.0])
        .build();
    let node = current.create_node("Node", props).unwrap();

    let executor = QueryExecutor::new(current, historical);

    // Create a plan that queries WRONG property
    let plan = PhysicalPlan {
        root: PhysicalOp::SimilarToNode {
            source_node: node,
            property_key: "public_property".to_string(), // Wrong property
            k: 5,
            label_filter: None,
        },
        estimated_cost: Default::default(),
        temporal_context: None,
        parallel: false,
        include_provenance: false,
    };

    let result = executor.execute(plan);

    if let Err(e) = result {
        let err_msg = e.to_string();
        // Correct behavior: Error message should NOT contain "secret_property".
        assert!(
            !err_msg.contains("secret_property"),
            "Error message leaked the indexed property name: {}",
            err_msg
        );
    } else {
        panic!("Expected execution error due to property mismatch");
    }
}
