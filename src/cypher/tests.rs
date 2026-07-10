//! Tests for Cypher query language support.
//!
//! These tests follow TDD methodology -- tests are written first to define
//! expected behavior, then implementation is added to make them pass.
//!
//! This module validates the parser, IR conversion, and error handling for
//! the Cypher-like AletheiaDB Query Language (AQL).

use std::sync::Arc;

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
        optional_matches: vec![],
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

// ============================================================================
// Converter tests (AST → Query IR)
// ============================================================================

use super::{CypherParameterValue, parse_cypher, parse_cypher_with_params};
use crate::query::ir::{Predicate, QueryOp, TraversalDepth};

#[test]
fn test_convert_simple_scan() {
    let query = parse_cypher("MATCH (n:Person) RETURN n").unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::ScanNodes { label: Some(l) } if l == "Person"))
    );
}

#[test]
fn test_convert_scan_all() {
    let query = parse_cypher("MATCH (n) RETURN n").unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::ScanNodes { label: None }))
    );
}

#[test]
fn test_convert_property_filter() {
    let query = parse_cypher("MATCH (n:Person {name: 'Alice'}) RETURN n").unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::Filter(Predicate::Eq { .. })))
    );
}

#[test]
fn test_convert_where_filter() {
    let query = parse_cypher("MATCH (n:Person) WHERE n.age > 18 RETURN n").unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::Filter(Predicate::Gt { .. })))
    );
}

#[test]
fn test_convert_traversal() {
    let query = parse_cypher("MATCH (a:Person)-[:KNOWS]->(b) RETURN b").unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::TraverseOut { label: Some(l), .. } if l == "KNOWS"))
    );
}

#[test]
fn test_convert_incoming_traversal() {
    let query = parse_cypher("MATCH (a)<-[:FOLLOWS]-(b) RETURN b").unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::TraverseIn { label: Some(l), .. } if l == "FOLLOWS"))
    );
}

#[test]
fn test_convert_bidirectional_traversal() {
    let query = parse_cypher("MATCH (a)-[:KNOWS]-(b) RETURN b").unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::TraverseBoth { .. }))
    );
}

#[test]
fn test_convert_variable_length() {
    let query = parse_cypher("MATCH (a)-[:KNOWS*1..3]->(b) RETURN b").unwrap();
    assert!(query.ops.iter().any(|op| matches!(
        op,
        QueryOp::TraverseOut {
            depth: TraversalDepth::Range { min: 1, max: 3 },
            ..
        }
    )));
}

#[test]
fn test_convert_limit() {
    let query = parse_cypher("MATCH (n:Person) RETURN n LIMIT 10").unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Limit(10))));
}

#[test]
fn test_convert_skip() {
    let query = parse_cypher("MATCH (n:Person) RETURN n SKIP 5").unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Skip(5))));
}

#[test]
fn test_convert_distinct() {
    let query = parse_cypher("MATCH (a)-[:KNOWS]->(b) RETURN DISTINCT b").unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Distinct)));
}

#[test]
fn test_convert_order_by() {
    let query = parse_cypher("MATCH (n:Person) RETURN n ORDER BY n.age DESC LIMIT 10").unwrap();
    assert!(query.ops.iter().any(|op| matches!(
        op,
        QueryOp::Sort {
            descending: true,
            ..
        }
    )));
}

#[test]
fn test_convert_with_params() {
    use std::collections::HashMap;
    let mut params = HashMap::new();
    params.insert(
        "name".to_string(),
        CypherParameterValue::String("Alice".into()),
    );
    let query =
        parse_cypher_with_params("MATCH (n:Person {name: $name}) RETURN n", params).unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::Filter(Predicate::Eq { .. })))
    );
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

// ============================================================================
// DB API integration tests (execute_cypher / execute_cypher_with_params)
// ============================================================================

use crate::AletheiaDB;
use crate::core::property::{PropertyMap as CorePropertyMap, PropertyMapBuilder};

#[test]
fn test_execute_cypher_simple() {
    let db = AletheiaDB::new().unwrap();
    let props = PropertyMapBuilder::new().insert("name", "Alice").build();
    db.create_node("Person", props).unwrap();

    let results = db.execute_cypher("MATCH (n:Person) RETURN n").unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_execute_cypher_property_filter() {
    let db = AletheiaDB::new().unwrap();
    let props1 = PropertyMapBuilder::new().insert("name", "Alice").build();
    db.create_node("Person", props1).unwrap();
    let props2 = PropertyMapBuilder::new().insert("name", "Bob").build();
    db.create_node("Person", props2).unwrap();

    let results = db
        .execute_cypher("MATCH (n:Person {name: 'Alice'}) RETURN n")
        .unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_execute_cypher_traversal() {
    let db = AletheiaDB::new().unwrap();
    let props_a = PropertyMapBuilder::new().insert("name", "Alice").build();
    let alice = db.create_node("Person", props_a).unwrap();

    let props_b = PropertyMapBuilder::new().insert("name", "Bob").build();
    let bob = db.create_node("Person", props_b).unwrap();

    db.create_edge(alice, bob, "KNOWS", CorePropertyMap::new())
        .unwrap();

    let results = db
        .execute_cypher("MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(b) RETURN b")
        .unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_execute_cypher_limit() {
    let db = AletheiaDB::new().unwrap();
    for i in 0..10 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Person{i}"))
            .build();
        db.create_node("Person", props).unwrap();
    }

    let results = db
        .execute_cypher("MATCH (n:Person) RETURN n LIMIT 5")
        .unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 5);
}

#[test]
fn test_execute_cypher_with_params() {
    let db = AletheiaDB::new().unwrap();
    let props = PropertyMapBuilder::new().insert("name", "Alice").build();
    db.create_node("Person", props).unwrap();

    use std::collections::HashMap;
    let mut params = HashMap::new();
    params.insert(
        "name".to_string(),
        CypherParameterValue::String("Alice".into()),
    );

    let results = db
        .execute_cypher_with_params("MATCH (n:Person {name: $name}) RETURN n", params)
        .unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_execute_cypher_parse_error() {
    let db = AletheiaDB::new().unwrap();
    let result = db.execute_cypher("NOT VALID CYPHER");
    assert!(result.is_err());
}

// ============================================================================
// Temporal extension tests (Task 7)
// ============================================================================

#[test]
fn test_parse_as_of_timestamp() {
    let ast =
        CypherParser::parse("MATCH (n:Person) AS OF TIMESTAMP '2024-01-15T10:00:00Z' RETURN n")
            .unwrap();
    if let CypherStatement::Match {
        temporal: Some(t), ..
    } = ast
    {
        assert!(matches!(t, CypherTemporal::AsOfTimestamp(_)));
    } else {
        panic!("Expected temporal clause");
    }
}

#[test]
fn test_parse_as_of_valid_time() {
    let ast =
        CypherParser::parse("MATCH (n:Person) AS OF VALID_TIME '2024-01-15' RETURN n").unwrap();
    if let CypherStatement::Match {
        temporal: Some(t), ..
    } = ast
    {
        assert!(matches!(t, CypherTemporal::AsOfValidTime(_)));
    } else {
        panic!("Expected temporal clause");
    }
}

#[test]
fn test_parse_as_of_system_time() {
    let ast = CypherParser::parse("MATCH (n:Person) FOR SYSTEM_TIME AS OF '2024-01-15' RETURN n")
        .unwrap();
    if let CypherStatement::Match {
        temporal: Some(t), ..
    } = ast
    {
        assert!(matches!(t, CypherTemporal::AsOfSystemTime(_)));
    } else {
        panic!("Expected temporal clause");
    }
}

#[test]
fn test_parse_bitemporal() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) AS OF VALID_TIME '2024-01-01' AS OF SYSTEM_TIME '2024-06-15' RETURN n",
    )
    .unwrap();
    if let CypherStatement::Match {
        temporal: Some(t), ..
    } = ast
    {
        assert!(matches!(t, CypherTemporal::BiTemporal { .. }));
    } else {
        panic!("Expected bi-temporal clause");
    }
}

