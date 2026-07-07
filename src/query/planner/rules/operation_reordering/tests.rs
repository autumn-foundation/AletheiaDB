use super::*;
use crate::core::NodeId;
use crate::query::ir::Predicate;
use crate::query::plan::{BinaryOp, ScanOp, UnaryOp};

fn test_stats() -> Statistics {
    let stats = Statistics::default();
    // Set up some statistics for testing
    stats.refresh(1000, 5000, 100, vec![], 5.0);

    // Set up property statistics for selectivity estimation
    stats.update_property_stats("rare_property", 10); // Very selective (1/10)
    stats.update_property_stats("common_property", 500); // Less selective (1/500)

    stats
}

// ==================== Filter Reordering Tests ====================

#[test]
fn test_reorder_filters_by_selectivity() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Original: Filter(common) -> Filter(rare) -> Scan
    // common_property: 500 distinct values → 0.2% selectivity (MORE selective)
    // rare_property: 10 distinct values → 10% selectivity (LESS selective)
    // Should be reordered to: Filter(rare) -> Filter(common) -> Scan
    // (most selective at bottom/applied first)
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("common_property", "value")),
        LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("rare_property", "value")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: None,
                estimated_rows: Some(1000),
            }),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some(), "Should reorder filters by selectivity");

    let optimized = result.unwrap();
    // The outermost filter should be the LESS selective one (rare_property)
    // The innermost/deepest filter should be the MORE selective one (common_property)
    match &optimized.root {
        LogicalOp::Unary {
            op: UnaryOp::Filter(predicate),
            input,
        } => {
            // Root filter should be rare (less selective, applied last)
            assert!(matches!(
                predicate,
                Predicate::Eq { key, .. } if key == "rare_property"
            ));

            // Inner filter should be common (more selective, applied first)
            match input.as_ref() {
                LogicalOp::Unary {
                    op: UnaryOp::Filter(inner_pred),
                    ..
                } => {
                    assert!(matches!(
                        inner_pred,
                        Predicate::Eq { key, .. } if key == "common_property"
                    ));
                }
                _ => panic!("Expected inner filter"),
            }
        }
        _ => panic!("Expected Filter at root"),
    }
}

#[test]
fn test_no_reorder_when_filters_already_optimal() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Filter(rare - less selective) -> Filter(common - more selective) -> Scan
    // Already optimal: most selective at bottom
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("rare_property", "value")),
        LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("common_property", "value")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: None,
                estimated_rows: Some(1000),
            }),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    // Should return None since already optimal
    assert!(
        result.is_none(),
        "Should not reorder already optimal filters"
    );
}

#[test]
fn test_reorder_three_filters() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Add a third property with medium selectivity
    stats.update_property_stats("medium_property", 100); // Medium (1/100)

    // Filter(common) -> Filter(medium) -> Filter(rare) -> Scan
    // Should become: Filter(rare) -> Filter(medium) -> Filter(common) -> Scan
    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("common_property", "value")),
        LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("medium_property", "value")),
            LogicalOp::unary(
                UnaryOp::Filter(Predicate::eq("rare_property", "value")),
                LogicalOp::Scan(ScanOp::NodeScan {
                    label: None,
                    estimated_rows: Some(1000),
                }),
            ),
        ),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some(), "Should reorder three filters");
}

// ==================== Join Reordering Tests ====================

#[test]
fn test_reorder_join_operands_by_size() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Join with large left side and small right side
    // Should be reordered to put small side first (for hash join build)
    let large_scan = LogicalOp::Scan(ScanOp::NodeScan {
        label: Some("LargeTable".to_string()),
        estimated_rows: Some(10000),
    });

    let small_scan = LogicalOp::Scan(ScanOp::NodeScan {
        label: Some("SmallTable".to_string()),
        estimated_rows: Some(100),
    });

    let plan = LogicalPlan::new(LogicalOp::binary(
        BinaryOp::Join {
            left_key: "id".to_string(),
            right_key: "ref_id".to_string(),
        },
        large_scan,
        small_scan,
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some(), "Should reorder join operands");

    let optimized = result.unwrap();
    // Small table should be on the left (build side)
    match &optimized.root {
        LogicalOp::Binary {
            op: BinaryOp::Join { .. },
            left,
            ..
        } => {
            if let LogicalOp::Scan(ScanOp::NodeScan {
                estimated_rows: Some(rows),
                ..
            }) = left.as_ref()
            {
                assert_eq!(*rows, 100, "Smaller table should be build side");
            } else {
                panic!("Expected NodeScan on left");
            }
        }
        _ => panic!("Expected Join at root"),
    }
}

