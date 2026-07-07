use super::*;
use crate::core::NodeId;
use crate::query::plan::ScanOp;
use std::sync::Arc;

fn test_stats() -> Statistics {
    Statistics::default()
}

#[test]
fn test_combine_consecutive_limits() {
    let rule = LimitPushdown;
    let stats = test_stats();

    // Limit(5, Limit(10, Scan)) -> Limit(5, Scan)
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::Limit(10),
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some());

    let new_plan = result.unwrap();
    // Should be Limit(5, Scan) - no nested limit
    match &new_plan.root {
        LogicalOp::Unary {
            op: UnaryOp::Limit(n),
            input,
        } => {
            assert_eq!(*n, 5);
            assert!(matches!(input.as_ref(), LogicalOp::Scan(_)));
        }
        _ => panic!("Expected Limit"),
    }
}

#[test]
fn test_propagate_limit_to_vector_rank() {
    let rule = LimitPushdown;
    let stats = test_stats();

    // Limit(5, VectorRank(top_k=10, Scan)) -> Limit(5, VectorRank(top_k=5, Scan))
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
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
    assert!(result.is_some());

    let new_plan = result.unwrap();
    // VectorRank should have top_k=5 now
    match &new_plan.root {
        LogicalOp::Unary {
            op: UnaryOp::Limit(_),
            input,
        } => match input.as_ref() {
            LogicalOp::Unary {
                op: UnaryOp::VectorRank { top_k, .. },
                ..
            } => {
                assert_eq!(*top_k, Some(5));
            }
            _ => panic!("Expected VectorRank"),
        },
        _ => panic!("Expected Limit"),
    }
}

#[test]
fn test_no_change_for_simple_limit() {
    let rule = LimitPushdown;
    let stats = test_stats();

    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(10),
        LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none()); // No change needed
}

#[test]
fn test_propagate_limit_through_filter() {
    use crate::query::ir::{Predicate, PredicateValue};
    let rule = LimitPushdown;
    let stats = test_stats();
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq(
                "name".to_string(),
                PredicateValue::String("Alice".to_string()),
            )),
            LogicalOp::unary(
                UnaryOp::Limit(10),
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none()); // We shouldn't push the limit down through a filter
}

#[test]
fn test_propagate_limit_through_project() {
    let rule = LimitPushdown;
    let stats = test_stats();
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::Project(vec!["name".to_string()]),
            LogicalOp::unary(
                UnaryOp::Limit(10),
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some());
}

#[test]
fn test_binary_op_limit_pushdown() {
    let rule = LimitPushdown;
    let stats = test_stats();
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::binary(
            crate::query::plan::BinaryOp::Union,
            LogicalOp::unary(
                UnaryOp::Limit(10),
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
            LogicalOp::unary(
                UnaryOp::Limit(15),
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(2).unwrap()])),
            ),
        ),
    ));
    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_binary_op_limit_pushdown_children() {
    let rule = LimitPushdown;
    let stats = test_stats();
    let plan = LogicalPlan::new(LogicalOp::binary(
        crate::query::plan::BinaryOp::Union,
        LogicalOp::unary(
            UnaryOp::Limit(10),
            LogicalOp::unary(
                UnaryOp::Limit(20),
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ),
        LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(2).unwrap()])),
    ));
    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some());
}

#[test]
fn test_propagate_limit_to_vector_rank_equal_limit() {
    let rule = LimitPushdown;
    let stats = test_stats();
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(10),
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
    assert!(result.is_none());
}

#[test]
fn test_propagate_limit_through_sort() {
    let rule = LimitPushdown;
    let stats = test_stats();
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::Sort {
                key: crate::query::plan::SortKey::Property("age".into()),
                descending: true,
            },
            LogicalOp::unary(
                UnaryOp::Limit(10),
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none()); // Sort requires all elements to sort them before limiting
}
