#![cfg(feature = "sql")]
use aletheiadb::sql::parse_sql;

#[test]
fn test_extract_order_by_panic() {
    let sql = "ORDER BY ΐ <=>";
    let _ = parse_sql(sql);
}
