use aletheiadb::core::NodeId;
use aletheiadb::query::ir::Predicate;
use aletheiadb::query::plan::{BinaryOp, LogicalOp, LogicalPlan, ScanOp, SortKey, UnaryOp};
use aletheiadb::query::planner::rules::{OptimizationRule, PredicatePushdown};
use aletheiadb::query::planner::stats::Statistics;

#[test]
fn test_sentry_predicate_pushdown_returns_error_or_none() {
    let rule = PredicatePushdown;
    let stats = Statistics::default();

    // Verify name
    assert_eq!(rule.name(), "predicate-pushdown");

    // Empty plan
    let plan = LogicalPlan::new(LogicalOp::Empty);
    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none());

    // Left vs Right branch && vs || mutation in Binary ops
    // We want to verify `left_changed || right_changed` logic.
    // Let's create a Binary op where left doesn't change, but right DOES change.
    // If it was mutated to &&, the result would be no change overall.
    let left = LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()]));

    // Right: Filter(Sort(Scan)) -> Should change to Sort(Filter(Scan))
    let right = LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("a", 1)),
        LogicalOp::unary(
            UnaryOp::Sort {
                key: SortKey::Property("a".to_string()),
                descending: true,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(2).unwrap()])),
        ),
    );

    let root = LogicalOp::binary(BinaryOp::Union, left, right);
    let plan = LogicalPlan::new(root);

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(
        result.is_some(),
        "Partial optimization on RIGHT branch should trigger change"
    );
}

#[test]
fn test_sentry_predicate_pushdown_returns_error_or_none_for_both_branches() {
    let rule = PredicatePushdown;
    let stats = Statistics::default();

    // Binary Op: Union(Right, Left) (swapped to test both logic branches)
    let left = LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()]));

    // Right: Filter(Sort(Scan)) -> Should change to Sort(Filter(Scan))
    let right = LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("a", 1)),
        LogicalOp::unary(
            UnaryOp::Sort {
                key: SortKey::Property("a".to_string()),
                descending: true,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(2).unwrap()])),
        ),
    );

    let root = LogicalOp::binary(BinaryOp::Union, right, left);
    let plan = LogicalPlan::new(root);

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(
        result.is_some(),
        "Partial optimization on LEFT branch should trigger change"
    );
}

#[test]
fn test_sentry_predicate_pushdown_unsupported_unary() {
    let rule = PredicatePushdown;
    let stats = Statistics::default();

    // Test a generic unary operation (like Limit) that shouldn't let filter pass,
    // to test the `_ =>` arm in push_down's match over `optimized_input`.
    let limit_op = LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
    );
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("a", 1)),
        limit_op,
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_sentry_predicate_pushdown_non_filter_unary() {
    let rule = PredicatePushdown;
    let stats = Statistics::default();

    // Test a generic unary operation pushing down a non-filter (which passes through).
    // Specifically, Limit(Sort(Scan)). Wait, Sort is unary, Limit is unary.
    // If we have Limit(Filter(Sort(Scan))), the inner Filter(Sort) optimizes,
    // and Limit just passes through that change.

    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("a", 1)),
            LogicalOp::unary(
                UnaryOp::Sort {
                    key: SortKey::Property("a".to_string()),
                    descending: true,
                },
                LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
            ),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some());
}
