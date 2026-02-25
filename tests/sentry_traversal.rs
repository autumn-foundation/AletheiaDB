//! Sentry traversal tests fixed for variable depth API.
//!
//! This file replaces the previous version which used the deprecated `depth` field.

use aletheiadb::query::planner::physical::PhysicalOp;
use aletheiadb::query::ir::Direction;
use aletheiadb::core::id::NodeId;

#[test]
fn test_indexed_traversal_construction() {
    // Verify that IndexedTraversal can be constructed with min/max depth
    let op = PhysicalOp::IndexedTraversal {
        input: Box::new(PhysicalOp::NodeLookup { node_ids: vec![NodeId::new(1).unwrap()] }),
        direction: Direction::Outgoing,
        label: Some("KNOWS".to_string()),
        min_depth: 1,
        max_depth: 2,
        temporal_context: None,
    };

    if let PhysicalOp::IndexedTraversal { min_depth, max_depth, .. } = op {
        assert_eq!(min_depth, 1);
        assert_eq!(max_depth, 2);
    } else {
        panic!("Construction failed");
    }
}

#[test]
fn test_indexed_traversal_fixed_depth() {
    // Verify construction for fixed depth (min == max)
    let op = PhysicalOp::IndexedTraversal {
        input: Box::new(PhysicalOp::Empty),
        direction: Direction::Incoming,
        label: None,
        min_depth: 3,
        max_depth: 3,
        temporal_context: None,
    };

    if let PhysicalOp::IndexedTraversal { min_depth, max_depth, .. } = op {
        assert_eq!(min_depth, 3);
        assert_eq!(max_depth, 3);
    }
}
