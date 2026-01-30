#[cfg(test)]
mod tests {
    use gallifreydb::query::parser::Parser;

    #[test]
    fn test_parser_stack_overflow() {
        // Create a deeply nested query
        let depth = 10000;
        let mut query = "MATCH (n) WHERE ".to_string();
        for _ in 0..depth {
            query.push_str("(");
        }
        query.push_str("n.age > 10");
        for _ in 0..depth {
            query.push_str(")");
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
