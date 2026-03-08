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
fn test_node_get_property_missing() {
    // 🛡️ Sentry Test: Node::get_property
    // Mutants targeted: returning `None` instead of `Some(Box::new(...))`
    // Or returning `Some(Default::default())`
    let label = GLOBAL_INTERNER.intern("User").unwrap();
    let mut props = PropertyMapBuilder::new();
    props = props.insert("age", 42i64);
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        props.build(),
        VersionId::new(1).unwrap(),
    );

    // Positive check
    assert_eq!(
        node.get_property("age").and_then(|v| v.as_int()),
        Some(42),
        "get_property must correctly return property value"
    );

    // Negative check
    assert!(
        node.get_property("missing").is_none(),
        "get_property must return None for missing property"
    );
}

#[test]
fn test_edge_get_property_missing() {
    // 🛡️ Sentry Test: Edge::get_property
    // Mutants targeted: returning `None` instead of `Some(Box::new(...))`
    // Or returning `Some(Default::default())`
    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let mut props = PropertyMapBuilder::new();
    props = props.insert("weight", 1.5f64);
    let edge = Edge::new(
        EdgeId::new(1).unwrap(),
        label,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        props.build(),
        VersionId::new(1).unwrap(),
    );

    // Positive check
    assert_eq!(
        edge.get_property("weight").and_then(|v| v.as_float()),
        Some(1.5),
        "get_property must correctly return property value"
    );

    // Negative check
    assert!(
        edge.get_property("missing").is_none(),
        "get_property must return None for missing property"
    );
}

#[test]
fn test_node_has_label_strict() {
    // 🛡️ Sentry Test: Node::has_label
    // Mutants targeted: `replace == with !=`, returning `true`, returning `false`.
    let label = GLOBAL_INTERNER.intern("Alpha").unwrap();
    let other_label = GLOBAL_INTERNER.intern("Beta").unwrap();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    );

    assert!(node.has_label(label), "Node should have its own label");
    assert!(
        !node.has_label(other_label),
        "Node should not have a different label"
    );
}

#[test]
fn test_edge_has_label_strict() {
    // 🛡️ Sentry Test: Edge::has_label
    // Mutants targeted: `replace == with !=`, returning `true`, returning `false`.
    let label = GLOBAL_INTERNER.intern("AlphaEdge").unwrap();
    let other_label = GLOBAL_INTERNER.intern("BetaEdge").unwrap();
    let edge = Edge::new(
        EdgeId::new(1).unwrap(),
        label,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    );

    assert!(edge.has_label(label), "Edge should have its own label");
    assert!(
        !edge.has_label(other_label),
        "Edge should not have a different label"
    );
}

#[test]
fn test_node_fmt_debug() {
    // 🛡️ Sentry Test: Node::fmt
    // Mutants targeted: returning `Ok(Default::default())`
    let label = GLOBAL_INTERNER.intern("DebugNodeLabel").unwrap();
    let mut props = PropertyMapBuilder::new();
    props = props.insert("debug_key", "debug_val");
    let node = Node::new(
        NodeId::new(777).unwrap(),
        label,
        props.build(),
        VersionId::new(1).unwrap(),
    );

    let debug_str = format!("{:?}", node);
    assert!(
        debug_str.contains("DebugNodeLabel"),
        "Node debug format must contain label name"
    );
    assert!(
        debug_str.contains("777"),
        "Node debug format must contain node ID"
    );
    assert!(
        debug_str.contains("debug_key"),
        "Node debug format must contain property keys"
    );
}

#[test]
fn test_edge_fmt_debug() {
    // 🛡️ Sentry Test: Edge::fmt
    // Mutants targeted: returning `Ok(Default::default())`
    let label = GLOBAL_INTERNER.intern("DebugEdgeLabel").unwrap();
    let edge = Edge::new(
        EdgeId::new(888).unwrap(),
        label,
        NodeId::new(111).unwrap(),
        NodeId::new(222).unwrap(),
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    );

    let debug_str = format!("{:?}", edge);
    assert!(
        debug_str.contains("DebugEdgeLabel"),
        "Edge debug format must contain label name"
    );
    assert!(
        debug_str.contains("888"),
        "Edge debug format must contain edge ID"
    );
    assert!(
        debug_str.contains("111"),
        "Edge debug format must contain source ID"
    );
    assert!(
        debug_str.contains("222"),
        "Edge debug format must contain target ID"
    );
}

#[test]
fn test_node_has_label_str_strict() {
    // 🛡️ Sentry Test: Node::has_label_str
    // Mutants targeted: returning true, returning false
    let label = GLOBAL_INTERNER.intern("TargetNode").unwrap();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        label,
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    );

    assert!(
        node.has_label_str("TargetNode"),
        "Should correctly match its label string"
    );
    assert!(
        !node.has_label_str("OtherNode"),
        "Should not match a different string"
    );
}

#[test]
fn test_edge_has_label_str_strict() {
    // 🛡️ Sentry Test: Edge::has_label_str
    // Mutants targeted: returning true, returning false
    let label = GLOBAL_INTERNER.intern("TargetEdge").unwrap();
    let edge = Edge::new(
        EdgeId::new(1).unwrap(),
        label,
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    );

    assert!(
        edge.has_label_str("TargetEdge"),
        "Should correctly match its label string"
    );
    assert!(
        !edge.has_label_str("OtherEdge"),
        "Should not match a different string"
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
