
#![cfg(feature = "nova")]

use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::experimental::semantic_navigator::SemanticNavigator;
use aletheiadb::core::vector::cosine_similarity;

#[test]
fn test_semantic_navigator_nan_handling() {
    let db = AletheiaDB::new().unwrap();

    // Setup graph: A -> B
    // A has vector [1.0, 0.0]
    // B has vector [NaN, NaN] (or something that causes NaN similarity)

    // Using NaN directly in vector might be filtered by validation, let's try.
    // Core vector validation usually happens.
    // But let's assume we can insert it or cause it.

    // Alternatively, we use vectors that result in NaN cosine similarity.
    // Zero vectors result in 0.0 similarity (handled).
    // Infinity?

    // Let's try to insert NaN vector.
    let vec_a = vec![1.0, 0.0];
    let vec_b = vec![f32::NAN, f32::NAN];

    let props_a = PropertyMapBuilder::new()
        .insert_vector("vec", &vec_a)
        .build();
    // Bypassing normal validation if possible, or hoping builder allows it.
    // PropertyMapBuilder calls PropertyValue::vector which might validate?
    // Let's check.
    // PropertyValue::vector(v) just stores it.

    let props_b = PropertyMapBuilder::new()
        .insert_vector("vec", &vec_b)
        .build();

    let a = db.create_node("Node", props_a).unwrap();
    let b = db.create_node("Node", props_b).unwrap();

    db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build()).unwrap();

    let nav = SemanticNavigator::new(&db);

    // Pathfinding from A to B.
    // Cost(A, B) = 1.0 - sim(A, B)
    // sim(A, B) with NaN should return NaN (from cosine_similarity).
    // Cost should be NaN.
    // If Cost is NaN, it should be treated as high cost (1.0), but current implementation propagates NaN.
    // If it propagates NaN, the priority queue logic `tentative_g < neighbor_g` fails (NaN < Inf is false).
    // So B is never added to open set.
    // Path finding fails.

    let path = nav.find_path(a, b, "vec");

    // If fix is applied, it should find path (cost 1.0).
    // If bug exists, it should fail.
    assert!(path.is_ok(), "Should find path even with NaN vectors (treating as high cost)");
}
