use super::*;
use crate::core::id::VersionId;
use crate::core::interning::InternedString;
use crate::core::property::PropertyMapBuilder;

fn test_node(id: u64, name: &str) -> Node {
    let props = PropertyMapBuilder::new().insert("name", name).build();
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    Node::new(
        NodeId::new(id).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    )
}

fn test_node_with_age(id: u64, name: &str, age: i64) -> Node {
    let props = PropertyMapBuilder::new()
        .insert("name", name)
        .insert("age", age)
        .build();
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    Node::new(
        NodeId::new(id).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    )
}

fn test_node_with_vector(id: u64, name: &str, embedding: Vec<f32>) -> Node {
    let props = PropertyMapBuilder::new()
        .insert("name", name)
        .insert_vector("embedding", &embedding)
        .build();
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    Node::new(
        NodeId::new(id).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    )
}

/// Mock iterator for testing
struct MockIterator {
    items: std::vec::IntoIter<Result<QueryRow>>,
}

impl MockIterator {
    fn from_nodes(nodes: Vec<Node>) -> Self {
        let items: Vec<Result<QueryRow>> = nodes
            .into_iter()
            .map(|n| Ok(QueryRow::from_entity(EntityResult::Node(n))))
            .collect();
        MockIterator {
            items: items.into_iter(),
        }
    }

    fn from_results(results: Vec<Result<QueryRow>>) -> Self {
        MockIterator {
            items: results.into_iter(),
        }
    }
}

impl ResultIterator for MockIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.items.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.items.size_hint()
    }
}

// ==================== EmptyIterator Tests ====================

#[test]
fn test_empty_iterator() {
    let mut iter = EmptyIterator;
    assert!(iter.next().is_none());
    assert_eq!(iter.size_hint(), (0, Some(0)));
}

#[test]
fn test_empty_iterator_multiple_calls() {
    let mut iter = EmptyIterator;
    assert!(iter.next().is_none());
    assert!(iter.next().is_none());
    assert!(iter.next().is_none());
}

// ==================== FilterIterator Predicate Tests ====================

