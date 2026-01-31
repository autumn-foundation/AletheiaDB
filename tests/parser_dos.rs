use gallifreydb::query::parser::Parser;

#[test]
fn test_parser_stack_overflow_protection() {
    // Construct a deeply nested query in the WHERE clause
    // MATCH (n) WHERE (((((... true ...))))) RETURN n
    // This targets parse_predicate -> parse_primary_predicate -> parse_grouped_predicate recursion
    let depth = 5000;
    let mut query = String::with_capacity(depth * 2 + 50);

    query.push_str("MATCH (n) WHERE ");
    for _ in 0..depth {
        query.push('(');
    }
    query.push_str("true");
    for _ in 0..depth {
        query.push(')');
    }
    query.push_str(" RETURN n");

    // Attempt to parse
    // Before fix: This crashes with stack overflow (SIGSEGV/SIGBUS)
    // After fix: This should return Err(ParseError) with message about recursion limit
    let result = Parser::parse(&query);

    assert!(result.is_err(), "Parser should reject deeply nested query");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("Recursion limit"),
        "Error should mention recursion limit, got: {}",
        err.message
    );
}
