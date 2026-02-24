use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::experimental::chameleon::Chameleon;

#[test]
fn test_chameleon_dimension_mismatch_panic() {
    let db = AletheiaDB::new().unwrap();
    // Do NOT enable vector index. Just use property storage.

    let center = db
        .create_node("Center", PropertyMapBuilder::new().build())
        .unwrap();

    // Neighbor 1: 2D vector
    let n1 = db
        .create_node(
            "N1",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 1.0])
                .build(),
        )
        .unwrap();

    // Neighbor 2: 3D vector (Mismatch!)
    // Without index, this should succeed if DB is schematic-less for non-indexed props.
    let n2 = db
        .create_node(
            "N2",
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

    // This calls analyze_context which uses MiniKMeans.
    // MiniKMeans assumes all vectors have same dimension as the first one.
    // If n1 is first (2D), and n2 is second (3D), it will crash when processing n2.
    // If n2 is first (3D), and n1 is second (2D), it will panic in zip or just calculate wrong.

    let result = chameleon.analyze_context(center, "vec", 2);

    assert!(
        result.is_ok(),
        "analyze_context should handle dimension mismatch gracefully"
    );
    let aspects = result.unwrap();
    // We expect at least one aspect (from the valid vector(s))
    // Or if filtering removes all, it returns empty.
    // Here n1 is 2D, n2 is 3D.
    // If n1 is processed first, n2 is skipped. Result has n1.
    // If n2 is processed first, n1 is skipped. Result has n2.
    // In either case, we should get aspects (unless both filtered out, which shouldn't happen).
    assert!(
        !aspects.is_empty(),
        "Should return aspects from the valid vector subset"
    );
}
