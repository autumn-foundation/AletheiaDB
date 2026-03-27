//! Tests for Cypher query language support.
//!
//! These tests follow TDD methodology -- tests are written first to define
//! expected behavior, then implementation is added to make them pass.

use super::*;

// ============================================================================
// Module scaffolding smoke tests
// ============================================================================

#[test]
fn smoke_test_cypher_error_display() {
    let err = CypherError::ParseError {
        position: 10,
        message: "unexpected token".to_string(),
    };
    assert_eq!(
        err.to_string(),
        "Cypher parse error at position 10: unexpected token"
    );
}

#[test]
fn smoke_test_lex_error() {
    let err = CypherError::LexError {
        position: 0,
        message: "invalid character".to_string(),
    };
    assert_eq!(
        err.to_string(),
        "Cypher lexer error at position 0: invalid character"
    );
}

#[test]
fn smoke_test_unsupported_feature() {
    let err = CypherError::UnsupportedFeature("MERGE".to_string());
    assert_eq!(err.to_string(), "Unsupported Cypher feature: MERGE");
}

#[test]
fn smoke_test_invalid_temporal_clause() {
    let err = CypherError::InvalidTemporalClause("missing timestamp".to_string());
    assert_eq!(
        err.to_string(),
        "Invalid temporal clause: missing timestamp"
    );
}

#[test]
fn smoke_test_invalid_timestamp() {
    let err = CypherError::InvalidTimestamp("not-a-date".to_string());
    assert_eq!(err.to_string(), "Invalid timestamp: not-a-date");
}

#[test]
fn smoke_test_parameter_error() {
    let err = CypherError::ParameterError("$foo not bound".to_string());
    assert_eq!(err.to_string(), "Parameter error: $foo not bound");
}

#[test]
fn smoke_test_semantic_error() {
    let err = CypherError::SemanticError("undefined variable x".to_string());
    assert_eq!(
        err.to_string(),
        "Cypher semantic error: undefined variable x"
    );
}

// ============================================================================
// AST construction tests
// ============================================================================

// CypherParser and all AST types are available via `use super::*;` above.

#[test]
fn test_cypher_ast_basic_match() {
    let ast = CypherStatement::Match {
        optional: false,
        pattern: vec![CypherPattern {
            elements: vec![CypherPatternElement::Node(CypherNodePattern {
                variable: Some("n".into()),
                labels: vec!["Person".into()],
                properties: vec![],
            })],
        }],
        where_clause: None,
        return_clause: CypherReturn {
            distinct: false,
            items: vec![CypherReturnItem::Variable("n".into())],
            order_by: vec![],
            skip: None,
            limit: None,
        },
        temporal: None,
        with_clauses: vec![],
    };
    assert!(matches!(ast, CypherStatement::Match { .. }));
}

#[test]
fn test_cypher_pattern_chain() {
    let pattern = CypherPattern {
        elements: vec![
            CypherPatternElement::Node(CypherNodePattern {
                variable: Some("a".into()),
                labels: vec!["Person".into()],
                properties: vec![],
            }),
            CypherPatternElement::Relationship(CypherRelPattern {
                variable: None,
                rel_types: vec!["KNOWS".into()],
                direction: CypherDirection::Outgoing,
                depth: None,
                properties: vec![],
            }),
            CypherPatternElement::Node(CypherNodePattern {
                variable: Some("b".into()),
                labels: vec![],
                properties: vec![],
            }),
        ],
    };
    assert_eq!(pattern.elements.len(), 3);
}

// ============================================================================
// Parser tests
// ============================================================================

#[test]
fn test_parse_simple_match() {
    let ast = CypherParser::parse("MATCH (n) RETURN n").unwrap();
    match ast {
        CypherStatement::Match {
            pattern,
            return_clause,
            ..
        } => {
            assert_eq!(pattern.len(), 1);
            assert_eq!(pattern[0].elements.len(), 1);
            assert_eq!(return_clause.items.len(), 1);
        }
    }
}

#[test]
fn test_parse_match_with_label() {
    let ast = CypherParser::parse("MATCH (n:Person) RETURN n").unwrap();
    match ast {
        CypherStatement::Match { pattern, .. } => {
            let node = match &pattern[0].elements[0] {
                CypherPatternElement::Node(n) => n,
                _ => panic!("expected node"),
            };
            assert_eq!(node.variable, Some("n".into()));
            assert_eq!(node.labels, vec!["Person".to_string()]);
        }
    }
}

