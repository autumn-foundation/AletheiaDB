#![cfg(feature = "nova")]

use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::experimental::aura::AuraEngine;

#[test]
fn test_aura_dim_mismatch() {
    let db = AletheiaDB::new().unwrap();
    let mut n1 = aletheiadb::core::id::NodeId::new(0).unwrap();

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

    std::thread::sleep(std::time::Duration::from_millis(10));

    db.write(|tx| {
        tx.update_node(
            n1,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.0, 1.0, 2.0]) // Different dimension!
                .build(),
        )
        .unwrap();
        Ok::<(), aletheiadb::core::error::Error>(())
    })
    .unwrap();

    let engine = AuraEngine::new(&db);
    let result = engine.calculate_aura(n1, "vec", 1_000_000).unwrap();
    println!("Result: {:?}", result);
}