#[test]
fn test_filter_predicate_eq() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::eq("name", "Alice");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_eq_false() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::eq("name", "Bob");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_eq_missing_property() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::eq("missing", "value");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_ne() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::ne("name", "Bob");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_ne_same_value() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::ne("name", "Alice");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_ne_missing_property() {
    let node = test_node(1, "Alice");
    // Missing property != anything is true
    let predicate = Predicate::ne("missing", "value");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_gt() {
    let node = test_node_with_age(1, "Alice", 30);
    let predicate = Predicate::gt("age", 18i64);

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_gt_equal_value() {
    let node = test_node_with_age(1, "Alice", 18);
    let predicate = Predicate::gt("age", 18i64);

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_gt_less_value() {
    let node = test_node_with_age(1, "Alice", 15);
    let predicate = Predicate::gt("age", 18i64);

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_lt() {
    let node = test_node_with_age(1, "Alice", 15);
    let predicate = Predicate::lt("age", 18i64);

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_lt_equal_value() {
    let node = test_node_with_age(1, "Alice", 18);
    let predicate = Predicate::lt("age", 18i64);

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_gte() {
    let node = test_node_with_age(1, "Alice", 18);
    let predicate = Predicate::Gte {
        key: "age".to_string(),
        value: PredicateValue::Int(18),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_gte_greater() {
    let node = test_node_with_age(1, "Alice", 20);
    let predicate = Predicate::Gte {
        key: "age".to_string(),
        value: PredicateValue::Int(18),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_gte_less() {
    let node = test_node_with_age(1, "Alice", 15);
    let predicate = Predicate::Gte {
        key: "age".to_string(),
        value: PredicateValue::Int(18),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_lte() {
    let node = test_node_with_age(1, "Alice", 18);
    let predicate = Predicate::Lte {
        key: "age".to_string(),
        value: PredicateValue::Int(18),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_lte_less() {
    let node = test_node_with_age(1, "Alice", 15);
    let predicate = Predicate::Lte {
        key: "age".to_string(),
        value: PredicateValue::Int(18),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_lte_greater() {
    let node = test_node_with_age(1, "Alice", 20);
    let predicate = Predicate::Lte {
        key: "age".to_string(),
        value: PredicateValue::Int(18),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_exists() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::exists("name");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_exists_missing() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::exists("missing");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_not_exists() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::NotExists("missing".to_string());

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_not_exists_present() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::NotExists("name".to_string());

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_contains() {
    let node = test_node(1, "Alice Johnson");
    let predicate = Predicate::contains("name", "John");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_contains_not_found() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::contains("name", "Bob");

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_starts_with() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::StartsWith {
        key: "name".to_string(),
        prefix: "Ali".to_string(),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_starts_with_not_match() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::StartsWith {
        key: "name".to_string(),
        prefix: "Bob".to_string(),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_ends_with() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::EndsWith {
        key: "name".to_string(),
        suffix: "ice".to_string(),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_ends_with_not_match() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::EndsWith {
        key: "name".to_string(),
        suffix: "Bob".to_string(),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_in() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::In {
        key: "name".to_string(),
        values: vec![
            PredicateValue::String("Alice".to_string()),
            PredicateValue::String("Bob".to_string()),
            PredicateValue::String("Charlie".to_string()),
        ],
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_in_not_found() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::In {
        key: "name".to_string(),
        values: vec![
            PredicateValue::String("Bob".to_string()),
            PredicateValue::String("Charlie".to_string()),
        ],
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_and() {
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    );

    let predicate = Predicate::eq("name", "Alice").and(Predicate::gt("age", 18i64));

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_and_one_false() {
    let node = test_node_with_age(1, "Alice", 15);
    let predicate = Predicate::eq("name", "Alice").and(Predicate::gt("age", 18i64));

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_or() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::eq("name", "Alice").or(Predicate::eq("name", "Bob"));

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_or_second_true() {
    let node = test_node(1, "Bob");
    let predicate = Predicate::eq("name", "Alice").or(Predicate::eq("name", "Bob"));

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_or_both_false() {
    let node = test_node(1, "Charlie");
    let predicate = Predicate::eq("name", "Alice").or(Predicate::eq("name", "Bob"));

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_not() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::Not(Box::new(Predicate::eq("name", "Bob")));

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_not_negates_true() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::Not(Box::new(Predicate::eq("name", "Alice")));

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_true() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::True;

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_false() {
    let node = test_node(1, "Alice");
    let predicate = Predicate::False;

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_float_comparison() {
    let props = PropertyMapBuilder::new().insert("score", 3.5f64).build();
    let label = GLOBAL_INTERNER.intern("Score").unwrap();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    );

    let predicate = Predicate::gt("score", 3.0f64);
    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));

    let predicate = Predicate::lt("score", 4.0f64);
    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_predicate_bool_comparison() {
    let props = PropertyMapBuilder::new().insert("active", true).build();
    let label = GLOBAL_INTERNER.intern("Status").unwrap();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    );

    let predicate = Predicate::eq("active", true);
    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));

    let predicate = Predicate::eq("active", false);
    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

// ==================== FilterIterator Integration Tests ====================

#[test]
fn test_filter_iterator_passes_matching_nodes() {
    let nodes = vec![
        test_node_with_age(1, "Alice", 30),
        test_node_with_age(2, "Bob", 25),
        test_node_with_age(3, "Charlie", 35),
    ];

    let input = MockIterator::from_nodes(nodes);
    let predicate = Predicate::gt("age", 28i64);
    let mut filter = FilterIterator::new(Box::new(input), predicate);

    let mut results = Vec::new();
    while let Some(Ok(row)) = filter.next() {
        results.push(row);
    }

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].entity.node_id(), Some(NodeId::new(1).unwrap())); // Alice (30)
    assert_eq!(results[1].entity.node_id(), Some(NodeId::new(3).unwrap())); // Charlie (35)
}

#[test]
fn test_filter_iterator_no_matches() {
    let nodes = vec![
        test_node_with_age(1, "Alice", 20),
        test_node_with_age(2, "Bob", 25),
    ];

    let input = MockIterator::from_nodes(nodes);
    let predicate = Predicate::gt("age", 100i64);
    let mut filter = FilterIterator::new(Box::new(input), predicate);

    assert!(filter.next().is_none());
}

#[test]
fn test_filter_iterator_propagates_errors() {
    let results = vec![
        Ok(QueryRow::from_entity(EntityResult::Node(test_node(
            1, "Alice",
        )))),
        Err(crate::core::error::Error::other("test error")),
    ];

    let input = MockIterator::from_results(results);
    let predicate = Predicate::True;
    let mut filter = FilterIterator::new(Box::new(input), predicate);

    // First result succeeds
    assert!(filter.next().unwrap().is_ok());
    // Second result is error
    assert!(filter.next().unwrap().is_err());
}

// ==================== LimitIterator Tests ====================

#[test]
fn test_limit_iterator() {
    let test_label = GLOBAL_INTERNER.intern("Test").unwrap();

    struct CountingIterator {
        count: usize,
        max: usize,
        label: InternedString,
    }

    impl ResultIterator for CountingIterator {
        fn next(&mut self) -> Option<Result<QueryRow>> {
            if self.count < self.max {
                self.count += 1;
                let node = Node::new(
                    NodeId::new(self.count as u64).unwrap(),
                    self.label,
                    PropertyMapBuilder::new().build(),
                    VersionId::new(1).unwrap(),
                );
                Some(Ok(QueryRow::from_entity(EntityResult::Node(node))))
            } else {
                None
            }
        }
    }

    let input = Box::new(CountingIterator {
        count: 0,
        max: 10,
        label: test_label,
    });
    let mut limit = LimitIterator::new(input, 2, 3);

    // Should skip 2, return 3
    let mut results = Vec::new();
    while let Some(Ok(row)) = limit.next() {
        results.push(row);
    }

    assert_eq!(results.len(), 3);
    // First result should be node 3 (after skipping 2)
    assert_eq!(results[0].entity.node_id(), Some(NodeId::new(3).unwrap()));
}

#[test]
fn test_limit_iterator_no_offset() {
    let nodes = vec![
        test_node(1, "Alice"),
        test_node(2, "Bob"),
        test_node(3, "Charlie"),
        test_node(4, "Dave"),
    ];

    let input = MockIterator::from_nodes(nodes);
    let mut limit = LimitIterator::new(Box::new(input), 0, 2);

    let mut results = Vec::new();
    while let Some(Ok(row)) = limit.next() {
        results.push(row);
    }

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].entity.node_id(), Some(NodeId::new(1).unwrap()));
    assert_eq!(results[1].entity.node_id(), Some(NodeId::new(2).unwrap()));
}

#[test]
fn test_limit_iterator_offset_exceeds_input() {
    let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

    let input = MockIterator::from_nodes(nodes);
    let mut limit = LimitIterator::new(Box::new(input), 5, 10);

    // Offset exceeds input, should return nothing
    assert!(limit.next().is_none());
}

#[test]
fn test_limit_iterator_count_zero() {
    let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

    let input = MockIterator::from_nodes(nodes);
    let mut limit = LimitIterator::new(Box::new(input), 0, 0);

    // Count is 0, should return nothing
    assert!(limit.next().is_none());
}

#[test]
fn test_limit_iterator_count_exceeds_remaining() {
    let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

    let input = MockIterator::from_nodes(nodes);
    let mut limit = LimitIterator::new(Box::new(input), 1, 10);

    let mut results = Vec::new();
    while let Some(Ok(row)) = limit.next() {
        results.push(row);
    }

    // Skipped 1, only 1 remaining
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entity.node_id(), Some(NodeId::new(2).unwrap()));
}

#[test]
fn test_limit_iterator_propagates_errors_during_skip() {
    let results = vec![
        Err(crate::core::error::Error::other("test error")),
        Ok(QueryRow::from_entity(EntityResult::Node(test_node(
            1, "Alice",
        )))),
    ];

    let input = MockIterator::from_results(results);
    let mut limit = LimitIterator::new(Box::new(input), 1, 5);

    // Should get error during skip phase
    let result = limit.next();
    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn test_limit_iterator_size_hint() {
    let nodes = vec![
        test_node(1, "Alice"),
        test_node(2, "Bob"),
        test_node(3, "Charlie"),
    ];

    let input = MockIterator::from_nodes(nodes);
    let limit = LimitIterator::new(Box::new(input), 0, 2);

    // Size hint should respect the limit
    let (lower, upper) = limit.size_hint();
    assert!(lower <= 2);
    assert!(upper.map(|u| u <= 2).unwrap_or(true));
}

// ==================== VectorRerankIterator Tests ====================

#[test]
fn test_vector_rerank_no_vector_index_error() {
    let nodes = vec![test_node_with_vector(1, "Alice", vec![1.0, 0.0, 0.0, 0.0])];

    // Create CurrentStorage without vector index
    let current = Arc::new(CurrentStorage::new());

    let input = MockIterator::from_nodes(nodes);
    let query = Arc::from(vec![1.0f32, 0.0, 0.0, 0.0]);

    let mut rerank = VectorRerankIterator::new(Box::new(input), query, 10, current, None);

    // Should return error because no vector index is configured
    let result = rerank.next();
    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn test_vector_rerank_size_hint_before_init() {
    let nodes = vec![test_node_with_vector(1, "Alice", vec![1.0, 0.0, 0.0, 0.0])];

    let current = Arc::new(CurrentStorage::new());
    let input = MockIterator::from_nodes(nodes);
    let query = Arc::from(vec![1.0f32, 0.0, 0.0, 0.0]);

    let rerank = VectorRerankIterator::new(Box::new(input), query, 5, current, None);

    // Before initialization, size_hint upper bound is k
    let (lower, upper) = rerank.size_hint();
    assert_eq!(lower, 0);
    assert_eq!(upper, Some(5));
}

// ==================== ProjectIterator Tests ====================

#[test]
fn test_project_iterator_filters_properties() {
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30)
        .insert("city", "Paris")
        .build();
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    );

    let input = MockIterator::from_nodes(vec![node]);
    let mut project = ProjectIterator::new(
        Box::new(input),
        vec!["name".to_string(), "city".to_string()],
    );

    let row = project.next().unwrap().unwrap();
    let projected_node = row.entity.as_node().unwrap();

    assert_eq!(
        projected_node
            .properties
            .get("name")
            .unwrap()
            .as_str()
            .unwrap(),
        "Alice"
    );
    assert_eq!(
        projected_node
            .properties
            .get("city")
            .unwrap()
            .as_str()
            .unwrap(),
        "Paris"
    );
    assert!(projected_node.properties.get("age").is_none());
}

#[test]
fn test_project_iterator_missing_property() {
    let node = test_node(1, "Alice"); // Only has "name"
    let input = MockIterator::from_nodes(vec![node]);
    let mut project =
        ProjectIterator::new(Box::new(input), vec!["name".to_string(), "age".to_string()]);

    let row = project.next().unwrap().unwrap();
    let projected_node = row.entity.as_node().unwrap();

    assert_eq!(
        projected_node
            .properties
            .get("name")
            .unwrap()
            .as_str()
            .unwrap(),
        "Alice"
    );
    assert!(projected_node.properties.get("age").is_none());
}

#[test]
fn test_project_iterator_non_node_pass_through() {
    // Projecting on non-node entities (like EdgeId) should be a no-op currently
    // as the implementation only checks for Node
    let row = QueryRow::from_entity(EntityResult::NodeId(NodeId::new(1).unwrap()));
    let input = MockIterator::from_results(vec![Ok(row)]);

    let mut project = ProjectIterator::new(Box::new(input), vec!["name".to_string()]);

    let result = project.next().unwrap().unwrap();
    assert!(matches!(result.entity, EntityResult::NodeId(_)));
}

// ==================== MockIterator Tests ====================

#[test]
fn test_mock_iterator_from_nodes() {
    let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

    let mut iter = MockIterator::from_nodes(nodes);

    let row1 = iter.next().unwrap().unwrap();
    assert_eq!(row1.entity.node_id(), Some(NodeId::new(1).unwrap()));

    let row2 = iter.next().unwrap().unwrap();
    assert_eq!(row2.entity.node_id(), Some(NodeId::new(2).unwrap()));

    assert!(iter.next().is_none());
}

#[test]
fn test_mock_iterator_size_hint() {
    let nodes = vec![test_node(1, "Alice"), test_node(2, "Bob")];

    let iter = MockIterator::from_nodes(nodes);

    let (lower, upper) = iter.size_hint();
    assert_eq!(lower, 2);
    assert_eq!(upper, Some(2));
}

// ==================== Type comparison edge cases ====================

#[test]
fn test_filter_type_mismatch_returns_false() {
    // String property compared to Int predicate
    let node = test_node(1, "Alice"); // name is String
    let predicate = Predicate::gt("name", 10i64); // Comparing String to Int

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node)); // Type mismatch returns false
}

#[test]
fn test_filter_contains_on_non_string_returns_false() {
    let node = test_node_with_age(1, "Alice", 30);
    let predicate = Predicate::contains("age", "30"); // age is Int, not String

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_starts_with_on_non_string_returns_false() {
    let node = test_node_with_age(1, "Alice", 30);
    let predicate = Predicate::StartsWith {
        key: "age".to_string(),
        prefix: "3".to_string(),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

#[test]
fn test_filter_ends_with_on_non_string_returns_false() {
    let node = test_node_with_age(1, "Alice", 30);
    let predicate = Predicate::EndsWith {
        key: "age".to_string(),
        suffix: "0".to_string(),
    };

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

// ==================== Null handling ====================

#[test]
fn test_filter_null_equality() {
    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("optional", PropertyValue::Null)
        .build();
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    );

    // Null == Null should be true
    let predicate = Predicate::Eq {
        key: "optional".to_string(),
        value: PredicateValue::Null,
    };
    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

// ==================== Complex nested predicates ====================

#[test]
fn test_filter_deeply_nested_predicate() {
    let node = test_node_with_age(1, "Alice", 30);

    // (name == "Alice" AND age > 20) OR (name == "Bob")
    let predicate = Predicate::Or(vec![
        Predicate::And(vec![
            Predicate::eq("name", "Alice"),
            Predicate::gt("age", 20i64),
        ]),
        Predicate::eq("name", "Bob"),
    ]);

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_empty_and_is_true() {
    let node = test_node(1, "Alice");
    // Empty AND is vacuously true
    let predicate = Predicate::And(vec![]);

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(filter.evaluate(&node));
}

#[test]
fn test_filter_empty_or_is_false() {
    let node = test_node(1, "Alice");
    // Empty OR is vacuously false
    let predicate = Predicate::Or(vec![]);

    let filter = FilterIterator::new(Box::new(EmptyIterator), predicate);
    assert!(!filter.evaluate(&node));
}

// ==================== NodeLookupIterator Tests ====================

#[test]
fn test_node_lookup_iterator_success() {
    let current = Arc::new(CurrentStorage::new());

    // Create test nodes
    let node1 = current
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let node2 = current
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    let node_ids = vec![node1, node2];
    let mut iter = NodeLookupIterator::new(node_ids, current);

    // Should get both nodes
    let row1 = iter.next().unwrap().unwrap();
    assert_eq!(row1.entity.node_id(), Some(node1));

    let row2 = iter.next().unwrap().unwrap();
    assert_eq!(row2.entity.node_id(), Some(node2));

    assert!(iter.next().is_none());
}

#[test]
fn test_node_lookup_iterator_missing_node() {
    let current = Arc::new(CurrentStorage::new());

    // Don't add the node
    let node_ids = vec![NodeId::new(999).unwrap()];
    let mut iter = NodeLookupIterator::new(node_ids, current);

    // Should return error for missing node
    let result = iter.next().unwrap();
    assert!(result.is_err());
}

#[test]
fn test_node_lookup_iterator_size_hint() {
    let current = Arc::new(CurrentStorage::new());
    let node_ids = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
    let iter = NodeLookupIterator::new(node_ids, current);

    let (lower, upper) = iter.size_hint();
    assert_eq!(lower, 2);
    assert_eq!(upper, Some(2));
}

// ==================== NodeScanIterator Tests ====================

#[test]
fn test_node_scan_iterator_all_nodes() {
    let current = Arc::new(CurrentStorage::new());

    current
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    current
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();

    let mut iter = NodeScanIterator::new(None, current);

    let mut results = Vec::new();
    while let Some(Ok(row)) = iter.next() {
        results.push(row);
    }

    assert_eq!(results.len(), 2);
}

#[test]
fn test_node_scan_iterator_with_label_filter() {
    let current = Arc::new(CurrentStorage::new());

    let person = current
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    current
        .create_node(
            "Company",
            PropertyMapBuilder::new().insert("name", "Acme").build(),
        )
        .unwrap();

    let mut iter = NodeScanIterator::new(Some("Person".to_string()), current);

    let mut results = Vec::new();
    while let Some(Ok(row)) = iter.next() {
        results.push(row);
    }

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entity.node_id(), Some(person));
}

#[test]
fn test_node_scan_iterator_empty_storage() {
    let current = Arc::new(CurrentStorage::new());
    let mut iter = NodeScanIterator::new(None, current);

    assert!(iter.next().is_none());
}

// ==================== VectorResultIterator Tests ====================

#[test]
fn test_vector_result_iterator_with_scores() {
    let current = Arc::new(CurrentStorage::new());

    let node1 = current
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();
    let node2 = current
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let results = vec![(node1, 0.95), (node2, 0.85)];

    let mut iter = VectorResultIterator::new(results, current);

    let row1 = iter.next().unwrap().unwrap();
    assert_eq!(row1.entity.node_id(), Some(node1));
    assert_eq!(row1.score, Some(0.95));

    let row2 = iter.next().unwrap().unwrap();
    assert_eq!(row2.entity.node_id(), Some(node2));
    assert_eq!(row2.score, Some(0.85));

    assert!(iter.next().is_none());
}

#[test]
fn test_vector_result_iterator_missing_node() {
    let current = Arc::new(CurrentStorage::new());

    // Node doesn't exist
    let results = vec![(NodeId::new(999).unwrap(), 0.95)];
    let mut iter = VectorResultIterator::new(results, current);

    let result = iter.next().unwrap();
    assert!(result.is_err());
}

// ==================== TemporalNodeIterator Tests ====================

#[test]
fn test_temporal_node_iterator_returns_current_state() {
    use crate::core::version::AnchorConfig;
    use crate::storage::historical::HistoricalStorage;

    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
        AnchorConfig::default(),
    )));

    let props = PropertyMapBuilder::new().insert("name", "Alice").build();
    let node = current.create_node("Person", props.clone()).unwrap();

    // Add version to historical storage
    use crate::core::temporal::time;
    let now = time::now();
    let label = crate::core::interning::GLOBAL_INTERNER
        .intern("Person")
        .unwrap();
    {
        let mut hist = historical.write();
        hist.add_node_version(
            node,
            crate::core::id::VersionId::new(1).unwrap(),
            now,
            now,
            label,
            props,
            false, // not a tombstone
        )
        .unwrap();
    }

    let node_ids = vec![node];

    let mut iter = TemporalNodeIterator::new(node_ids, now, now, historical);

    let row = iter.next().unwrap().unwrap();
    assert_eq!(row.entity.node_id(), Some(node));
    assert_eq!(row.timestamp, Some(now));
}

#[test]
fn test_temporal_node_iterator_empty() {
    use crate::core::version::AnchorConfig;
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
        AnchorConfig::default(),
    )));

    let node_ids = vec![];
    let now = crate::core::temporal::time::now();

    let mut iter = TemporalNodeIterator::new(node_ids, now, now, historical);

    assert!(iter.next().is_none());
}

// ==================== BatchTemporalNodeIterator Tests ====================

#[test]
fn test_batch_temporal_node_iterator_success() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let mut hist = historical.write();

    // Add 3 nodes
    for i in 1..=3 {
        let node_id = NodeId::new(i).unwrap();
        let version_id = VersionId::new(i * 100).unwrap();
        let label = GLOBAL_INTERNER.intern("Person").unwrap();
        let timestamp = ((i * 1000) as i64).into();

        let props = PropertyMapBuilder::new()
            .insert("name", format!("Person{}", i).as_str())
            .build();

        hist.add_node_version(
            node_id, version_id, timestamp, timestamp, label, props, false,
        )
        .unwrap();
    }
    drop(hist);

    // Create batch iterator
    let node_ids = vec![
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        NodeId::new(3).unwrap(),
    ];
    let mut iter =
        BatchTemporalNodeIterator::new(node_ids, 5000.into(), 5000.into(), historical).unwrap();

    // Verify all nodes retrieved
    let mut count = 0;
    while let Some(Ok(_)) = iter.next() {
        count += 1;
    }
    assert_eq!(count, 3);
}

#[test]
fn test_batch_temporal_node_iterator_node_not_found() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));

    let node_ids = vec![NodeId::new(999).unwrap()];
    let mut iter =
        BatchTemporalNodeIterator::new(node_ids, 1000.into(), 1000.into(), historical).unwrap();

    let result = iter.next().unwrap();
    assert!(result.is_err());
}

#[test]
fn test_batch_temporal_node_iterator_empty() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let node_ids = vec![];
    let mut iter =
        BatchTemporalNodeIterator::new(node_ids, 1000.into(), 1000.into(), historical).unwrap();

    assert!(iter.next().is_none());
}

// ==================== TemporalNodeScanIterator Tests (Issue #356) ====================
//
// These tests verify the refactored iterator with helper methods:
// - get_temporal_version(): Handles timestamp-based node retrieval
// - apply_label_filter(): Manages label-based filtering
// - filter_node(): Orchestrates filtering logic

#[test]
fn test_temporal_node_scan_iterator_get_temporal_version_success() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let node_id = NodeId::new(1).unwrap();
    let version_id = VersionId::new(100).unwrap();
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let timestamp: Timestamp = 1000.into();

    let props = PropertyMapBuilder::new().insert("name", "Alice").build();

    {
        let mut hist = historical.write();
        hist.add_node_version(
            node_id, version_id, timestamp, timestamp, label, props, false,
        )
        .unwrap();
    }

    // Test the get_temporal_version helper method directly
    let iter = TemporalNodeScanIterator::new(
        vec![node_id],
        timestamp,
        timestamp,
        historical.clone(),
        None, // No label filter
    );

    let guard = historical.read();
    let result = iter.get_temporal_version(node_id, &guard);
    assert!(result.is_ok());

    let node = result.unwrap();
    assert_eq!(node.id, node_id);
}

#[test]
fn test_temporal_node_scan_iterator_get_temporal_version_not_found() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let node_id = NodeId::new(999).unwrap();
    let timestamp: Timestamp = 1000.into();

    let iter = TemporalNodeScanIterator::new(
        vec![node_id],
        timestamp,
        timestamp,
        historical.clone(),
        None,
    );

    let guard = historical.read();
    let result = iter.get_temporal_version(node_id, &guard);
    assert!(result.is_err());
}

#[test]
fn test_temporal_node_scan_iterator_apply_label_filter_matches() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let timestamp: Timestamp = 1000.into();

    // Intern label BEFORE creating iterator (simulates real-world usage
    // where labels are interned when nodes are created in storage)
    let label = GLOBAL_INTERNER.intern("Person").unwrap();

    // Create iterator with "Person" label filter
    let iter = TemporalNodeScanIterator::new(
        vec![],
        timestamp,
        timestamp,
        historical,
        Some("Person".to_string()),
    );

    let props = PropertyMapBuilder::new().insert("name", "Alice").build();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    );

    // Label matches, should return true
    assert!(iter.apply_label_filter(&node));
}

#[test]
fn test_temporal_node_scan_iterator_apply_label_filter_no_match() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let timestamp: Timestamp = 1000.into();

    // Intern both labels BEFORE creating iterator
    let _company_label = GLOBAL_INTERNER.intern("Company").unwrap();
    let person_label = GLOBAL_INTERNER.intern("Person").unwrap();

    // Create iterator with "Company" label filter
    let iter = TemporalNodeScanIterator::new(
        vec![],
        timestamp,
        timestamp,
        historical,
        Some("Company".to_string()),
    );

    let props = PropertyMapBuilder::new().insert("name", "Alice").build();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        person_label,
        props,
        VersionId::new(1).unwrap(),
    );

    // Label doesn't match (Company != Person), should return false
    assert!(!iter.apply_label_filter(&node));
}

#[test]
fn test_temporal_node_scan_iterator_apply_label_filter_no_filter() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let timestamp: Timestamp = 1000.into();

    // Create iterator with no label filter
    let iter = TemporalNodeScanIterator::new(vec![], timestamp, timestamp, historical, None);

    let label = GLOBAL_INTERNER.intern("AnyLabel").unwrap();
    let props = PropertyMapBuilder::new().build();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        props,
        VersionId::new(1).unwrap(),
    );

    // No filter, should always return true
    assert!(iter.apply_label_filter(&node));
}

#[test]
fn test_temporal_node_scan_iterator_filter_node_success() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let node_id = NodeId::new(1).unwrap();
    let version_id = VersionId::new(100).unwrap();
    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let timestamp: Timestamp = 1000.into();

    let props = PropertyMapBuilder::new().insert("name", "Alice").build();

    {
        let mut hist = historical.write();
        hist.add_node_version(
            node_id, version_id, timestamp, timestamp, label, props, false,
        )
        .unwrap();
    }

    // Test filter_node orchestrator with matching label
    let iter = TemporalNodeScanIterator::new(
        vec![node_id],
        timestamp,
        timestamp,
        historical.clone(),
        Some("Person".to_string()),
    );

    let guard = historical.read();
    let result = iter.filter_node(node_id, &guard);

    // Should return Some(Ok(QueryRow)) for matching node
    assert!(result.is_some());
    let query_row = result.unwrap();
    assert!(query_row.is_ok());
    assert_eq!(query_row.unwrap().entity.node_id(), Some(node_id));
}

#[test]
fn test_temporal_node_scan_iterator_filter_node_label_mismatch() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let node_id = NodeId::new(1).unwrap();
    let version_id = VersionId::new(100).unwrap();
    // Intern both labels before use
    let _company_label = GLOBAL_INTERNER.intern("Company").unwrap();
    let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
    let timestamp: Timestamp = 1000.into();

    let props = PropertyMapBuilder::new().insert("name", "Alice").build();

    {
        let mut hist = historical.write();
        hist.add_node_version(
            node_id,
            version_id,
            timestamp,
            timestamp,
            person_label,
            props,
            false, // not a tombstone
        )
        .unwrap();
    }

    // Test filter_node with non-matching label
    let iter = TemporalNodeScanIterator::new(
        vec![node_id],
        timestamp,
        timestamp,
        historical.clone(),
        Some("Company".to_string()), // Different label
    );

    let guard = historical.read();
    let result = iter.filter_node(node_id, &guard);

    // Should return None when label doesn't match
    assert!(result.is_none());
}

#[test]
fn test_temporal_node_scan_iterator_filter_node_not_found() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let node_id = NodeId::new(999).unwrap();
    let timestamp: Timestamp = 1000.into();

    let iter = TemporalNodeScanIterator::new(
        vec![node_id],
        timestamp,
        timestamp,
        historical.clone(),
        None,
    );

    let guard = historical.read();
    let result = iter.filter_node(node_id, &guard);

    // Should return Some(Err(...)) when node not found
    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn test_temporal_node_scan_iterator_full_iteration() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let timestamp: Timestamp = 5000.into();

    // Add 3 Person nodes and 1 Company node
    {
        let mut hist = historical.write();
        for i in 1..=3 {
            let node_id = NodeId::new(i).unwrap();
            let version_id = VersionId::new(i * 100).unwrap();
            let label = GLOBAL_INTERNER.intern("Person").unwrap();

            let props = PropertyMapBuilder::new()
                .insert("name", format!("Person{}", i).as_str())
                .build();

            hist.add_node_version(
                node_id, version_id, timestamp, timestamp, label, props, false,
            )
            .unwrap();
        }

        // Add Company node
        let company_label = GLOBAL_INTERNER.intern("Company").unwrap();
        hist.add_node_version(
            NodeId::new(4).unwrap(),
            VersionId::new(400).unwrap(),
            timestamp,
            timestamp,
            company_label,
            PropertyMapBuilder::new().insert("name", "Acme").build(),
            false, // not a tombstone
        )
        .unwrap();
    }

    // Iterate with "Person" filter - should get 3 results
    let node_ids = vec![
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        NodeId::new(3).unwrap(),
        NodeId::new(4).unwrap(),
    ];

    let mut iter = TemporalNodeScanIterator::new(
        node_ids,
        timestamp,
        timestamp,
        historical.clone(),
        Some("Person".to_string()),
    );

    let mut count = 0;
    while let Some(result) = iter.next() {
        assert!(result.is_ok());
        count += 1;
    }

    assert_eq!(count, 3); // Only Person nodes, not Company
}

