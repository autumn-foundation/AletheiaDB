    use super::*;
    use crate::query::parser::Parser;

    // ========================================================================
    // RED PHASE: Failing tests for basic conversion
    // ========================================================================

    #[test]
    fn test_convert_simple_match() {
        // MATCH (n:Person) RETURN n
        let ast = Parser::parse("MATCH (n:Person) RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have ScanNodes with label "Person"
        assert!(!query.ops.is_empty());
        assert!(matches!(
            &query.ops[0],
            QueryOp::ScanNodes {
                label: Some(l)
            } if l == "Person"
        ));
    }

    #[test]
    fn test_convert_match_with_traversal() {
        // MATCH (n:Person)-[:KNOWS]->(m) RETURN m
        let ast = Parser::parse("MATCH (n:Person)-[:KNOWS]->(m) RETURN m").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have ScanNodes + TraverseOut
        assert!(query.ops.len() >= 2);
        assert!(matches!(
            &query.ops[0],
            QueryOp::ScanNodes { label: Some(l) } if l == "Person"
        ));
        assert!(matches!(
            &query.ops[1],
            QueryOp::TraverseOut {
                label: Some(l),
                depth: TraversalDepth::Exact(1)
            } if l == "KNOWS"
        ));
    }

    #[test]
    fn test_convert_match_with_where() {
        // MATCH (n:Person) WHERE n.age > 25 RETURN n
        let ast = Parser::parse("MATCH (n:Person) WHERE n.age > 25 RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have ScanNodes + Filter
        assert!(query.ops.len() >= 2);
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::Gt { key, .. } if key == "age"));
        }
    }

    #[test]
    fn test_convert_match_with_limit() {
        // MATCH (n:Person) RETURN n LIMIT 10
        let ast = Parser::parse("MATCH (n:Person) RETURN n LIMIT 10").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have Limit operation
        let limit_op = query.ops.iter().find(|op| matches!(op, QueryOp::Limit(_)));
        assert!(limit_op.is_some());
        if let Some(QueryOp::Limit(n)) = limit_op {
            assert_eq!(*n, 10);
        }
    }

    #[test]
    fn test_convert_match_with_skip() {
        // MATCH (n:Person) RETURN n SKIP 5 LIMIT 10
        let ast = Parser::parse("MATCH (n:Person) RETURN n SKIP 5 LIMIT 10").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have Skip operation
        let skip_op = query.ops.iter().find(|op| matches!(op, QueryOp::Skip(_)));
        assert!(skip_op.is_some());
        if let Some(QueryOp::Skip(n)) = skip_op {
            assert_eq!(*n, 5);
        }
    }

    #[test]
    fn test_convert_vector_search() {
        // SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10
        let ast = Parser::parse("SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have VectorSearch operation
        assert!(matches!(&query.ops[0], QueryOp::VectorSearch { k: 10, .. }));
    }

    #[test]
    fn test_convert_find_similar_with_parameter() {
        // FIND SIMILAR TO ($node_id) LIMIT 5
        let ast = Parser::parse("FIND SIMILAR TO ($node_id) LIMIT 5").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("node_id", ParameterValue::NodeId(NodeId::new(42).unwrap()));
        let query = converter.convert(&ast).unwrap();

        // Should have SimilarTo operation
        assert!(matches!(
            &query.ops[0],
            QueryOp::SimilarTo {
                source_node,
                k: 5,
                ..
            } if source_node.as_u64() == 42
        ));
    }

    #[test]
    fn test_convert_temporal_as_of() {
        // AS OF 1000 MATCH (n:Person) RETURN n
        let ast = Parser::parse("AS OF 1000 MATCH (n:Person) RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have temporal context
        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.unwrap();
        let as_of_tuple = ctx.as_of_tuple();
        assert!(as_of_tuple.is_some());
        let (vt, _tt) = as_of_tuple.unwrap();
        // Timestamp is stored as microseconds, so 1000 is 1000 microseconds
        assert_eq!(vt.wallclock(), 1000);
    }

    #[test]
    fn test_convert_temporal_between() {
        // BETWEEN 1000 AND 2000 MATCH (n:Person) RETURN n
        let ast = Parser::parse("BETWEEN 1000 AND 2000 MATCH (n:Person) RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have temporal context with between
        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.unwrap();
        assert!(ctx.valid_time_between.is_some());
    }

    #[test]
    fn test_convert_predicate_and() {
        // MATCH (n) WHERE n.a = 1 AND n.b = 2 RETURN n
        let ast = Parser::parse("MATCH (n) WHERE n.a = 1 AND n.b = 2 RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::And(_)));
        }
    }

    #[test]
    fn test_convert_predicate_or() {
        // MATCH (n) WHERE n.a = 1 OR n.b = 2 RETURN n
        let ast = Parser::parse("MATCH (n) WHERE n.a = 1 OR n.b = 2 RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::Or(_)));
        }
    }

    #[test]
    fn test_convert_predicate_not() {
        // MATCH (n) WHERE NOT n.active = true RETURN n
        let ast = Parser::parse("MATCH (n) WHERE NOT n.active = true RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::Not(_)));
        }
    }

    #[test]
    fn test_convert_predicate_contains() {
        // MATCH (n) WHERE n.name CONTAINS 'test' RETURN n
        let ast = Parser::parse("MATCH (n) WHERE n.name CONTAINS 'test' RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::Contains { key, substring } if key == "name" && substring == "test"
            ));
        }
    }

    #[test]
    fn test_convert_predicate_starts_with() {
        // MATCH (n) WHERE n.name STARTS WITH 'Al' RETURN n
        let ast = Parser::parse("MATCH (n) WHERE n.name STARTS WITH 'Al' RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::StartsWith { key, prefix } if key == "name" && prefix == "Al"
            ));
        }
    }

    #[test]
    fn test_convert_predicate_in() {
        // MATCH (n) WHERE n.status IN ['active', 'pending'] RETURN n
        let ast =
            Parser::parse("MATCH (n) WHERE n.status IN ['active', 'pending'] RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(
                matches!(pred, Predicate::In { key, values } if key == "status" && values.len() == 2)
            );
        }
    }

    #[test]
    fn test_convert_variable_length_traversal() {
        // MATCH (n)-[:KNOWS*1..3]->(m) RETURN m
        let ast = Parser::parse("MATCH (n)-[:KNOWS*1..3]->(m) RETURN m").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let traverse_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::TraverseOut { .. }));
        assert!(traverse_op.is_some());
        if let Some(QueryOp::TraverseOut { depth, .. }) = traverse_op {
            assert!(matches!(depth, TraversalDepth::Range { min: 1, max: 3 }));
        }
    }

    #[test]
    fn test_convert_incoming_traversal() {
        // MATCH (n)<-[:KNOWS]-(m) RETURN m
        let ast = Parser::parse("MATCH (n)<-[:KNOWS]-(m) RETURN m").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let traverse_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::TraverseIn { .. }));
        assert!(traverse_op.is_some());
    }

    #[test]
    fn test_convert_bidirectional_traversal() {
        // MATCH (n)-[:KNOWS]-(m) RETURN m
        let ast = Parser::parse("MATCH (n)-[:KNOWS]-(m) RETURN m").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let traverse_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::TraverseBoth { .. }));
        assert!(traverse_op.is_some());
    }

    #[test]
    fn test_convert_rank_by_similarity() {
        // MATCH (n:Document) RANK BY SIMILARITY TO [0.1, 0.2] TOP 5 RETURN n
        let ast =
            Parser::parse("MATCH (n:Document) RANK BY SIMILARITY TO [0.1, 0.2] TOP 5 RETURN n")
                .unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let rank_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::RankBySimilarity { .. }));
        assert!(rank_op.is_some());
        if let Some(QueryOp::RankBySimilarity { top_k, .. }) = rank_op {
            assert_eq!(*top_k, Some(5));
        }
    }

    #[test]
    fn test_convert_distinct() {
        // MATCH (n) RETURN DISTINCT n
        let ast = Parser::parse("MATCH (n) RETURN DISTINCT n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let distinct_op = query.ops.iter().find(|op| matches!(op, QueryOp::Distinct));
        assert!(distinct_op.is_some());
    }

    #[test]
    fn test_convert_with_embedding_parameter() {
        // SIMILAR TO $embedding LIMIT 10
        let ast = Parser::parse("SIMILAR TO $embedding LIMIT 10").unwrap();
        let mut converter = AstConverter::new();
        converter.bind(
            "embedding",
            ParameterValue::Embedding(Arc::from([0.1f32, 0.2, 0.3].as_slice())),
        );
        let query = converter.convert(&ast).unwrap();

        assert!(matches!(&query.ops[0], QueryOp::VectorSearch { k: 10, .. }));
    }

    #[test]
    fn test_convert_error_missing_parameter() {
        // SIMILAR TO $embedding LIMIT 10 (without binding)
        let ast = Parser::parse("SIMILAR TO $embedding LIMIT 10").unwrap();
        let converter = AstConverter::new();
        let result = converter.convert(&ast);

        assert!(result.is_err());
    }

    // ========================================================================
    // Convenience function tests
    // ========================================================================

    #[test]
    fn test_parse_query() {
        let query = super::parse_query("MATCH (n:Person) RETURN n").unwrap();
        assert!(!query.ops.is_empty());
    }

    #[test]
    fn test_parse_query_with_params() {
        use std::collections::HashMap;

        let mut params = HashMap::new();
        params.insert(
            "embedding".to_string(),
            ParameterValue::Embedding(Arc::from([0.1f32, 0.2, 0.3].as_slice())),
        );

        let query =
            super::parse_query_with_params("SIMILAR TO $embedding LIMIT 10", params).unwrap();
        assert!(matches!(&query.ops[0], QueryOp::VectorSearch { k: 10, .. }));
    }

    // ========================================================================
    // Planner integration tests
    // ========================================================================

    #[test]
    fn test_planner_integration_simple_match() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert
        let query = super::parse_query("MATCH (n:Person) RETURN n LIMIT 10").unwrap();

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query - should succeed
        let result = planner.plan(query);
        assert!(result.is_ok());

        let plan = result.unwrap();
        // Verify the plan has a valid root operation (not empty)
        assert!(!matches!(
            plan.root,
            crate::query::planner::PhysicalOp::Empty
        ));
    }

    #[test]
    fn test_planner_integration_with_traversal() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert
        let query = super::parse_query("MATCH (n:Person)-[:KNOWS]->(m:Person) RETURN m").unwrap();

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query
        let result = planner.plan(query);
        assert!(result.is_ok());
    }

    #[test]
    fn test_planner_integration_with_filter() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert
        let query =
            super::parse_query("MATCH (n:Person) WHERE n.age > 25 RETURN n LIMIT 10").unwrap();

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query
        let result = planner.plan(query);
        assert!(result.is_ok());
    }

    #[test]
    fn test_planner_integration_temporal() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert - temporal query
        let query = super::parse_query("AS OF 1000000 MATCH (n:Person) RETURN n").unwrap();
        assert!(query.temporal_context.is_some());

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query
        let result = planner.plan(query);
        assert!(result.is_ok());

        let plan = result.unwrap();
        // Temporal queries should include temporal context in the plan
        assert!(plan.is_temporal());
    }

    #[test]
    fn test_full_pipeline_parse_convert_plan() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Complex query with multiple operations
        let aql = "MATCH (n:Person)-[:KNOWS*1..3]->(m:Person) WHERE n.age > 21 AND m.active = true RETURN m LIMIT 100";

        // Parse
        let ast = Parser::parse(aql).unwrap();

        // Convert
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Verify conversion produced expected operations
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::ScanNodes { .. }))
        );
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::TraverseOut { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Limit(100))));

        // Plan
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        let plan = planner.plan(query).unwrap();
        // Verify the plan has a valid root operation (not empty)
        assert!(!matches!(
            plan.root,
            crate::query::planner::PhysicalOp::Empty
        ));
    }

    // ========================================================================
    // ORDER BY conversion tests
    // ========================================================================

    #[test]
    fn test_convert_order_by_property() {
        // MATCH (n:Person) RETURN n ORDER BY n.age DESC
        let ast = Parser::parse("MATCH (n:Person) RETURN n ORDER BY n.age DESC").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let sort_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::Sort { .. }));
        assert!(sort_op.is_some(), "Expected Sort operation");
        if let Some(QueryOp::Sort { key, descending }) = sort_op {
            assert!(
                matches!(key, SortKey::Property(p) if p == "age"),
                "Expected property key 'age'"
            );
            assert!(*descending, "Expected descending order");
        }
    }

    #[test]
    fn test_convert_order_by_ascending() {
        // MATCH (n:Person) RETURN n ORDER BY n.name ASC
        let ast = Parser::parse("MATCH (n:Person) RETURN n ORDER BY n.name ASC").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let sort_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::Sort { .. }));
        assert!(sort_op.is_some(), "Expected Sort operation");
        if let Some(QueryOp::Sort { key, descending }) = sort_op {
            assert!(
                matches!(key, SortKey::Property(p) if p == "name"),
                "Expected property key 'name'"
            );
            assert!(!*descending, "Expected ascending order");
        }
    }

    #[test]
    fn test_convert_order_by_score() {
        // SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10 ORDER BY score DESC
        let ast = Parser::parse("SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10 ORDER BY score DESC").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let sort_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::Sort { .. }));
        assert!(sort_op.is_some(), "Expected Sort operation");
        if let Some(QueryOp::Sort { key, descending }) = sort_op {
            assert!(matches!(key, SortKey::Score), "Expected Score key");
            assert!(*descending, "Expected descending order");
        }
    }

    #[test]
    fn test_convert_order_by_multiple() {
        // MATCH (n) RETURN n ORDER BY n.age DESC, n.name ASC
        let ast = Parser::parse("MATCH (n) RETURN n ORDER BY n.age DESC, n.name ASC").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let sort_ops: Vec<_> = query
            .ops
            .iter()
            .filter(|op| matches!(op, QueryOp::Sort { .. }))
            .collect();

        assert_eq!(sort_ops.len(), 2, "Expected 2 Sort operations");

        // First sort by age DESC
        if let QueryOp::Sort { key, descending } = sort_ops[0] {
            assert!(
                matches!(key, SortKey::Property(p) if p == "age"),
                "First sort should be by age"
            );
            assert!(*descending, "First sort should be descending");
        }

        // Second sort by name ASC
        if let QueryOp::Sort { key, descending } = sort_ops[1] {
            assert!(
                matches!(key, SortKey::Property(p) if p == "name"),
                "Second sort should be by name"
            );
            assert!(!*descending, "Second sort should be ascending");
        }
    }

    #[test]
    fn test_planner_integration_with_order_by() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert
        let query =
            super::parse_query("MATCH (n:Person) RETURN n ORDER BY n.age DESC LIMIT 10").unwrap();

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query - should succeed
        let result = planner.plan(query);
        assert!(result.is_ok(), "Planning should succeed");

        let plan = result.unwrap();
        assert!(!matches!(
            plan.root,
            crate::query::planner::PhysicalOp::Empty
        ));
    }

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use crate::core::NodeId;
    use crate::query::parser::Parser;

    #[test]
    fn test_start_node_optimization_with_parameter() {
        // 🎯 Target: convert_node_pattern optimization for id lookup
        // 💣 Risk: Optimization missed when using parameters -> full scan -> slow
        // 🧪 Strategy: Bind a parameter for ID and check for QueryOp::StartNode

        let ast = Parser::parse("MATCH (n {id: $id}) RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("id", ParameterValue::NodeId(NodeId::new(123).unwrap()));

        let query = converter.convert(&ast).unwrap();

        // Should optimize to StartNode(123)
        let has_start_node = query.ops.iter().any(|op| match op {
            QueryOp::StartNode(id) => id.as_u64() == 123,
            _ => false,
        });

        assert!(
            has_start_node,
            "Query should be optimized to use StartNode when id is a parameter. Ops: {:?}",
            query.ops
        );
    }

    #[test]
    fn test_comparison_asymmetry() {
        // 🎯 Target: convert_comparison
        // 💣 Risk: Inconvenient API (WHERE 1 = n.a fails)
        // 🧪 Strategy: Try WHERE value = property and assert success

        let ast = Parser::parse("MATCH (n) WHERE 1 = n.age RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast);

        assert!(
            query.is_ok(),
            "Comparison should be symmetric (value = property). Error: {:?}",
            query.err()
        );

        let query = query.unwrap();
        let has_filter = query.ops.iter().any(|op| match op {
            QueryOp::Filter(Predicate::Eq { key, .. }) => key == "age",
            _ => false,
        });
        assert!(has_filter, "Should produce equality filter for age");
    }

    #[test]
    fn test_invalid_node_id_in_match() {
        // 🎯 Target: convert_node_pattern with invalid ID
        // 💣 Risk: Undefined behavior or panic
        // 🧪 Strategy: Use negative ID (invalid for u64 NodeId)

        let ast = Parser::parse("MATCH (n {id: -1}) RETURN n").unwrap();
        let converter = AstConverter::new();
        let result = converter.convert(&ast);

        // Should handle gracefully (error or empty result), but definitely not panic
        // Currently expect error because NodeId::new fails
        assert!(result.is_err());
    }

    #[test]
    fn test_duplicate_property_keys() {
        // 🎯 Target: convert_node_pattern property handling
        // 💣 Risk: Ambiguous behavior
        // 🧪 Strategy: Use duplicate keys

        let ast = Parser::parse("MATCH (n {a: 1, a: 2}) RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should produce two filters
        let filters = query
            .ops
            .iter()
            .filter(|op| matches!(op, QueryOp::Filter(_)))
            .count();
        // convert_node_pattern generates QueryOp::Filter for each property
        assert_eq!(filters, 2);
    }

    #[test]
    fn test_start_node_optimization_preserves_filters() {
        // 🎯 Target: convert_node_pattern optimization correctness
        // 💣 Risk: Optimization drops other filters (e.g. {id: 1, active: true} -> active ignored)
        // 🧪 Strategy: Bind parameter for ID, add another property, verify both StartNode and Filter present

        let ast = Parser::parse("MATCH (n {id: $id, active: true}) RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("id", ParameterValue::NodeId(NodeId::new(123).unwrap()));

        let query = converter.convert(&ast).unwrap();

        let has_start_node = query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::StartNode(_)));
        let has_filter = query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::Filter(Predicate::Eq { key, .. }) if key == "active"));

        assert!(has_start_node, "Should use StartNode");
        assert!(has_filter, "Should preserve 'active' filter");
    }

    #[test]
    fn test_start_node_optimization_with_integer_parameter() {
        // 🎯 Target: convert_node_pattern optimization with Integer value
        // 💣 Risk: Integer parameters (common from JSON) not optimizing
        // 🧪 Strategy: Bind ParameterValue::Value(Int) instead of NodeId

        let ast = Parser::parse("MATCH (n {id: $id}) RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("id", ParameterValue::Value(PredicateValue::Int(123)));

        let query = converter.convert(&ast).unwrap();

        let has_start_node = query.ops.iter().any(|op| match op {
            QueryOp::StartNode(id) => id.as_u64() == 123,
            _ => false,
        });
        assert!(has_start_node, "Should optimize Int parameter to StartNode");
    }

    #[test]
    fn test_symmetric_comparison_operators() {
        // 🎯 Target: convert_comparison operator flipping
        // 💣 Risk: Incorrect logic (e.g., < becomes > instead of >)
        // 🧪 Strategy: Test all inequalities in swapped position

        let cases = vec![
            (
                "10 = n.a",
                Predicate::Eq {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ),
            (
                "10 <> n.a",
                Predicate::Ne {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ),
            (
                "10 > n.a",
                Predicate::Lt {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ), // 10 > a  => a < 10
            (
                "10 >= n.a",
                Predicate::Lte {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ), // 10 >= a => a <= 10
            (
                "10 < n.a",
                Predicate::Gt {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ), // 10 < a  => a > 10
            (
                "10 <= n.a",
                Predicate::Gte {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ), // 10 <= a => a >= 10
        ];

        for (query_str, expected) in cases {
            let full_query = format!("MATCH (n) WHERE {} RETURN n", query_str);
            let ast = Parser::parse(&full_query).unwrap();
            let converter = AstConverter::new();
            let query = converter.convert(&ast).unwrap();

            let found = query.ops.iter().any(|op| {
                if let QueryOp::Filter(pred) = op {
                    *pred == expected
                } else {
                    false
                }
            });
            assert!(
                found,
                "Failed to convert '{}'. Expected {:?}",
                query_str, expected
            );
        }
    }

    #[test]
    fn test_comparison_invalid_syntax() {
        // 🎯 Target: convert_comparison error path
        // 💣 Risk: Panic or incorrect behavior on invalid syntax
        // 🧪 Strategy: Compare two literals

        let ast = Parser::parse("MATCH (n) WHERE 1 = 1 RETURN n").unwrap();
        let converter = AstConverter::new();
        let result = converter.convert(&ast);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Comparison must involve a property")
        );
    }

    #[test]
    fn test_comparison_with_parameter_resolution() {
        // 🎯 Target: expression_to_predicate_value with parameter
        // 💣 Risk: Parameters not resolving in WHERE clause
        // 🧪 Strategy: Use parameter in WHERE

        let ast = Parser::parse("MATCH (n) WHERE n.age = $age RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("age", ParameterValue::Value(PredicateValue::Int(30)));
        let query = converter.convert(&ast).unwrap();

        let found = query.ops.iter().any(|op| {
            matches!(
                op,
                QueryOp::Filter(Predicate::Eq {
                    value: PredicateValue::Int(30),
                    ..
                })
            )
        });
        assert!(found, "Should resolve parameter in comparison");

        // Also test swapped with parameter
        let ast_swapped = Parser::parse("MATCH (n) WHERE $age = n.age RETURN n").unwrap();
        let query_swapped = converter.convert(&ast_swapped).unwrap();
        let found_swapped = query_swapped.ops.iter().any(|op| {
            matches!(
                op,
                QueryOp::Filter(Predicate::Eq {
                    value: PredicateValue::Int(30),
                    ..
                })
            )
        });
        assert!(
            found_swapped,
            "Should resolve parameter in swapped comparison"
        );
    }

    #[test]
    fn test_parameter_not_found_error() {
        // 🎯 Target: expression_to_predicate_value error path
        // 💣 Risk: Silent failure or panic on missing parameter
        // 🧪 Strategy: Use unbound parameter

        let ast = Parser::parse("MATCH (n) WHERE n.age = $missing RETURN n").unwrap();
        let converter = AstConverter::new();
        let result = converter.convert(&ast);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing"));
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_start_node_optimization_preserves_labels() {
        // 🎯 Target: convert_node_pattern optimization security
        // 💣 Risk: Optimization drops label check (e.g. MATCH (n:Secret {id: 1}) returns node 1 even if not Secret)
        // 🧪 Strategy: Bind parameter for ID, include label, verify both StartNode and FilterLabel present

        let ast = Parser::parse("MATCH (n:Secret {id: $id}) RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("id", ParameterValue::NodeId(NodeId::new(123).unwrap()));

        let query = converter.convert(&ast).unwrap();

        let has_start_node = query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::StartNode(_)));
        let has_label_filter = query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::FilterLabel(label) if label == "Secret"));

        assert!(has_start_node, "Should use StartNode");
        assert!(has_label_filter, "Should preserve 'Secret' label check");
    }

    #[test]
    fn test_pagination_order() {
        // 🎯 Target: convert_pagination order (Skip before Limit)
        // 💣 Risk: Semantic change (Skip 5 then Take 10 vs Take 10 then Skip 5)
        // 🧪 Strategy: Parse query with both and check IR order

        let ast = Parser::parse("MATCH (n) RETURN n SKIP 5 LIMIT 10").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let skip_idx = query
            .ops
            .iter()
            .position(|op| matches!(op, QueryOp::Skip(_)));
        let limit_idx = query
            .ops
            .iter()
            .position(|op| matches!(op, QueryOp::Limit(_)));

        assert!(skip_idx.is_some(), "Skip op missing");
        assert!(limit_idx.is_some(), "Limit op missing");

        assert!(
            skip_idx.unwrap() < limit_idx.unwrap(),
            "Skip operation must precede Limit operation"
        );
    }

    #[test]
    fn test_filter_before_project() {
        // 🎯 Target: Pipeline order (Filter before Project)
        // 💣 Risk: Filtering on projected-away columns
        // 🧪 Strategy: Parse query with WHERE and RETURN specific property

        let ast = Parser::parse("MATCH (n) WHERE n.age > 10 RETURN n.name").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_idx = query
            .ops
            .iter()
            .position(|op| matches!(op, QueryOp::Filter(_)));
        let project_idx = query
            .ops
            .iter()
            .position(|op| matches!(op, QueryOp::Project(_)));

        assert!(filter_idx.is_some(), "Filter op missing");
        assert!(project_idx.is_some(), "Project op missing");

        assert!(
            filter_idx.unwrap() < project_idx.unwrap(),
            "Filter operation must precede Project operation"
        );
    }

    #[test]
    fn test_inline_property_filter_does_not_use_start_node() {
        // 🎯 Target: convert_node_pattern "id" check
        // 💣 Risk: Treating other properties as ID -> StartNode optimization -> Wrong results
        // 🧪 Strategy: Match with inline property (not id), ensure StartNode is NOT used.

        let ast = Parser::parse("MATCH (n {age: 30}) RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let has_start_node = query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::StartNode(_)));
        assert!(
            !has_start_node,
            "Should not use StartNode for non-id property"
        );

        // Also ensure it produces a Filter
        let has_filter = query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_)));
        assert!(has_filter, "Should produce Filter for age");
    }

    #[test]
    #[should_panic(expected = "convert_logic_predicate called on non-logic expr")]
    fn test_convert_logic_predicate_unreachable() {
        let converter = AstConverter::new();
        let expr = PredicateExpr::Exists(crate::query::ast::PropertyAccess {
            variable: "n".to_string(),
            property: "prop".to_string(),
        });
        let _ = converter.convert_logic_predicate(&expr);
    }

    #[test]
    #[should_panic(expected = "convert_string_predicate called on non-string expr")]
    fn test_convert_string_predicate_unreachable() {
        let converter = AstConverter::new();
        let expr = PredicateExpr::Exists(crate::query::ast::PropertyAccess {
            variable: "n".to_string(),
            property: "prop".to_string(),
        });
        let _ = converter.convert_string_predicate(&expr);
    }}
