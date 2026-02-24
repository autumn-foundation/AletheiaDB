use aletheiadb::query::ast::{NodePattern, PatternElement, PropertyValue, SourceClause};
use aletheiadb::query::parser::Parser;

#[test]
fn test_parse_negative_property_value_spaced() {
    // This test targets the mutant: delete - in Parser::parse_value
    // It specifically tests the path where a negative number is parsed as `Dash` then `IntegerLiteral`.
    // Example: {score: - 5}
    let query = Parser::parse("MATCH (n {score: - 5}) RETURN n").unwrap();

    if let SourceClause::Match(patterns) = &query.source {
        if let PatternElement::Node(NodePattern {
            properties: Some(props),
            ..
        }) = &patterns[0].elements[0]
        {
            let (_, value) = props.iter().find(|(k, _)| k == "score").unwrap();
            assert_eq!(
                *value,
                PropertyValue::Int(-5),
                "Expected -5, got {:?}",
                value
            );
        } else {
            panic!("Expected node pattern with properties");
        }
    } else {
        panic!("Expected MATCH clause");
    }
}

#[test]
fn test_parse_negative_float_property_value_spaced() {
    // This test targets the mutant: delete - in Parser::parse_value (float branch)
    // Example: {score: - 0.5}
    let query = Parser::parse("MATCH (n {score: - 0.5}) RETURN n").unwrap();

    if let SourceClause::Match(patterns) = &query.source {
        if let PatternElement::Node(NodePattern {
            properties: Some(props),
            ..
        }) = &patterns[0].elements[0]
        {
            let (_, value) = props.iter().find(|(k, _)| k == "score").unwrap();
            if let PropertyValue::Float(f) = value {
                assert!((f - -0.5).abs() < f64::EPSILON, "Expected -0.5, got {}", f);
            } else {
                panic!("Expected float property value, got {:?}", value);
            }
        } else {
            panic!("Expected node pattern with properties");
        }
    } else {
        panic!("Expected MATCH clause");
    }
}

#[test]
fn test_parse_negative_property_value_unspaced() {
    // This tests the path where negative number is parsed as a single token (if lexer supports it)
    // or unspaced sequence. The mutant might not affect this if lexer produces `IntegerLiteral(-5)`.
    // Example: {score: -5}
    let query = Parser::parse("MATCH (n {score: -5}) RETURN n").unwrap();

    if let SourceClause::Match(patterns) = &query.source {
        if let PatternElement::Node(NodePattern {
            properties: Some(props),
            ..
        }) = &patterns[0].elements[0]
        {
            let (_, value) = props.iter().find(|(k, _)| k == "score").unwrap();
            assert_eq!(
                *value,
                PropertyValue::Int(-5),
                "Expected -5, got {:?}",
                value
            );
        } else {
            panic!("Expected node pattern with properties");
        }
    } else {
        panic!("Expected MATCH clause");
    }
}