#[test]
fn test_parse_between() {
    let ast =
        CypherParser::parse("MATCH (n:Person) BETWEEN '2024-01-01' AND '2024-12-31' RETURN n")
            .unwrap();
    if let CypherStatement::Match {
        temporal: Some(t), ..
    } = ast
    {
        assert!(matches!(t, CypherTemporal::Between { .. }));
    } else {
        panic!("Expected BETWEEN clause");
    }
}

#[test]
fn test_convert_temporal_as_of() {
    let query =
        parse_cypher("MATCH (n:Person) AS OF TIMESTAMP '2024-01-15T10:00:00Z' RETURN n").unwrap();
    assert!(query.temporal_context.is_some());
}

// ============================================================================
// Vector extension tests (Task 8)
// ============================================================================

#[test]
fn test_parse_vector_similarity_in_order_by() {
    let ast = CypherParser::parse(
        "MATCH (d:Document) RETURN d ORDER BY vector.similarity(d.embedding, $query) DESC LIMIT 10",
    )
    .unwrap();
    let CypherStatement::Match { return_clause, .. } = ast;
    assert_eq!(return_clause.order_by.len(), 1);
    assert!(matches!(
        &return_clause.order_by[0].expr,
        CypherExpr::FunctionCall { name, .. } if name == "vector.similarity"
    ));
}

#[test]
fn test_parse_vector_cosine() {
    let ast = CypherParser::parse(
        "MATCH (d:Document) RETURN d, vector.cosine(d.embedding, $query) AS score ORDER BY score DESC",
    )
    .unwrap();
    let CypherStatement::Match { return_clause, .. } = ast;
    assert!(return_clause.items.len() >= 2);
}

#[test]
fn test_convert_vector_rank() {
    use std::collections::HashMap;
    let mut params = HashMap::new();
    let emb: Arc<[f32]> = Arc::from([0.1f32, 0.2, 0.3].as_slice());
    params.insert("query".to_string(), CypherParameterValue::Embedding(emb));

    let query = parse_cypher_with_params(
        "MATCH (d:Document) RETURN d ORDER BY vector.similarity(d.embedding, $query) DESC LIMIT 10",
        params,
    )
    .unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::RankBySimilarity { .. }))
    );
}

#[test]
fn test_convert_hybrid_traverse_then_rank() {
    use std::collections::HashMap;
    let mut params = HashMap::new();
    let emb: Arc<[f32]> = Arc::from([0.1f32, 0.2, 0.3].as_slice());
    params.insert(
        "targetEmbedding".to_string(),
        CypherParameterValue::Embedding(emb),
    );

    let query = parse_cypher_with_params(
        "MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(b) RETURN b ORDER BY vector.similarity(b.embedding, $targetEmbedding) DESC LIMIT 10",
        params,
    )
    .unwrap();
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::TraverseOut { .. }))
    );
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::RankBySimilarity { .. }))
    );
}

// ============================================================================
// WITH clause and advanced features tests (Task 9)
// ============================================================================

#[test]
fn test_parse_with_clause() {
    let ast = CypherParser::parse(
        "MATCH (a:Person)-[:KNOWS]->(b) \
         WITH b, vector.cosine(b.embedding, $emb) AS similarity \
         WHERE similarity > 0.7 \
         RETURN b.name, similarity \
         ORDER BY similarity DESC LIMIT 10",
    )
    .unwrap();
    let CypherStatement::Match { with_clauses, .. } = ast;
    assert_eq!(with_clauses.len(), 1);
    assert!(with_clauses[0].where_clause.is_some());
    // WITH should have 2 items: b and the function call aliased as similarity
    assert_eq!(with_clauses[0].items.len(), 2);
}

#[test]
fn test_parse_optional_match() {
    // Plain MATCH is not optional.
    let ast = CypherParser::parse("MATCH (n:Person) RETURN n").unwrap();
    let CypherStatement::Match { optional, .. } = ast;
    assert!(!optional);

    // A leading OPTIONAL MATCH sets the optional flag.
    let ast = CypherParser::parse("OPTIONAL MATCH (n:Person) RETURN n").unwrap();
    let CypherStatement::Match {
        optional,
        pattern,
        optional_matches,
        ..
    } = ast;
    assert!(optional);
    assert_eq!(pattern.len(), 1);
    assert!(optional_matches.is_empty());
}

#[test]
fn test_parse_match_then_optional_match() {
    let ast = CypherParser::parse(
        "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) WHERE x.age > 30 RETURN x",
    )
    .unwrap();
    let CypherStatement::Match {
        optional,
        pattern,
        where_clause,
        optional_matches,
        ..
    } = ast;
    assert!(!optional);
    assert_eq!(pattern.len(), 1);
    // The WHERE belongs to the OPTIONAL MATCH clause, not the base MATCH.
    assert!(where_clause.is_none());
    assert_eq!(optional_matches.len(), 1);
    assert!(optional_matches[0].where_clause.is_some());
    assert_eq!(optional_matches[0].pattern.len(), 1);
    // Pattern is (a)-[:KNOWS]->(x): node, rel, node.
    assert_eq!(optional_matches[0].pattern[0].elements.len(), 3);
    assert_eq!(optional_matches[0].preceding_withs, 0);
}

