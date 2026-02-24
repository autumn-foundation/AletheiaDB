use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::experimental::chameleon::Chameleon;

#[test]
fn test_chameleon_dimension_mismatch_handled() {
    let db = AletheiaDB::new().unwrap();

    let center = db
        .create_node("Center", PropertyMapBuilder::new().build())
        .unwrap();

    // Neighbor 1: Dim 2
    let n1 = db
        .create_node(
            "N1",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build(),
        )
        .unwrap();

    // Neighbor 2: Dim 3
    let n2 = db
        .create_node(
            "N2",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0, 1.0])
                .build(),
        )
        .unwrap();

    db.create_edge(center, n1, "LINK", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(center, n2, "LINK", PropertyMapBuilder::new().build())
        .unwrap();

    let chameleon = Chameleon::new(&db);

    // This should NOT panic anymore
    let aspects = chameleon.analyze_context(center, "vec", 2).unwrap();

    // One of them should be filtered out, so we expect 1 aspect (since k=2 but valid data=1)
    assert_eq!(
        aspects.len(),
        1,
        "Should filter out mismatched dimensions and return 1 aspect"
    );
}
