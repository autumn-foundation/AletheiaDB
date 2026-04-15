#![cfg(feature = "sql")]
use aletheiadb::sql::parse_sql;

#[test]
fn test_extract_order_by_panic() {
    let sql = "ORDER BY ΐ <=>";
    let _ = parse_sql(sql);
}

/// Regression test: multi-byte characters *before* the ORDER BY clause must not
/// cause a panic due to incorrect byte offsets derived from an uppercased string.
#[test]
fn test_extract_order_by_multibyte_before_order_by() {
    // ΐ (U+0390) is 2 bytes in UTF-8 but expands to multiple bytes when uppercased.
    // The parser must still locate ORDER BY at the correct byte offset in the
    // original string and must not panic.
    let sql = "SELECT 'ΐ' FROM foo ORDER BY embedding <=> $vec";
    let _ = parse_sql(sql);
}