#[test]
fn test_parse_match_with_properties() {
    let ast = CypherParser::parse("MATCH (n:Person {name: 'Alice', age: 30}) RETURN n").unwrap();
    match ast {
        CypherStatement::Match { pattern, .. } => {
            let node = match &pattern[0].elements[0] {
                CypherPatternElement::Node(n) => n,
                _ => panic!("expected node"),
            };
            assert_eq!(node.properties.len(), 2);
            assert_eq!(node.properties[0].0, "name");
            assert_eq!(
                node.properties[0].1,
                CypherValue::String("Alice".to_string())
            );
            assert_eq!(node.properties[1].0, "age");
            assert_eq!(node.properties[1].1, CypherValue::Int(30));
        }
    }
}

#[test]
fn test_parse_traversal() {
    let ast = CypherParser::parse("MATCH (a:Person)-[:KNOWS]->(b) RETURN b").unwrap();
    match ast {
        CypherStatement::Match { pattern, .. } => {
            assert_eq!(pattern[0].elements.len(), 3);
            let rel = match &pattern[0].elements[1] {
                CypherPatternElement::Relationship(r) => r,
                _ => panic!("expected relationship"),
            };
            assert_eq!(rel.rel_types, vec!["KNOWS".to_string()]);
            assert_eq!(rel.direction, CypherDirection::Outgoing);
        }
    }
}

#[test]
fn test_parse_incoming_relationship() {
    let ast = CypherParser::parse("MATCH (a)<-[:FOLLOWS]-(b) RETURN a").unwrap();
    match ast {
        CypherStatement::Match { pattern, .. } => {
            let rel = match &pattern[0].elements[1] {
                CypherPatternElement::Relationship(r) => r,
                _ => panic!("expected relationship"),
            };
            assert_eq!(rel.direction, CypherDirection::Incoming);
        }
    }
}

#[test]
fn test_parse_bidirectional_relationship() {
    let ast = CypherParser::parse("MATCH (a)-[:KNOWS]-(b) RETURN a").unwrap();
    match ast {
        CypherStatement::Match { pattern, .. } => {
            let rel = match &pattern[0].elements[1] {
                CypherPatternElement::Relationship(r) => r,
                _ => panic!("expected relationship"),
            };
            assert_eq!(rel.direction, CypherDirection::Both);
        }
    }
}

#[test]
fn test_parse_variable_length_path() {
    let ast = CypherParser::parse("MATCH (a)-[:KNOWS*1..3]->(b) RETURN b").unwrap();
    match ast {
        CypherStatement::Match { pattern, .. } => {
            let rel = match &pattern[0].elements[1] {
                CypherPatternElement::Relationship(r) => r,
                _ => panic!("expected relationship"),
            };
            assert_eq!(rel.depth, Some(CypherDepth::Range { min: 1, max: 3 }));
        }
    }
}

#[test]
fn test_parse_unbounded_path() {
    let ast = CypherParser::parse("MATCH (a)-[:KNOWS*]->(b) RETURN b").unwrap();
    match ast {
        CypherStatement::Match { pattern, .. } => {
            let rel = match &pattern[0].elements[1] {
                CypherPatternElement::Relationship(r) => r,
                _ => panic!("expected relationship"),
            };
            assert_eq!(rel.depth, Some(CypherDepth::Unbounded));
        }
    }
}

#[test]
fn test_parse_where_clause() {
    let ast = CypherParser::parse("MATCH (n:Person) WHERE n.age > 18 RETURN n").unwrap();
    match ast {
        CypherStatement::Match { where_clause, .. } => {
            assert!(where_clause.is_some());
        }
    }
}

#[test]
fn test_parse_where_and() {
    let ast =
        CypherParser::parse("MATCH (n:Person) WHERE n.age > 18 AND n.name = 'Alice' RETURN n")
            .unwrap();
    match ast {
        CypherStatement::Match { where_clause, .. } => {
            assert!(matches!(where_clause, Some(CypherExpr::And(_, _))));
        }
    }
}

#[test]
fn test_parse_limit() {
    let ast = CypherParser::parse("MATCH (n) RETURN n LIMIT 10").unwrap();
    match ast {
        CypherStatement::Match { return_clause, .. } => {
            assert_eq!(return_clause.limit, Some(10));
        }
    }
}

