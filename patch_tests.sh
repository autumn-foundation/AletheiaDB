#!/bin/bash
cat << 'INNER_EOF' >> src/query/planner/rules/operation_reordering.rs

    #[test]
    fn test_operation_reordering_name() {
        let rule = OperationReordering;
        assert_eq!(rule.name(), "operation-reordering");
    }

    #[test]
    fn test_reorder_empty_plan() {
        let rule = OperationReordering;
        let stats = test_stats();

        let plan = LogicalPlan::new(LogicalOp::Empty);
        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_reorder_unary_non_filter_no_change() {
        let rule = OperationReordering;
        let stats = test_stats();

        let plan = LogicalPlan::new(LogicalOp::Unary {
            op: UnaryOp::Limit(10),
            input: Box::new(LogicalOp::Scan(ScanOp::NodeScan {
                label: None,
                estimated_rows: Some(100),
            })),
        });

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_reorder_unary_non_filter_with_inner_change() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Setup an inner reorderable filter chain inside a Limit
        let plan = LogicalPlan::new(LogicalOp::Unary {
            op: UnaryOp::Limit(10),
            input: Box::new(LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("common_property", "value")),
                LogicalOp::unary(
                    UnaryOp::Filter(Predicate::eq("rare_property", "value")),
                    LogicalOp::Scan(ScanOp::NodeScan {
                        label: None,
                        estimated_rows: Some(1000),
                    }),
                ),
            )),
        });

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some());

        // Assert outer is still Limit
        let opt_plan = result.unwrap();
        match &opt_plan.root {
            LogicalOp::Unary { op: UnaryOp::Limit(10), input } => {
                // Inner should be reordered: rare then common
                match input.as_ref() {
                    LogicalOp::Unary { op: UnaryOp::Filter(Predicate::Eq { key, .. }), .. } => {
                        assert_eq!(key, "rare_property");
                    },
                    _ => panic!("Expected inner Filter")
                }
            },
            _ => panic!("Expected outer Limit")
        }
    }

    #[test]
    fn test_reorder_binary_non_join() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Using Union to test non-Join binary op
        let plan = LogicalPlan::new(LogicalOp::Binary {
            op: BinaryOp::Union,
            left: Box::new(LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("common_property", "value")),
                LogicalOp::unary(
                    UnaryOp::Filter(Predicate::eq("rare_property", "value")),
                    LogicalOp::Scan(ScanOp::NodeScan { label: None, estimated_rows: Some(1000) }),
                )
            )),
            right: Box::new(LogicalOp::Empty),
        });

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some()); // Because the left side should be optimized
    }

    #[test]
    fn test_reorder_filters_stable_sort() {
        let rule = OperationReordering;
        let stats = test_stats();

        // Two filters with exactly same selectivity (e.g. unknown property falls back to default)
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("unknown_1", "value")),
            LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("unknown_2", "value")),
                LogicalOp::Scan(ScanOp::NodeScan { label: None, estimated_rows: Some(1000) })
            )
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        // Since they have the same selectivity, order should not change
        assert!(result.is_none());
    }

    #[test]
    fn test_predicates_not_equal_structural() {
        let rule = OperationReordering;

        // AND vs OR with same children
        let p1 = Predicate::And(vec![Predicate::eq("x", 1i64)]);
        let p2 = Predicate::Or(vec![Predicate::eq("x", 1i64)]);
        assert!(!rule.predicates_equal(&p1, &p2));

        // Different nested types
        let p3 = Predicate::Not(Box::new(Predicate::True));
        let p4 = Predicate::Not(Box::new(Predicate::False));
        assert!(!rule.predicates_equal(&p3, &p4));
    }
}
INNER_EOF
