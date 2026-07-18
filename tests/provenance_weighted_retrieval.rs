//! Integration tests for provenance-weighted retrieval fusion (Issue #3372).
//!
//! These exercise the Rust-API surface end-to-end:
//! [`AletheiaDB::similarity_search_fused`] with a
//! [`SimilarityQuery::fusion`] policy over a real HNSW vector index.
//!
//! The centerpiece is the **falsifiable AC4 fixture**: a high-trust, recent
//! candidate that is geometrically far (ranked well below position `3k` by pure
//! similarity) must still appear — indeed rank first — in the fused top-k. This
//! is impossible to satisfy by re-sorting a similarity-only shortlist, so it
//! distinguishes native fused top-k from post-hoc re-ranking.
#![cfg(feature = "semantic-retrieval-fusion")]

use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::NodeId;
use aletheiadb::core::provenance::Provenance;
use aletheiadb::core::temporal::Timestamp;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::{AletheiaDB, FusionPolicy, PropertyMapBuilder, SimilarityQuery};
use std::time::{SystemTime, UNIX_EPOCH};

const MICROS_PER_SEC: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 24 * 3_600 * MICROS_PER_SEC;

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_micros() as i64
}

/// Create a Document node with an embedding, optional provenance confidence, and
/// an explicit valid-from time.
fn make_doc(
    db: &AletheiaDB,
    title: &str,
    embedding: &[f32],
    confidence: Option<f64>,
    valid_from_micros: i64,
) -> NodeId {
    let props = PropertyMapBuilder::new()
        .insert("title", title)
        .insert_vector("embedding", embedding)
        .build();
    let mut opts = WriteRequestOptions::new().with_valid_from(Timestamp::from(valid_from_micros));
    if let Some(c) = confidence {
        let prov = Provenance::builder()
            .source("test")
            .confidence(c)
            .build()
            .expect("valid provenance");
        opts = opts.with_provenance(prov);
    }
    db.create_node_with_options("Document", props, opts)
        .expect("create document")
}

fn cosine_index(db: &AletheiaDB) {
    db.enable_vector_index(
        "embedding",
        HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(256),
    )
    .expect("enable vector index");
}

/// AC4 — true fused top-k, NOT a re-sort of a similarity shortlist.
#[test]
fn fused_topk_surfaces_high_trust_far_candidate_below_3k_by_similarity() {
    let db = AletheiaDB::new().unwrap();
    cosine_index(&db);

    let now = now_micros();
    let stale = now - 400 * MICROS_PER_DAY; // ~13 months old
    let query = vec![1.0f32, 0.0, 0.0];

    // 12 distractors: geometrically almost identical to the query, but
    // low-confidence and stale.
    for i in 0..12 {
        let jitter = 0.01 + (i as f32) * 0.003;
        make_doc(
            &db,
            &format!("distractor{i}"),
            &[1.0, jitter, 0.0],
            Some(0.02),
            stale,
        );
    }

    // The target: orthogonal to the query (cosine similarity 0 -> ranked last),
    // but high-confidence and current.
    let target = make_doc(&db, "target", &[0.0, 1.0, 0.0], Some(0.99), now);

    let k = 3;

    // Pure similarity: the target sits far below position 3k (=9).
    let pure = db
        .similarity_search(SimilarityQuery::from_embedding(query.clone()).k(100))
        .unwrap();
    let target_sim_rank = pure
        .iter()
        .position(|(id, _)| *id == target)
        .expect("target present in similarity results");
    assert!(
        target_sim_rank >= 3 * k,
        "target similarity rank {target_sim_rank} should be >= 3k ({}) for the fixture to be falsifiable",
        3 * k
    );
    // Anti-post-hoc: re-sorting only the similarity top-k could never surface it.
    let shortlist: Vec<NodeId> = pure.iter().take(k).map(|(id, _)| *id).collect();
    assert!(
        !shortlist.contains(&target),
        "target must be absent from the similarity top-k shortlist"
    );

    // Fused: confidence + recency dominate a tiny similarity weight.
    let policy = FusionPolicy::builder()
        .w_similarity(0.1)
        .w_confidence(1.0)
        .w_recency(1.0)
        .recency_half_life_secs(30.0 * 24.0 * 3_600.0)
        .build()
        .unwrap();
    let fused = db
        .similarity_search_fused(SimilarityQuery::from_embedding(query).k(k).fusion(policy))
        .unwrap();

    assert_eq!(fused.len(), k, "fused result honors k");
    assert_eq!(
        fused[0].node_id, target,
        "high-trust recent target should rank first under fusion"
    );
    // Its breakdown is complete and explains the win.
    let b = &fused[0].breakdown;
    assert!((b.confidence - 0.99).abs() < 1e-9);
    assert!(!b.confidence_defaulted);
    assert!(b.recency > 0.99);
    assert!(b.similarity < 0.01);
    assert!(b.fused > 0.9);
}

