#[cfg(test)]
mod tests {
    use aletheiadb::query::parser::Parser;

    #[test]
    fn test_parser_recursion_at_limit() {
        // Test that depth=100 works (limit is 100), depth=101 fails
        // Note: The limit applies to nested expressions.
        // The query "MATCH (n) WHERE ((...))" has nesting.

        // Case 1: Depth 99 (should pass)
        let mut query = "MATCH (n) WHERE ".to_string();
        for _ in 0..99 {
            query.push('(');
        }
        query.push_str("n.age > 10");
        for _ in 0..99 {
            query.push(')');
        }
        query.push_str(" RETURN n");

        let result = Parser::parse(&query);
        assert!(result.is_ok(), "Parser should accept depth=99");

        // Case 2: Depth 101 (should fail if limit is 100)
        let mut query = "MATCH (n) WHERE ".to_string();
        for _ in 0..101 {
            query.push('(');
        }
        query.push_str("n.age > 10");
        for _ in 0..101 {
            query.push(')');
        }
        query.push_str(" RETURN n");

        let result = Parser::parse(&query);
        assert!(result.is_err(), "Parser should reject depth=101");
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
