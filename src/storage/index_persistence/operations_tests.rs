use super::*;
use crate::core::GLOBAL_INTERNER;
use crate::core::id::{NodeId, VersionId};
use crate::core::property::PropertyMapBuilder;
use crate::core::temporal::time;
use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::index_persistence::formats::PersistedPropertyValue;
use crate::storage::index_persistence::graph::load_graph_index;
use crate::storage::index_persistence::strings::load_string_interner;
use crate::storage::index_persistence::temporal::load_temporal_index;
use crate::storage::index_persistence::tracker::PersistenceTracker;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn test_persist_vector_indexes_with_none_tracker() {
    // This is a minimal test to verify that the function accepts None
    // and doesn't panic. Real IO logic would be skipped or mockable
    // in a more complex setup, but here we just want to ensure
    // the Option handling logic (lines 109-111) is correct.

    // We can't easily mock CurrentStorage and IndexPersistenceManager fully
    // without trait abstraction, so we will construct minimal ones
    // and expect failure at IO step, but verify it didn't panic on tracker.

    let current = Arc::new(CurrentStorage::new());

    // We use a tempdir for manager to avoid polluting real FS
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));

    // This will likely fail on save_string_interner or empty indexes,
    // but the critical path we are testing is the None tracker handling
    // at the end of the function.
    let _ = persist_vector_indexes(&current, &manager, None);

    // If we reached here without panic, the Option check worked.
}

#[test]
fn test_persist_vector_indexes_with_tracker() {
    let current = Arc::new(CurrentStorage::new());
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    let tracker = Arc::new(PersistenceTracker::new());

    // Simulate mutation
    tracker.record_vector_mutation();

    // Even if persistence fails (e.g. IO error), we want to see if we attempted it
    let _ = persist_vector_indexes(&current, &manager, Some(&tracker));

    // NOTE: In the current implementation, if persistence fails early (e.g. IO),
    // the tracker reset might NOT be reached because of `?`.
    // This test mainly verifies signature compatibility.
}

#[test]
fn test_graph_persist_keeps_interner_consistent_with_graph_string_ids() {
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    let current = Arc::new(CurrentStorage::new());

    let unique_value = format!(
        "graph-persist-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos()
    );

    assert!(
        GLOBAL_INTERNER.get_id(&unique_value).is_none(),
        "unique test string unexpectedly already interned: {}",
        unique_value
    );

    let properties = PropertyMapBuilder::new()
        .insert("payload", unique_value.as_str())
        .build();

    current
        .create_node("GraphPersistConsistency", properties)
        .expect("failed to create test node");

    persist_graph_index(&current, &manager, None).expect("failed to persist graph index");

    let interner_data =
        load_string_interner(&manager.interner_path()).expect("failed to load persisted interner");
    let graph_data = load_graph_index(&manager.graph_path().join("adjacency.idx"))
        .expect("failed to load persisted graph index");

    let persisted_string_id = graph_data
        .nodes
        .iter()
        .flat_map(|node| node.properties.entries.iter())
        .find_map(|(_, value)| match value {
            PersistedPropertyValue::String(id) => Some(*id),
            _ => None,
        })
        .expect("expected at least one persisted string property in graph index");

    assert!(
        (persisted_string_id as u64) < interner_data.string_count,
        "graph index references string ID {} but interner contains only {} strings",
        persisted_string_id,
        interner_data.string_count
    );

    let resolved = interner_data
        .strings
        .get(persisted_string_id as usize)
        .expect("persisted string id should index into persisted interner");
    assert_eq!(resolved, &unique_value);
}

#[test]
fn test_temporal_persist_keeps_interner_consistent_with_temporal_string_ids() {
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    let tracker = Arc::new(PersistenceTracker::new());
    let temporal_indexes = Arc::new(TemporalIndexes::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));

    let unique_value = format!(
        "temporal-persist-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos()
    );

    assert!(
        GLOBAL_INTERNER.get_id(&unique_value).is_none(),
        "unique test string unexpectedly already interned: {}",
        unique_value
    );

    let label = GLOBAL_INTERNER
        .intern("TemporalPersistConsistency")
        .expect("failed to intern test label");
    let node_id = NodeId::new(1).expect("invalid node id");
    let version_id = VersionId::new(1).expect("invalid version id");
    let now = time::now();

    let properties = PropertyMapBuilder::new()
        .insert("payload", unique_value.as_str())
        .build();

    historical
        .write()
        .add_node_version(node_id, version_id, now, now, label, properties, false)
        .expect("failed to add node version");

    persist_temporal_index(&historical, &temporal_indexes, &manager, &tracker)
        .expect("failed to persist temporal index");

    let interner_data =
        load_string_interner(&manager.interner_path()).expect("failed to load persisted interner");
    let temporal_data = load_temporal_index(&manager.temporal_path().join("versions.idx"))
        .expect("failed to load persisted temporal index");

    let persisted_string_id = temporal_data
        .node_versions
        .iter()
        .flat_map(|entry| entry.properties.entries.iter())
        .find_map(|(_, value)| match value {
            PersistedPropertyValue::String(id) => Some(*id),
            _ => None,
        })
        .expect("expected at least one persisted string property in temporal index");

    assert!(
        (persisted_string_id as u64) < interner_data.string_count,
        "temporal index references string ID {} but interner contains only {} strings",
        persisted_string_id,
        interner_data.string_count
    );

    let resolved = interner_data
        .strings
        .get(persisted_string_id as usize)
        .expect("persisted string id should index into persisted interner");
    assert_eq!(resolved, &unique_value);
}
