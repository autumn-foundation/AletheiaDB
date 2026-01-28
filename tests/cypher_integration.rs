#![cfg(feature = "cypher")]

use gallifreydb::GallifreyDB;
// Macros with #[macro_export] are exported at the crate root
#[allow(unused_imports)]
use gallifreydb::params;

#[test]
fn test_cypher_api_surface() {
    let db = GallifreyDB::new().unwrap();

    // Test basic query - should fail compilation initially
    let results = db.cypher("MATCH (n:Person) RETURN n");
    assert!(results.is_ok());

    // Test query with params - should fail compilation initially
    // We expect params! to be available when feature is enabled
    let params_map = gallifreydb::params! {
        "name" => "Alice",
        "age" => 30,
    };

    let results_with_params =
        db.cypher_with_params("MATCH (n:Person {name: $name}) RETURN n", params_map);
    assert!(results_with_params.is_ok());
}
