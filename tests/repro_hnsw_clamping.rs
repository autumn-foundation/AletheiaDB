use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, Quantization, VectorIndex};

#[test]
fn test_hnsw_cosine_clamping() {
    // Vectors from repro_panic.rs
    let a = vec![-8.161245e-22f32];
    let b = vec![-125.53673f32];

    let index = HnswIndexBuilder::new(1, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .build()
        .expect("Failed to build index");

    let id1 = NodeId::new(1).unwrap();
    let id2 = NodeId::new(2).unwrap();

    index.add(id1, &a).expect("Failed to add a");
    index.add(id2, &b).expect("Failed to add b");

    // Search for 'b' using 'a' as query
    let results = index.search(&a, 10).expect("Search failed");

    let mut found = false;
    for (id, score) in results {
        if id == id2 {
            found = true;
            println!("Score for b: {:.20}", score);
            // Assert finite and in range
            assert!(
                score.is_finite(),
                "Cosine similarity is not finite: {}",
                score
            );
            assert!(
                (-1.0..=1.0).contains(&score),
                "Cosine similarity out of range: {}",
                score
            );
        }
    }
    assert!(found, "Did not find id2 in search results");
}
