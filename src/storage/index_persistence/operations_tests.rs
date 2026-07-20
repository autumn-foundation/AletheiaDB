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

/// Issue #3387 availability fix, save-path variant: `persist_temporal_index`
/// must materialize sparse vector deltas (node AND edge) against their
/// reconstructed base state in the persisted copy -- succeeding instead of
/// hard-failing -- while leaving the live in-memory delta sparse. The
/// persisted file must reconstruct the updated vectors on restore.
#[test]
fn test_persist_temporal_index_materializes_sparse_vector_deltas() {
    use crate::core::id::EdgeId;
    use crate::core::version::{VectorDelta, VersionData};
    use crate::storage::index_persistence::temporal::restore_into_historical_storage;

    let temp_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(IndexPersistenceManager::new(temp_dir.path()));
    let tracker = Arc::new(PersistenceTracker::new());
    let temporal_indexes = Arc::new(TemporalIndexes::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));

    let node_label = GLOBAL_INTERNER.intern("SparseSaveNode").unwrap();
    let edge_label = GLOBAL_INTERNER.intern("SPARSE_SAVE_EDGE").unwrap();
    let node_id = NodeId::new(1).unwrap();
    let source = NodeId::new(1).unwrap();
    let target = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(1).unwrap();

    let dim = 128usize;
    let base_vec: Vec<f32> = (0..dim).map(|i| i as f32 * 0.01).collect();
    let mut updated_vec = base_vec.clone();
    updated_vec[7] = 42.0; // 1 change * 2 < 128 -> sparse delta

    // Explicitly distinct tx timestamps: two same-microsecond `now()` calls
    // would make head selection and tx-closure ordering ambiguous.
    let now = time::now();
    let t1 = now;
    let t2 = crate::core::hlc::HybridTimestamp::new(now.wallclock() + 1_000_000, 0).unwrap();
    {
        let mut hist = historical.write();
        hist.add_node_version(
            node_id,
            VersionId::new(1).unwrap(),
            t1,
            t1,
            node_label,
            PropertyMapBuilder::new()
                .insert("name", "doc")
                .insert_vector("embedding", &base_vec)
                .build(),
            false,
        )
        .unwrap();
        hist.add_node_version(
            node_id,
            VersionId::new(2).unwrap(),
            t2,
            t2,
            node_label,
            PropertyMapBuilder::new()
                .insert("name", "doc")
                .insert_vector("embedding", &updated_vec)
                .build(),
            false,
        )
        .unwrap();

        hist.add_edge_version(
            edge_id,
            VersionId::new(3).unwrap(),
            t1,
            t1,
            edge_label,
            source,
            target,
            PropertyMapBuilder::new()
                .insert_vector("embedding", &base_vec)
                .build(),
            false,
        )
        .unwrap();
        hist.add_edge_version(
            edge_id,
            VersionId::new(4).unwrap(),
            t2,
            t2,
            edge_label,
            source,
            target,
            PropertyMapBuilder::new()
                .insert_vector("embedding", &updated_vec)
                .build(),
            false,
        )
        .unwrap();

        // Preconditions: both updates really produced SPARSE deltas.
        let is_sparse = |data: &VersionData| match data {
            VersionData::Delta { delta } => delta
                .vector_deltas
                .values()
                .any(|d| matches!(d, VectorDelta::Sparse { .. })),
            VersionData::Anchor { .. } => false,
        };
        let node_head = hist.get_current_node_version(node_id).unwrap();
        assert!(
            is_sparse(&hist.get_node_version(node_head).unwrap().data),
            "precondition: node update must produce VectorDelta::Sparse"
        );
        let edge_head = hist.get_current_edge_version(edge_id).unwrap();
        assert!(
            is_sparse(&hist.get_edge_version(edge_head).unwrap().data),
            "precondition: edge update must produce VectorDelta::Sparse"
        );
    }

    // The save must succeed (pre-fix: hard error demanding materialization).
    persist_temporal_index(&historical, &temporal_indexes, &manager, &tracker, 0)
        .expect("persist_temporal_index must materialize sparse deltas, not fail");

    // Live in-memory state keeps its sparse deltas untouched.
    {
        let hist = historical.read();
        let node_head = hist.get_current_node_version(node_id).unwrap();
        let VersionData::Delta { delta } = &hist.get_node_version(node_head).unwrap().data else {
            panic!("live node head must remain a delta");
        };
        assert!(
            delta
                .vector_deltas
                .values()
                .any(|d| matches!(d, VectorDelta::Sparse { .. })),
            "persist must not mutate live node state"
        );
    }

    // The persisted file reconstructs the updated vectors on restore.
    let temporal_data = load_temporal_index(&manager.temporal_path().join("versions.idx"))
        .expect("failed to load persisted temporal index");
    let mut restored = HistoricalStorage::new();
    restore_into_historical_storage(&temporal_data, &mut restored).unwrap();

    let node_head = restored.get_current_node_version(node_id).unwrap();
    let node_props = restored.reconstruct_node_properties(node_head).unwrap();
    assert_eq!(
        node_props.get("embedding").and_then(|v| v.as_vector()),
        Some(updated_vec.as_slice()),
        "restored node head must reconstruct the updated embedding"
    );

    let edge_head = restored.get_current_edge_version(edge_id).unwrap();
    let edge_props = restored.reconstruct_edge_properties(edge_head).unwrap();
    assert_eq!(
        edge_props.get("embedding").and_then(|v| v.as_vector()),
        Some(updated_vec.as_slice()),
        "restored edge head must reconstruct the updated embedding"
    );
}