#[test]
fn test_temporal_node_scan_iterator_no_label_filter() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let timestamp: Timestamp = 5000.into();

    // Add 2 nodes with different labels
    {
        let mut hist = historical.write();

        let person_label = GLOBAL_INTERNER.intern("Person").unwrap();
        hist.add_node_version(
            NodeId::new(1).unwrap(),
            VersionId::new(100).unwrap(),
            timestamp,
            timestamp,
            person_label,
            PropertyMapBuilder::new().insert("name", "Alice").build(),
            false, // not a tombstone
        )
        .unwrap();

        let company_label = GLOBAL_INTERNER.intern("Company").unwrap();
        hist.add_node_version(
            NodeId::new(2).unwrap(),
            VersionId::new(200).unwrap(),
            timestamp,
            timestamp,
            company_label,
            PropertyMapBuilder::new().insert("name", "Acme").build(),
            false, // not a tombstone
        )
        .unwrap();
    }

    // Iterate without label filter - should get all nodes
    let node_ids = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];

    let mut iter = TemporalNodeScanIterator::new(node_ids, timestamp, timestamp, historical, None);

    let mut count = 0;
    while let Some(result) = iter.next() {
        assert!(result.is_ok());
        count += 1;
    }

    assert_eq!(count, 2); // Both nodes returned
}

