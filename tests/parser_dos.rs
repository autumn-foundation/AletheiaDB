#[cfg(test)]
mod tests {
    use aletheiadb::query::parser::Parser;

    #[test]
    fn test_parser_recursion_at_limit() {
        // Test that depth=200 works (limit is 200), depth=201 fails
        // Note: The limit applies to nested expressions.
        // The query "MATCH (n) WHERE ((...))" has nesting.

        // Case 1: Depth 199 (should pass)
        let mut query = "MATCH (n) WHERE ".to_string();
        for _ in 0..199 {
            query.push('(');
        }
        query.push_str("n.age > 10");
        for _ in 0..199 {
            query.push(')');
        }
        query.push_str(" RETURN n");

        let result = Parser::parse(&query);
        assert!(result.is_ok(), "Parser should accept depth=199");

        // Case 1.5: Depth 200 (should pass if limit is 200)
        let mut query = "MATCH (n) WHERE ".to_string();
        for _ in 0..200 {
            query.push('(');
        }
        query.push_str("n.age > 10");
        for _ in 0..200 {
            query.push(')');
        }
        query.push_str(" RETURN n");

        let result = Parser::parse(&query);
        assert!(result.is_ok(), "Parser should accept depth=200");

        // Case 2: Depth 201 (should fail if limit is 200)
        let mut query = "MATCH (n) WHERE ".to_string();
        for _ in 0..201 {
            query.push('(');
        }
        query.push_str("n.age > 10");
        for _ in 0..201 {
            query.push(')');
        }
        query.push_str(" RETURN n");

        let result = Parser::parse(&query);
        assert!(result.is_err(), "Parser should reject depth=201");
    }

    #[test]
    fn test_parser_stack_overflow() {
        // Create a deeply nested query
        let depth = 10000;
        let mut query = "MATCH (n) WHERE ".to_string();
        for _ in 0..depth {
            query.push('(');
        }
        query.push_str("n.age > 10");
        for _ in 0..depth {
            query.push(')');
        }
        query.push_str(" RETURN n");

        // This should fail with a parser error (due to depth limit)
        // OR crash with stack overflow if not protected.
        // We want it to return an error gracefully.
        let result = Parser::parse(&query);
        assert!(
            result.is_err(),
            "Parser should return error for deep nesting"
        );

        // Verify the error message mentions recursion limit
        if let Err(e) = result {
            assert!(
                e.message.contains("Recursion limit exceeded"),
                "Error should mention recursion limit, got: {}",
                e.message
            );
        }
    }
}
