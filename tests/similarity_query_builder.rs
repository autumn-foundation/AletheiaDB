//! Tests for the unified `SimilarityQuery` builder API (Issue #352).
//!
//! These verify the builder-based `db.similarity_search(...)` entry point that
//! consolidates the previously separate `find_similar*` methods, and that it
//! stays in parity with the (now deprecated) legacy methods.

use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::index::vector::temporal::TemporalVectorConfig;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::{AletheiaDB, SimilarityQuery};

/// Build a small database with three labeled documents and a vector index.
fn setup_db() -> (AletheiaDB, NodeId, NodeId, NodeId) {
    let db = AletheiaDB::new().unwrap();
    let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config).unwrap();

    let doc1 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Rust Programming")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();
    let doc2 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Rust Advanced")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                .build(),
        )
        .unwrap();
    let person = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert_vector("embedding", &[0.85f32, 0.15, 0.0])
                .build(),
        )
        .unwrap();
    (db, doc1, doc2, person)
}

#[test]
fn builder_defaults_and_setters() {
    let (_db, doc1, _doc2, _person) = setup_db();
    // A from_node query with default k should be constructible.
    let _q = SimilarityQuery::from_node(doc1);
    // And fully configured via the fluent setters.
    let _q2 = SimilarityQuery::from_embedding(vec![0.1f32, 0.2, 0.3])
        .k(5)
        .with_label("Document");
}

#[test]
fn from_node_matches_legacy_find_similar() {
    let (db, doc1, _doc2, _person) = setup_db();

    let expected = {
        #[allow(deprecated)]
        db.find_similar(doc1, 2).unwrap()
    };
    let actual = db
        .similarity_search(SimilarityQuery::from_node(doc1).k(2))
        .unwrap();
    assert_eq!(expected, actual);
}

#[test]
fn from_node_with_label_matches_legacy() {
    let (db, doc1, _doc2, person) = setup_db();

    let expected = {
        #[allow(deprecated)]
        db.find_similar_with_label(doc1, "Document", 5).unwrap()
    };
    let actual = db
        .similarity_search(SimilarityQuery::from_node(doc1).k(5).with_label("Document"))
        .unwrap();
    assert_eq!(expected, actual);
    // Label filtering must exclude the Person node.
    assert!(actual.iter().all(|(id, _)| *id != person));
}

#[test]
fn from_embedding_matches_legacy() {
    let (db, _doc1, _doc2, _person) = setup_db();
    let query = [0.95f32, 0.05, 0.0];

    let expected = {
        #[allow(deprecated)]
        db.find_similar_by_embedding(&query, 3).unwrap()
    };
    let actual = db
        .similarity_search(SimilarityQuery::from_embedding(query.to_vec()).k(3))
        .unwrap();
    assert_eq!(expected, actual);
}

#[test]
fn from_embedding_with_label_matches_legacy() {
    let (db, _doc1, _doc2, _person) = setup_db();
    let query = [0.95f32, 0.05, 0.0];

    let expected = {
        #[allow(deprecated)]
        db.find_similar_by_embedding_with_label(&query, "Document", 5)
            .unwrap()
    };
    let actual = db
        .similarity_search(
            SimilarityQuery::from_embedding(query.to_vec())
                .k(5)
                .with_label("Document"),
        )
        .unwrap();
    assert_eq!(expected, actual);
}

#[test]
fn from_embedding_at_time_matches_legacy() {
    let db = AletheiaDB::new().unwrap();
    let hnsw = HnswConfig::new(3, DistanceMetric::Cosine);
    db.enable_temporal_vector_index("embedding", TemporalVectorConfig::default_with_hnsw(hnsw))
        .unwrap();

    let _node = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    let ts = aletheiadb::core::temporal::time::now();
    let query = [1.0f32, 0.0, 0.0];

    let expected = {
        #[allow(deprecated)]
        db.find_similar_as_of(&query, 5, ts)
    };
    let actual = db.similarity_search(
        SimilarityQuery::from_embedding(query.to_vec())
            .k(5)
            .at_time(ts),
    );

    // Both paths should agree (either both Ok with equal results, or both Err).
    match (expected, actual) {
        (Ok(e), Ok(a)) => assert_eq!(e, a),
        (Err(_), Err(_)) => {}
        other => panic!("legacy and builder paths disagree: {other:?}"),
    }
}

#[test]
fn unsupported_node_plus_time_errors() {
    let (db, doc1, _doc2, _person) = setup_db();
    let ts = aletheiadb::core::temporal::time::now();
    let result = db.similarity_search(SimilarityQuery::from_node(doc1).k(5).at_time(ts));
    assert!(result.is_err(), "node + at_time should be rejected");
}

#[test]
fn unsupported_embedding_label_plus_time_errors() {
    let (db, _doc1, _doc2, _person) = setup_db();
    let ts = aletheiadb::core::temporal::time::now();
    let result = db.similarity_search(
        SimilarityQuery::from_embedding(vec![0.1f32, 0.2, 0.3])
            .k(5)
            .with_label("Document")
            .at_time(ts),
    );
    assert!(
        result.is_err(),
        "embedding + label + at_time should be rejected"
    );
}