#[test]
fn test_parse_skip_limit() {
    let ast = CypherParser::parse("MATCH (n) RETURN n SKIP 5 LIMIT 10").unwrap();
    match ast {
        CypherStatement::Match { return_clause, .. } => {
            assert_eq!(return_clause.skip, Some(5));
            assert_eq!(return_clause.limit, Some(10));
        }
    }
}

#[test]
fn test_parse_order_by() {
    let ast = CypherParser::parse("MATCH (n) RETURN n ORDER BY n.age DESC").unwrap();
    match ast {
        CypherStatement::Match { return_clause, .. } => {
            assert_eq!(return_clause.order_by.len(), 1);
            assert!(return_clause.order_by[0].descending);
        }
    }
}

#[test]
fn test_parse_return_distinct() {
    let ast = CypherParser::parse("MATCH (n)-[:KNOWS]->(m) RETURN DISTINCT m").unwrap();
    match ast {
        CypherStatement::Match { return_clause, .. } => {
            assert!(return_clause.distinct);
        }
    }
}

#[test]
fn test_parse_return_expression_with_alias() {
    let ast = CypherParser::parse("MATCH (n:Person) RETURN n.name AS personName, n.age").unwrap();
    match ast {
        CypherStatement::Match { return_clause, .. } => {
            assert_eq!(return_clause.items.len(), 2);
            match &return_clause.items[0] {
                CypherReturnItem::Expression {
                    alias: Some(alias), ..
                } => {
                    assert_eq!(alias, "personName");
                }
                other => panic!("expected Expression with alias, got: {other:?}"),
            }
            // Second item has no alias
            match &return_clause.items[1] {
                CypherReturnItem::Expression { alias: None, .. } => {}
                other => panic!("expected Expression without alias, got: {other:?}"),
            }
        }
    }
}

#[test]
fn test_parse_return_star() {
    let ast = CypherParser::parse("MATCH (n) RETURN *").unwrap();
    match ast {
        CypherStatement::Match { return_clause, .. } => {
            assert_eq!(return_clause.items.len(), 1);
            assert!(matches!(return_clause.items[0], CypherReturnItem::Star));
        }
    }
}

#[test]
fn test_parse_parameter() {
    let ast = CypherParser::parse("MATCH (n:Person {name: $name}) RETURN n").unwrap();
    match ast {
        CypherStatement::Match { pattern, .. } => {
            let node = match &pattern[0].elements[0] {
                CypherPatternElement::Node(n) => n,
                _ => panic!("expected node"),
            };
            assert_eq!(node.properties.len(), 1);
            assert_eq!(
                node.properties[0].1,
                CypherValue::Parameter("name".to_string())
            );
        }
    }
}

#[test]
fn test_parse_error_missing_return() {
    let result = CypherParser::parse("MATCH (n:Person)");
    assert!(result.is_err());
    match result.unwrap_err() {
        CypherError::ParseError { message, .. } => {
            assert!(
                message.contains("expected Return"),
                "error should mention RETURN: {message}"
            );
        }
        other => panic!("expected ParseError, got: {other:?}"),
    }
}

#[test]
fn test_parse_error_invalid_syntax() {
    let result = CypherParser::parse("MATCH RETURN");
    assert!(result.is_err());
}

// ============================================================================
// Lexer tests
// ============================================================================

use crate::cypher::lexer::{CypherLexer, Token, TokenKind};

/// Helper to extract just the kinds from a token list.
fn kinds(tokens: &[Token]) -> Vec<&TokenKind> {
    tokens.iter().map(|t| &t.kind).collect()
}

#[test]
fn test_lex_empty() {
    let tokens = CypherLexer::tokenize("").unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, TokenKind::Eof);
}

#[test]
fn test_lex_keywords() {
    let tokens = CypherLexer::tokenize("MATCH RETURN WHERE").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Match);
    assert_eq!(tokens[1].kind, TokenKind::Return);
    assert_eq!(tokens[2].kind, TokenKind::Where);
    assert_eq!(tokens[3].kind, TokenKind::Eof);
}

#[test]
fn test_lex_case_insensitive() {
    let tokens = CypherLexer::tokenize("match RETURN Where").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Match);
    assert_eq!(tokens[0].text, "match");
    assert_eq!(tokens[1].kind, TokenKind::Return);
    assert_eq!(tokens[1].text, "RETURN");
    assert_eq!(tokens[2].kind, TokenKind::Where);
    assert_eq!(tokens[2].text, "Where");
}

