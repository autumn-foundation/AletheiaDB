use super::*;
use crate::core::NodeId;
use crate::query::ir::Predicate;
use crate::query::plan::{ScanOp, SortKey};
use std::sync::Arc;

fn test_stats() -> Statistics {
    Statistics::default()
}

#[test]
fn test_no_change_on_simple_filter() {
    let rule = PredicatePushdown;
    let stats = test_stats();

    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("name", "Alice")),
        LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none()); // No change needed
}

#[test]
fn test_push_filter_below_vector_rank_no_limit() {
    let rule = PredicatePushdown;
    let stats = test_stats();

    // Filter(VectorRank(top_k=None, Scan)) -> VectorRank(Filter(Scan))
    // Should push down because no limit
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("name", "Alice")),
        LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                top_k: None,
                property_key: None,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();

    let expected_plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::VectorRank {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            top_k: None,
            property_key: None,
        },
        LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));

    assert_eq!(result, Some(expected_plan));
}

#[test]
fn test_stop_filter_at_traverse() {
    let rule = PredicatePushdown;
    let stats = test_stats();

    // Filter(Traverse(Scan))
    // Should NOT push down because we stop at traversals
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("name", "Alice")),
        LogicalOp::unary(
            UnaryOp::Traverse {
                label: None,
                direction: crate::query::ir::Direction::Outgoing,
                depth: crate::query::ir::TraversalDepth::Exact(1),
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    // Should return None because pushdown was blocked
    assert!(result.is_none());
}

#[test]
fn test_stop_filter_at_scan() {
    let rule = PredicatePushdown;
    let stats = test_stats();

    // Filter(Scan)
    // Should NOT push down because we stop at scans
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("name", "Alice")),
        LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    // Should return None because pushdown was blocked by Scan
    assert!(result.is_none());
}

#[test]
fn test_stop_filter_at_vector_rank_with_limit() {
    let rule = PredicatePushdown;
    let stats = test_stats();

    // Filter(VectorRank(top_k=Some(10), Scan))
    // Should NOT push down because limit exists
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("name", "Alice")),
        LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                top_k: Some(10),
                property_key: None,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    // Should return None because pushdown was blocked
    assert!(result.is_none());
}

#[test]
fn test_push_filter_below_sort() {
    let rule = PredicatePushdown;
    let stats = test_stats();

    // Filter(Sort(Scan)) -> Sort(Filter(Scan))
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("active", true)),
        LogicalOp::unary(
            UnaryOp::Sort {
                key: SortKey::Property("created".to_string()),
                descending: true,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();

    let expected_plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Sort {
            key: SortKey::Property("created".to_string()),
            descending: true,
        },
        LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("active", true)),
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));

    assert_eq!(result, Some(expected_plan));
}

#[test]
fn test_multi_level_pushdown() {
    let rule = PredicatePushdown;
    let stats = test_stats();

    // Filter -> Sort -> VectorRank(no limit) -> Scan
    // Should become: Sort -> VectorRank -> Filter -> Scan
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("active", true)),
        LogicalOp::unary(
            UnaryOp::Sort {
                key: SortKey::Property("created".to_string()),
                descending: true,
            },
            LogicalOp::unary(
                UnaryOp::VectorRank {
                    embedding: Arc::from([0.1f32; 4].as_slice()),
                    top_k: None,
                    property_key: None,
                },
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ),
    ));

    // Simulate planner loop to get full pushdown
    let mut current_plan = plan;
    let mut changed = true;
    let mut iterations = 0;

    while changed && iterations < 10 {
        let result = rule.apply(&current_plan, &stats).unwrap();
        if let Some(new_plan) = result {
            current_plan = new_plan;
            changed = true;
        } else {
            changed = false;
        }
        iterations += 1;
    }

    // Now verify full pushdown: Sort -> VectorRank -> Filter -> Scan
    let expected_plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Sort {
            key: SortKey::Property("created".to_string()),
            descending: true,
        },
        LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: Arc::from([0.1f32; 4].as_slice()),
                top_k: None,
                property_key: None,
            },
            LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("active", true)),
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ),
    ));

    assert_eq!(current_plan, expected_plan);
}

#[test]
fn test_binary_op_recursion_logic() {
    use crate::query::plan::BinaryOp;

    let rule = PredicatePushdown;
    let stats = test_stats();

    // Binary(Union, Filter(Sort(Scan)), Scan)
    // Left side: Filter(Sort(Scan)) -> Sort(Filter(Scan)) (Optimized, changed=true)
    // Right side: Scan -> Scan (No change, changed=false)
    // Total change: true || false = true

    let left_op = LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("active", true)),
        LogicalOp::unary(
            UnaryOp::Sort {
                key: SortKey::Property("created".to_string()),
                descending: true,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    );

    let right_op = LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(2).unwrap()]));

    let plan = LogicalPlan::new(LogicalOp::binary(BinaryOp::Union, left_op, right_op));

    // Expect optimization to occur on the left branch
    let result = rule.apply(&plan, &stats).unwrap();

    let expected_plan = LogicalPlan::new(LogicalOp::binary(
        BinaryOp::Union,
        LogicalOp::unary(
            UnaryOp::Sort {
                key: SortKey::Property("created".to_string()),
                descending: true,
            },
            LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("active", true)),
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ),
        LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(2).unwrap()])),
    ));

    assert_eq!(
        result,
        Some(expected_plan),
        "Binary op with one changed branch should return Some"
    );
}
