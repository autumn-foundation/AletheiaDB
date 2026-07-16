//! Regression tests for Issue #3621: the transactional node-update path must
//! de-index vectors from the TEMPORAL vector index, not only add to it.
//!
//! Before the fix, `CurrentStorage::update_node_direct` fanned vector updates
//! into the current-state HNSW correctly (add/remove/upsert) but drove the
//! temporal vector index through an ADD-ONLY helper. Removing a vector property
//! (Some -> None) therefore left the stale embedding in the temporal index's
//! `current_state.vectors`, and it leaked into the next snapshot, so
//! `find_similar_as_of(now)` returned a phantom hit for a fact whose embedding
//! had been deleted.
//!
//! The bi-temporal contract these tests exercise: removing/overwriting an
//! embedding CLOSES the interval at the current time (a snapshot taken AFTER the
//! change no longer contains the old vector), while snapshots taken BEFORE the
//! change still retain it (history is preserved, never erased).
//!
//! Fix-discriminating vs regression-lock: only the REMOVAL case (`Some -> None`)
//! actually fails on trunk before the fix — that is what the add-only temporal
//! helper mishandled — so `remove_vector_property_deindexes_current_but_preserves_history`
//! is the guard that pins the fix. The overwrite (`Some -> Some`) and
//! unrelated-update tests below PASS on trunk too (trunk's add-only helper
//! already upserted on `Some -> Some` via `HnswIndex::add`); they are kept as
//! regression locks that the de-index change did not break overwrite or
//! over-remove on an unrelated update.

use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::db::SimilarityQuery;
use aletheiadb::index::vector::temporal::{
    RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
};
use aletheiadb::{
    AletheiaDB, DistanceMetric, Error, HnswConfig, NodeId, PropertyMapBuilder, WriteOps,
};

/// Build a DB with a temporal vector index that snapshots on EVERY committed
/// transaction (the proven deterministic mechanism used by the existing
/// `find_similar_as_of` tests): each `create_node`/`update_node` commit calls
/// `on_temporal_vector_transaction`, which — with `TransactionInterval(1)` —
/// materializes a snapshot keyed at that commit's wallclock.
fn temporal_db() -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    let hnsw = HnswConfig::new(4, DistanceMetric::Cosine);
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(100),
        max_snapshots: 100,
        full_snapshot_interval: 5,
        hnsw_config: Some(hnsw),
    };
    db.enable_temporal_vector_index("embedding", config)
        .expect("enable temporal vector index");
    db
}

/// Small wall-clock advance so distinct commits land in distinct, ordered
/// snapshot buckets (snapshots are keyed by wallclock at commit time).
fn advance() {
    std::thread::sleep(std::time::Duration::from_millis(25));
}

/// Point-in-time similarity via the current public API (`similarity_search`
/// with `.at_time(..)`), which routes to the temporal vector index.
fn as_of(db: &AletheiaDB, query: &[f32], k: usize, ts: Timestamp) -> Vec<(NodeId, f32)> {
    db.similarity_search(
        SimilarityQuery::from_embedding(query.to_vec())
            .k(k)
            .at_time(ts),
    )
    .expect("as-of similarity search")
}

fn contains(results: &[(NodeId, f32)], id: NodeId) -> bool {
    results.iter().any(|(nid, _)| *nid == id)
}

fn score_of(results: &[(NodeId, f32)], id: NodeId) -> Option<f32> {
    results.iter().find(|(nid, _)| *nid == id).map(|(_, s)| *s)
}