// ============================================================================
// Issue #3490: cross-process reload must not lose multi-property nodes to a
// "String interner mismatch". See src/storage/index_persistence/strings.rs.
// ============================================================================

/// Issue #3490 regression: a node persisted with MULTIPLE string-keyed
/// properties (and string values) must survive a fresh-process reload whose
/// WAL replay has already re-interned the property keys/values in a DIFFERENT
/// order than the saved interner file — the exact ordering divergence that
/// made `restore_string_interner`'s positional identity assertion fail and
/// silently abandon the graph index (data loss).
///
/// The GLOBAL_INTERNER is a process-global singleton, so this test cannot
/// safely `clear()` it in-process without corrupting other parallel tests.
/// It therefore spawns a genuine subprocess that runs the isolated
/// `regression_3490_interner_remap_subprocess_helper` (an `#[ignore]`d test
/// that owns the whole interner) and asserts it exits 0. The helper encodes
/// the root-cause ordering divergence deterministically so it fails RELIABLY
/// on unfixed code (never intermittently).
#[test]
fn test_multi_property_cross_process_reload_survives_interner_remap_3490() {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let exe = std::env::current_exe().expect("failed to locate current test binary");
    let mut child = Command::new(exe)
        .args([
            "--ignored",
            "--exact",
            "--nocapture",
            "storage::index_persistence::operations::tests::regression_3490_interner_remap_subprocess_helper",
        ])
        .spawn()
        .expect("failed to spawn subprocess for Issue #3490 reload test");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(
                    status.success(),
                    "Issue #3490 subprocess reload helper failed (multi-property node \
                     lost or corrupted across a fresh-process reload): {status}"
                );
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("Issue #3490 subprocess reload helper did not complete in time");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("failed while polling Issue #3490 subprocess: {e}"),
        }
    }
}

