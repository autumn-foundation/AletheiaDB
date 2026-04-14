//! Sentinel targeted tests for Query Parser mutants.
//!
//! These tests are specifically designed to catch regressions in boundary conditions
//! and edge cases that were identified by mutation testing.

use aletheiadb::query::ast::{DepthSpec, PatternElement, SourceClause};
use aletheiadb::query::parser::Parser;

#[test]
fn test_parse_exact_depth_range() {
    // 🎯 Target: Kills `replace > with >=` in `parse_depth_range` (line 707)
    // 💣 Risk: Off-by-one error rejecting valid `min..min` ranges
    let query = Parser::parse("MATCH (a)-[:REL*2..2]->(b) RETURN b")
        .expect("Should parse exact depth range 2..2");

    if let SourceClause::Match(patterns) = &query.source {
        if let PatternElement::Relationship(rel) = &patterns[0].elements[1] {
            assert_eq!(
                rel.depth,
                Some(DepthSpec::Range { min: 2, max: 2 }),
                "Expected Range {{ min: 2, max: 2 }}"
            );
        } else {
            panic!("Expected relationship pattern");
        }
    } else {
        panic!("Expected MATCH clause");
    }
}

#[test]
fn test_parse_unbounded_max_depth() {
    // 🎯 Target: Kills `replace / with %` or `*` in `parse_depth_range` (line 720)
    // 💣 Risk: Incorrect max depth calculation for `min..` ranges
    let query = Parser::parse("MATCH (a)-[:REL*2..]->(b) RETURN b")
        .expect("Should parse unbounded max depth 2..");

    if let SourceClause::Match(patterns) = &query.source {
        if let PatternElement::Relationship(rel) = &patterns[0].elements[1] {
            if let Some(DepthSpec::Range { min, max }) = rel.depth {
                assert_eq!(min, 2, "Min depth should be 2");
                // Original implementation uses usize::MAX / 2
                // We assert it's "very large" (e.g. > 1000) to distinguish from small mutation values
                assert!(
                    max > 1000,
                    "Max depth should be large (unbounded), got {}",
                    max
                );
            } else {
                panic!("Expected Range depth spec");
            }
        } else {
            panic!("Expected relationship pattern");
        }
    } else {
        panic!("Expected MATCH clause");
    }
}

#[test]
fn test_parse_limit_zero() {
    // 🎯 Target: Kills `replace < with <=` in `parse_usize` (used for LIMIT/SKIP)
    // 💣 Risk: Rejecting valid LIMIT 0
    let query = Parser::parse("MATCH (n) RETURN n LIMIT 0").expect("Should parse LIMIT 0");

    assert_eq!(query.limit, Some(0));
}

#[test]
fn test_parse_skip_zero() {
    // 🎯 Target: Kills `replace < with <=` in `parse_usize` (used for LIMIT/SKIP)
    // 💣 Risk: Rejecting valid SKIP 0
    let query = Parser::parse("MATCH (n) RETURN n SKIP 0").expect("Should parse SKIP 0");

    assert_eq!(query.skip, Some(0));
}

#[test]
fn test_parse_depth_zero() {
    // 🎯 Target: Kills `replace < with <=` in `validate_non_negative` (used for depth)
    // 💣 Risk: Rejecting valid zero-length paths (*0)
    let query = Parser::parse("MATCH (a)-[:REL*0]->(b) RETURN b").expect("Should parse depth *0");

    if let SourceClause::Match(patterns) = &query.source {
        if let PatternElement::Relationship(rel) = &patterns[0].elements[1] {
            assert_eq!(rel.depth, Some(DepthSpec::Exact(0)));
        } else {
            panic!("Expected relationship pattern");
        }
    } else {
        panic!("Expected MATCH clause");
    }
}
