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

/// Helper: persist one healthy "valid_embedding" index over a single node so
/// corruption tests have an intact sibling to assert isolation against.
/// Returns the manager rooted at `base` (directories already created).
fn persist_one_valid_vector_index(base: &std::path::Path) -> Arc<IndexPersistenceManager> {
    use crate::index::vector::{DistanceMetric, HnswConfig};

    let manager = Arc::new(IndexPersistenceManager::new(base));
    manager.ensure_directories().unwrap();

    let current = Arc::new(CurrentStorage::new());
    current
        .enable_vector_index(
            "valid_embedding",
            HnswConfig::new(4, DistanceMetric::Cosine),
        )
        .expect("failed to enable vector index");
    current
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("valid_embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("failed to create node with embedding");
    persist_vector_indexes(&current, &manager, None, 0).expect("failed to persist vector indexes");
    manager
}

/// Issue #451: a panic inside one per-index load task must be caught by the
/// `catch_unwind` isolation in `load_vector_indexes` and converted into a
/// skip — never abort startup or the sibling loads. Panics are injected via
/// the test-only magic directory names consulted by
/// `load_single_vector_index`; the three names exercise the three
/// panic-payload downcast arms (&str, String, non-string).
#[test]
fn test_load_vector_indexes_panicking_load_is_isolated_to_a_skip() {
    let temp_dir = tempfile::tempdir().unwrap();
    let manager = persist_one_valid_vector_index(temp_dir.path());

    let vector_base = manager.indexes_path().join("vector");
    for magic in [
        "__panic_injection_str__",
        "__panic_injection_string__",
        "__panic_injection_other__",
    ] {
        std::fs::create_dir_all(vector_base.join(magic)).unwrap();
    }

    let fresh = Arc::new(CurrentStorage::new());
    let summary = load_vector_indexes(&fresh, &manager)
        .expect("a panicking per-index load must be isolated, not abort all vector loading");
    assert_eq!(
        summary.skipped, 3,
        "every panicking load must be counted as a skip"
    );
    assert_eq!(summary.loaded, 1, "the valid sibling must still load");
    assert!(fresh.is_vector_index_enabled_for("valid_embedding"));
    for magic in [
        "__panic_injection_str__",
        "__panic_injection_string__",
        "__panic_injection_other__",
    ] {
        assert!(
            !fresh.is_vector_index_enabled_for(magic),
            "a panicked index '{magic}' must not be registered"
        );
    }
}

/// Issue #451: a vector index directory whose name is not valid UTF-8 cannot
/// name a property, so it is skipped (with the lossy path as the reported
/// name) while valid siblings still load.
#[cfg(unix)]
#[test]
fn test_load_vector_indexes_skips_non_utf8_directory_name() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let temp_dir = tempfile::tempdir().unwrap();
    let manager = persist_one_valid_vector_index(temp_dir.path());

    // 0xFF is never valid in UTF-8, so this directory name has no &str form.
    let non_utf8_name = OsStr::from_bytes(b"bad_\xff\xfe_embedding");
    let non_utf8_dir = manager.indexes_path().join("vector").join(non_utf8_name);
    if std::fs::create_dir_all(&non_utf8_dir).is_err() {
        // Filesystem (e.g. APFS on macOS) rejects non-UTF-8 names with EILSEQ;
        // the non-UTF-8-directory scenario cannot exist here.
        return;
    }

    let fresh = Arc::new(CurrentStorage::new());
    let summary = load_vector_indexes(&fresh, &manager)
        .expect("a non-UTF-8 directory name must be skipped, not abort all vector loading");
    assert_eq!(
        summary.skipped, 1,
        "the non-UTF-8 directory must be skipped"
    );
    assert_eq!(summary.loaded, 1, "the valid sibling must still load");
    assert!(fresh.is_vector_index_enabled_for("valid_embedding"));
}

/// Issue #451: when `meta.idx` is intact but `current.usearch` is missing,
/// the index is registered EMPTY (a fresh HNSW index) and the hollow-index
/// warning branch fires (metadata records N > 0 vectors but 0 were
/// restored). The load must still succeed so the caller can recover via
/// `rebuild_vector_index`.
#[test]
fn test_load_vector_indexes_registers_empty_index_when_usearch_missing() {
    use crate::index::vector::VectorIndex;

    let temp_dir = tempfile::tempdir().unwrap();
    let manager = persist_one_valid_vector_index(temp_dir.path());

    // Remove the HNSW graph file (and its sidecar) but keep meta.idx, whose
    // vector_count is 1 — the hollow-index condition.
    let index_dir = manager.vector_path("valid_embedding");
    std::fs::remove_file(index_dir.join("current.usearch")).unwrap();
    let sidecar = index_dir.join("current.usearch.mappings");
    if sidecar.exists() {
        std::fs::remove_file(sidecar).unwrap();
    }

    let fresh = Arc::new(CurrentStorage::new());
    let summary = load_vector_indexes(&fresh, &manager)
        .expect("a missing current.usearch must register an empty index, not fail the load");
    assert_eq!(summary.loaded, 1, "the hollow index must still be loaded");
    assert_eq!(summary.skipped, 0);
    assert!(
        fresh.is_vector_index_enabled_for("valid_embedding"),
        "the hollow index must be registered so rebuild_vector_index can recover it"
    );
    let (index, _, _, _) = fresh
        .get_vector_index_for_persistence("valid_embedding")
        .expect("hollow index must be registered");
    assert_eq!(
        index.len(),
        0,
        "no vectors can be restored without current.usearch"
    );
}

