use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::AletheiaDB;
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};

#[test]
fn test_vector_rerank_memory_risk_iterator_lazy_loading() {
    let db = AletheiaDB::new().unwrap();
    db.vector_index("embedding").hnsw(HnswConfig::new(2, DistanceMetric::Cosine)).enable().unwrap();

    let _n1 = db.create_node("Node", PropertyMapBuilder::new()
        .insert_vector("embedding", &[1.0, 0.0])
        .build()).unwrap();
    let _n2 = db.create_node("Node", PropertyMapBuilder::new()
        .insert_vector("embedding", &[0.0, 1.0])
        .build()).unwrap();

    let results = db.query()
        .scan_label("Node")
        .rank_by_similarity(&[1.0, 0.0], 0)
        .execute(&db)
        .unwrap();

    let mut count = 0;
    for _ in results {
        count += 1;
    }
    assert_eq!(count, 0);
}
