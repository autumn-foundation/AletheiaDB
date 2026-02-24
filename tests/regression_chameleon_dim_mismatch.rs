use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::experimental::chameleon::Chameleon;

#[test]
// This test verifies that Chameleon::analyze_context handles dimension mismatch
// gracefully by ignoring vectors with inconsistent dimensions, instead of panicking.
fn test_chameleon_ignores_dimension_mismatch() {
    let db = AletheiaDB::new().unwrap();

    // Create Central Node
    let center = db
        .create_node("Center", PropertyMapBuilder::new().build())
        .unwrap();

    // Create Neighbor 1 with dimension 2 (This sets the expected dimension)
    let n1 = db
        .create_node(
            "N1",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build(),
        )
        .unwrap();

    // Create Neighbor 2 with dimension 3 (mismatch!)
    let n2 = db
        .create_node(
            "N2",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.0, 1.0, 0.5])
                .build(),
        )
        .unwrap();

    // Connect
    db.create_edge(center, n1, "LINK", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(center, n2, "LINK", PropertyMapBuilder::new().build())
        .unwrap();

    let chameleon = Chameleon::new(&db);

    // This should NOT panic. It should return aspects based on valid vectors (n1).
    let aspects = chameleon.analyze_context(center, "vec", 2);

    assert!(
        aspects.is_ok(),
        "analyze_context failed: {:?}",
        aspects.err()
    );
    let aspects = aspects.unwrap();

    // Should find at least 1 aspect (from n1)
    // n2 is ignored, so effectively we have 1 neighbor.
    // If K=2 requested but only 1 point, we get 1 cluster.
    assert!(!aspects.is_empty(), "Should find at least one aspect");

    // Verify the aspect centroid matches n1 (since it's the only valid neighbor)
    let centroid = &aspects[0].centroid;
    assert_eq!(centroid.len(), 2, "Aspect centroid should have dimension 2");
    // n1 is [1.0, 0.0], normalized is [1.0, 0.0]
    assert!((centroid[0] - 1.0).abs() < 1e-6);
    assert!((centroid[1] - 0.0).abs() < 1e-6);
}