#[test]
fn test_no_reorder_when_join_already_optimal() {
    let rule = OperationReordering;
    let stats = test_stats();

    let small_scan = LogicalOp::Scan(ScanOp::NodeScan {
        label: Some("SmallTable".to_string()),
        estimated_rows: Some(100),
    });

    let large_scan = LogicalOp::Scan(ScanOp::NodeScan {
        label: Some("LargeTable".to_string()),
        estimated_rows: Some(10000),
    });

    // Small already on left - optimal
    let plan = LogicalPlan::new(LogicalOp::binary(
        BinaryOp::Join {
            left_key: "id".to_string(),
            right_key: "ref_id".to_string(),
        },
        small_scan,
        large_scan,
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none(), "Should not reorder optimal join");
}

// ==================== Complex Query Reordering Tests ====================

#[test]
fn test_reorder_complex_query() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Complex query with multiple opportunities for reordering:
    // Join(large, small) with filters in non-optimal order
    let plan = LogicalPlan::new(LogicalOp::binary(
        BinaryOp::Join {
            left_key: "id".to_string(),
            right_key: "ref_id".to_string(),
        },
        // Left side: large scan with non-optimal filter order
        LogicalOp::unary(
            UnaryOp::Filter(Predicate::eq("common_property", "value")),
            LogicalOp::Scan(ScanOp::NodeScan {
                label: Some("LargeTable".to_string()),
                estimated_rows: Some(10000),
            }),
        ),
        // Right side: small scan
        LogicalOp::Scan(ScanOp::NodeScan {
            label: Some("SmallTable".to_string()),
            estimated_rows: Some(100),
        }),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_some(), "Should optimize complex query");
}

// ==================== Edge Cases ====================

#[test]
fn test_no_change_for_simple_scan() {
    let rule = OperationReordering;
    let stats = test_stats();

    let plan = LogicalPlan::new(LogicalOp::Scan(ScanOp::NodeLookup(vec![
        NodeId::new(1).unwrap(),
    ])));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none(), "Should not change simple scan");
}

#[test]
fn test_single_filter_no_reorder() {
    let rule = OperationReordering;
    let stats = test_stats();

    let plan = LogicalPlan::new(LogicalOp::unary(
        UnaryOp::Filter(Predicate::eq("name", "Alice")),
        LogicalOp::Scan(ScanOp::NodeScan {
            label: None,
            estimated_rows: Some(1000),
        }),
    ));

    let result = rule.apply(&plan, &stats).unwrap();
    assert!(result.is_none(), "Single filter cannot be reordered");
}

#[test]
fn test_selectivity_and_predicate() {
    let rule = OperationReordering;
    let stats = test_stats();

    // AND of two predicates: selectivity should be product
    // Use predicates with fixed selectivity (not dependent on property stats)
    let pred = Predicate::And(vec![
        Predicate::gt("score", 50i64), // RANGE_PREDICATE_SELECTIVITY = 0.3
        Predicate::contains("name", "test"), // STRING_PREDICATE_SELECTIVITY = 0.2
    ]);

    let sel = rule.estimate_filter_selectivity(&pred, &stats);
    // Expected: 0.3 * 0.2 = 0.06
    assert!(
        (sel - 0.06).abs() < 0.001,
        "AND selectivity should be product, got {}",
        sel
    );
}

#[test]
fn test_selectivity_or_predicate() {
    let rule = OperationReordering;
    let stats = test_stats();

    // OR of two predicates: selectivity = 1 - (1-sel1) * (1-sel2)
    // Use predicates with fixed selectivity
    let pred = Predicate::Or(vec![
        Predicate::gt("score", 50i64), // RANGE_PREDICATE_SELECTIVITY = 0.3
        Predicate::contains("name", "test"), // STRING_PREDICATE_SELECTIVITY = 0.2
    ]);

    let sel = rule.estimate_filter_selectivity(&pred, &stats);
    // Expected: 1 - (1-0.3) * (1-0.2) = 1 - 0.7 * 0.8 = 1 - 0.56 = 0.44
    assert!(
        (sel - 0.44).abs() < 0.001,
        "OR selectivity should use union rule, got {}",
        sel
    );
}

