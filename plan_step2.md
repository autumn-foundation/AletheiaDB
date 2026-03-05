<<<<<<< SEARCH
        // The Aura vector should be somewhere between [1.0, 0.0] and [0.0, 1.0].
        // But the *current* vector is purely [0.0, 1.0].
        // This means there should be significant divergence!
        assert!(
            result.divergence_score > 0.1,
            "Expected significant divergence, got {}",
            result.divergence_score
        );
    }
}
=======
        // The Aura vector should be somewhere between [1.0, 0.0] and [0.0, 1.0].
        // But the *current* vector is purely [0.0, 1.0].
        // This means there should be significant divergence!
        assert!(
            result.divergence_score > 0.1,
            "Expected significant divergence, got {}",
            result.divergence_score
        );
    }

    #[test]
    fn test_aura_dimension_mismatch() {
        let db = AletheiaDB::new().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut n1 = crate::core::id::NodeId::new(0).unwrap();

        // State 1: 2-dimensional vector
        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // State 2: 3-dimensional vector
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0, 0.5])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let engine = AuraEngine::new(&db);
        let result = engine.calculate_aura(n1, "vec", 1_000_000).unwrap();

        // Because dimensions mismatched over time, divergence should be handled
        // gracefully and default to 1.0 divergence (meaning total semantic shift)
        assert_eq!(result.divergence_score, 1.0);
    }
}
>>>>>>> REPLACE
