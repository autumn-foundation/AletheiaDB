use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn fuzz_aql_parser(s in ".*") {
        let _ = aletheiadb::query::parser::Parser::parse(&s);
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn fuzz_cypher_parser(s in ".*") {
        let _ = aletheiadb::cypher::parse_cypher(&s);
    }

    #[cfg(feature = "sql")]
    #[test]
    fn fuzz_sql_parser(s in ".*") {
        let _ = aletheiadb::sql::parse_sql(&s);
    }
}
