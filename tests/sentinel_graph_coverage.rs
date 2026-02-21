//! Sentinel Graph Coverage Tests
//!
//! These tests are specifically designed to cover gaps identified by mutation analysis (manual or automated).
//! They ensure that the graph structure implementations correctly handle edge cases and store all passed data.

use aletheiadb::core::graph::{Edge, Node};
use aletheiadb::core::id::{EdgeId, NodeId, TxId, VersionId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::temporal::Timestamp;
use aletheiadb::core::version::VersionMetadata;

#[test]
fn test_edge_connects_source_mismatch() {
    // 🛡️ Sentinel Test: Kill mutants in Edge::connects
    //
    // Specifically targets:
    // - `self.target == target` (ignores source check)
    // - `self.source == source || self.target == target` (OR instead of AND)
    //
    // Existing tests cover (Match, Match), (Mismatch, Mismatch), (Match, Mismatch).
    // This adds coverage for (Mismatch, Match).

    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let edge = Edge::new(
        EdgeId::new(1).unwrap(),
        label,
        NodeId::new(1).unwrap(), // Source: 1
        NodeId::new(2).unwrap(), // Target: 2
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    );

    // Check: connects(source=3, target=2)
    // Source mismatch (1 != 3), Target match (2 == 2)
    // Should return FALSE.
    // Mutant `self.target == target` would return TRUE.
    assert!(
        !edge.connects(NodeId::new(3).unwrap(), NodeId::new(2).unwrap()),
        "Edge should not connect if source mismatches, even if target matches"
    );
}

#[test]
fn test_node_with_metadata_stores_metadata() {
    // 🛡️ Sentinel Test: Kill mutants in Node::with_metadata
    //
    // Specifically targets:
    // - Ignoring the `metadata` argument and using `VersionMetadata::default()`
    //
    // This constructor was previously unused in tests, allowing any implementation to pass.

    let label = GLOBAL_INTERNER.intern("Person").unwrap();
    let tx_id = TxId::new(12345);
    let timestamp = Timestamp::from(67890);
    let metadata = VersionMetadata::new(tx_id, timestamp);

    let node = Node::with_metadata(
        NodeId::new(1).unwrap(),
        label,
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
        metadata, // Pass specific metadata
    );

    // Verify metadata is stored correctly
    assert_eq!(
        node.metadata, metadata,
        "Node::with_metadata should store the passed metadata, not default"
    );
    assert_eq!(
        node.metadata.created_by_tx, tx_id,
        "Metadata should have correct TxId"
    );
}

#[test]
fn test_edge_with_metadata_stores_metadata() {
    // 🛡️ Sentinel Test: Kill mutants in Edge::with_metadata
    //
    // Specifically targets:
    // - Ignoring the `metadata` argument and using `VersionMetadata::default()`
    //
    // This constructor was previously unused in tests.

    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let tx_id = TxId::new(54321);
    let timestamp = Timestamp::from(98765);
    let metadata = VersionMetadata::new(tx_id, timestamp);

    let edge = Edge::with_metadata(
        EdgeId::new(1).unwrap(),
        label,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
        metadata, // Pass specific metadata
    );

    // Verify metadata is stored correctly
    assert_eq!(
        edge.metadata, metadata,
        "Edge::with_metadata should store the passed metadata, not default"
    );
    assert_eq!(
        edge.metadata.created_by_tx, tx_id,
        "Metadata should have correct TxId"
    );
}