#[test]
fn test_selectivity_not_predicate() {
    let rule = OperationReordering;
    let stats = test_stats();

    // NOT of a predicate: selectivity = 1 - inner_sel
    // Use predicate with fixed selectivity
    let pred = Predicate::Not(Box::new(Predicate::gt("score", 50i64))); // RANGE_PREDICATE_SELECTIVITY = 0.3

    let sel = rule.estimate_filter_selectivity(&pred, &stats);
    // Expected: 1 - 0.3 = 0.7
    assert!(
        (sel - 0.7).abs() < 0.001,
        "NOT selectivity should be complement, got {}",
        sel
    );
}

#[test]
fn test_selectivity_empty_and() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Empty AND should have high selectivity (filters nothing)
    let pred = Predicate::And(vec![]);
    let sel = rule.estimate_filter_selectivity(&pred, &stats);
    assert_eq!(sel, TRUE_SELECTIVITY);
}

#[test]
fn test_selectivity_empty_or() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Empty OR should have low selectivity (filters everything)
    let pred = Predicate::Or(vec![]);
    let sel = rule.estimate_filter_selectivity(&pred, &stats);
    assert_eq!(sel, FALSE_SELECTIVITY);
}

#[test]
fn test_selectivity_nested_predicates() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Nested: AND(OR(a, b), c)
    // Use predicates with fixed selectivity
    let pred = Predicate::And(vec![
        Predicate::Or(vec![
            Predicate::gt("score", 50i64),       // RANGE = 0.3
            Predicate::contains("name", "test"), // STRING = 0.2
        ]),
        Predicate::lt("age", 100i64), // RANGE_PREDICATE_SELECTIVITY = 0.3
    ]);

    let sel = rule.estimate_filter_selectivity(&pred, &stats);
    // OR: 1 - (1-0.3) * (1-0.2) = 1 - 0.7 * 0.8 = 0.44
    // AND: 0.44 * 0.3 = 0.132
    assert!(
        (sel - 0.132).abs() < 0.001,
        "Nested predicates should compute correctly, got {}",
        sel
    );
}

#[test]
fn test_selectivity_all_types() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Test all predicate types have defined selectivity
    assert_eq!(
        rule.estimate_filter_selectivity(&Predicate::True, &stats),
        TRUE_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(&Predicate::False, &stats),
        FALSE_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(&Predicate::gt("x", 1i64), &stats),
        RANGE_PREDICATE_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(&Predicate::lt("x", 1i64), &stats),
        RANGE_PREDICATE_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(&Predicate::contains("x", "y"), &stats),
        STRING_PREDICATE_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(
            &Predicate::StartsWith {
                key: "x".to_string(),
                prefix: "y".to_string()
            },
            &stats
        ),
        STRING_PREDICATE_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(
            &Predicate::EndsWith {
                key: "x".to_string(),
                suffix: "y".to_string()
            },
            &stats
        ),
        STRING_PREDICATE_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(&Predicate::ne("x", 1i64), &stats),
        NOT_EQUALS_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(
            &Predicate::In {
                key: "x".to_string(),
                values: vec![
                    crate::query::ir::PredicateValue::Int(1),
                    crate::query::ir::PredicateValue::Int(2)
                ]
            },
            &stats
        ),
        IN_PREDICATE_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(&Predicate::exists("x"), &stats),
        EXISTENCE_CHECK_SELECTIVITY
    );
    assert_eq!(
        rule.estimate_filter_selectivity(&Predicate::NotExists("x".to_string()), &stats),
        EXISTENCE_CHECK_SELECTIVITY
    );
}

#[test]
fn test_predicates_equal_basic() {
    // Same predicates should be equal (Predicate derives PartialEq)
    let p1 = Predicate::eq("name", "Alice");
    let p2 = Predicate::eq("name", "Alice");
    assert_eq!(p1, p2);

    // Different values should not be equal
    let p3 = Predicate::eq("name", "Bob");
    assert_ne!(p1, p3);

    // Different keys should not be equal
    let p4 = Predicate::eq("age", "Alice");
    assert_ne!(p1, p4);
}