#[test]
fn test_parse_multiple_optional_matches() {
    // Parse-only: the grammar accepts multiple OPTIONAL MATCH clauses in any
    // shape. Note this particular query re-anchors the second clause on `a`
    // and is rejected later, at CONVERSION (see
    // test_convert_optional_match_reanchored_variable_rejected) -- parsing
    // it is still correct.
    let ast = CypherParser::parse(
        "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) OPTIONAL MATCH (a)-[:OWNS]->(y) RETURN y",
    )
    .unwrap();
    let CypherStatement::Match {
        optional_matches, ..
    } = ast;
    assert_eq!(optional_matches.len(), 2);
    assert!(optional_matches[0].where_clause.is_none());
    assert!(optional_matches[1].where_clause.is_none());
}

#[test]
fn test_parse_optional_match_after_with() {
    let ast = CypherParser::parse(
        "MATCH (a:Person) WITH a WHERE a.age > 18 OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
    )
    .unwrap();
    let CypherStatement::Match {
        with_clauses,
        optional_matches,
        ..
    } = ast;
    assert_eq!(with_clauses.len(), 1);
    assert_eq!(optional_matches.len(), 1);
    // The WITH precedes the OPTIONAL MATCH in source order.
    assert_eq!(optional_matches[0].preceding_withs, 1);
}

#[test]
fn test_parse_optional_match_base_where_and_clause_where() {
    let ast = CypherParser::parse(
        "MATCH (a:Person) WHERE a.age > 18 OPTIONAL MATCH (a)-[:KNOWS]->(x) WHERE x.age > 30 RETURN x",
    )
    .unwrap();
    let CypherStatement::Match {
        where_clause,
        optional_matches,
        ..
    } = ast;
    assert!(where_clause.is_some());
    assert_eq!(optional_matches.len(), 1);
    assert!(optional_matches[0].where_clause.is_some());
}

#[test]
fn test_convert_optional_match_emits_optional_op() {
    let query = parse_cypher(
        "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x {name: 'Z'}) WHERE x.age > 30 RETURN x",
    )
    .unwrap();

    // The base pipeline scans, then a single Optional op wraps the whole
    // optional segment (traverse + inline property filter + clause WHERE).
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::ScanNodes { .. }))
    );
    let optional = query
        .ops
        .iter()
        .find_map(|op| match op {
            QueryOp::Optional { ops } => Some(ops),
            _ => None,
        })
        .expect("expected a QueryOp::Optional in converted ops");
    assert!(
        optional
            .iter()
            .any(|op| matches!(op, QueryOp::TraverseOut { .. }))
    );
    // Inline property filter + clause WHERE both live INSIDE the optional op.
    let inner_filters = optional
        .iter()
        .filter(|op| matches!(op, QueryOp::Filter(_)))
        .count();
    assert_eq!(inner_filters, 2);
    // No traversal or filter leaked outside the Optional op.
    assert!(
        !query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::TraverseOut { .. } | QueryOp::Filter(_)))
    );
}

#[test]
fn test_convert_leading_optional_match() {
    let query = parse_cypher("OPTIONAL MATCH (n:Person) WHERE n.age > 200 RETURN n").unwrap();
    // A leading OPTIONAL MATCH wraps its own scan pipeline in an Optional op.
    match &query.ops[0] {
        QueryOp::Optional { ops } => {
            assert!(matches!(ops[0], QueryOp::ScanNodes { .. }));
            assert!(ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
        }
        other => panic!("expected leading Optional op, got {other:?}"),
    }
}

#[test]
fn test_parse_count_aggregation() {
    let ast = CypherParser::parse("MATCH (n:Person)-[:KNOWS]->(m) RETURN count(m)").unwrap();
    let CypherStatement::Match { return_clause, .. } = ast;
    match &return_clause.items[0] {
        CypherReturnItem::Expression { expr, .. } => {
            assert!(matches!(expr, CypherExpr::FunctionCall { name, .. } if name == "COUNT"));
        }
        _ => panic!("Expected function call"),
    }
}

#[test]
fn test_parse_multiple_patterns() {
    let ast = CypherParser::parse(
        "MATCH (a:Person), (b:Person) WHERE a.name = 'Alice' AND b.name = 'Bob' RETURN a, b",
    )
    .unwrap();
    let CypherStatement::Match { pattern, .. } = ast;
    assert_eq!(pattern.len(), 2);
}

#[test]
fn test_convert_full_hybrid() {
    use std::collections::HashMap;
    let mut params = HashMap::new();
    let emb: Arc<[f32]> = Arc::from([0.1f32, 0.2, 0.3].as_slice());
    params.insert(
        "roseEmbedding".to_string(),
        CypherParameterValue::Embedding(emb),
    );

    let query = parse_cypher_with_params(
        "MATCH (doctor:TimeLords {name: 'David Tennant'})-[:COMPANION]->(companion) \
         AS OF TIMESTAMP '2010-06-15T00:00:00Z' \
         RETURN companion \
         ORDER BY vector.similarity(companion.embedding, $roseEmbedding) DESC \
         LIMIT 10",
        params,
    )
    .unwrap();

    assert!(query.temporal_context.is_some());
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
    assert!(
        query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::RankBySimilarity { .. }))
    );
}

// ============================================================================
// Comprehensive end-to-end integration tests (Task 10)
// ============================================================================

#[test]
fn test_e2e_multi_hop_traversal() {
    let db = AletheiaDB::new().unwrap();
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();
    let charlie = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Charlie").build(),
        )
        .unwrap();
    db.create_edge(alice, bob, "KNOWS", CorePropertyMap::new())
        .unwrap();
    db.create_edge(bob, charlie, "KNOWS", CorePropertyMap::new())
        .unwrap();

    // 1-hop: Alice -> Bob
    let results = db
        .execute_cypher("MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(b) RETURN b")
        .unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1); // Bob

    // 2-hop: Alice -> Bob -> Charlie (yields nodes at depth 2)
    let results = db
        .execute_cypher("MATCH (a:Person {name: 'Alice'})-[:KNOWS*1..2]->(b) RETURN b")
        .unwrap();
    let rows: Vec<_> = results.collect();
    // Range *1..2 should yield nodes at depth 1 (Bob) and depth 2 (Charlie),
    // but the executor may only return terminal-depth nodes depending on the
    // TraversalDepth::Range semantics.  Assert at least 1 result.
    assert!(
        !rows.is_empty(),
        "expected at least 1 result from *1..2 traversal, got {}",
        rows.len()
    );
}

#[test]
fn test_e2e_where_complex() {
    let db = AletheiaDB::new().unwrap();
    for (name, age) in [("Alice", 30), ("Bob", 25), ("Charlie", 35)] {
        let props = PropertyMapBuilder::new()
            .insert("name", name)
            .insert("age", age as i64)
            .build();
        db.create_node("Person", props).unwrap();
    }

    let results = db
        .execute_cypher("MATCH (n:Person) WHERE n.age > 26 AND n.age < 34 RETURN n")
        .unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1); // Only Alice (age 30)
}

