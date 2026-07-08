//! Tests for high-level index persistence operations.
//!
//! This module verifies the orchestration of saving and loading various indexes
//! (graph, temporal, vector, interner) through the unified persistence API.

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
    let _ = persist_vector_indexes(&current, &manager, None, 0);

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
    let _ = persist_vector_indexes(&current, &manager, Some(&tracker), 100);

    // NOTE: In the current implementation, if persistence fails early (e.g. IO),
    // the tracker reset might NOT be reached because of `?`.
    // This test mainly verifies signature compatibility.
}

/// Issue #451: a corrupted vector index on disk must not abort loading of the
/// remaining (valid) vector indexes. `load_vector_indexes` documents that
/// "errors during loading of individual indexes are logged but do not abort
/// the process", so a single corrupt `meta.idx` must be skipped while every
/// valid sibling index is still registered with `CurrentStorage`.
#[test]
fn test_load_vector_indexes_skips_corrupt_and_loads_valid() {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    manager.ensure_directories().unwrap();

    // Build and persist a valid vector index with one indexed node.
    let current = Arc::new(CurrentStorage::new());
    current
        .enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .expect("failed to enable vector index");
    current
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("failed to create node with embedding");
    persist_vector_indexes(&current, &manager, None, 0).expect("failed to persist vector indexes");

    // Fabricate a corrupted sibling vector index directory.
    let corrupt_dir = manager.vector_path("corrupt_embedding");
    std::fs::create_dir_all(&corrupt_dir).unwrap();
    std::fs::write(corrupt_dir.join("meta.idx"), b"not a valid meta file").unwrap();

    // Load into a fresh storage: the corrupt index must be skipped, the
    // valid one must load.
    let fresh = Arc::new(CurrentStorage::new());
    let result = load_vector_indexes(&fresh, &manager);
    let summary = match result {
        Ok(summary) => summary,
        Err(e) => {
            panic!("a corrupt vector index must be skipped, not abort all vector loading: {e}")
        }
    };
    assert_eq!(summary.loaded, 1, "exactly one valid index should load");
    assert_eq!(
        summary.skipped, 1,
        "exactly one corrupt index should be skipped"
    );
    assert!(
        fresh.is_vector_index_enabled_for("embedding"),
        "the valid vector index must still be loaded when a sibling is corrupt"
    );
    assert!(
        !fresh.is_vector_index_enabled_for("corrupt_embedding"),
        "the corrupt vector index must not be registered"
    );
}

/// Issue #451: loading from a directory tree with no vector indexes must
/// succeed with an all-zero summary (fresh database startup path).
#[test]
fn test_load_vector_indexes_no_vector_dir_returns_empty_summary() {
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));

    let fresh = Arc::new(CurrentStorage::new());
    let summary = load_vector_indexes(&fresh, &manager).expect("empty data dir must load cleanly");
    assert_eq!(summary.loaded, 0);
    assert_eq!(summary.skipped, 0);
    assert!(!fresh.is_vector_index_enabled());
}

/// Issue #451: multiple per-property vector indexes are loaded (in parallel)
/// in one pass, each restored with its persisted configuration (dimensions,
/// distance metric) and its indexed vectors.
#[test]
fn test_load_vector_indexes_multi_property_round_trip() {
    use crate::index::vector::{DistanceMetric, HnswConfig, VectorIndex};

    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    manager.ensure_directories().unwrap();

    // Three properties with distinct dimensions and metrics.
    let specs: [(&str, usize, DistanceMetric); 3] = [
        ("title_embedding", 4, DistanceMetric::Cosine),
        ("body_embedding", 8, DistanceMetric::Euclidean),
        ("summary_embedding", 6, DistanceMetric::DotProduct),
    ];

    let current = Arc::new(CurrentStorage::new());
    let mut created_nodes = std::collections::HashMap::new();
    for (property, dims, metric) in specs {
        current
            .enable_vector_index(property, HnswConfig::new(dims, metric))
            .unwrap_or_else(|e| panic!("failed to enable index for {property}: {e}"));
        let mut builder = PropertyMapBuilder::new();
        let mut embedding = vec![0.0f32; dims];
        embedding[0] = 1.0;
        builder = builder.insert_vector(property, &embedding);
        let node_id = current
            .create_node("Doc", builder.build())
            .unwrap_or_else(|e| panic!("failed to create node for {property}: {e}"));
        created_nodes.insert(property, node_id);
    }
    persist_vector_indexes(&current, &manager, None, 0).expect("failed to persist vector indexes");

    // Load into a fresh storage and verify every index round-tripped.
    let fresh = Arc::new(CurrentStorage::new());
    let summary = load_vector_indexes(&fresh, &manager).expect("load must succeed");
    assert_eq!(summary.loaded, 3, "all three indexes must load");
    assert_eq!(summary.skipped, 0);

    for (property, dims, metric) in specs {
        let config = fresh
            .get_hnsw_config_for(property)
            .unwrap_or_else(|| panic!("index config for {property} must survive the round trip"));
        assert_eq!(config.dimensions, dims, "dimensions for {property}");
        assert_eq!(config.metric, metric, "metric for {property}");

        let (index, _, count, mappings) = fresh
            .get_vector_index_for_persistence(property)
            .unwrap_or_else(|| panic!("index for {property} must be registered"));
        assert_eq!(count, 1, "vector count for {property}");
        assert_eq!(mappings.len(), 1, "id mappings for {property}");
        let created_id = created_nodes[property];
        assert!(
            mappings
                .iter()
                .any(|(node_id, _)| *node_id == created_id.as_u64()),
            "restored mappings for {property} must contain the created node id \
             {created_id:?}, got: {mappings:?}"
        );
        assert_eq!(index.len(), 1, "HNSW length for {property}");
    }
}

