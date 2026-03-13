import re
with open("src/query/planner/rules/predicate_pushdown.rs", "r") as f:
    content = f.read()

test_code = """
    #[test]
    fn test_name_and_apply_passthrough() {
        let rule = PredicatePushdown;
        assert_eq!(rule.name(), "predicate-pushdown");

        let plan = LogicalPlan::new(LogicalOp::Empty);
        let stats = Statistics::default();
        let result = rule.apply(&plan, &stats).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_apply_returns_some_on_change() {
        let rule = PredicatePushdown;
        let stats = Statistics::default();

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
        assert!(result.is_some());
    }

    #[test]
    fn test_push_down_no_change_returns_false() {
        let rule = PredicatePushdown;
        let op = LogicalOp::Scan(ScanOp::NodeLookup(vec![NodeId::new(1).unwrap()]));
        let (new_op, changed) = rule.push_down(&op).unwrap();
        assert!(!changed);
        assert_eq!(new_op, op);
    }
"""

content = content.replace("    #[test]\n    fn test_no_change_on_simple_filter() {", test_code + "\n    #[test]\n    fn test_no_change_on_simple_filter() {")

with open("src/query/planner/rules/predicate_pushdown.rs", "w") as f:
    f.write(content)