#[test]
fn test_temporal_node_scan_iterator_size_hint() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let timestamp: Timestamp = 1000.into();

    let node_ids = vec![
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        NodeId::new(3).unwrap(),
    ];

    let iter = TemporalNodeScanIterator::new(node_ids, timestamp, timestamp, historical, None);

    let (lower, upper) = iter.size_hint();
    assert_eq!(lower, 3);
    assert_eq!(upper, Some(3));
}

#[test]
fn test_temporal_node_scan_iterator_empty() {
    use crate::storage::historical::HistoricalStorage;

    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let timestamp: Timestamp = 1000.into();

    let mut iter = TemporalNodeScanIterator::new(vec![], timestamp, timestamp, historical, None);

    assert!(iter.next().is_none());
}

#[test]
fn test_vector_rerank_heap_logic() {
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    // This test verifies that the heap logic correctly maintains the top-k items
    // and orders them correctly (descending score).

    let current = Arc::new(CurrentStorage::new());
    // Enable vector index
    current
        .enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();

    // Create 5 nodes with predictable embeddings/scores relative to query [1,0,0,0]
    // Node 1: [1,0,0,0] -> score 1.0 (Best)
    // Node 2: [0,1,0,0] -> score 0.0
    // Node 3: [0.5, 0.866, 0, 0] -> score 0.5
    // Node 4: [0.8, 0.6, 0, 0] -> score 0.8
    // Node 5: [-1, 0, 0, 0] -> score -1.0 (Worst)

    let create_node = |name: &str, vec: Vec<f32>| {
        let props = PropertyMapBuilder::new()
            .insert("name", name)
            .insert_vector("embedding", &vec)
            .build();
        current.create_node("Person", props).unwrap()
    };

    let n1 = create_node("N1", vec![1.0, 0.0, 0.0, 0.0]);
    let n2 = create_node("N2", vec![0.0, 1.0, 0.0, 0.0]);
    let n3 = create_node("N3", vec![0.5, 0.866, 0.0, 0.0]);
    let n4 = create_node("N4", vec![0.8, 0.6, 0.0, 0.0]);
    let n5 = create_node("N5", vec![-1.0, 0.0, 0.0, 0.0]);

    // Case 1: k=3. Expect top 3: N1 (1.0), N4 (0.8), N3 (0.5)
    let nodes = vec![n1, n2, n3, n4, n5];
    let input = Box::new(NodeLookupIterator::new(nodes.clone(), current.clone()));
    let query_embedding: Arc<[f32]> = vec![1.0, 0.0, 0.0, 0.0].into();

    let mut rerank =
        VectorRerankIterator::new(input, query_embedding.clone(), 3, current.clone(), None);

    let mut results = Vec::new();
    while let Some(Ok(row)) = rerank.next() {
        results.push(row);
    }

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].entity.node_id(), Some(n1)); // 1.0
    assert_eq!(results[1].entity.node_id(), Some(n4)); // 0.8
    assert_eq!(results[2].entity.node_id(), Some(n3)); // 0.5

    // Case 2: k=1. Expect top 1: N1
    let input = Box::new(NodeLookupIterator::new(nodes.clone(), current.clone()));
    let mut rerank =
        VectorRerankIterator::new(input, query_embedding.clone(), 1, current.clone(), None);
    let mut results = Vec::new();
    while let Some(Ok(row)) = rerank.next() {
        results.push(row);
    }
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entity.node_id(), Some(n1));

    // Case 3: k=10 (more than available). Expect all 5 sorted.
    let input = Box::new(NodeLookupIterator::new(nodes.clone(), current.clone()));
    let mut rerank =
        VectorRerankIterator::new(input, query_embedding.clone(), 10, current.clone(), None);
    let mut results = Vec::new();
    while let Some(Ok(row)) = rerank.next() {
        results.push(row);
    }
    assert_eq!(results.len(), 5);
    assert_eq!(results[0].entity.node_id(), Some(n1));
    assert_eq!(results[4].entity.node_id(), Some(n5));

    // Case 4: k=0. Expect 0 results.
    let input = Box::new(NodeLookupIterator::new(nodes.clone(), current.clone()));
    let mut rerank =
        VectorRerankIterator::new(input, query_embedding.clone(), 0, current.clone(), None);
    let mut results = Vec::new();
    while let Some(Ok(row)) = rerank.next() {
        results.push(row);
    }
    assert_eq!(results.len(), 0);
}
