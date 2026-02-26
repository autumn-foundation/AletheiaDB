#[cfg(feature = "nova")]
#[test]
fn test_pygmalion_integration() {
    use aletheiadb::AletheiaDB;
    use aletheiadb::core::property::PropertyMapBuilder;
    use aletheiadb::experimental::pygmalion::{ImputationStrategy, Pygmalion};
    use aletheiadb::index::vector::{DistanceMetric, HnswConfig};

    let db = AletheiaDB::new().unwrap();
    db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
        .unwrap();

    // 1. Create Data
    // Target: [1.0, 0.0] (Missing "price")
    let target = db
        .create_node(
            "Item",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build(),
        )
        .unwrap();

    // Neighbor: [1.0, 0.0], Price 100
    db.create_node(
        "Item",
        PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .insert("price", 100)
            .build(),
    )
    .unwrap();

    // 2. Impute
    let pygmalion = Pygmalion::new(&db);
    let result = pygmalion
        .impute(target, "price", ImputationStrategy::Nearest, 5)
        .unwrap()
        .unwrap();

    assert_eq!(result.as_int(), Some(100));
}
