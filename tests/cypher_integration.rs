#![cfg(feature = "cypher")]

use aletheiadb::AletheiaDB;
// Macros with #[macro_export] are exported at the crate root
#[allow(unused_imports)]
use aletheiadb::params;

#[test]
fn test_cypher_api_surface() {
    let db = AletheiaDB::new().unwrap();

    // Test basic cypher query
    let results = db.cypher("MATCH (n:Person) RETURN n");
    assert!(results.is_ok());

    // Test query with params
    // We expect params! to be available when feature is enabled
    let params_map = aletheiadb::params! {
        "name" => "Alice",
        "age" => 30,
    };

    let results_with_params =
        db.cypher_with_params("MATCH (n:Person {name: $name}) RETURN n", params_map);
    assert!(results_with_params.is_ok());
}
