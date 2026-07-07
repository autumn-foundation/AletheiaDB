use super::*;
use crate::query::ir::Predicate;
use crate::query::plan::ScanOp;
use crate::query::planner::stats::Statistics;

fn test_stats() -> Statistics {
    Statistics::default()
}

#[test]
fn test_fuses_eq_filter_on_labeled_scan() {
    let rule = FilterScanFusion;
    let stats = test_stats();

    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("name", "Alice")),
        LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("Person".to_string()),
            estimated_rows: None,
        }),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some(), "Should fuse Filter+NodeScan");

    let new_plan = result.unwrap();
    match &new_plan.root {
        LogicalOp::Scan(ScanOp::PropertyScan { label, key, .. }) => {
            assert_eq!(label, "Person");
            assert_eq!(key, "name");
        }
        other => panic!("Expected PropertyScan, got {:?}", other),
    }
}

#[test]
fn test_no_fusion_without_label() {
    let rule = FilterScanFusion;
    let stats = test_stats();

    // NodeScan without label - can't fuse
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("name", "Alice")),
        LogicalOp::Scan(ScanOp::NodeScan {
            label: None,
            estimated_rows: None,
        }),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none(), "Should not fuse without label");
}

#[test]
fn test_no_fusion_for_non_eq_filter() {
    let rule = FilterScanFusion;
    let stats = test_stats();

    // Gt filter - not an Eq, so no fusion
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::gt("age", 30i64)),
        LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("Person".to_string()),
            estimated_rows: None,
        }),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none(), "Should not fuse non-Eq filter");
}

#[test]
fn test_preserves_temporal_context() {
    use crate::query::plan::TemporalContext;

    let rule = FilterScanFusion;
    let stats = test_stats();

    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("name", "Alice")),
        LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("Person".to_string()),
            estimated_rows: None,
        }),
    ))
    .with_temporal_context(TemporalContext::default());

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some());
    assert!(result.unwrap().temporal_context.is_some());
}
