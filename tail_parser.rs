    #[test]
    fn test_parse_similar_with_metric() {
        let query = Parser::parse("SIMILAR TO $emb USING COSINE LIMIT 10").unwrap();

        if let SourceClause::VectorSearch { metric, .. } = &query.source {
            assert_eq!(*metric, Some(DistanceMetric::Cosine));
        }
    }

    #[test]
    fn test_parse_find_similar() {
        let query = Parser::parse("FIND SIMILAR TO (node_id) LIMIT 10").unwrap();

        assert!(matches!(query.source, SourceClause::FindSimilar { .. }));
    }

    // =====================================================
    // Temporal Tests
    // =====================================================

    #[test]
    fn test_parse_as_of_string() {
        let query = Parser::parse("AS OF '2024-01-15T10:00:00Z' MATCH (n) RETURN n").unwrap();

        assert!(query.temporal.is_some());
        if let Some(TemporalClause::AsOf {
            valid_time,
            transaction_time,
        }) = &query.temporal
        {
            assert!(matches!(valid_time, TimestampLiteral::String(_)));
            assert!(transaction_time.is_none());
        }
    }

    #[test]
    fn test_parse_as_of_bitemporal() {
        let query = Parser::parse("AS OF '2024-01-15', 1705315200000 MATCH (n) RETURN n").unwrap();

        if let Some(TemporalClause::AsOf {
            valid_time,
            transaction_time,
        }) = &query.temporal
        {
            assert!(matches!(valid_time, TimestampLiteral::String(_)));
            assert!(transaction_time.is_some());
            assert!(matches!(
                transaction_time.as_ref().unwrap(),
                TimestampLiteral::Integer(_)
            ));
        }
    }

    #[test]
    fn test_parse_between() {
        let query =
            Parser::parse("BETWEEN '2024-01-01' AND '2024-12-31' MATCH (n) RETURN n").unwrap();

        assert!(matches!(
            query.temporal,
            Some(TemporalClause::Between { .. })
        ));
    }

    // =====================================================
    // WHERE Clause Tests
    // =====================================================

    #[test]
    fn test_parse_where_equality() {
        let query = Parser::parse("MATCH (n) WHERE n.name = 'Alice' RETURN n").unwrap();

        assert!(query.where_clause.is_some());
        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::Comparison { .. }));
        }
    }

    #[test]
    fn test_parse_where_comparison() {
        let query = Parser::parse("MATCH (n) WHERE n.age > 18 RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause
            && let PredicateExpr::Comparison { op, .. } = predicate
        {
            assert_eq!(*op, ComparisonOp::Gt);
        }
    }

    #[test]
    fn test_parse_where_and() {
        let query =
            Parser::parse("MATCH (n) WHERE n.age > 18 AND n.name = 'Alice' RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::And(_, _)));
        }
    }

    #[test]
    fn test_parse_where_or() {
        let query = Parser::parse("MATCH (n) WHERE n.age = 20 OR n.age = 30 RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::Or(_, _)));
        }
    }

    #[test]
    fn test_parse_where_not() {
        let query = Parser::parse("MATCH (n) WHERE NOT n.active = true RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::Not(_)));
        }
    }

    #[test]
    fn test_parse_where_is_null() {
        let query = Parser::parse("MATCH (n) WHERE n.email IS NULL RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::IsNull(_)));
        }
    }

    #[test]
    fn test_parse_where_is_not_null() {
        let query = Parser::parse("MATCH (n) WHERE n.email IS NOT NULL RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::IsNotNull(_)));
        }
    }

    #[test]
    fn test_parse_where_contains() {
        let query = Parser::parse("MATCH (n) WHERE n.name CONTAINS 'Ali' RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::Contains { .. }));
        }
    }

    #[test]
    fn test_parse_where_starts_with() {
        let query = Parser::parse("MATCH (n) WHERE n.name STARTS WITH 'A' RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::StartsWith { .. }));
        }
    }

    #[test]
    fn test_parse_where_in() {
        let query = Parser::parse("MATCH (n) WHERE n.age IN [20, 30, 40] RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause
            && let PredicateExpr::In { values, .. } = predicate
        {
            assert_eq!(values.len(), 3);
        }
    }

    #[test]
    fn test_parse_where_grouped() {
        let query =
            Parser::parse("MATCH (n) WHERE (n.a = 1 OR n.b = 2) AND n.c = 3 RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::And(_, _)));
        }
    }

    // =====================================================
    // RANK BY SIMILARITY Tests
    // =====================================================

    #[test]
    fn test_parse_rank_by_similarity() {
        let query = Parser::parse(
            "MATCH (a:Person)-[:KNOWS]->(b) RANK BY SIMILARITY TO $embedding TOP 10 RETURN b",
        )
        .unwrap();

        assert!(query.rank.is_some());
        if let Some(rank) = &query.rank {
            assert!(matches!(rank.embedding, EmbeddingRef::Parameter(_)));
            assert_eq!(rank.top_k, Some(10));
        }
    }

    // =====================================================
    // RETURN Clause Tests
    // =====================================================

    #[test]
    fn test_parse_return_multiple() {
        let query = Parser::parse("MATCH (n) RETURN n.name, n.age").unwrap();

        if let Some(ret) = &query.return_clause {
            assert_eq!(ret.items.len(), 2);
        }
    }

    #[test]
    fn test_parse_return_with_alias() {
        let query = Parser::parse("MATCH (n) RETURN n.name AS name, n.age AS years").unwrap();

        if let Some(ret) = &query.return_clause {
            assert_eq!(ret.items[0].alias, Some("name".to_string()));
            assert_eq!(ret.items[1].alias, Some("years".to_string()));
        }
    }

    #[test]
    fn test_parse_return_distinct() {
        let query = Parser::parse("MATCH (n) RETURN DISTINCT n.name").unwrap();

        if let Some(ret) = &query.return_clause {
            assert!(ret.distinct);
        }
    }

    #[test]
    fn test_parse_return_count() {
        let query = Parser::parse("MATCH (n) RETURN COUNT(*)").unwrap();

        if let Some(ret) = &query.return_clause {
            assert_eq!(ret.items.len(), 1);
            if let Expression::FunctionCall { name, args } = &ret.items[0].expression {
                assert_eq!(name, "COUNT");
                assert_eq!(args.len(), 1);
                // COUNT(*) should have "*" as the argument
                assert!(matches!(&args[0], Expression::Identifier(s) if s == "*"));
            } else {
                panic!("Expected FunctionCall");
            }
        } else {
            panic!("Expected return clause");
        }
    }

    #[test]
    fn test_parse_return_count_expression() {
        let query = Parser::parse("MATCH (n) RETURN COUNT(n)").unwrap();

        if let Some(ret) = &query.return_clause {
            assert_eq!(ret.items.len(), 1);
            if let Expression::FunctionCall { name, args } = &ret.items[0].expression {
                assert_eq!(name, "COUNT");
                assert_eq!(args.len(), 1);
                // COUNT(n) should have "n" as the argument
                assert!(matches!(&args[0], Expression::Identifier(s) if s == "n"));
            } else {
                panic!("Expected FunctionCall");
            }
        } else {
            panic!("Expected return clause");
        }
    }

    // =====================================================
    // ORDER BY Tests
    // =====================================================

    #[test]
    fn test_parse_order_by() {
        let query = Parser::parse("MATCH (n) RETURN n ORDER BY n.age DESC").unwrap();

        assert!(query.order.is_some());
        if let Some(order) = &query.order {
            assert_eq!(order.items.len(), 1);
            assert!(order.items[0].descending);
        }
    }

    #[test]
    fn test_parse_order_by_multiple() {
        let query = Parser::parse("MATCH (n) RETURN n ORDER BY n.age DESC, n.name ASC").unwrap();

        if let Some(order) = &query.order {
            assert_eq!(order.items.len(), 2);
            assert!(order.items[0].descending);
            assert!(!order.items[1].descending);
        }
    }

    // =====================================================
    // SKIP/LIMIT Tests
    // =====================================================

    #[test]
    fn test_parse_limit() {
        let query = Parser::parse("MATCH (n) RETURN n LIMIT 10").unwrap();
        assert_eq!(query.limit, Some(10));
    }

    #[test]
    fn test_parse_skip() {
        let query = Parser::parse("MATCH (n) RETURN n SKIP 5 LIMIT 10").unwrap();
        assert_eq!(query.skip, Some(5));
        assert_eq!(query.limit, Some(10));
    }

    // =====================================================
    // Hybrid Query Tests
    // =====================================================

    #[test]
    fn test_parse_full_hybrid_query() {
        let query = Parser::parse(
            "AS OF '2024-01-01' MATCH (a:Person)-[:KNOWS]->(b) \
             RANK BY SIMILARITY TO $emb TOP 10 \
             WHERE b.age > 18 \
             RETURN b.name, b.age \
             ORDER BY b.age DESC \
             SKIP 5 LIMIT 10",
        )
        .unwrap();

        assert!(query.temporal.is_some());
        assert!(matches!(query.source, SourceClause::Match(_)));
        assert!(query.rank.is_some());
        assert!(query.where_clause.is_some());
        assert!(query.return_clause.is_some());
        assert!(query.order.is_some());
        assert_eq!(query.skip, Some(5));
        assert_eq!(query.limit, Some(10));
    }

    // =====================================================
    // Error Tests
    // =====================================================

    #[test]
    fn test_parse_error_missing_source() {
        let result = Parser::parse("WHERE n.age > 18");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_invalid_pattern() {
        let result = Parser::parse("MATCH n RETURN n");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_unclosed_paren() {
        let result = Parser::parse("MATCH (n:Person RETURN n");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_missing_label() {
        let result = Parser::parse("MATCH (n:) RETURN n");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_negative_limit() {
        let result = Parser::parse("MATCH (n) RETURN n LIMIT -10");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("non-negative"));
    }

    #[test]
    fn test_parse_error_negative_skip() {
        let result = Parser::parse("MATCH (n) RETURN n SKIP -5");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_empty_embedding() {
        let result = Parser::parse("SIMILAR TO [] LIMIT 10");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("cannot be empty"));
    }

    #[test]
    fn test_parse_error_negative_depth() {
        let result = Parser::parse("MATCH (a)-[:KNOWS*-5]->(b) RETURN b");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("non-negative"));
    }

    #[test]
    fn test_parse_error_invalid_depth_range() {
        let result = Parser::parse("MATCH (a)-[:KNOWS*10..5]->(b) RETURN b");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Invalid depth range"));
    }

    #[test]
    fn test_parse_error_contains_on_non_property() {
        let result = Parser::parse("MATCH (n) WHERE 'hello' CONTAINS 'ell' RETURN n");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("property expression"));
    }

    #[test]
    fn test_parse_error_in_on_non_property() {
        let result = Parser::parse("MATCH (n) WHERE 5 IN [1, 2, 3] RETURN n");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("property expression"));
    }

    // =====================================================
    // Coverage Tests
    // =====================================================

    #[test]
    fn test_parse_error_traits() {
        let err1 = ParseError {
            message: "msg".to_string(),
            position: 0,
            expected: None,
            found: None,
        };
        // Test Clone
        let err2 = err1.clone();
        // Test PartialEq
        assert_eq!(err1, err2);
        // Test Debug
        let debug_str = format!("{:?}", err1);
        assert!(debug_str.contains("ParseError"));
        assert!(debug_str.contains("msg"));
    }

    #[test]
    fn test_parse_error_display_full() {
        // Case 1: All fields present
        let err = ParseError {
            message: "Error".to_string(),
            position: 1,
            expected: Some("exp".to_string()),
            found: Some(Token::Eof),
        };
        let s = format!("{}", err);
        assert!(s.contains("Parse error at position 1: Error"));
        assert!(s.contains("(expected exp)"));
        assert!(s.contains("(found EOF)"));

        // Case 2: No expected, no found
        let err = ParseError {
            message: "Error".to_string(),
            position: 1,
            expected: None,
            found: None,
        };
        let s = format!("{}", err);
        assert_eq!(s, "Parse error at position 1: Error");

        // Case 3: Expected only
        let err = ParseError {
            message: "Error".to_string(),
            position: 1,
            expected: Some("exp".to_string()),
            found: None,
        };
        let s = format!("{}", err);
        assert!(s.contains("(expected exp)"));
        assert!(!s.contains("(found"));

        // Case 4: Found only
        let err = ParseError {
            message: "Error".to_string(),
            position: 1,
            expected: None,
            found: Some(Token::Eof),
        };
        let s = format!("{}", err);
        assert!(!s.contains("(expected"));
        assert!(s.contains("(found EOF)"));
    }

    #[test]
    fn test_relationship_direction_half_open() {
        // Test fallback for missing end arrow: -[]
        let query = Parser::parse("MATCH (a)-[:REL](b) RETURN a").unwrap();
        if let SourceClause::Match(patterns) = &query.source
            && let PatternElement::Relationship(rel) = &patterns[0].elements[1]
        {
            // Should default to Both
            assert_eq!(rel.direction, RelationshipDirection::Both);
        }
    }

    #[test]
    fn test_depth_range_negative() {
        // Test validate_non_negative
        let result = Parser::parse("MATCH (a)-[:REL*-5]->(b) RETURN a");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("non-negative"));
    }

    #[test]
    fn test_depth_range_negative_max() {
        // Test validate_non_negative in max position
        let result = Parser::parse("MATCH (a)-[:REL*1..-5]->(b) RETURN a");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("non-negative"));
    }

    #[test]
    fn test_depth_range_inverted() {
        // Test parse_depth_range min > max
        let result = Parser::parse("MATCH (a)-[:REL*5..1]->(b) RETURN a");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Invalid depth range"));
    }

    #[test]
    fn test_expression_error_unexpected() {
        // Test parse_expression fallback
        let result = Parser::parse("MATCH (n) WHERE )");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Expected expression"));
    }

    #[test]
    fn test_parse_error_from_lexer_error() {
        // Trigger a lexer error (invalid character)
        let result = Parser::parse("MATCH @");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Unexpected character"));
        // This implicitly tests From<LexerError> for ParseError
    }
}