#[test]
fn test_lex_identifier() {
    let tokens = CypherLexer::tokenize("myVar").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Identifier);
    assert_eq!(tokens[0].text, "myVar");
}

#[test]
fn test_lex_string_single_quotes() {
    let tokens = CypherLexer::tokenize("'hello world'").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::StringLiteral);
    assert_eq!(tokens[0].text, "hello world");
}

#[test]
fn test_lex_string_double_quotes() {
    let tokens = CypherLexer::tokenize("\"hello\"").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::StringLiteral);
    assert_eq!(tokens[0].text, "hello");
}

#[test]
fn test_lex_integer() {
    let tokens = CypherLexer::tokenize("42").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::IntegerLiteral);
    assert_eq!(tokens[0].text, "42");
}

#[test]
fn test_lex_float() {
    let tokens = CypherLexer::tokenize("3.14").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::FloatLiteral);
    assert_eq!(tokens[0].text, "3.14");
}

#[test]
fn test_lex_symbols() {
    let tokens = CypherLexer::tokenize("()[]{}:.,->").unwrap();
    let k = kinds(&tokens);
    assert_eq!(
        k,
        vec![
            &TokenKind::LParen,
            &TokenKind::RParen,
            &TokenKind::LBracket,
            &TokenKind::RBracket,
            &TokenKind::LBrace,
            &TokenKind::RBrace,
            &TokenKind::Colon,
            &TokenKind::Dot,
            &TokenKind::Comma,
            &TokenKind::Arrow,
            &TokenKind::Eof,
        ]
    );

    // Also check left arrow separately since it starts with '<'
    let tokens2 = CypherLexer::tokenize("<-").unwrap();
    assert_eq!(tokens2[0].kind, TokenKind::LeftArrow);
}

#[test]
fn test_lex_comparison_operators() {
    let tokens = CypherLexer::tokenize("= <> < <= > >=").unwrap();
    let k = kinds(&tokens);
    assert_eq!(
        k,
        vec![
            &TokenKind::Eq,
            &TokenKind::Ne,
            &TokenKind::Lt,
            &TokenKind::Le,
            &TokenKind::Gt,
            &TokenKind::Ge,
            &TokenKind::Eof,
        ]
    );
}

#[test]
fn test_lex_parameter() {
    let tokens = CypherLexer::tokenize("$myParam").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Parameter);
    assert_eq!(tokens[0].text, "myParam");
}

#[test]
fn test_lex_star() {
    let tokens = CypherLexer::tokenize("*").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Star);
}

#[test]
fn test_lex_dotdot() {
    let tokens = CypherLexer::tokenize("1..3").unwrap();
    let k = kinds(&tokens);
    assert_eq!(
        k,
        vec![
            &TokenKind::IntegerLiteral,
            &TokenKind::DotDot,
            &TokenKind::IntegerLiteral,
            &TokenKind::Eof,
        ]
    );
    assert_eq!(tokens[0].text, "1");
    assert_eq!(tokens[2].text, "3");
}

#[test]
fn test_lex_full_query() {
    let query = "MATCH (n:Person {name: 'Alice'})-[:KNOWS]->(m) RETURN m.name";
    let tokens = CypherLexer::tokenize(query).unwrap();
    // Just make sure it doesn't error and ends with Eof
    assert!(tokens.len() > 5);
    assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);
    // Verify a few key tokens
    assert_eq!(tokens[0].kind, TokenKind::Match);
}

#[test]
fn test_lex_comment_line() {
    let tokens = CypherLexer::tokenize("MATCH // comment\n(n)").unwrap();
    let k = kinds(&tokens);
    assert_eq!(
        k,
        vec![
            &TokenKind::Match,
            &TokenKind::LParen,
            &TokenKind::Identifier,
            &TokenKind::RParen,
            &TokenKind::Eof,
        ]
    );
}

#[test]
fn test_lex_unterminated_string() {
    let result = CypherLexer::tokenize("'unterminated");
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        CypherError::LexError { message, .. } => {
            assert!(
                message.contains("Unterminated"),
                "expected 'Unterminated' in message: {message}"
            );
        }
        other => panic!("Expected LexError, got: {other:?}"),
    }
}