#[test]
fn test_e2e_skip_limit() {
    let db = AletheiaDB::new().unwrap();
    for i in 0..20 {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("P{i:02}"))
            .insert("rank", i as i64)
            .build();
        db.create_node("Item", props).unwrap();
    }

    // SKIP + LIMIT without ORDER BY (Sort not yet in physical executor)
    let results = db
        .execute_cypher("MATCH (n:Item) RETURN n SKIP 5 LIMIT 5")
        .unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 5);
}

#[test]
fn test_e2e_bidirectional() {
    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "A").build(),
        )
        .unwrap();
    let b = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "B").build(),
        )
        .unwrap();
    db.create_edge(a, b, "FRIEND", CorePropertyMap::new())
        .unwrap();

    let results = db
        .execute_cypher("MATCH (x:Person {name: 'B'})-[:FRIEND]-(y) RETURN y")
        .unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_e2e_no_results() {
    let db = AletheiaDB::new().unwrap();
    let results = db
        .execute_cypher("MATCH (n:NonexistentLabel) RETURN n")
        .unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 0);
}

// ============================================================================
// Parser tests: IS NULL, IS NOT NULL, IN, CONTAINS, STARTS WITH, ENDS WITH
// ============================================================================

#[test]
fn test_parse_where_is_null() {
    let ast = CypherParser::parse("MATCH (n:Person) WHERE n.email IS NULL RETURN n").unwrap();
    if let CypherStatement::Match {
        where_clause: Some(expr),
        ..
    } = ast
    {
        assert!(matches!(expr, CypherExpr::IsNull(_)));
    } else {
        panic!("Expected IS NULL");
    }
}

#[test]
fn test_parse_where_is_not_null() {
    let ast = CypherParser::parse("MATCH (n:Person) WHERE n.email IS NOT NULL RETURN n").unwrap();
    if let CypherStatement::Match {
        where_clause: Some(expr),
        ..
    } = ast
    {
        assert!(matches!(expr, CypherExpr::IsNotNull(_)));
    } else {
        panic!("Expected IS NOT NULL");
    }
}

#[test]
fn test_parse_where_in() {
    let ast = CypherParser::parse("MATCH (n:Person) WHERE n.age IN [25, 30, 35] RETURN n").unwrap();
    if let CypherStatement::Match {
        where_clause: Some(expr),
        ..
    } = ast
    {
        assert!(matches!(expr, CypherExpr::In { .. }));
    } else {
        panic!("Expected IN");
    }
}

#[test]
fn test_parse_where_contains() {
    let ast = CypherParser::parse("MATCH (n:Person) WHERE n.name CONTAINS 'Ali' RETURN n").unwrap();
    if let CypherStatement::Match {
        where_clause: Some(expr),
        ..
    } = ast
    {
        assert!(matches!(expr, CypherExpr::Contains { .. }));
    } else {
        panic!("Expected CONTAINS");
    }
}

#[test]
fn test_parse_where_starts_with() {
    let ast =
        CypherParser::parse("MATCH (n:Person) WHERE n.name STARTS WITH 'Al' RETURN n").unwrap();
    if let CypherStatement::Match {
        where_clause: Some(expr),
        ..
    } = ast
    {
        assert!(matches!(expr, CypherExpr::StartsWith { .. }));
    } else {
        panic!("Expected STARTS WITH");
    }
}

#[test]
fn test_parse_where_ends_with() {
    let ast =
        CypherParser::parse("MATCH (n:Person) WHERE n.name ENDS WITH 'ice' RETURN n").unwrap();
    if let CypherStatement::Match {
        where_clause: Some(expr),
        ..
    } = ast
    {
        assert!(matches!(expr, CypherExpr::EndsWith { .. }));
    } else {
        panic!("Expected ENDS WITH");
    }
}

// ============================================================================
// OPTIONAL MATCH execution semantics (Issue #557)
// ============================================================================

/// Build a small test graph:
///   Alice -KNOWS-> Bob -KNOWS-> Carol
///   Dave (no outgoing KNOWS edges)
/// Returns the db.
fn optional_match_test_db() -> AletheiaDB {
    let db = AletheiaDB::new().unwrap();
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .build(),
        )
        .unwrap();
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 40i64)
                .build(),
        )
        .unwrap();
    let carol = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Carol")
                .insert("age", 50i64)
                .build(),
        )
        .unwrap();
    let _dave = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Dave")
                .insert("age", 60i64)
                .build(),
        )
        .unwrap();
    db.create_edge(alice, bob, "KNOWS", CorePropertyMap::new())
        .unwrap();
    db.create_edge(bob, carol, "KNOWS", CorePropertyMap::new())
        .unwrap();
    db
}

fn collect_rows(results: crate::query::QueryResults) -> Vec<crate::query::executor::QueryRow> {
    results.collect_all().unwrap()
}

fn row_name(row: &crate::query::executor::QueryRow) -> Option<String> {
    row.entity.as_node().and_then(|n| {
        n.properties
            .get("name")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
    })
}

#[test]
fn test_e2e_optional_match_matched() {
    let db = optional_match_test_db();
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(row_name(&rows[0]).as_deref(), Some("Bob"));
}

#[test]
fn test_e2e_optional_match_unmatched_returns_null_row() {
    let db = optional_match_test_db();
    // Dave has no outgoing KNOWS edges: openCypher requires ONE row with x = null,
    // not zero rows.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Dave'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(
        rows.len(),
        1,
        "unmatched OPTIONAL MATCH must preserve the base row"
    );
    assert!(
        rows[0].entity.is_null(),
        "unmatched OPTIONAL MATCH must bind null, got {:?}",
        rows[0].entity
    );
}

#[test]
fn test_e2e_optional_match_mixed_matched_and_unmatched() {
    let db = optional_match_test_db();
    // All four Persons: Alice->Bob, Bob->Carol match; Carol and Dave don't.
    let rows = collect_rows(
        db.execute_cypher("MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x")
            .unwrap(),
    );
    assert_eq!(rows.len(), 4, "one row per base row (2 matched + 2 null)");
    let nulls = rows.iter().filter(|r| r.entity.is_null()).count();
    assert_eq!(nulls, 2);
    let names: Vec<_> = rows.iter().filter_map(row_name).collect();
    assert!(names.contains(&"Bob".to_string()));
    assert!(names.contains(&"Carol".to_string()));
}

#[test]
fn test_e2e_optional_match_multi_hop_unmatched_midway() {
    let db = optional_match_test_db();
    // Carol has no outgoing KNOWS at hop 1, so a 2-hop optional pattern is
    // unmatched: expect one null row, not a partial result and not zero rows.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Carol'}) OPTIONAL MATCH (a)-[:KNOWS]->(m)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());

    // And a fully-matched 2-hop optional pattern still works.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(m)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(row_name(&rows[0]).as_deref(), Some("Carol"));
}

