//! Issue #3888: the temporal vector index survives a reopen, and an index
//! enabled on old data covers its past.

use aletheiadb::core::NodeId;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::index::vector::temporal::{
    RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
};
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::{AletheiaDB, PropertyMapBuilder, SimilarityQuery};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

fn hnsw() -> HnswConfig {
    HnswConfig::new(2, DistanceMetric::Cosine)
}

/// A config that keeps every snapshot, so a test can check all of history.
fn keep_all() -> TemporalVectorConfig {
    TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 10_000,
        full_snapshot_interval: 4,
        hnsw_config: Some(hnsw()),
    }
}

fn hits_at(db: &AletheiaDB, t: Timestamp) -> usize {
    db.similarity_search(
        SimilarityQuery::from_embedding(vec![1.0, 0.0])
            .k(50)
            .at_time(t),
    )
    .expect("as-of search")
    .len()
}

fn ids_at(db: &AletheiaDB, t: Timestamp) -> BTreeSet<NodeId> {
    db.find_similar_as_of_in("embedding", &[1.0, 0.0], 1000, t)
        .expect("as-of search")
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// The reproduction from the issue.
#[test]
fn as_of_search_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (between, after) = {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.enable_vector_index("embedding", hnsw()).unwrap();
        db.enable_temporal_vector_index(
            "embedding",
            TemporalVectorConfig::default_with_hnsw(hnsw()),
        )
        .unwrap();
        let id = db
            .create_node(
                "Fact",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
        for _ in 0..20 {
            db.create_node("Pad", PropertyMapBuilder::new().insert("x", 1i64).build())
                .unwrap();
        }
        let between = time::now();
        db.retract_node(id, time::now()).unwrap();
        for _ in 0..20 {
            db.create_node("Pad", PropertyMapBuilder::new().insert("x", 2i64).build())
                .unwrap();
        }
        let after = time::now();
        assert_eq!(hits_at(&db, between), 1, "before reopen");
        db.persist_indexes().unwrap();
        (between, after)
    };

    let db = AletheiaDB::open(dir.path()).unwrap();
    assert!(
        db.is_temporal_vector_index_enabled(),
        "enabled after reopen"
    );
    assert_eq!(hits_at(&db, between), 1, "as-of before retract");
    assert_eq!(hits_at(&db, after), 0, "as-of after retract");

    // The caller can enable it again with the same config (a no-op).
    db.enable_temporal_vector_index("embedding", TemporalVectorConfig::default_with_hnsw(hnsw()))
        .expect("same config is a no-op");
    assert_eq!(hits_at(&db, between), 1, "no-op enable keeps history");

    // A different config is an error.
    let err = db
        .enable_temporal_vector_index("embedding", keep_all())
        .expect_err("different config must fail");
    assert!(err.to_string().contains("already enabled"), "{err}");
}

/// The live index continues to work after a reopen.
#[test]
fn writes_after_reopen_are_indexed() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.enable_temporal_vector_index("embedding", keep_all())
            .unwrap();
        db.create_node(
            "Fact",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0])
                .build(),
        )
        .unwrap();
    }
    let db = AletheiaDB::open(dir.path()).unwrap();
    let second = db
        .create_node(
            "Fact",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.9, 0.1])
                .build(),
        )
        .unwrap();
    db.create_temporal_vector_snapshot("embedding").unwrap();
    let ids = ids_at(&db, time::now());
    assert_eq!(ids.len(), 2, "both nodes at now: {ids:?}");
    assert!(ids.contains(&second));
}

/// Enable on old data: the index covers the past (option 2 in the issue).
#[test]
fn enable_on_existing_data_covers_past() {
    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0])
                .build(),
        )
        .unwrap();
    let t1 = time::now();
    let b = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.0, 1.0])
                .build(),
        )
        .unwrap();
    let t2 = time::now();
    db.delete_node_with_valid_time(a, None).unwrap();
    let t3 = time::now();

    db.enable_temporal_vector_index("embedding", keep_all())
        .unwrap();

    assert_eq!(ids_at(&db, t1), BTreeSet::from([a]));
    assert_eq!(ids_at(&db, t2), BTreeSet::from([a, b]));
    assert_eq!(ids_at(&db, t3), BTreeSet::from([b]));
    // The current-state index was created and filled too.
    assert!(db.is_vector_index_enabled_for("embedding"));
    let current = db
        .similarity_search(SimilarityQuery::from_embedding(vec![0.0, 1.0]).k(5))
        .unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].0, b);
}