/// Removing a vector property de-indexes it from the temporal index at the
/// current time, while an earlier snapshot still finds it (history preserved).
#[test]
fn remove_vector_property_deindexes_current_but_preserves_history() {
    let db = temporal_db();

    // A decoy keeps the temporal index non-empty across the removal so a
    // snapshot is always materialized (and its embedding is orthogonal to the
    // query, so it never masks the target assertion).
    let decoy = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "decoy")
                .insert_vector("embedding", &[0.0f32, 0.0, 0.0, 1.0])
                .build(),
        )
        .expect("create decoy");

    let target = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "target")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("create target");

    advance();
    let t_before_remove: Timestamp = time::now();
    advance();

    // Remove the embedding (Some -> None) via a full-map replace that drops it.
    db.replace_node(
        target,
        "Doc",
        PropertyMapBuilder::new().insert("name", "target").build(),
    )
    .expect("replace removes vector");

    advance();
    let t_after_remove: Timestamp = time::now();

    let query = [1.0f32, 0.0, 0.0, 0.0];

    // History preserved: as-of BEFORE the removal, the target is still found.
    let historical = as_of(&db, &query, 10, t_before_remove);
    assert!(
        contains(&historical, target),
        "target must remain findable as-of before the removal (history preserved), got {historical:?}"
    );

    // De-indexed now: as-of AFTER the removal, the phantom must be gone.
    let current = as_of(&db, &query, 10, t_after_remove);
    assert!(
        !contains(&current, target),
        "target must NOT be returned by temporal similarity as-of after removal (Issue #3621 phantom), got {current:?}"
    );

    // The decoy survives — the removal only de-indexed the target.
    assert!(
        contains(&current, decoy),
        "decoy must remain indexed after target's removal, got {current:?}"
    );

    // Current-state index must also exclude the target (covered by #3596, kept
    // here as a cross-check that both indexes agree post-fix).
    let live = db
        .similarity_search(SimilarityQuery::from_embedding(query.to_vec()).k(10))
        .expect("current-state search");
    assert!(
        !contains(&live, target),
        "target must be gone from the current-state index too, got {live:?}"
    );
}

/// Overwriting a vector property replaces the old embedding at the current time
/// (nearest to the NEW vector, no longer to the old region), while an earlier
/// snapshot still returns it nearest to the OLD vector.
///
/// Regression lock, not a fix discriminator: this `Some -> Some` overwrite
/// already passes on trunk (the add-only temporal helper upserted on overwrite
/// via `HnswIndex::add`). It guards that the #3621 de-index change did not
/// regress the overwrite path.
#[test]
fn overwrite_vector_property_deindexes_old_but_preserves_history() {
    let db = temporal_db();

    let target = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "target")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("create target");

    advance();
    let t_before: Timestamp = time::now();
    advance();

    // Overwrite with a very different embedding (Some -> Some, changed).
    db.write(|tx| {
        tx.update_node(
            target,
            PropertyMapBuilder::new()
                .insert("name", "target")
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0, 0.0])
                .build(),
        )
    })
    .expect("overwrite embedding");

    advance();
    let t_after: Timestamp = time::now();

    let old_region = [1.0f32, 0.0, 0.0, 0.0];
    let new_region = [0.0f32, 1.0, 0.0, 0.0];

    // As-of BEFORE the overwrite: nearest to the OLD region.
    let before = as_of(&db, &old_region, 10, t_before);
    assert!(
        score_of(&before, target).is_some_and(|s| s > 0.99),
        "as-of before overwrite, target must be ~identical to the old embedding, got {before:?}"
    );

    // As-of AFTER the overwrite: nearest to the NEW region.
    let after_new = as_of(&db, &new_region, 10, t_after);
    assert!(
        score_of(&after_new, target).is_some_and(|s| s > 0.99),
        "as-of after overwrite, target must be ~identical to the new embedding, got {after_new:?}"
    );

    // As-of AFTER the overwrite: the OLD region no longer matches the target
    // (the stale embedding is gone from the current snapshot).
    let after_old = as_of(&db, &old_region, 10, t_after);
    assert!(
        score_of(&after_old, target).is_none_or(|s| s < 0.1),
        "as-of after overwrite, the OLD embedding must no longer match the target, got {after_old:?}"
    );
}

