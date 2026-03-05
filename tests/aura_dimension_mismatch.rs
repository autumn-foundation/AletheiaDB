#[cfg(feature = "nova")]
mod tests {
    use aletheiadb::AletheiaDB;
    use aletheiadb::experimental::aura::AuraEngine;
    use aletheiadb::core::property::PropertyMapBuilder;
    use aletheiadb::api::transaction::WriteOps;

    #[test]
    fn test_aura_dimension_mismatch() {
        let db = AletheiaDB::new().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut n1 = aletheiadb::core::id::NodeId::new(0).unwrap();

        // State 1: vector dim 2
        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), aletheiadb::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));

        // State 2: vector dim 4
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0, 0.5, 0.5])
                    .build(),
            )
            .unwrap();
            Ok::<(), aletheiadb::core::error::Error>(())
        })
        .unwrap();

        let engine = AuraEngine::new(&db);
        let result = engine.calculate_aura(n1, "vec", 1_000_000);
        assert!(result.is_ok(), "Should not return an error due to dimension mismatch");
        let result = result.unwrap();
        assert_eq!(result.divergence_score, 1.0, "Divergence should be 1.0 on dimension mismatch");
    }
}