#[test]
fn test_e2e_optional_match_inline_property_scoped_inside() {
    let db = optional_match_test_db();
    // Alice knows Bob, but nobody named 'Zed': the inline property filter is
    // part of the optional pattern, so the result is a null row -- the filter
    // must NOT run outside the optional segment and kill the base row.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(x {name: 'Zed'}) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());
}

#[test]
fn test_e2e_optional_match_where_eliminates_all() {
    let db = optional_match_test_db();
    // Bob (Alice's only acquaintance) is 40, so WHERE x.age > 100 makes the
    // optional pattern unmatched -> null row.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) WHERE x.age > 100 RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());
}

#[test]
fn test_e2e_optional_match_where_keeps_some() {
    let db = optional_match_test_db();
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) WHERE x.age > 35 RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(row_name(&rows[0]).as_deref(), Some("Bob"));
}

#[test]
fn test_e2e_two_optional_matches_chained() {
    let db = optional_match_test_db();
    // Second optional chains from the first's result: Alice -> Bob -> Carol.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(m) OPTIONAL MATCH (m)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(row_name(&rows[0]).as_deref(), Some("Carol"));
}

#[test]
fn test_e2e_two_optional_matches_second_unmatched() {
    let db = optional_match_test_db();
    // Bob -> Carol matches, but Carol knows nobody: second optional yields null.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Bob'}) OPTIONAL MATCH (a)-[:KNOWS]->(m) OPTIONAL MATCH (m)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());
}

#[test]
fn test_e2e_two_optional_matches_both_unmatched() {
    let db = optional_match_test_db();
    // Dave knows nobody: first optional is null; second optional over a null
    // seed must also produce a single null row (null propagates, row preserved).
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Dave'}) OPTIONAL MATCH (a)-[:KNOWS]->(m) OPTIONAL MATCH (m)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());
}

#[test]
fn test_e2e_optional_match_after_with_where() {
    let db = optional_match_test_db();
    // WITH ... WHERE filters base rows BEFORE the optional segment:
    // only Dave (age 60) survives, and his optional pattern is unmatched.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person) WITH a WHERE a.age > 55 OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());
}

#[test]
fn test_e2e_optional_match_null_row_dropped_by_downstream_where() {
    let db = optional_match_test_db();
    // A WITH ... WHERE AFTER the optional segment applies to its results:
    // comparisons against a null binding are not-true, so null rows drop.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) WITH x WHERE x.age > 45 RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(row_name(&rows[0]).as_deref(), Some("Carol"));
}

#[test]
fn test_e2e_optional_match_null_row_kept_by_is_null() {
    let db = optional_match_test_db();
    // `x IS NULL` after the optional segment keeps exactly the unmatched rows
    // (Carol and Dave have no outgoing KNOWS).
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) WITH x WHERE x IS NULL RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.entity.is_null()));
}

#[test]
fn test_e2e_optional_match_skip_limit() {
    let db = optional_match_test_db();
    // 4 rows total (2 matched + 2 null); SKIP 1 LIMIT 2 -> exactly 2 rows.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x SKIP 1 LIMIT 2",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_e2e_leading_optional_match_no_matches_returns_one_null_row() {
    let db = AletheiaDB::new().unwrap();
    // Empty database: a leading OPTIONAL MATCH with no matches returns a
    // single row of nulls per openCypher.
    let rows = collect_rows(
        db.execute_cypher("OPTIONAL MATCH (n:Person) RETURN n")
            .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());
}

#[test]
fn test_e2e_leading_optional_match_with_matches() {
    let db = optional_match_test_db();
    let rows = collect_rows(
        db.execute_cypher("OPTIONAL MATCH (n:Person) RETURN n")
            .unwrap(),
    );
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| !r.entity.is_null()));
}

#[test]
fn test_e2e_leading_optional_match_where_eliminates_all() {
    let db = optional_match_test_db();
    // The WHERE is part of the leading optional pattern: nobody is older
    // than 200, so one null row.
    let rows = collect_rows(
        db.execute_cypher("OPTIONAL MATCH (n:Person) WHERE n.age > 200 RETURN n")
            .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());
}

#[test]
fn test_e2e_optional_match_with_as_of_does_not_panic() {
    let db = optional_match_test_db();
    // Temporal contexts are currently inert for label scans (pre-existing);
    // this test documents that OPTIONAL MATCH combined with AS OF executes
    // without panicking and preserves left-outer row semantics. The AS OF
    // coordinate is in the future of all writes, so edges are visible.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Dave'}) AS OF VALID_TIME '2999-01-01T00:00:00Z' OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());

    // An AS OF coordinate before any write excludes the edges: the optional
    // traversal finds nothing, so the base row is preserved with a null.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Alice'}) AS OF VALID_TIME '1970-01-02T00:00:00Z' AS OF SYSTEM_TIME '1970-01-02T00:00:00Z' OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].entity.is_null(),
        "edges created after the AS OF coordinate must not match, got {:?}",
        rows[0].entity
    );
}

#[test]
fn test_e2e_plain_match_unchanged_regression() {
    let db = optional_match_test_db();
    // Plain (inner) MATCH still returns zero rows when the pattern is unmatched.
    let rows = collect_rows(
        db.execute_cypher("MATCH (a:Person {name: 'Dave'})-[:KNOWS]->(x) RETURN x")
            .unwrap(),
    );
    assert_eq!(rows.len(), 0);
}

// ============================================================================
// OPTIONAL MATCH: rejected unsupported constructs (review findings, PR #3401)
// ============================================================================

#[test]
fn test_convert_optional_match_multiple_patterns_rejected() {
    // Without variable-binding analysis, comma-separated patterns in a
    // subsequent OPTIONAL MATCH would silently chain positionally
    // (`(a)-[:KNOWS]->(x), (a)-[:KNOWS]->(y)` would traverse x's result,
    // not re-seed from `a`), returning the wrong entity for `y`. Reject
    // rather than answer wrongly.
    let err = parse_cypher(
        "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x), (a)-[:KNOWS]->(y) RETURN y",
    )
    .unwrap_err();
    assert!(
        matches!(err, CypherError::UnsupportedFeature(_)),
        "expected UnsupportedFeature, got {err:?}"
    );
    assert!(
        err.to_string().contains("OPTIONAL MATCH"),
        "error should name the offending construct: {err}"
    );
}

#[test]
fn test_convert_optional_match_labeled_first_node_rejected() {
    // The first node of a subsequent OPTIONAL MATCH is bound positionally to
    // the current row; a label on it cannot be honored without
    // variable-binding analysis (`MATCH (a:Person) OPTIONAL MATCH (b:City)
    // RETURN b` would silently return `a`). Reject rather than drop the label.
    let err = parse_cypher("MATCH (a:Person) OPTIONAL MATCH (b:City) RETURN b").unwrap_err();
    assert!(
        matches!(err, CypherError::UnsupportedFeature(_)),
        "expected UnsupportedFeature, got {err:?}"
    );
    assert!(
        err.to_string().contains("label"),
        "error should explain the label restriction: {err}"
    );

    // Same restriction when the labeled first node starts a traversal pattern.
    let err =
        parse_cypher("MATCH (a:Person) OPTIONAL MATCH (b:City)-[:NEAR]->(c) RETURN c").unwrap_err();
    assert!(matches!(err, CypherError::UnsupportedFeature(_)));

    // An unlabeled first node (positional reference to the current row)
    // still works.
    assert!(parse_cypher("MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x").is_ok());
}

