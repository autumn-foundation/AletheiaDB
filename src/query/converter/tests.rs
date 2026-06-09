#[allow(unused_imports)]
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
    let ast = Parser::parse("MATCH (n) WHERE n.status IN ['active', 'pending'] RETURN n").unwrap();
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
    let ast = Parser::parse("MATCH (n:Document) RANK BY SIMILARITY TO [0.1, 0.2] TOP 5 RETURN n")
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

    let query = super::parse_query_with_params("SIMILAR TO $embedding LIMIT 10", params).unwrap();
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
    let query = super::parse_query("MATCH (n:Person) WHERE n.age > 25 RETURN n LIMIT 10").unwrap();

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