/// The backfill keeps only the newest `max_snapshots` change points.
#[test]
fn backfill_keeps_newest_max_snapshots() {
    let db = AletheiaDB::new().unwrap();
    let mut times = Vec::new();
    for i in 0..6 {
        db.create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, i as f32])
                .build(),
        )
        .unwrap();
        times.push(time::now());
    }
    let config = TemporalVectorConfig {
        max_snapshots: 3,
        ..keep_all()
    };
    db.enable_temporal_vector_index("embedding", config)
        .unwrap();
    assert_eq!(db.temporal_vector_snapshot_count("embedding"), Some(3));
    // Before the oldest kept snapshot there is no snapshot.
    assert!(ids_at(&db, times[2]).is_empty());
    assert_eq!(ids_at(&db, times[3]).len(), 4);
    assert_eq!(ids_at(&db, times[5]).len(), 6);
}

/// A bad sidecar does not stop open. The file is put aside.
#[test]
fn corrupt_sidecar_is_quarantined() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.enable_temporal_vector_index("embedding", keep_all())
            .unwrap();
    }
    let sidecar = dir
        .path()
        .join("indexes")
        .join("temporal_vector_indexes.dat");
    assert!(sidecar.exists(), "enable writes the sidecar");
    std::fs::write(&sidecar, b"not a sidecar").unwrap();

    let db = AletheiaDB::open(dir.path()).unwrap();
    assert!(!db.is_temporal_vector_index_enabled());
    assert!(!sidecar.exists());
    let quarantined = std::fs::read_dir(dir.path().join("indexes"))
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("temporal_vector_indexes.dat.corrupt")
        });
    assert!(quarantined, "bad sidecar is kept for diagnosis");
}

/// Enable during concurrent commits: no vector is lost or added two times.
#[test]
fn enable_during_concurrent_writes_loses_nothing() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let db = Arc::new(AletheiaDB::new().unwrap());
    let committed = Arc::new(AtomicUsize::new(0));
    let writer = {
        let db = Arc::clone(&db);
        let committed = Arc::clone(&committed);
        std::thread::spawn(move || {
            (0..400)
                .map(|i| {
                    let id = db
                        .create_node(
                            "Doc",
                            PropertyMapBuilder::new()
                                .insert_vector("embedding", &[1.0, i as f32 / 400.0])
                                .build(),
                        )
                        .unwrap();
                    committed.fetch_add(1, Ordering::SeqCst);
                    id
                })
                .collect::<BTreeSet<NodeId>>()
        })
    };
    // Enable while the writer is in the middle of its commits.
    while committed.load(Ordering::SeqCst) < 50 {
        std::thread::yield_now();
    }
    db.enable_temporal_vector_index("embedding", keep_all())
        .unwrap();
    let during = committed.load(Ordering::SeqCst);
    let written = writer.join().unwrap();
    assert!(during < 400, "the enable must overlap the writes");

    db.create_temporal_vector_snapshot("embedding").unwrap();
    assert_eq!(ids_at(&db, time::now()), written);
}

/// Equal-config enable after reopen is a no-op, also when the caller gives no
/// HNSW config and the vector index has non-default fields.
#[test]
fn temporal_only_config_is_a_no_op_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let hnsw = hnsw().with_capacity(100);
    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.enable_vector_index("embedding", hnsw).unwrap();
        db.enable_temporal_vector_index("embedding", TemporalVectorConfig::default_temporal_only())
            .unwrap();
        db.persist_indexes().unwrap();
    }
    let db = AletheiaDB::open(dir.path()).unwrap();
    assert!(db.is_temporal_vector_index_enabled());
    db.enable_temporal_vector_index("embedding", TemporalVectorConfig::default_temporal_only())
        .expect("same config is a no-op");
}