/// Issue #451: a mapping whose node id exceeds `MAX_VALID_ID` is skipped
/// with a warning while the rest of the index still loads — an invalid node
/// id poisons only that one mapping, not the whole index (unlike an
/// out-of-range usearch key, which would poison the key allocator).
#[test]
fn test_load_vector_indexes_warns_on_invalid_node_id_in_mappings_but_loads() {
    use crate::storage::index_persistence::formats::VectorMapping;
    use crate::storage::index_persistence::vector::{new_vector_mappings, save_vector_mappings};

    let temp_dir = tempfile::tempdir().unwrap();
    let manager = persist_one_valid_vector_index(temp_dir.path());

    // Rewrite the mappings with one valid entry and one whose node id is
    // beyond MAX_VALID_ID (u64::MAX - 1000), which NodeId::new rejects.
    let mut mappings = new_vector_mappings();
    mappings.count = 2;
    mappings.mappings = vec![
        VectorMapping {
            node_id: 1,
            usearch_key: 0,
        },
        VectorMapping {
            node_id: u64::MAX - 100,
            usearch_key: 1,
        },
    ];
    save_vector_mappings(
        &mappings,
        &manager.vector_path("valid_embedding").join("mappings.idx"),
    )
    .expect("failed to write mappings with invalid node id");

    let fresh = Arc::new(CurrentStorage::new());
    let summary = load_vector_indexes(&fresh, &manager)
        .expect("an invalid node id in mappings must be a per-mapping warning, not a failure");
    assert_eq!(
        summary.loaded, 1,
        "the index must load despite the invalid mapping"
    );
    assert_eq!(summary.skipped, 0);
    let (_, _, _, restored_mappings) = fresh
        .get_vector_index_for_persistence("valid_embedding")
        .expect("index must be registered");
    assert!(
        restored_mappings.iter().any(|(node_id, _)| *node_id == 1),
        "the valid mapping must be restored, got: {restored_mappings:?}"
    );
    assert!(
        !restored_mappings
            .iter()
            .any(|(node_id, _)| *node_id == u64::MAX - 100),
        "the invalid mapping must be dropped, got: {restored_mappings:?}"
    );
}

/// Issue #451: an unreadable vector directory (here: `vector` is a file, so
/// `read_dir` fails) is an infrastructure error for `load_vector_indexes`,
/// but `load_indexes_startup` must degrade it to a warning — vector indexes
/// are auxiliary and their loss must never abort database startup.
#[test]
fn test_load_indexes_startup_survives_unreadable_vector_directory() {
    use crate::core::id::IdGenerator;

    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    manager.ensure_directories().unwrap();

    // Replace the vector directory with a regular file: it "exists" but
    // read_dir on it fails, which is the only Err path out of
    // load_vector_indexes.
    let vector_base = manager.indexes_path().join("vector");
    if vector_base.exists() {
        std::fs::remove_dir_all(&vector_base).unwrap();
    }
    std::fs::write(&vector_base, b"not a directory").unwrap();

    let current = Arc::new(CurrentStorage::new());
    assert!(
        load_vector_indexes(&current, &manager).is_err(),
        "read_dir on a file must surface as an infrastructure error"
    );

    // Startup loading must swallow that error with a warning, not panic or
    // abort.
    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let node_id_gen = Arc::new(IdGenerator::new());
    let edge_id_gen = Arc::new(IdGenerator::new());
    let version_id_gen = Arc::new(IdGenerator::new());
    let manifest_lsn = load_indexes_startup(
        &manager,
        &current,
        &historical,
        &node_id_gen,
        &edge_id_gen,
        &version_id_gen,
    );
    assert!(
        manifest_lsn.is_none(),
        "no manifest was persisted, so no LSN should be reported"
    );
    assert!(
        !current.is_vector_index_enabled(),
        "no vector index can load from an unreadable vector directory"
    );
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
