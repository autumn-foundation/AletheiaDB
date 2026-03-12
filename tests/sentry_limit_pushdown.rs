use aletheiadb::core::NodeId;
use aletheiadb::query::ir::Predicate;
use aletheiadb::query::plan::{BinaryOp, LogicalOp, LogicalPlan, ScanOp, UnaryOp};
use aletheiadb::query::planner::rules::{LimitPushdown, OptimizationRule};
use aletheiadb::query::planner::stats::Statistics;

#[test]
fn test_sentry_limit_pushdown_returns_error_or_none() {
    let rule = LimitPushdown;
    let stats = Statistics::default();

    // Verify name
    assert_eq!(rule.name(), "limit-pushdown");

    // Empty plan
    let plan = LogicalPlan::new(LogicalOp::Empty);
    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none());

    // Binary ops logic branch tracking (&& vs || test)
    // Left: doesn't change
    let left = LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()]));

    // Right: Limit(VectorRank) -> limit propagates down and VectorRank changes
    let right = LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: vec![0.1].into(),
                top_k: None,
                property_key: None,
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

    // Swapped left and right
    let left2 = LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: vec![0.1].into(),
                top_k: None,
                property_key: None,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(2).unwrap()])),
        ),
    );
    let right2 = LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()]));
    let root2 = LogicalOp::binary(BinaryOp::Union, left2, right2);
    let plan2 = LogicalPlan::new(root2);

    let result2 = rule.apply(&plan2, &stats).unwrap();
    assert!(
        result2.is_some(),
        "Partial optimization on LEFT branch should trigger change"
    );
}

#[test]
fn test_sentry_limit_pushdown_unsupported_unary() {
    let rule = LimitPushdown;
    let stats = Statistics::default();

    // Not limit
    let limit_op = LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("a", 1)),
        LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
    );
    let plan = LogicalPlan::new(limit_op);

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_sentry_limit_pushdown_limit_merging() {
    let rule = LimitPushdown;
    let stats = Statistics::default();

    // Limit(10, Limit(5, Scan)) -> Limit(5, Scan)
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(10),
        LogicalOp::unary(
            UnaryOp::Limit(5),
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some());

    // Check if new limit is smaller
    let plan2 = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::Limit(10),
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));
    let result2 = rule.apply(&plan2, &stats).unwrap();
    assert!(result2.is_some());
}

#[test]
fn test_sentry_limit_pushdown_limit_pass_through() {
    let rule = LimitPushdown;
    let stats = Statistics::default();

    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: vec![0.1].into(),
                top_k: None,
                property_key: None,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some());

    // Let's test a case where Limit(5, VectorRank(10)) changes to Limit(5, VectorRank(5))
    let plan_2 = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::VectorRank {
                embedding: vec![0.1].into(),
                top_k: Some(10),
                property_key: None,
            },
            LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
        ),
    ));
    let result2 = rule.apply(&plan_2, &stats).unwrap();
    assert!(result2.is_some());
}

#[test]
fn test_sentry_limit_pushdown_limit_pass_through_project() {
    let rule = LimitPushdown;
    let stats = Statistics::default();

    // Limit(5, Filter, Project, VectorRank)
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Limit(5),
        LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("a", 1)),
            LogicalOp::unary(
                UnaryOp::Project(vec!["a".to_string()]),
                LogicalOp::unary(
                    UnaryOp::VectorRank {
                        embedding: vec![0.1].into(),
                        top_k: None,
                        property_key: None,
                    },
                    LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()])),
                ),
            ),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some());
}