#[test]
fn test_predicates_equal_different_types() {
    // Different predicate types should not be equal
    let p1 = Predicate::eq("name", "Alice");
    let p2 = Predicate::ne("name", "Alice");
    assert_ne!(p1, p2);

    let p3 = Predicate::gt("age", 30i64);
    let p4 = Predicate::lt("age", 30i64);
    assert_ne!(p3, p4);
}

#[test]
fn test_predicates_equal_and_or() {
    // Same AND predicates
    let p1 = Predicate::And(vec![
        Predicate::eq("name", "Alice"),
        Predicate::gt("age", 30i64),
    ]);
    let p2 = Predicate::And(vec![
        Predicate::eq("name", "Alice"),
        Predicate::gt("age", 30i64),
    ]);
    assert_eq!(p1, p2);

    // Different order in AND should not be equal (structural comparison)
    let p3 = Predicate::And(vec![
        Predicate::gt("age", 30i64),
        Predicate::eq("name", "Alice"),
    ]);
    assert_ne!(p1, p3);

    // Different length AND
    let p4 = Predicate::And(vec![Predicate::eq("name", "Alice")]);
    assert_ne!(p1, p4);

    // AND vs OR
    let p5 = Predicate::Or(vec![
        Predicate::eq("name", "Alice"),
        Predicate::gt("age", 30i64),
    ]);
    assert_ne!(p1, p5);
}

#[test]
fn test_predicates_equal_not() {
    // Same NOT predicates
    let p1 = Predicate::Not(Box::new(Predicate::eq("active", true)));
    let p2 = Predicate::Not(Box::new(Predicate::eq("active", true)));
    assert_eq!(p1, p2);

    // Different inner predicates
    let p3 = Predicate::Not(Box::new(Predicate::eq("active", false)));
    assert_ne!(p1, p3);
}

#[test]
fn test_predicates_equal_all_variants() {
    // Test all variants for coverage (Predicate derives PartialEq)
    assert_eq!(Predicate::True, Predicate::True);
    assert_eq!(Predicate::False, Predicate::False);
    assert_ne!(Predicate::True, Predicate::False);

    // String predicates
    let c1 = Predicate::contains("text", "hello");
    let c2 = Predicate::contains("text", "hello");
    assert_eq!(c1, c2);

    let s1 = Predicate::StartsWith {
        key: "text".to_string(),
        prefix: "hello".to_string(),
    };
    let s2 = Predicate::StartsWith {
        key: "text".to_string(),
        prefix: "hello".to_string(),
    };
    assert_eq!(s1, s2);

    let e1 = Predicate::EndsWith {
        key: "text".to_string(),
        suffix: "world".to_string(),
    };
    let e2 = Predicate::EndsWith {
        key: "text".to_string(),
        suffix: "world".to_string(),
    };
    assert_eq!(e1, e2);

    // In predicate
    let i1 = Predicate::In {
        key: "id".to_string(),
        values: vec![
            crate::query::ir::PredicateValue::Int(1),
            crate::query::ir::PredicateValue::Int(2),
        ],
    };
    let i2 = Predicate::In {
        key: "id".to_string(),
        values: vec![
            crate::query::ir::PredicateValue::Int(1),
            crate::query::ir::PredicateValue::Int(2),
        ],
    };
    assert_eq!(i1, i2);

    // Exists predicates
    let ex1 = Predicate::exists("prop");
    let ex2 = Predicate::exists("prop");
    assert_eq!(ex1, ex2);

    let nex1 = Predicate::NotExists("prop".to_string());
    let nex2 = Predicate::NotExists("prop".to_string());
    assert_eq!(nex1, nex2);
}

#[test]
fn test_selectivity_null_check() {
    let rule = OperationReordering;
    let stats = test_stats();

    // Null checks should use NULL_CHECK_SELECTIVITY
    let pred = Predicate::Eq {
        key: "value".to_string(),
        value: crate::query::ir::PredicateValue::Null,
    };

    let sel = rule.estimate_filter_selectivity(&pred, &stats);
    assert_eq!(sel, NULL_CHECK_SELECTIVITY);
}

