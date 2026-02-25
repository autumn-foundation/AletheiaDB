use super::*;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::index::vector::hnsw::HnswIndex;
use crate::index::vector::{DistanceMetric, HnswConfig};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;
use std::sync::Arc;

use crate::core::hasher::IdentityHasher;

#[test]
fn test_config_coverage() {
    let config = TemporalVectorConfig::with_change_threshold(
        HnswConfig::new(4, DistanceMetric::Cosine),
        0.5,
    );
    assert!(
        matches!(config.snapshot_strategy, SnapshotStrategy::ChangeThreshold(t) if (t - 0.5).abs() < f64::EPSILON)
    );
}

#[test]
fn test_stats_coverage() {
    let stats = MemoryStats {
        changes_accumulated_size: 100,
        vectors_changed_since_snapshot: 0,
        snapshots_since_full: 0,
        total_snapshots: 0,
        current_vectors: 0,
    };
    assert_eq!(stats.estimated_accumulated_bytes(), 800);
}

#[test]
fn test_snapshot_debug_coverage() -> Result<()> {
    use super::snapshot::{DeltaIndex, SnapshotIndex};

    // Test SnapshotIndex::Full Debug
    let hnsw = Arc::new(HnswIndex::new(HnswConfig::new(4, DistanceMetric::Cosine))?);
    let full = SnapshotIndex::Full(hnsw.clone());
    let debug_str = format!("{:?}", full);
    assert!(debug_str.contains("SnapshotIndex::Full"));
    assert!(debug_str.contains("len"));

    // Test SnapshotIndex::Delta Debug
    let delta = SnapshotIndex::Delta(Arc::new(DeltaIndex {
        base: Arc::new(SnapshotIndex::Full(hnsw.clone())),
        added: hnsw.clone(),
        removed: Arc::new(HashSet::with_hasher(BuildHasherDefault::default())),
    }));
    let debug_str = format!("{:?}", delta);
    assert!(debug_str.contains("SnapshotIndex::Delta"));
    assert!(debug_str.contains("added_len"));

    // Test DeltaIndex Debug (direct)
    let delta_struct = DeltaIndex {
        base: Arc::new(SnapshotIndex::Full(hnsw.clone())),
        added: hnsw.clone(),
        removed: Arc::new(HashSet::with_hasher(BuildHasherDefault::default())),
    };
    let debug_str_struct = format!("{:?}", delta_struct);
    assert!(debug_str_struct.contains("DeltaIndex"));
    assert!(debug_str_struct.contains("removed_count"));

    Ok(())
}

#[test]
fn test_delta_search_with_filter_coverage() -> Result<()> {
    // Setup: Create a temporal index
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1), // Snapshot every txn
        full_snapshot_interval: 10,                                  // Ensure next is Delta
        hnsw_config: Some(HnswConfig::new(2, DistanceMetric::Cosine)),
        ..TemporalVectorConfig::default_temporal_only()
    };
    let index = TemporalVectorIndex::new(config)?;

    let id1 = NodeId::new(1).unwrap();
    let id2 = NodeId::new(2).unwrap();
    let id3 = NodeId::new(3).unwrap(); // Will be removed

    // Batch 1: id1, id3
    index.add_batch(&[
        (id1, vec![1.0, 0.0], 100.into()),
        (id3, vec![0.0, 1.0], 100.into()),
    ])?;
    index.on_transaction_at(100.into())?; // Snapshot 1 (Full) containing id1, id3

    // Batch 2: Add id2, Remove id3
    index.add(id2, &[0.5, 0.5], 200.into())?;
    index.remove(id3, 200.into())?;
    index.on_transaction_at(200.into())?; // Snapshot 2 (Delta)
    // Delta should have: Added: {id2}, Removed: {id3}, Base: Snapshot 1

    // Access the specific snapshot to test search_with_filter
    let snapshot = index.find_nearest_snapshot(200.into()).unwrap();

    // Predicate: Accept all
    let results = snapshot.search_with_filter(&[1.0, 0.0], 10, &|_| true)?;
    // Should contain id1 (from Base), id2 (from Delta). Should NOT contain id3.
    let ids: HashSet<NodeId> = results.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&id1));
    assert!(ids.contains(&id2));
    assert!(!ids.contains(&id3));

    // Predicate: Accept only id1
    let results = snapshot.search_with_filter(&[1.0, 0.0], 10, &|id| *id == id1)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, id1);

    Ok(())
}

