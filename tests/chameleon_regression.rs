#![cfg(feature = "nova")]

use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::experimental::chameleon::Chameleon;

#[test]
fn test_chameleon_dimension_mismatch_panic() {
    let db = AletheiaDB::new().unwrap();
    // Do NOT enable vector index to allow mixed dimensions in storage

    let center = db
        .create_node("Center", PropertyMapBuilder::new().build())
        .unwrap();

    // Node 1: Dimension 2
    let n1 = db
        .create_node(
            "A",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 1.0])
                .build(),
        )
        .unwrap();

    // Node 2: Dimension 3
    let n2 = db
        .create_node(
            "B",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 1.0, 1.0])
                .build(),
        )
        .unwrap();

    db.create_edge(center, n1, "LINK", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(center, n2, "LINK", PropertyMapBuilder::new().build())
        .unwrap();

    let chameleon = Chameleon::new(&db);

    // This should no longer panic, and return a valid result (even if partial)
    let result = chameleon.analyze_context(center, "vec", 2);
    assert!(result.is_ok());
}