/// A plain PATCH `update_node` that overwrites the embedding key with a
/// NON-vector value (and carries no other vector property) must fire a temporal
/// snapshot so the phantom is de-indexed as-of-now — not left stale until an
/// unrelated future vector transaction happens to snapshot.
///
/// Fix discriminator for the snapshot-gating gap (Issue #3621, PATCH path):
/// `WriteBuffer::add` sets `has_vector_operations` only from the merged/new
/// property map, so a PATCH that turns `embedding` from a vector into a scalar
/// leaves the flag false → `on_temporal_vector_transaction` never fires → no
/// snapshot at t2 → `find_similar_as_of(now)` keeps returning the phantom
/// (unbounded staleness). The `update_node_with_options` gate on the EXISTING
/// node's vector — mirroring `buffer_node_replace` — closes it. The storage
/// layer already de-indexes the LIVE temporal index; only the snapshot was
/// missing.
#[test]
fn patch_overwriting_vector_with_scalar_deindexes_current_but_preserves_history() {
    let db = temporal_db();

    // A decoy keeps the temporal index non-empty across the change so a
    // snapshot is always materialized (orthogonal to the query so it never
    // masks the target assertion).
    let decoy = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "decoy")
                .insert_vector("embedding", &[0.0f32, 0.0, 0.0, 1.0])
                .build(),
        )
        .expect("create decoy");

    let target = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "target")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("create target");

    advance();
    let t_before: Timestamp = time::now();
    advance();

    // PLAIN PATCH `update_node` (routes through `update_node_with_options`, the
    // ungated path — NOT `replace_node`): overwrite the vector-typed key with a
    // scalar, with NO other vector property in the patch. The merged map has no
    // vector, so `WriteBuffer::add` leaves `has_vector_operations` false.
    db.write(|tx| {
        tx.update_node(
            target,
            PropertyMapBuilder::new()
                .insert("embedding", "not-a-vector")
                .build(),
        )?;
        Ok::<_, Error>(())
    })
    .expect("PATCH overwrites embedding with scalar");

    advance();
    let t_after: Timestamp = time::now();

    // NO further/unrelated vector transaction is issued after this point.

    let query = [1.0f32, 0.0, 0.0, 0.0];

    // History preserved: as-of BEFORE the change, the target is still found.
    let historical = as_of(&db, &query, 10, t_before);
    assert!(
        contains(&historical, target),
        "target must remain findable as-of before the PATCH (history preserved), got {historical:?}"
    );

    // De-indexed now: as-of AFTER the change, the phantom must be gone — this is
    // what fails without the snapshot-gating fix (no snapshot fired at t2).
    let current = as_of(&db, &query, 10, t_after);
    assert!(
        !contains(&current, target),
        "target must NOT be returned by temporal similarity as-of after the PATCH (Issue #3621 snapshot-gating phantom), got {current:?}"
    );

    // The decoy survives — the change only de-indexed the target.
    assert!(
        contains(&current, decoy),
        "decoy must remain indexed after the target's PATCH, got {current:?}"
    );

    // Current-state index must also exclude the target.
    let live = db
        .similarity_search(SimilarityQuery::from_embedding(query.to_vec()).k(10))
        .expect("current-state search");
    assert!(
        !contains(&live, target),
        "target must be gone from the current-state index too, got {live:?}"
    );
}

/// Sanity: without touching the vector (a non-vector-property update that
/// re-supplies the same embedding), the embedding stays indexed — the fix must
/// not over-remove.
///
/// Regression lock, not a fix discriminator: this passes on trunk too (the
/// `Some -> Some` unchanged case was already a no-op via the `if o != n` guard).
/// It guards that the de-index change did not over-remove on an unrelated
/// update. (A no-churn assertion — that re-supplying the same embedding does
/// not redundantly re-add — is intentionally omitted: the temporal index's
/// change-tracking is `HashSet<NodeId>`-keyed, so a single node re-supplying the
/// same vector yields the identical count whether or not the guard fires, and
/// snapshot counts grow per commit regardless; no reachable, non-fragile
/// observable discriminates the guard.)
#[test]
fn unrelated_update_keeps_vector_indexed() {
    let db = temporal_db();

    let target = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "target")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("create target");

    // Update a non-vector property while re-supplying the same embedding.
    db.write(|tx| {
        tx.update_node(
            target,
            PropertyMapBuilder::new()
                .insert("name", "renamed")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )?;
        Ok::<_, Error>(())
    })
    .expect("update non-vector property");

    advance();
    let now: Timestamp = time::now();

    let results = as_of(&db, &[1.0f32, 0.0, 0.0, 0.0], 10, now);
    assert!(
        contains(&results, target),
        "target must remain indexed after an unrelated update, got {results:?}"
    );
}
