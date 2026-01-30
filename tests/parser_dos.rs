use gallifreydb::query::parser::Parser;

#[test]
fn test_parser_recursion_limit_parentheses() {
    // Deeply nested parentheses exceeding the limit (100)
    let depth = 200;
    let mut query = String::from("MATCH (n) WHERE ");
    for _ in 0..depth {
        query.push('(');
    }
    query.push_str("n.age > 18");
    for _ in 0..depth {
        query.push(')');
    }
    query.push_str(" RETURN n");

    let result = Parser::parse(&query);
    assert!(
        result.is_err(),
        "Parser should fail with recursion limit error"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("Recursion limit reached"),
        "Error message should mention recursion limit"
    );
}

#[test]
fn test_parser_recursion_limit_not() {
    // Deeply nested NOT
    let depth = 200;
    let mut query = String::from("MATCH (n) WHERE ");
    for _ in 0..depth {
        query.push_str("NOT ");
    }
    query.push_str("n.active = true RETURN n");

    let result = Parser::parse(&query);
    assert!(
        result.is_err(),
        "Parser should fail with recursion limit error"
    );
    let err = result.unwrap_err();
    assert!(
        err.message.contains("Recursion limit reached"),
        "Error message should mention recursion limit"
    );
}
