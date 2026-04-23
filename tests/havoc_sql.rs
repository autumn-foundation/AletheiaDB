use aletheiadb::sql::{parse_sql, SqlParser};
use proptest::prelude::*;

// A really mean generator that biases towards operators and single quotes
fn sql_mean_fuzz_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        proptest::sample::select(&[
            "SELECT", "FROM", "WHERE", "ORDER BY", "LIMIT", "MATCH", "FOR SYSTEM_TIME AS OF",
            "=", "<", ">", "AND", "OR", "NOT", "(", ")", "[", "]", "'", "\"", ",", ".", "*",
            "nodes", "edges", "1", "0.5", "TRUE", "FALSE", "NULL", "::vector", "<=>", "<->",
            "\n", "\t", " ", "A", "B", "_", "-", "a", "b", "''", "'''", "''''", "'''::vector",
            "'''<=>'''"
        ][..]),
        0..100
    ).prop_map(|v| v.join(""))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn test_parser_mean_fuzz(s in sql_mean_fuzz_strategy()) {
        let _ = SqlParser::parse_multiple(&s);
    }

    #[test]
    fn test_converter_mean_fuzz(s in sql_mean_fuzz_strategy()) {
        let _ = parse_sql(&s);
    }
}

// We use purely random printable characters and all unicode combinations
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn test_parser_fuzz_random(s in ".*") {
        let _ = SqlParser::parse_multiple(&s);
    }

    #[test]
    fn test_converter_fuzz_random(s in ".*") {
        let _ = parse_sql(&s);
    }
}