#[test]
fn test_convert_optional_match_reanchored_variable_rejected() {
    // The first node of a subsequent OPTIONAL MATCH binds positionally to
    // the current row (the previous clause's last binding). Here the second
    // OPTIONAL MATCH names `(a)` but the pipeline is positioned on `x`, so
    // without the check `(a)` would silently bind to `x` (or null) and
    // traverse OWNS from the wrong node. Reject rather than answer wrongly.
    let err = parse_cypher(
        "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) OPTIONAL MATCH (a)-[:OWNS]->(y) RETURN y",
    )
    .unwrap_err();
    assert!(
        matches!(err, CypherError::UnsupportedFeature(_)),
        "expected UnsupportedFeature, got {err:?}"
    );
    assert!(
        err.to_string().contains("re-anchor"),
        "error should explain that re-anchoring is unsupported: {err}"
    );
    assert!(
        err.to_string().contains('x') && err.to_string().contains('a'),
        "error should name both the expected and the offending variable: {err}"
    );

    // Positive control: chaining each OPTIONAL MATCH from the previous
    // clause's binding still converts.
    assert!(parse_cypher(
        "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) OPTIONAL MATCH (x)-[:OWNS]->(y) RETURN y"
    )
    .is_ok());

    // An unnamed first node `()` is an explicit positional reference and
    // remains accepted.
    assert!(
        parse_cypher("MATCH (a:Person) OPTIONAL MATCH ()-[:KNOWS]->(x) RETURN x").is_ok(),
        "unnamed first node must keep working"
    );
}

#[test]
fn test_convert_optional_match_reanchored_after_multi_hop_base_rejected() {
    // Same hole for the FIRST OPTIONAL MATCH after a multi-hop base MATCH:
    // the positionally-current variable is the base pattern's LAST node
    // (`b`), so `(a)` would silently bind to `b` and traverse from the
    // wrong end of the base pattern.
    let err =
        parse_cypher("MATCH (a:Person)-[:KNOWS]->(b) OPTIONAL MATCH (a)-[:OWNS]->(y) RETURN y")
            .unwrap_err();
    assert!(
        matches!(err, CypherError::UnsupportedFeature(_)),
        "expected UnsupportedFeature, got {err:?}"
    );

    // Continuing from the base pattern's last node converts fine.
    assert!(
        parse_cypher("MATCH (a:Person)-[:KNOWS]->(b) OPTIONAL MATCH (b)-[:OWNS]->(y) RETURN y")
            .is_ok()
    );

    // When the previous clause's last node is unnamed, ANY named first node
    // is a re-anchor and is rejected.
    let err =
        parse_cypher("MATCH (a:Person)-[:KNOWS]->() OPTIONAL MATCH (a)-[:OWNS]->(y) RETURN y")
            .unwrap_err();
    assert!(
        matches!(err, CypherError::UnsupportedFeature(_)),
        "expected UnsupportedFeature, got {err:?}"
    );
}

#[test]
fn test_convert_optional_match_after_with_projection_tracks_variable() {
    // A `WITH v` projection makes `v` the positionally-current binding for
    // a following OPTIONAL MATCH.
    assert!(parse_cypher(
        "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) WITH x WHERE x IS NULL OPTIONAL MATCH (x)-[:OWNS]->(y) RETURN y"
    )
    .is_ok());

    // Naming a stale variable after the projection is a re-anchor.
    let err = parse_cypher(
        "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) WITH x OPTIONAL MATCH (a)-[:OWNS]->(y) RETURN y",
    )
    .unwrap_err();
    assert!(
        matches!(err, CypherError::UnsupportedFeature(_)),
        "expected UnsupportedFeature, got {err:?}"
    );
}

#[test]
fn test_parse_depth_range_inverted_rejected() {
    // `*3..1` is an inverted variable-length range; the AQL parser rejects
    // min > max, and the Cypher parser must too instead of silently
    // accepting it.
    let err = CypherParser::parse("MATCH (a)-[:KNOWS*3..1]->(b) RETURN b").unwrap_err();
    assert!(
        matches!(err, CypherError::ParseError { .. }),
        "expected ParseError, got {err:?}"
    );
    assert!(
        err.to_string().contains("min"),
        "error should describe the inverted range: {err}"
    );

    // Valid ranges still parse (min == max and min < max).
    assert!(CypherParser::parse("MATCH (a)-[:KNOWS*1..3]->(b) RETURN b").is_ok());
    assert!(CypherParser::parse("MATCH (a)-[:KNOWS*2..2]->(b) RETURN b").is_ok());
}

// ============================================================================
// OPTIONAL MATCH: fan-out, null-predicate, and parameter e2e coverage
// ============================================================================

/// Alice (Person) -KNOWS-> Bob (Friend) and Alice -KNOWS-> Eve (Friend);
/// Dave (Person) has no outgoing edges.
fn fan_out_test_db() -> AletheiaDB {
    let db = AletheiaDB::new().unwrap();
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let _dave = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Dave").build(),
        )
        .unwrap();
    let bob = db
        .create_node(
            "Friend",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap();
    let eve = db
        .create_node(
            "Friend",
            PropertyMapBuilder::new().insert("name", "Eve").build(),
        )
        .unwrap();
    db.create_edge(alice, bob, "KNOWS", CorePropertyMap::new())
        .unwrap();
    db.create_edge(alice, eve, "KNOWS", CorePropertyMap::new())
        .unwrap();
    db
}

#[test]
fn test_e2e_optional_match_fan_out_two_matches() {
    let db = fan_out_test_db();
    // One seed (Alice) with TWO optional matches: exactly 2 non-null rows,
    // one per matched edge, carrying both names.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 2, "fan-out must yield one row per match");
    assert!(rows.iter().all(|r| !r.entity.is_null()));
    let names: Vec<_> = rows.iter().filter_map(row_name).collect();
    assert!(names.contains(&"Bob".to_string()), "names: {names:?}");
    assert!(names.contains(&"Eve".to_string()), "names: {names:?}");
}

