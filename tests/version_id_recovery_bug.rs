/// TDD test to confirm version_id restoration bug in with_unified_config
///
/// This test verifies that version IDs are PRESERVED when loading from
/// index persistence (not checkpoints).
use gallifreydb::core::GLOBAL_INTERNER;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::storage::index_persistence::IndexPersistenceManager;
use gallifreydb::storage::index_persistence::formats::{PersistedEdge, PersistedNode};
use gallifreydb::storage::index_persistence::graph::{
    new_graph_index_data, persist_property_map, save_graph_index,
};
use std::sync::Mutex;
use tempfile::tempdir;

/// Global mutex to serialize tests that use GLOBAL_INTERNER
static INTERNER_TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Test that version IDs are preserved when loading from persisted graph data.
///
/// This is a CRITICAL correctness requirement - if version IDs are not preserved,
/// current state becomes disconnected from historical versions, breaking temporal queries.
#[test]
fn test_version_id_preserved_when_loading_graph() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());
    manager.ensure_directories().unwrap();

    // Setup interner
    GLOBAL_INTERNER.intern("Person").unwrap();
    GLOBAL_INTERNER.intern("name").unwrap();
    manager.save_string_interner().unwrap();

    // Create graph data with specific version IDs
    let mut graph_data = new_graph_index_data();

    let props = PropertyMapBuilder::new().insert("name", "Alice").build();

    // Node with version_id = 42 (specific value to detect if it's preserved)
    graph_data.nodes.push(PersistedNode {
        id: 1,
        label_idx: GLOBAL_INTERNER.intern("Person").unwrap().as_u32(),
        version_id: 42, // ← Critical: This must be preserved on load
        properties: persist_property_map(&props).unwrap(),
    });

    // Node with version_id = 99
    let props2 = PropertyMapBuilder::new().insert("name", "Bob").build();
    graph_data.nodes.push(PersistedNode {
        id: 2,
        label_idx: GLOBAL_INTERNER.intern("Person").unwrap().as_u32(),
        version_id: 99, // ← Critical: This must also be preserved
        properties: persist_property_map(&props2).unwrap(),
    });

    // Save to disk
    let graph_path = manager.graph_path().join("adjacency.idx");
    save_graph_index(&graph_data, &graph_path).unwrap();

    // NOW LOAD IT BACK - this exercises the buggy code path in with_unified_config
    use gallifreydb::storage::index_persistence::graph::load_graph_index;
    let loaded_data = load_graph_index(&graph_path).unwrap();

    // Verify version IDs are preserved in the loaded data
    assert_eq!(loaded_data.nodes.len(), 2);
    assert_eq!(
        loaded_data.nodes[0].version_id, 42,
        "PersistedNode.version_id not preserved during load"
    );
    assert_eq!(
        loaded_data.nodes[1].version_id, 99,
        "PersistedNode.version_id not preserved during load"
    );

    // THIS IS WHERE THE BUG OCCURS: with_unified_config ignores these version_ids
    // and generates new ones via version_gen.next()!

    println!(
        "✓ version_ids preserved in loaded data: {:?} and {:?}",
        loaded_data.nodes[0].version_id, loaded_data.nodes[1].version_id
    );
}

/// Test edge version_id preservation
#[test]
fn test_edge_version_id_preserved_when_loading_graph() {
    let _guard = INTERNER_TEST_MUTEX.lock().unwrap();

    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());
    manager.ensure_directories().unwrap();

    // Setup interner
    GLOBAL_INTERNER.intern("KNOWS").unwrap();
    manager.save_string_interner().unwrap();

    // Create edge with specific version ID
    let mut graph_data = new_graph_index_data();

    graph_data.edges.push(PersistedEdge {
        id: 1,
        source_id: 1,
        target_id: 2,
        label_idx: GLOBAL_INTERNER.intern("KNOWS").unwrap().as_u32(),
        version_id: 77, // ← Critical: Must be preserved
        properties: Default::default(),
    });

    // Save and load
    let graph_path = manager.graph_path().join("adjacency.idx");
    save_graph_index(&graph_data, &graph_path).unwrap();

    use gallifreydb::storage::index_persistence::graph::load_graph_index;
    let loaded_data = load_graph_index(&graph_path).unwrap();

    assert_eq!(loaded_data.edges.len(), 1);
    assert_eq!(
        loaded_data.edges[0].version_id, 77,
        "PersistedEdge.version_id not preserved during load"
    );

    println!(
        "✓ Edge version_id preserved: {}",
        loaded_data.edges[0].version_id
    );
}