/// Issue #451: every per-index skip branch must skip ONLY the broken index
/// and still load a valid sibling. Parameterized over the corruption modes
/// so each error path in `load_single_vector_index` has direct coverage.
#[test]
fn test_load_vector_indexes_skip_branches_per_corruption_mode() {
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use crate::storage::index_persistence::formats::PersistedHnswConfig;
    use crate::storage::index_persistence::vector::{new_vector_meta, save_vector_meta};
    use std::path::Path;

    fn truncate_file(path: &Path) {
        let data = std::fs::read(path).unwrap();
        assert!(
            data.len() > 8,
            "file {path:?} too small ({} bytes) to truncate meaningfully",
            data.len()
        );
        std::fs::write(path, &data[..data.len() / 2]).unwrap();
    }

    // Each corruption is applied to the persisted "bad_embedding" directory
    // while its sibling "valid_embedding" stays untouched.
    #[allow(clippy::type_complexity)]
    let modes: Vec<(&str, Box<dyn Fn(&Path)>)> = vec![
        (
            "missing meta.idx",
            Box::new(|dir: &Path| std::fs::remove_file(dir.join("meta.idx")).unwrap()),
        ),
        (
            "truncated meta.idx",
            Box::new(|dir: &Path| truncate_file(&dir.join("meta.idx"))),
        ),
        (
            "valid meta with unknown metric byte",
            Box::new(|dir: &Path| {
                // Structurally valid (magic, version, CRC all pass) but the
                // metric byte 99 maps to no DistanceMetric.
                let meta = new_vector_meta(
                    "bad_embedding",
                    4,
                    99,
                    PersistedHnswConfig {
                        m: 16,
                        ef_construction: 200,
                        ef_search: 50,
                    },
                );
                save_vector_meta(&meta, &dir.join("meta.idx")).unwrap();
            }),
        ),
        (
            "garbage mappings.idx",
            Box::new(|dir: &Path| {
                std::fs::write(dir.join("mappings.idx"), b"garbage, not bitcode").unwrap()
            }),
        ),
        (
            "garbage current.usearch",
            Box::new(|dir: &Path| {
                std::fs::write(dir.join("current.usearch"), b"garbage, not usearch").unwrap()
            }),
        ),
        (
            "truncated current.usearch",
            Box::new(|dir: &Path| truncate_file(&dir.join("current.usearch"))),
        ),
        (
            "garbage current.usearch.mappings sidecar",
            Box::new(|dir: &Path| {
                std::fs::write(dir.join("current.usearch.mappings"), b"garbage sidecar").unwrap()
            }),
        ),
    ];

    for (mode, corrupt) in modes {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
        manager.ensure_directories().unwrap();

        // Persist two healthy indexes over one node.
        let current = Arc::new(CurrentStorage::new());
        for property in ["bad_embedding", "valid_embedding"] {
            current
                .enable_vector_index(property, HnswConfig::new(4, DistanceMetric::Cosine))
                .unwrap_or_else(|e| panic!("[{mode}] failed to enable {property}: {e}"));
        }
        current
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("bad_embedding", &[1.0f32, 0.0, 0.0, 0.0])
                    .insert_vector("valid_embedding", &[1.0f32, 0.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap_or_else(|e| panic!("[{mode}] failed to create node: {e}"));
        persist_vector_indexes(&current, &manager, None, 0)
            .unwrap_or_else(|e| panic!("[{mode}] failed to persist: {e}"));

        // Apply this mode's corruption to the bad index's directory.
        corrupt(&manager.vector_path("bad_embedding"));

        // Load into a fresh storage: exactly the corrupted index is skipped.
        let fresh = Arc::new(CurrentStorage::new());
        let summary = load_vector_indexes(&fresh, &manager)
            .unwrap_or_else(|e| panic!("[{mode}] per-index corruption must not abort load: {e}"));
        assert_eq!(summary.skipped, 1, "[{mode}] skipped count");
        assert_eq!(summary.loaded, 1, "[{mode}] loaded count");
        assert!(
            fresh.is_vector_index_enabled_for("valid_embedding"),
            "[{mode}] the valid sibling must load"
        );
        assert!(
            !fresh.is_vector_index_enabled_for("bad_embedding"),
            "[{mode}] the corrupted index must not be registered"
        );
    }
}

/// Issue #451: a `mappings.idx` that decodes cleanly but contains a usearch
/// key beyond `MAX_VALID_KEY` (only possible via corruption) must cause the
/// index to be SKIPPED — not panic, and not poison the index's key
/// allocator via `usearch_key + 1` — while the valid sibling still loads.
#[test]
fn test_load_vector_indexes_skips_index_with_out_of_range_usearch_key() {
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use crate::storage::index_persistence::formats::VectorMapping;
    use crate::storage::index_persistence::vector::{new_vector_mappings, save_vector_mappings};

    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    manager.ensure_directories().unwrap();

    let current = Arc::new(CurrentStorage::new());
    for property in ["huge_key_embedding", "valid_embedding"] {
        current
            .enable_vector_index(property, HnswConfig::new(4, DistanceMetric::Cosine))
            .expect("failed to enable vector index");
    }
    current
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("huge_key_embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .insert_vector("valid_embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("failed to create node");
    persist_vector_indexes(&current, &manager, None, 0).expect("failed to persist");

    // Overwrite the mappings with a structurally valid file holding a key
    // beyond MAX_VALID_KEY (u64::MAX - 1000).
    let mut mappings = new_vector_mappings();
    mappings.count = 1;
    mappings.mappings = vec![VectorMapping {
        node_id: 1,
        usearch_key: u64::MAX,
    }];
    save_vector_mappings(
        &mappings,
        &manager
            .vector_path("huge_key_embedding")
            .join("mappings.idx"),
    )
    .expect("failed to write huge-key mappings");

    let fresh = Arc::new(CurrentStorage::new());
    let summary = load_vector_indexes(&fresh, &manager)
        .expect("an out-of-range usearch key must be skipped, not abort or panic");
    assert_eq!(summary.skipped, 1, "the huge-key index must be skipped");
    assert_eq!(summary.loaded, 1, "the valid sibling must still load");
    assert!(fresh.is_vector_index_enabled_for("valid_embedding"));
    assert!(!fresh.is_vector_index_enabled_for("huge_key_embedding"));
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

    persist_graph_index(&current, &manager, None, 0).expect("failed to persist graph index");

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
fn test_persist_all_indexes_creates_manifest() {
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let temporal_indexes = Arc::new(TemporalIndexes::new());

    // Use a separate temp dir for WAL to avoid conflicts
    let wal_dir = tempfile::tempdir().unwrap();
    let config =
        crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig::new(wal_dir.path());
    let wal =
        Arc::new(crate::storage::wal::concurrent_system::ConcurrentWalSystem::new(config).unwrap());

    let tracker = Arc::new(PersistenceTracker::new());

    // Should succeed and create manifest
    persist_all_indexes(
        &current,
        &historical,
        &temporal_indexes,
        &wal,
        &manager,
        &tracker,
    )
    .expect("persist_all_indexes failed");

    // Verify manifest exists
    // Note: IndexPersistenceManager adds "indexes" subdir and uses .idx extension
    let manifest_path = manager.base_path().join("indexes").join("manifest.idx");
    assert!(
        manifest_path.exists(),
        "Manifest file should be created by persist_all_indexes at {:?}",
        manifest_path
    );

    // Verify string interner exists (created by default)
    let interner_path = manager
        .base_path()
        .join("indexes")
        .join("strings")
        .join("interner.idx");
    assert!(
        interner_path.exists(),
        "String interner should be persisted by persist_all_indexes at {:?}",
        interner_path
    );
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

    persist_temporal_index(&historical, &temporal_indexes, &manager, &tracker, 0)
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