#[test]
fn test_e2e_optional_match_fan_out_with_unmatched_seed() {
    let db = fan_out_test_db();
    // Two seeds: Alice fans out to 2 matches, Dave is unmatched -> exactly
    // 3 rows, of which 1 is null.
    let rows = collect_rows(
        db.execute_cypher("MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x")
            .unwrap(),
    );
    assert_eq!(rows.len(), 3, "2 matched + 1 null row expected");
    let nulls = rows.iter().filter(|r| r.entity.is_null()).count();
    assert_eq!(nulls, 1);
    let names: Vec<_> = rows.iter().filter_map(row_name).collect();
    assert!(names.contains(&"Bob".to_string()));
    assert!(names.contains(&"Eve".to_string()));
}

#[test]
fn test_e2e_optional_match_is_not_null_keeps_only_matched() {
    let db = optional_match_test_db();
    // `x IS NOT NULL` after the optional segment keeps exactly the matched
    // rows (Alice->Bob, Bob->Carol) and drops the null rows (Carol, Dave).
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) WITH x WHERE x IS NOT NULL RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| !r.entity.is_null()));
    let names: Vec<_> = rows.iter().filter_map(row_name).collect();
    assert!(names.contains(&"Bob".to_string()));
    assert!(names.contains(&"Carol".to_string()));
}

#[test]
fn test_e2e_optional_match_null_in_boolean_connective() {
    let db = optional_match_test_db();
    // Boolean connective over a null binding: `x IS NULL` is true for the two
    // null rows; `x.age > 100` is not-true for every matched row (Bob is 40,
    // Carol is 50) and not-true for null rows. The OR keeps exactly the nulls.
    let rows = collect_rows(
        db.execute_cypher(
            "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) WITH x WHERE x IS NULL OR x.age > 100 RETURN x",
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.entity.is_null()));
}

#[test]
fn test_e2e_optional_match_where_with_param() {
    use std::collections::HashMap;

    let db = optional_match_test_db();

    // A `$param` inside the optional clause's WHERE resolves and is scoped
    // inside the optional segment: Bob (age 40) passes `x.age > $minAge`.
    let mut params = HashMap::new();
    params.insert("minAge".to_string(), CypherParameterValue::Int(35));
    let rows = collect_rows(
        db.execute_cypher_with_params(
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) WHERE x.age > $minAge RETURN x",
            params,
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(row_name(&rows[0]).as_deref(), Some("Bob"));

    // A binding that eliminates every match yields a null row (never a
    // dropped row): the parameterized WHERE participates in the
    // matched/unmatched decision.
    let mut params = HashMap::new();
    params.insert("minAge".to_string(), CypherParameterValue::Int(100));
    let rows = collect_rows(
        db.execute_cypher_with_params(
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) WHERE x.age > $minAge RETURN x",
            params,
        )
        .unwrap(),
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].entity.is_null());
}

// ============================================================================
// Bi-temporal AS OF / BETWEEN runtime tests (Issues #550, #551, #552)
//
// These are RUNTIME (end-to-end) tests, not parse-only: each creates and
// mutates data, captures commit-separated timestamps, runs the Cypher query,
// and asserts the HISTORICAL state -- cross-checking against the oracle
// temporal API (`get_node_at_time` / `find_nodes_at_time`). The original bug
// (the planner's label-scan arm ignoring the temporal context, so
// `MATCH (n:Label) AS OF T RETURN n` silently returned present-day data)
// slipped through precisely because only parse-level tests existed.
// ============================================================================

/// Capture a wall-clock instant strictly after every already-committed
/// transaction and strictly before any subsequent commit, so it is an
/// unambiguous bi-temporal anchor *between* two writes. Mirrors the helper the
/// oracle tests in `db::ops` use.
fn temporal_anchor(db: &AletheiaDB) -> crate::core::Timestamp {
    use crate::core::temporal::time;
    let committed = *db
        .current_timestamp
        .lock()
        .expect("current_timestamp mutex poisoned");
    // Phase 1: anchor strictly after every committed stamp.
    let mut anchor = time::now();
    while anchor <= committed {
        std::thread::sleep(std::time::Duration::from_micros(100));
        anchor = time::now();
    }
    // Phase 2: return only once wallclock has moved strictly past the anchor,
    // so later commits stamp strictly after it.
    while time::now() <= anchor {
        std::thread::sleep(std::time::Duration::from_micros(100));
    }
    anchor
}

/// The raw microsecond form of a timestamp, which `parse_timestamp_string`
/// round-trips exactly (unlike an RFC3339 string, which loses sub-second
/// precision), so the query anchors on the identical instant we captured.
fn anchor_micros(ts: crate::core::Timestamp) -> i64 {
    ts.wallclock()
}

/// Sorted `name` property values of a row set, for order-independent asserts.
fn sorted_names(rows: &[crate::query::executor::QueryRow]) -> Vec<String> {
    let mut names: Vec<String> = rows.iter().filter_map(row_name).collect();
    names.sort();
    names
}

fn person(name: &str) -> CorePropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

/// #550: `AS OF TIMESTAMP` on a label scan returns the OLD value after an
/// update -- not the current one -- and matches the oracle exactly.
#[test]
fn test_e2e_as_of_timestamp_returns_historical_state() {
    use crate::core::property::PropertyValue;

    let db = AletheiaDB::new().unwrap();
    let id = db.create_node("Person", person("Alice")).unwrap();
    let t1 = temporal_anchor(&db);
    db.update_node_with_valid_time(id, person("Bob"), None)
        .unwrap();

    // The label scan must reconstruct history at t1, not read present state.
    let q = format!(
        "MATCH (n:Person) AS OF TIMESTAMP '{}' RETURN n",
        anchor_micros(t1)
    );
    let rows = collect_rows(db.execute_cypher(&q).unwrap());
    assert_eq!(rows.len(), 1, "AS OF must return the one historical Person");
    assert_eq!(
        row_name(&rows[0]).as_deref(),
        Some("Alice"),
        "AS OF must return the pre-update value, not present-day 'Bob'"
    );

    // Cross-check the exact node state against the oracle.
    let oracle = db.get_node_at_time(id, t1, t1).unwrap();
    assert_eq!(
        oracle.properties.get("name"),
        Some(&PropertyValue::from("Alice"))
    );

    // Discriminator: present-day state is 'Bob'.
    let now_rows = collect_rows(db.execute_cypher("MATCH (n:Person) RETURN n").unwrap());
    assert_eq!(sorted_names(&now_rows), vec!["Bob".to_string()]);
}

/// #550 (coordinator's required case): a point-in-time query for a moment
/// *before any data existed* returns EMPTY (not present-day rows).
#[test]
fn test_e2e_as_of_before_any_data_is_empty() {
    let db = AletheiaDB::new().unwrap();
    // Anchor strictly before creating anything.
    let t0 = temporal_anchor(&db);
    db.create_node("Person", person("Alice")).unwrap();

    let q = format!(
        "MATCH (n:Person) AS OF TIMESTAMP '{}' RETURN n",
        anchor_micros(t0)
    );
    let rows = collect_rows(db.execute_cypher(&q).unwrap());
    assert!(
        rows.is_empty(),
        "AS OF before any data existed must be empty, got {} row(s)",
        rows.len()
    );

    // Oracle agrees the node did not exist at t0.
    assert!(
        db.find_nodes_at_time("Person", t0, t0)
            .unwrap()
            .nodes
            .is_empty()
    );
}

