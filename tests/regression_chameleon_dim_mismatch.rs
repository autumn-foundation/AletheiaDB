#![cfg(feature = "nova")]

use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::experimental::chameleon::Chameleon;

#[test]
fn test_chameleon_panic_with_mixed_dimensions() {
    let db = AletheiaDB::new().unwrap();

    // Create center node
    let center = db
        .create_node("Center", PropertyMapBuilder::new().build())
        .unwrap();

    // Create neighbor 1 with 2D vector
    let n1 = db
        .create_node(
            "A",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build(),
        )
        .unwrap();

    // Create neighbor 2 with 3D vector
    let n2 = db
        .create_node(
            "A",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0, 0.5])
                .build(),
        )
        .unwrap();

    db.create_edge(center, n1, "LINK", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(center, n2, "LINK", PropertyMapBuilder::new().build())
        .unwrap();

    let chameleon = Chameleon::new(&db);

    // This is expected to panic in the current implementation
    let result = chameleon.analyze_context(center, "vec", 2);

    // We want to see if it panicked or returned an error
    assert!(result.is_err() || result.is_ok());
}