#[test]
fn test_lex_boolean_and_null() {
    let tokens = CypherLexer::tokenize("true false null").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::True);
    assert_eq!(tokens[1].kind, TokenKind::False);
    assert_eq!(tokens[2].kind, TokenKind::Null);
}

#[test]
fn test_lex_temporal_keywords() {
    let tokens = CypherLexer::tokenize("AS OF TIMESTAMP BETWEEN AND").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::As);
    assert_eq!(tokens[1].kind, TokenKind::Of);
    assert_eq!(tokens[2].kind, TokenKind::Timestamp);
    assert_eq!(tokens[3].kind, TokenKind::Between);
    assert_eq!(tokens[4].kind, TokenKind::And);
}

#[test]
fn test_lex_vector_dot_function() {
    let tokens = CypherLexer::tokenize("vector.similarity").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Identifier);
    assert_eq!(tokens[0].text, "vector");
    assert_eq!(tokens[1].kind, TokenKind::Dot);
    assert_eq!(tokens[2].kind, TokenKind::Identifier);
    assert_eq!(tokens[2].text, "similarity");
}

#[test]
fn test_lex_escaped_string() {
    let tokens = CypherLexer::tokenize(r"'it\'s'").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::StringLiteral);
    assert_eq!(tokens[0].text, "it's");
}

#[test]
fn test_lex_negative_number() {
    // Lexer treats -42 as Dash + IntegerLiteral; parser handles unary minus
    let tokens = CypherLexer::tokenize("-42").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Dash);
    assert_eq!(tokens[1].kind, TokenKind::IntegerLiteral);
    assert_eq!(tokens[1].text, "42");
}

#[test]
fn test_lex_ne_bang_equal() {
    // != should also produce Ne
    let tokens = CypherLexer::tokenize("!=").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Ne);
}

#[test]
fn test_lex_all_keywords() {
    // Verify every keyword variant tokenizes correctly
    let input = "MATCH OPTIONAL MATCH WHERE RETURN WITH UNWIND \
                 ORDER BY LIMIT SKIP AS DISTINCT \
                 AND OR NOT IN IS \
                 ASC DESC \
                 TRUE FALSE NULL \
                 CONTAINS STARTS ENDS \
                 COUNT COLLECT AVG SUM MIN MAX \
                 OF TIMESTAMP BETWEEN FOR SYSTEM_TIME VALID_TIME";
    let tokens = CypherLexer::tokenize(input).unwrap();
    // Spot-check several variants
    assert!(tokens.iter().any(|t| t.kind == TokenKind::With));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Unwind));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Distinct));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Contains));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::StartsWith));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::EndsWith));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Count));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Collect));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Avg));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Sum));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Min));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Max));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::SystemTime));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::ValidTime));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::For));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Timestamp));
}

#[test]
fn test_lex_arithmetic_operators() {
    let tokens = CypherLexer::tokenize("+ / %").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Plus);
    assert_eq!(tokens[1].kind, TokenKind::Slash);
    assert_eq!(tokens[2].kind, TokenKind::Percent);
}

#[test]
fn test_lex_pipe() {
    let tokens = CypherLexer::tokenize("|").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Pipe);
}

#[test]
fn test_lex_position_tracking() {
    let tokens = CypherLexer::tokenize("MATCH (n)").unwrap();
    assert_eq!(tokens[0].position, 0); // MATCH at 0
    assert_eq!(tokens[1].position, 6); // ( at 6
    assert_eq!(tokens[2].position, 7); // n at 7
    assert_eq!(tokens[3].position, 8); // ) at 8
}

#[test]
fn test_lex_unexpected_character() {
    let result = CypherLexer::tokenize("MATCH ~");
    assert!(result.is_err());
    match result.unwrap_err() {
        CypherError::LexError { position, .. } => {
            assert_eq!(position, 6);
        }
        other => panic!("Expected LexError, got: {other:?}"),
    }
}

#[test]
fn test_lex_optional_match() {
    // OPTIONAL and MATCH are two separate keywords
    let tokens = CypherLexer::tokenize("OPTIONAL MATCH").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::OptionalMatch);
    assert_eq!(tokens[1].kind, TokenKind::Match);
}

#[test]
fn test_lex_whitespace_only() {
    let tokens = CypherLexer::tokenize("   \t\n  ").unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, TokenKind::Eof);
}