/// Two temporal properties: both are restored, and both get new snapshots
/// after the reopen.
#[test]
fn two_properties_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let both = || {
        PropertyMapBuilder::new()
            .insert_vector("a", &[1.0, 0.0])
            .insert_vector("b", &[0.0, 1.0])
            .build()
    };
    let t1 = {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.enable_temporal_vector_index("a", keep_all()).unwrap();
        db.enable_temporal_vector_index("b", keep_all()).unwrap();
        db.create_node("Doc", both()).unwrap();
        time::now()
    };
    let db = AletheiaDB::open(dir.path()).unwrap();
    let mut names = db.list_temporal_vector_indexes();
    names.sort();
    assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    for p in ["a", "b"] {
        let hits = db.find_similar_as_of_in(p, &[1.0, 1.0], 10, t1).unwrap();
        assert_eq!(hits.len(), 1, "property {p}");
    }

    let before = (
        db.temporal_vector_snapshot_count("a").unwrap(),
        db.temporal_vector_snapshot_count("b").unwrap(),
    );
    // A new node is an anchor. Each index makes a snapshot for it.
    db.create_node("Doc", both()).unwrap();
    assert!(db.temporal_vector_snapshot_count("a").unwrap() > before.0);
    assert!(db.temporal_vector_snapshot_count("b").unwrap() > before.1);
}

/// A failed enable leaves no index behind.
#[test]
fn failed_enable_rolls_back() {
    let db = AletheiaDB::new().unwrap();
    // A vector with the wrong dimension stops the current-state fill.
    db.create_node(
        "Doc",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0, 0.0])
            .build(),
    )
    .unwrap();
    assert!(
        db.enable_temporal_vector_index("embedding", keep_all())
            .is_err()
    );
    assert!(!db.is_vector_index_enabled_for("embedding"));
    assert!(!db.is_temporal_vector_index_enabled());
}

#[derive(Debug, Clone)]
enum Op {
    Create(f32),
    Update(usize, f32),
    RemoveVector(usize),
    Delete(usize),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0.0f32..1.0).prop_map(Op::Create),
        (any::<usize>(), 0.0f32..1.0).prop_map(|(i, y)| Op::Update(i, y)),
        any::<usize>().prop_map(Op::RemoveVector),
        any::<usize>().prop_map(Op::Delete),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// I1 + I2: after enable, the snapshot at each commit holds exactly the
    /// vectors that current storage held after that commit.
    #[test]
    fn backfill_matches_model(ops in prop::collection::vec(op_strategy(), 1..25)) {
        let db = AletheiaDB::new().unwrap();
        let mut live: Vec<NodeId> = Vec::new();
        let mut model: BTreeSet<NodeId> = BTreeSet::new();
        let mut expected: BTreeMap<Timestamp, BTreeSet<NodeId>> = BTreeMap::new();

        for op in ops {
            match op {
                Op::Create(y) => {
                    let id = db.create_node("Doc", PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, y]).build()).unwrap();
                    live.push(id);
                    model.insert(id);
                }
                Op::Update(i, y) if !live.is_empty() => {
                    let id = live[i % live.len()];
                    db.update_node_with_valid_time(id, PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, y]).build(), None).unwrap();
                    model.insert(id);
                }
                Op::RemoveVector(i) if !live.is_empty() => {
                    let id = live[i % live.len()];
                    db.remove_node_property(id, "embedding").unwrap();
                    model.remove(&id);
                }
                Op::Delete(i) if !live.is_empty() => {
                    let id = live.remove(i % live.len());
                    db.delete_node_with_valid_time(id, None).unwrap();
                    model.remove(&id);
                }
                _ => continue,
            }
            expected.insert(time::now(), model.clone());
        }

        db.enable_temporal_vector_index("embedding", keep_all()).unwrap();
        for (t, want) in &expected {
            prop_assert_eq!(&ids_at(&db, *t), want, "at {}", t);
        }
        // I2: the live index matches current storage.
        db.create_temporal_vector_snapshot("embedding").unwrap();
        prop_assert_eq!(ids_at(&db, time::now()), model);
    }
}