#[test]
fn test_predicates_equal_exhaustive_mismatches() {
    // Predicate derives PartialEq, so we use assert_ne! directly.

    // Eq mismatches
    assert_ne!(Predicate::eq("a", 1), Predicate::eq("b", 1));
    assert_ne!(Predicate::eq("a", 1), Predicate::eq("a", 2));

    // Ne mismatches
    assert_ne!(Predicate::ne("a", 1), Predicate::ne("b", 1));
    assert_ne!(Predicate::ne("a", 1), Predicate::ne("a", 2));

    // Gt mismatches
    assert_ne!(Predicate::gt("a", 1), Predicate::gt("b", 1));
    assert_ne!(Predicate::gt("a", 1), Predicate::gt("a", 2));

    // Gte mismatches
    assert_ne!(
        Predicate::Gte {
            key: "a".to_string(),
            value: crate::query::ir::PredicateValue::Int(1)
        },
        Predicate::Gte {
            key: "b".to_string(),
            value: crate::query::ir::PredicateValue::Int(1)
        }
    );
    assert_ne!(
        Predicate::Gte {
            key: "a".to_string(),
            value: crate::query::ir::PredicateValue::Int(1)
        },
        Predicate::Gte {
            key: "a".to_string(),
            value: crate::query::ir::PredicateValue::Int(2)
        }
    );

    // Lt mismatches
    assert_ne!(Predicate::lt("a", 1), Predicate::lt("b", 1));
    assert_ne!(Predicate::lt("a", 1), Predicate::lt("a", 2));

    // Lte mismatches
    assert_ne!(
        Predicate::Lte {
            key: "a".to_string(),
            value: crate::query::ir::PredicateValue::Int(1)
        },
        Predicate::Lte {
            key: "b".to_string(),
            value: crate::query::ir::PredicateValue::Int(1)
        }
    );
    assert_ne!(
        Predicate::Lte {
            key: "a".to_string(),
            value: crate::query::ir::PredicateValue::Int(1)
        },
        Predicate::Lte {
            key: "a".to_string(),
            value: crate::query::ir::PredicateValue::Int(2)
        }
    );

    // In mismatches
    assert_ne!(
        Predicate::In {
            key: "a".to_string(),
            values: vec![crate::query::ir::PredicateValue::Int(1)]
        },
        Predicate::In {
            key: "b".to_string(),
            values: vec![crate::query::ir::PredicateValue::Int(1)]
        }
    );
    assert_ne!(
        Predicate::In {
            key: "a".to_string(),
            values: vec![crate::query::ir::PredicateValue::Int(1)]
        },
        Predicate::In {
            key: "a".to_string(),
            values: vec![crate::query::ir::PredicateValue::Int(2)]
        }
    );

    // Contains mismatches
    assert_ne!(
        Predicate::contains("a", "abc"),
        Predicate::contains("b", "abc")
    );
    assert_ne!(
        Predicate::contains("a", "abc"),
        Predicate::contains("a", "xyz")
    );

    // StartsWith mismatches
    assert_ne!(
        Predicate::StartsWith {
            key: "a".to_string(),
            prefix: "abc".to_string()
        },
        Predicate::StartsWith {
            key: "b".to_string(),
            prefix: "abc".to_string()
        }
    );
    assert_ne!(
        Predicate::StartsWith {
            key: "a".to_string(),
            prefix: "abc".to_string()
        },
        Predicate::StartsWith {
            key: "a".to_string(),
            prefix: "xyz".to_string()
        }
    );

    // EndsWith mismatches
    assert_ne!(
        Predicate::EndsWith {
            key: "a".to_string(),
            suffix: "abc".to_string()
        },
        Predicate::EndsWith {
            key: "b".to_string(),
            suffix: "abc".to_string()
        }
    );
    assert_ne!(
        Predicate::EndsWith {
            key: "a".to_string(),
            suffix: "abc".to_string()
        },
        Predicate::EndsWith {
            key: "a".to_string(),
            suffix: "xyz".to_string()
        }
    );

    // Exists mismatches
    assert_ne!(Predicate::exists("a"), Predicate::exists("b"));

    // NotExists mismatches
    assert_ne!(
        Predicate::NotExists("a".to_string()),
        Predicate::NotExists("b".to_string())
    );

    // And / Or structural mismatches (different sizes)
    assert_ne!(
        Predicate::And(vec![Predicate::eq("a", 1)]),
        Predicate::And(vec![Predicate::eq("a", 1), Predicate::eq("b", 2)])
    );
    assert_ne!(
        Predicate::Or(vec![Predicate::eq("a", 1)]),
        Predicate::Or(vec![Predicate::eq("a", 1), Predicate::eq("b", 2)])
    );
}
