#[cfg(test)]
mod tests {
    use aletheiadb::sql::parse_sql;

    #[test]
    fn test_sql_parser_stack_overflow() {
        let depth = 10000;
        let mut query = "SELECT * FROM nodes WHERE ".to_string();
        for _ in 0..depth {
            query.push_str("id = 1 AND (");
        }
        query.push_str("id = 1");
        for _ in 0..depth {
            query.push(')');
        }

        let result = parse_sql(&query);
        assert!(result.is_err());
    }
}