#[test]
fn test_max_delta_chain_depth_error() {
    use super::config::MAX_DELTA_CHAIN_DEPTH;
    use super::snapshot::VectorSnapshot;

    // To test `get_vector` depth check, we need to populate a map where
    // T1 -> Full
    // T2 -> Delta(base=T1)
    // T3 -> Delta(base=T2) ...
    // And call get_vector on the last one.

    let mut snapshots = std::collections::BTreeMap::new();
    let root_time: i64 = 0;
    snapshots.insert(
        root_time.into(),
        VectorSnapshot::Full(Arc::new(
            HashMap::with_hasher(BuildHasherDefault::default()),
        )),
    );

    let mut last_time = root_time;
    for i in 1..=MAX_DELTA_CHAIN_DEPTH + 1 {
        let time = i as i64;
        snapshots.insert(
            time.into(),
            VectorSnapshot::Delta {
                base_time: last_time.into(),
                added: Arc::new(HashMap::with_hasher(BuildHasherDefault::default())),
                removed: Arc::new(HashSet::with_hasher(BuildHasherDefault::default())),
            },
        );
        last_time = time;
    }

    // Now call get_vector on the tip
    let tip = snapshots.get(&last_time.into()).unwrap();
    let result = tip.get_vector(&NodeId::new(1).unwrap(), &snapshots);

    assert!(result.is_err());
    match result {
        Err(e) => assert!(e.to_string().contains("Delta chain depth exceeded")),
        _ => panic!("Expected error"),
    }

    // Also test to_hashmap depth check
    let result_map = tip.to_hashmap(&snapshots);
    assert!(result_map.is_err());
    match result_map {
        Err(e) => assert!(e.to_string().contains("Delta chain depth exceeded")),
        _ => panic!("Expected error"),
    }
}

#[test]
fn test_vector_snapshot_delta_len_coverage() {
    use super::config::MIN_CAPACITY_ESTIMATE;
    use super::snapshot::VectorSnapshot;

    let delta = VectorSnapshot::Delta {
        base_time: 0.into(),
        added: Arc::new(HashMap::with_hasher(BuildHasherDefault::default())),
        removed: Arc::new(HashSet::with_hasher(BuildHasherDefault::default())),
    };

    // Should return MIN_CAPACITY_ESTIMATE since added is empty
    assert_eq!(delta.len(), MIN_CAPACITY_ESTIMATE);

    // Create one with more items than MIN
    let mut added = HashMap::with_hasher(BuildHasherDefault::<IdentityHasher>::default());
    for i in 0..MIN_CAPACITY_ESTIMATE + 10 {
        added.insert(NodeId::new(i as u64).unwrap(), Arc::from(vec![0.0f32]));
    }

    let delta_large = VectorSnapshot::Delta {
        base_time: 0.into(),
        added: Arc::new(added.clone()),
        removed: Arc::new(HashSet::with_hasher(BuildHasherDefault::default())),
    };

    assert_eq!(delta_large.len(), MIN_CAPACITY_ESTIMATE + 10);
}

#[test]
fn test_delta_get_vector_removed_coverage() -> Result<()> {
    use super::snapshot::VectorSnapshot;

    let id = NodeId::new(1).unwrap();
    let mut removed = HashSet::with_hasher(BuildHasherDefault::<IdentityHasher>::default());
    removed.insert(id);

    let delta = VectorSnapshot::Delta {
        base_time: 0.into(),
        added: Arc::new(HashMap::with_hasher(BuildHasherDefault::default())),
        removed: Arc::new(removed),
    };

    let snapshots = std::collections::BTreeMap::new(); // Empty map should be fine as we hit removed check first

    let result = delta.get_vector(&id, &snapshots)?;
    assert!(result.is_none());

    Ok(())
}
