    #[test]
    fn test_filter_scan_fusion_name() {
        let rule = FilterScanFusion;
        assert_eq!(rule.name(), "filter-scan-fusion");
    }

    #[test]
    fn test_apply_returns_none_if_no_change() {
        let rule = FilterScanFusion;
        let stats = test_stats();
        let plan = LogicalPlan::new(LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("Person".to_string()),
            estimated_rows: None,
        }));
        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_fuse_changed_boolean_logic() {
        let rule = FilterScanFusion;
        let stats = test_stats();

        let left = LogicalOp::unary(
            UnaryOp::Filter(Predicate::gt("age", 30i64)),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        );
        let right = LogicalOp::unary(
            UnaryOp::Filter(Predicate::gt("age", 40i64)),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        );
        let plan = LogicalPlan::new(LogicalOp::binary(
            crate::query::plan::BinaryOp::Union,
            left,
            right,
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none(), "Should not fuse either side and return none");
    }

    #[test]
    fn test_fuse_changed_boolean_logic_changed_right() {
        let rule = FilterScanFusion;
        let stats = test_stats();

        let left = LogicalOp::unary(
            UnaryOp::Filter(Predicate::gt("age", 30i64)),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        );
        let right = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        );
        let plan = LogicalPlan::new(LogicalOp::binary(
            crate::query::plan::BinaryOp::Union,
            left,
            right,
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some(), "Should fuse right side and return some");
    }

    #[test]
    fn test_fuse_recursively_handles_binary_ops() {
        let rule = FilterScanFusion;
        let stats = test_stats();

        // Left: can be fused
        let left = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        );

        // Right: cannot be fused
        let right = LogicalOp::unary(
            UnaryOp::Filter(Predicate::gt("age", 30i64)),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        );

        // Binary op combining them
        let plan = LogicalPlan::new(LogicalOp::binary(
            crate::query::plan::BinaryOp::Union,
            left,
            right,
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some(), "Should fuse left side and return some");

        let new_plan = result.unwrap();
        if let LogicalOp::Binary { op: _, left, right } = new_plan.root {
            // Left should be PropertyScan
            assert!(matches!(*left, LogicalOp::Scan(ScanOp::PropertyScan { .. })));

            // Right should be Filter over NodeScan
            assert!(matches!(*right, LogicalOp::Unary { op: UnaryOp::Filter(_), .. }));
        } else {
            panic!("Expected BinaryOp at root");
        }
    }

    #[test]
    fn test_pseudo_key_not_fused() {
        let rule = FilterScanFusion;
        let stats = test_stats();

        // NodeScan with label but key starts with _
        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("_label", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none(), "Should not fuse pseudo keys");
    }

    #[test]
    fn test_fuses_unary_op_recursively() {
        let rule = FilterScanFusion;
        let stats = test_stats();

        let inner = LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("name", "Alice")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: None,
            }),
        );

        let plan = LogicalPlan::new(LogicalOp::unary(
            UnaryOp::Limit(10),
            inner
        ));

        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_some(), "Should fuse nested");

        let new_plan = result.unwrap();
        if let LogicalOp::Unary { op: UnaryOp::Limit(10), input } = new_plan.root {
            assert!(matches!(*input, LogicalOp::Scan(ScanOp::PropertyScan { .. })));
        } else {
            panic!("Expected Limit at root");
        }
    }