/// Isolated subprocess body for Issue #3490 (see the spawning test above).
///
/// Runs entirely inside a dedicated subprocess (`--exact` selects only this
/// test, so nothing else touches the interner concurrently), which makes it
/// safe to `clear()` the GLOBAL_INTERNER to emulate a pristine, freshly
/// started process.
///
/// Coverage extends beyond graph nodes to every persisted-id resolution site
/// that #3490 can corrupt: graph **edges** (string-valued property), the
/// **temporal** index (node anchor version property, node delta `removed_keys`,
/// edge version label + property), and the **temporal adjacency** index
/// (edge-type label used by #3225 AS-OF edge-type filtering). Each is asserted
/// to resolve to its ORIGINAL string after a fresh-process reload whose interner
/// order was polluted away from the saved file positions.
#[test]
#[ignore]
fn regression_3490_interner_remap_subprocess_helper() {
    use crate::core::hlc::HybridTimestamp;
    use crate::core::id::{EdgeId, IdGenerator};
    use crate::core::property::PropertyValue;
    use crate::core::version::VersionData;
    use crate::core::{GLOBAL_INTERNER, TIMESTAMP_MAX};
    use crate::index::temporal_adjacency::{TemporalAdjacencyConfig, TemporalAdjacencyIndex};
    use crate::storage::index_persistence::formats::{
        EdgeVersionEntry, IndexManifest, NodeVersionEntry, PersistedEdge, PersistedNode,
        PersistedPropertyMap, PersistedVersionType, TemporalIndexData,
    };
    use crate::storage::index_persistence::graph::{new_graph_index_data, save_graph_index};
    use crate::storage::index_persistence::temporal::save_temporal_index;
    use crate::storage::index_persistence::temporal_adjacency::save_temporal_adjacency_index;
    use crate::storage::index_persistence::{MANIFEST_VERSION, TEMPORAL_MAGIC};

    // Multiple distinct string KEYS and string VALUES across a couple of nodes,
    // to force the collision on real property data (not just labels).
    let label = "Employee";
    let keys = ["dept", "title", "city"];
    let str_values = ["engineering", "staff-swe"];

    // Additional distinct strings exercising edges, temporal versions, and the
    // temporal adjacency index (each must survive the polluted reload).
    let edge_type = "REPORTS_TO"; // graph edge label + temporal edge label + adjacency label
    let edge_prop_key = "since"; // edge string-valued property key
    let edge_prop_val = "2021-tenure"; // edge string-valued property value
    let anchor_key = "clearance"; // temporal node ANCHOR version property key
    let anchor_val = "top-secret"; // temporal node ANCHOR version property value
    let removed_key = "legacy_field"; // temporal node DELTA removed_keys entry

    // ---- Fresh process #1 (the writer): pristine warmed interner. ----
    GLOBAL_INTERNER.clear();
    GLOBAL_INTERNER.warm_common_strings();

    let dir = tempfile::tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());
    manager.ensure_directories().unwrap();

    // Intern in "save order": label, then keys, then string values, then the
    // edge/temporal/adjacency strings. Capture their FILE-space ids.
    let label_id = GLOBAL_INTERNER.intern(label).unwrap();
    let key_ids: Vec<u32> = keys
        .iter()
        .map(|k| GLOBAL_INTERNER.intern(k).unwrap().as_u32())
        .collect();
    let val_ids: Vec<u32> = str_values
        .iter()
        .map(|v| GLOBAL_INTERNER.intern(v).unwrap().as_u32())
        .collect();
    let edge_type_id = GLOBAL_INTERNER.intern(edge_type).unwrap();
    let edge_prop_key_id = GLOBAL_INTERNER.intern(edge_prop_key).unwrap().as_u32();
    let edge_prop_val_id = GLOBAL_INTERNER.intern(edge_prop_val).unwrap().as_u32();
    let anchor_key_id = GLOBAL_INTERNER.intern(anchor_key).unwrap().as_u32();
    let anchor_val_id = GLOBAL_INTERNER.intern(anchor_val).unwrap().as_u32();
    let removed_key_id = GLOBAL_INTERNER.intern(removed_key).unwrap().as_u32();

    // Two nodes: node 1 carries 2 string-valued props + 1 int prop; node 2
    // carries a single string prop. Plus one EDGE (1->2) with a string-valued
    // property, exercising the graph edge label + edge string-property path.
    let mut graph = new_graph_index_data();
    graph.nodes.push(PersistedNode {
        id: 1,
        label_idx: label_id.as_u32(),
        version_id: 1,
        properties: PersistedPropertyMap {
            entries: vec![
                (key_ids[0], PersistedPropertyValue::String(val_ids[0])),
                (key_ids[1], PersistedPropertyValue::String(val_ids[1])),
                (key_ids[2], PersistedPropertyValue::Int(42)),
            ],
        },
    });
    graph.nodes.push(PersistedNode {
        id: 2,
        label_idx: label_id.as_u32(),
        version_id: 2,
        properties: PersistedPropertyMap {
            entries: vec![(key_ids[0], PersistedPropertyValue::String(val_ids[0]))],
        },
    });
    graph.node_count = 2;
    graph.edges.push(PersistedEdge {
        id: 1,
        source_id: 1,
        target_id: 2,
        label_idx: edge_type_id.as_u32(),
        version_id: 3,
        properties: PersistedPropertyMap {
            entries: vec![(
                edge_prop_key_id,
                PersistedPropertyValue::String(edge_prop_val_id),
            )],
        },
    });
    graph.edge_count = 1;

    save_graph_index(&graph, &manager.graph_path().join("adjacency.idx")).unwrap();

    // ---- Temporal index: node ANCHOR version (property), node DELTA version
    // (removed_keys), and an edge version (label + string property). ----
    let node_anchor_vid = 10u64;
    let node_delta_vid = 11u64;
    let edge_vid = 12u64;
    let temporal_data = TemporalIndexData {
        magic: TEMPORAL_MAGIC,
        version: MANIFEST_VERSION,
        node_versions: vec![
            NodeVersionEntry {
                version_id: node_anchor_vid,
                node_id: 1,
                label_idx: label_id.as_u32(),
                valid_from: 1_000_000,
                valid_to: None,
                valid_from_logical: 0,
                valid_to_logical: None,
                tx_time: 1_000_000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Anchor,
                properties: PersistedPropertyMap {
                    entries: vec![(anchor_key_id, PersistedPropertyValue::String(anchor_val_id))],
                },
                vector_snapshot_id: None,
                provenance: None,
                tx_end: None,
                tx_end_logical: None,
                prev_version: None,
                next_version: None,
            },
            NodeVersionEntry {
                version_id: node_delta_vid,
                node_id: 1,
                label_idx: label_id.as_u32(),
                valid_from: 2_000_000,
                valid_to: None,
                valid_from_logical: 0,
                valid_to_logical: None,
                tx_time: 2_000_000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Delta {
                    base_anchor_tx: 1_000_000,
                    base_anchor_tx_logical: 0,
                    removed_keys: vec![removed_key_id],
                },
                properties: PersistedPropertyMap { entries: vec![] },
                vector_snapshot_id: None,
                provenance: None,
                tx_end: None,
                tx_end_logical: None,
                prev_version: None,
                next_version: None,
            },
        ],
        node_anchors: vec![],
        edge_versions: vec![EdgeVersionEntry {
            version_id: edge_vid,
            edge_id: 1,
            source_id: 1,
            target_id: 2,
            label_idx: edge_type_id.as_u32(),
            valid_from: 1_000_000,
            valid_to: None,
            valid_from_logical: 0,
            valid_to_logical: None,
            tx_time: 1_000_000,
            tx_time_logical: 0,
            version_type: PersistedVersionType::Anchor,
            properties: PersistedPropertyMap {
                entries: vec![(
                    edge_prop_key_id,
                    PersistedPropertyValue::String(edge_prop_val_id),
                )],
            },
            provenance: None,
            tx_end: None,
            tx_end_logical: None,
            prev_version: None,
            next_version: None,
        }],
        edge_anchors: vec![],
    };
    save_temporal_index(
        &temporal_data,
        &manager.temporal_path().join("versions.idx"),
    )
    .unwrap();

    // ---- Temporal adjacency index: one edge (1->2) of type `edge_type`. ----
    // The stored entry `label` is a FILE-space interner id; #3225 AS-OF
    // edge-type traversal filters on it, so it must survive the reload.
    let adj_valid_from = HybridTimestamp::new_unchecked(1_000_000, 0);
    let adj_tx_from = HybridTimestamp::new_unchecked(1_000_000, 0);
    {
        let adj = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        adj.insert_edge(
            EdgeId::new(1).unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            edge_type_id,
            adj_valid_from,
            TIMESTAMP_MAX,
            adj_tx_from,
            TIMESTAMP_MAX,
        )
        .unwrap();
        save_temporal_adjacency_index(&adj, manager.base_path()).unwrap();
    }

    manager.save_string_interner().unwrap();
    manager.save_manifest(&IndexManifest::new(0)).unwrap();

    // ---- Fresh process #2 (the reader): pristine warmed interner, THEN a
    // divergent re-intern (as WAL bootstrap / a shared process-global interner
    // would produce) shifts every live id away from its saved file position.
    // Filler strings + a scrambled order guarantee positional identity is
    // broken for ALL the strings our entities reference. ----
    GLOBAL_INTERNER.clear();
    GLOBAL_INTERNER.warm_common_strings();
    for filler in ["__f0__", "__f1__", "__f2__"] {
        GLOBAL_INTERNER.intern(filler).unwrap();
    }
    for s in [
        removed_key,
        anchor_val,
        anchor_key,
        edge_prop_val,
        edge_prop_key,
        edge_type,
        "city",
        "title",
        "dept",
        "staff-swe",
        "engineering",
        label,
    ] {
        GLOBAL_INTERNER.intern(s).unwrap();
    }

    // ---- Run the real startup restore path. ----
    let current = Arc::new(CurrentStorage::new());
    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let node_gen = Arc::new(IdGenerator::new());
    let edge_gen = Arc::new(IdGenerator::new());
    let version_gen = Arc::new(IdGenerator::new());

    load_indexes_startup(
        &manager,
        &current,
        &historical,
        &node_gen,
        &edge_gen,
        &version_gen,
    );

    // ---- Assert: BOTH nodes restored with ALL properties intact. ----
    let node1 = current
        .get_node(NodeId::new(1).unwrap())
        .expect("Issue #3490: node 1 lost across reload (graph index abandoned)");
    assert_eq!(
        node1.properties.len(),
        3,
        "Issue #3490: node 1 must retain all 3 properties, got {}",
        node1.properties.len()
    );
    assert_eq!(
        node1.properties.get("dept"),
        Some(&PropertyValue::String(Arc::from("engineering"))),
        "Issue #3490: node 1 'dept' property corrupted or lost"
    );
    assert_eq!(
        node1.properties.get("title"),
        Some(&PropertyValue::String(Arc::from("staff-swe"))),
        "Issue #3490: node 1 'title' property corrupted or lost"
    );
    assert_eq!(
        node1.properties.get("city"),
        Some(&PropertyValue::Int(42)),
        "Issue #3490: node 1 'city' property corrupted or lost"
    );
    assert_eq!(
        GLOBAL_INTERNER.resolve_with(node1.label, |s| s.to_string()),
        Some(label.to_string()),
        "Issue #3490: node 1 label corrupted across reload"
    );

    let node2 = current
        .get_node(NodeId::new(2).unwrap())
        .expect("Issue #3490: node 2 lost across reload (graph index abandoned)");
    assert_eq!(
        node2.properties.get("dept"),
        Some(&PropertyValue::String(Arc::from("engineering"))),
        "Issue #3490: node 2 'dept' property corrupted or lost"
    );

    // ---- Assert: graph EDGE restored with correct label + string property. ----
    let edge = current
        .get_edge(EdgeId::new(1).unwrap())
        .expect("Issue #3490: edge 1 lost across reload (graph index abandoned)");
    assert_eq!(
        GLOBAL_INTERNER.resolve_with(edge.label, |s| s.to_string()),
        Some(edge_type.to_string()),
        "Issue #3490: edge type label corrupted across reload"
    );
    assert_eq!(
        edge.properties.get(edge_prop_key),
        Some(&PropertyValue::String(Arc::from(edge_prop_val))),
        "Issue #3490: edge string-valued property key/value corrupted across reload"
    );

    // ---- Assert: TEMPORAL versions restored with correct interned strings. ----
    let hist = historical.read();

    // Node ANCHOR version property.
    let anchor_version = hist
        .get_node_version(VersionId::new(node_anchor_vid).unwrap())
        .expect("Issue #3490: node anchor version lost across reload");
    match &anchor_version.data {
        VersionData::Anchor { properties, .. } => {
            assert_eq!(
                properties.get(anchor_key),
                Some(&PropertyValue::String(Arc::from(anchor_val))),
                "Issue #3490: temporal node anchor property corrupted across reload"
            );
        }
        other => panic!("Issue #3490: expected anchor version, got {other:?}"),
    }

    // Node DELTA removed_keys.
    let delta_version = hist
        .get_node_version(VersionId::new(node_delta_vid).unwrap())
        .expect("Issue #3490: node delta version lost across reload");
    match &delta_version.data {
        VersionData::Delta { delta } => {
            let removed: Vec<String> = delta
                .removed
                .iter()
                .filter_map(|k| GLOBAL_INTERNER.resolve_with(*k, |s| s.to_string()))
                .collect();
            assert!(
                removed.iter().any(|s| s == removed_key),
                "Issue #3490: temporal delta removed_key corrupted across reload (got {removed:?})"
            );
        }
        other => panic!("Issue #3490: expected delta version, got {other:?}"),
    }

    // Edge version label + property.
    let edge_version = hist
        .get_edge_version(VersionId::new(edge_vid).unwrap())
        .expect("Issue #3490: edge version lost across reload");
    assert_eq!(
        GLOBAL_INTERNER.resolve_with(edge_version.label, |s| s.to_string()),
        Some(edge_type.to_string()),
        "Issue #3490: temporal edge version label corrupted across reload"
    );

    // ---- Assert: TEMPORAL ADJACENCY edge-type label resolves correctly. ----
    // This is the Finding-1 assertion: #3225 AS-OF edge-type filtering
    // (`get_outgoing_with_label_at_time`) compares the stored entry label
    // against the LIVE id of `edge_type`. Without the adjacency remap the
    // stored label is a file-space id that no longer equals the live id, so the
    // query returns EMPTY (the edge is invisible to edge-type traversal).
    let live_edge_type = GLOBAL_INTERNER
        .get_id(edge_type)
        .expect("edge_type must be interned live after reload");
    let adj_index = hist
        .get_temporal_adjacency_index()
        .expect("Issue #3490: temporal adjacency index not loaded")
        .clone();
    drop(hist);
    let query_time = HybridTimestamp::new_unchecked(1_500_000, 0);
    let matched = adj_index.get_outgoing_with_label_at_time(
        NodeId::new(1).unwrap(),
        live_edge_type,
        query_time,
        query_time,
    );
    assert_eq!(
        matched,
        vec![EdgeId::new(1).unwrap()],
        "Issue #3490: AS-OF edge-type traversal lost edge 1 — temporal adjacency \
         edge-type label was not remapped (resolves to the wrong live id)"
    );
}