/// AC5 — a candidate with no recorded provenance is scored at the neutral
/// confidence, never silently dropped.
#[test]
fn missing_provenance_uses_neutral_and_is_ranked_not_dropped() {
    let db = AletheiaDB::new().unwrap();
    cosine_index(&db);

    let now = now_micros();
    let query = vec![1.0f32, 0.0, 0.0];

    let attributed = make_doc(&db, "attributed", &[1.0, 0.02, 0.0], Some(0.9), now);
    // No provenance at all.
    let unattributed = make_doc(&db, "unattributed", &[1.0, 0.03, 0.0], None, now);

    let policy = FusionPolicy::builder()
        .neutral_confidence(0.5)
        .build()
        .unwrap();
    let fused = db
        .similarity_search_fused(SimilarityQuery::from_embedding(query).k(10).fusion(policy))
        .unwrap();

    let hit = fused
        .iter()
        .find(|h| h.node_id == unattributed)
        .expect("un-attributed node is present, not dropped");
    assert!(hit.breakdown.confidence_defaulted);
    assert!((hit.breakdown.confidence - 0.5).abs() < 1e-12);

    // The attributed one carries its real confidence, not the default.
    let a = fused.iter().find(|h| h.node_id == attributed).unwrap();
    assert!(!a.breakdown.confidence_defaulted);
    assert!((a.breakdown.confidence - 0.9).abs() < 1e-9);
}

/// AC2 — attaching a policy does not change the plain (unfused)
/// `similarity_search`: it still returns pure-similarity order.
#[test]
fn plain_similarity_search_ignores_attached_policy() {
    let db = AletheiaDB::new().unwrap();
    cosine_index(&db);

    let now = now_micros();
    let query = vec![1.0f32, 0.0, 0.0];
    let near = make_doc(&db, "near", &[1.0, 0.01, 0.0], Some(0.01), now);
    let far = make_doc(&db, "far", &[0.0, 1.0, 0.0], Some(0.99), now);

    let baseline = db
        .similarity_search(SimilarityQuery::from_embedding(query.clone()).k(10))
        .unwrap();
    let policy = FusionPolicy::builder().build().unwrap();
    let with_policy = db
        .similarity_search(SimilarityQuery::from_embedding(query).k(10).fusion(policy))
        .unwrap();

    assert_eq!(
        baseline, with_policy,
        "policy must not affect plain search (AC2)"
    );
    // Pure-similarity order: near before far.
    let order: Vec<NodeId> = baseline.iter().map(|(id, _)| *id).collect();
    assert_eq!(order.first(), Some(&near));
    assert_eq!(order.last(), Some(&far));
}

/// A fused search with no policy attached is a usage error, not a silent
/// fallback.
#[test]
fn fused_search_without_policy_errors() {
    let db = AletheiaDB::new().unwrap();
    cosine_index(&db);
    let now = now_micros();
    make_doc(&db, "doc", &[1.0, 0.0, 0.0], Some(0.5), now);

    let err = db
        .similarity_search_fused(SimilarityQuery::from_embedding(vec![1.0f32, 0.0, 0.0]).k(5))
        .unwrap_err();
    assert!(err.to_string().contains("fusion policy"), "got: {err}");
}