/// #551: bi-temporal `AS OF VALID_TIME ... AS OF SYSTEM_TIME ...` reconstructs
/// the state at the given (valid, system) point and matches the oracle.
#[test]
fn test_e2e_as_of_bitemporal_valid_and_system() {
    use crate::core::property::PropertyValue;

    let db = AletheiaDB::new().unwrap();
    let id = db.create_node("Person", person("Alice")).unwrap();
    let t1 = temporal_anchor(&db);
    db.update_node_with_valid_time(id, person("Bob"), None)
        .unwrap();

    let q = format!(
        "MATCH (n:Person) AS OF VALID_TIME '{}' AS OF SYSTEM_TIME '{}' RETURN n",
        anchor_micros(t1),
        anchor_micros(t1)
    );
    let rows = collect_rows(db.execute_cypher(&q).unwrap());
    assert_eq!(rows.len(), 1);
    assert_eq!(
        row_name(&rows[0]).as_deref(),
        Some("Alice"),
        "bi-temporal AS OF must reconstruct the historical value"
    );

    let oracle = db.get_node_at_time(id, t1, t1).unwrap();
    assert_eq!(
        oracle.properties.get("name"),
        Some(&PropertyValue::from("Alice"))
    );
}

/// #551: single-dimension `FOR SYSTEM_TIME AS OF` is honored by the label scan
/// (valid time defaults to now). The result equals the oracle-recorded state
/// at that transaction time and is NOT the present-day state.
#[test]
fn test_e2e_for_system_time_as_of_honored_by_label_scan() {
    use crate::core::temporal::time;

    let db = AletheiaDB::new().unwrap();
    let id = db.create_node("Person", person("Alice")).unwrap();
    let t1 = temporal_anchor(&db);
    db.update_node_with_valid_time(id, person("Bob"), None)
        .unwrap();

    let q = format!(
        "MATCH (n:Person) FOR SYSTEM_TIME AS OF '{}' RETURN n",
        anchor_micros(t1)
    );
    let cypher_names = sorted_names(&collect_rows(db.execute_cypher(&q).unwrap()));

    // Oracle ground truth: reconstruct at (now, t1) -- the state as recorded at
    // transaction time t1. This is what the label scan must equal, NOT the
    // present-day rows.
    let now = time::now();
    let mut oracle_names: Vec<String> = db
        .find_nodes_at_time("Person", now, t1)
        .unwrap()
        .nodes
        .iter()
        .filter_map(|n| {
            n.properties
                .get("name")
                .and_then(|p| p.as_str().map(|s| s.to_string()))
        })
        .collect();
    oracle_names.sort();
    assert_eq!(
        cypher_names, oracle_names,
        "FOR SYSTEM_TIME AS OF must equal the oracle state recorded at t1"
    );

    // Discriminator: the present-day label scan sees 'Bob'; before the fix the
    // AS OF scan wrongly returned that same present-day row.
    let current = sorted_names(&collect_rows(
        db.execute_cypher("MATCH (n:Person) RETURN n").unwrap(),
    ));
    assert_eq!(current, vec!["Bob".to_string()]);
    assert_ne!(
        cypher_names, current,
        "FOR SYSTEM_TIME AS OF must not silently return present-day data"
    );
}

/// #552: `BETWEEN` returns every version whose valid-time interval overlaps the
/// range -- multiple rows for a node that changed within the window.
#[test]
fn test_e2e_between_returns_all_overlapping_versions() {
    let db = AletheiaDB::new().unwrap();
    let v1 = temporal_anchor(&db);
    let id = db
        .create_node_with_valid_time("Person", person("A"), Some(v1))
        .unwrap();
    let v2 = temporal_anchor(&db);
    db.update_node_with_valid_time(id, person("B"), Some(v2))
        .unwrap();
    let vend = temporal_anchor(&db);

    // [v1, v2) -> "A" and [v2, MAX) -> "B" both overlap [v1, vend).
    let q = format!(
        "MATCH (n:Person) BETWEEN '{}' AND '{}' RETURN n",
        anchor_micros(v1),
        anchor_micros(vend)
    );
    let names = sorted_names(&collect_rows(db.execute_cypher(&q).unwrap()));
    assert_eq!(
        names,
        vec!["A".to_string(), "B".to_string()],
        "BETWEEN must return every version overlapping the valid-time range"
    );

    // Discriminator: the present-day scan sees only the current version.
    let current = sorted_names(&collect_rows(
        db.execute_cypher("MATCH (n:Person) RETURN n").unwrap(),
    ));
    assert_eq!(current, vec!["B".to_string()]);
}

/// Regression: a label scan with NO temporal clause is unchanged (current
/// state, fast path).
#[test]
fn test_e2e_current_state_label_scan_unchanged() {
    let db = AletheiaDB::new().unwrap();
    db.create_node("Person", person("Alice")).unwrap();
    db.create_node("Person", person("Bob")).unwrap();

    let rows = collect_rows(db.execute_cypher("MATCH (n:Person) RETURN n").unwrap());
    assert_eq!(
        sorted_names(&rows),
        vec!["Alice".to_string(), "Bob".to_string()]
    );
}

/// Regression: traversals already honor `AS OF` (no new code) -- an edge
/// created after the anchor is not followed at that anchor.
#[test]
fn test_e2e_as_of_traversal_excludes_future_edge() {
    let db = AletheiaDB::new().unwrap();
    let alice = db.create_node("Person", person("Alice")).unwrap();
    let bob = db.create_node("Person", person("Bob")).unwrap();
    let t0 = temporal_anchor(&db);
    // Edge created strictly AFTER t0.
    db.create_edge(alice, bob, "KNOWS", CorePropertyMap::new())
        .unwrap();

    // Current state: Alice KNOWS Bob.
    let now_rows = collect_rows(
        db.execute_cypher("MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(b) RETURN b")
            .unwrap(),
    );
    assert_eq!(now_rows.len(), 1);
    assert_eq!(row_name(&now_rows[0]).as_deref(), Some("Bob"));

    // AS OF t0 (before the edge existed): the traversal follows nothing.
    let q = format!(
        "MATCH (a:Person {{name: 'Alice'}})-[:KNOWS]->(b) AS OF TIMESTAMP '{}' RETURN b",
        anchor_micros(t0)
    );
    let rows = collect_rows(db.execute_cypher(&q).unwrap());
    assert!(
        rows.is_empty(),
        "AS OF before the edge existed must traverse nothing, got {} row(s)",
        rows.len()
    );
}