/// Regression guard for the #488 auto-merge break (PR #3578).
///
/// The #488 key-rotation merge removed `IndexPersistenceManager::index_cipher()`
/// and migrated the live persist paths to the `*_with_keyring` helpers, but the
/// two snapshot-persist functions added by #481
/// (`persist_graph_index_from_snapshot` / `persist_temporal_index_from_snapshot`)
/// were missed by the text-merge, leaving them calling the removed method
/// (E0599, red trunk). This test drives BOTH functions DIRECTLY with an
/// encryption-enabled manager and asserts the persisted graph + temporal
/// snapshot files carry the `AEIX` encrypted-index header on disk — the exact
/// invariant a wrong (plaintext / non-keyring) call would violate. It also
/// reloads through the keyring to prove the encrypted bytes round-trip.
///
/// Coverage: these are the two changed call sites in `operations.rs`
/// (`save_graph_index_with_keyring(&graph_data, &graph_path, manager.keyring())`
/// at ~L652 and
/// `save_temporal_index_with_keyring(&temporal_data, &temporal_path, manager.keyring())`
/// at ~L1035). The existing end-to-end coverage lives in the ignored-by-codecov
/// `tests/` integration suite, so this lib-level test is what greens the patch.
#[test]
fn snapshot_persist_from_snapshot_uses_keyring_and_writes_encrypted() {
    use crate::encryption::cipher::{Aes256GcmCipher, Cipher};
    use crate::storage::index_persistence::common::is_encrypted_index;
    use crate::storage::index_persistence::graph::load_graph_index_with_keyring;
    use crate::storage::index_persistence::temporal::load_temporal_index_with_keyring;
    use crate::storage::wal::LSN;
    use zeroize::Zeroizing;

    fn test_cipher(seed: u8) -> Arc<dyn Cipher> {
        let mut key = Zeroizing::new([0u8; 32]);
        key[0] = seed;
        key[5] = seed.wrapping_add(1);
        Arc::new(Aes256GcmCipher::new(&key))
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let cipher = test_cipher(0x71);
    // Encryption-enabled manager: `keyring()` is Some, so the persist paths MUST
    // route through the keyring and write AEIX-encrypted files.
    let manager = Arc::new(IndexPersistenceManager::with_cipher(
        temp_dir.path(),
        Some(cipher.clone()),
    ));
    let tracker = Arc::new(PersistenceTracker::new());

    // ── Graph snapshot path (persist_graph_index_from_snapshot) ─────────────
    let current = Arc::new(CurrentStorage::new());
    let graph_value = format!(
        "graph-snap-enc-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos()
    );
    current
        .create_node(
            "SnapshotPersistGraph",
            PropertyMapBuilder::new()
                .insert("payload", graph_value.as_str())
                .build(),
        )
        .expect("failed to create graph test node");
    let current_snapshot = current.create_snapshot(LSN(0));

    persist_graph_index_from_snapshot(&current_snapshot, &manager, Some(&tracker), 0)
        .expect("persist_graph_index_from_snapshot must succeed with an encrypted manager");

    // ── Temporal snapshot path (persist_temporal_index_from_snapshot) ───────
    let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    let temporal_value = format!(
        "temporal-snap-enc-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos()
    );
    let label = GLOBAL_INTERNER
        .intern("SnapshotPersistTemporal")
        .expect("failed to intern label");
    let now = time::now();
    historical
        .write()
        .add_node_version(
            NodeId::new(1).expect("invalid node id"),
            VersionId::new(1).expect("invalid version id"),
            now,
            now,
            label,
            PropertyMapBuilder::new()
                .insert("payload", temporal_value.as_str())
                .build(),
            false,
        )
        .expect("failed to add node version");
    let historical_snapshot = historical.read().create_snapshot(LSN(0));

    persist_temporal_index_from_snapshot(&historical_snapshot, &manager, &tracker, 0)
        .expect("persist_temporal_index_from_snapshot must succeed with an encrypted manager");

    // ── On-disk assertion: both snapshot-persisted files are AEIX-encrypted ──
    // This is the invariant the #488 merge break violated: a wrong (non-keyring)
    // call would write plaintext (or fail to compile). Reading raw bytes proves
    // the keyring path actually ran.
    let graph_path = manager.graph_path().join("adjacency.idx");
    let graph_bytes = std::fs::read(&graph_path).expect("graph snapshot file must exist");
    assert!(
        is_encrypted_index(&graph_bytes),
        "persist_graph_index_from_snapshot must write an AEIX-encrypted file when a \
         keyring is configured (found plaintext at {:?})",
        graph_path
    );

    let temporal_path = manager.temporal_path().join("versions.idx");
    let temporal_bytes = std::fs::read(&temporal_path).expect("temporal snapshot file must exist");
    assert!(
        is_encrypted_index(&temporal_bytes),
        "persist_temporal_index_from_snapshot must write an AEIX-encrypted file when a \
         keyring is configured (found plaintext at {:?})",
        temporal_path
    );

    // ── Round-trip: the encrypted bytes decrypt through the keyring ─────────
    let graph_data = load_graph_index_with_keyring(&graph_path, manager.keyring().as_ref())
        .expect("encrypted graph snapshot must decrypt through the keyring");
    assert_eq!(
        graph_data.nodes.len(),
        1,
        "round-tripped graph snapshot must recover the single persisted node"
    );
    let temporal_data =
        load_temporal_index_with_keyring(&temporal_path, manager.keyring().as_ref())
            .expect("encrypted temporal snapshot must decrypt through the keyring");
    assert_eq!(
        temporal_data.node_versions.len(),
        1,
        "round-tripped temporal snapshot must recover the single persisted version"
    );
}
