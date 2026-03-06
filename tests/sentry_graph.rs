use aletheiadb::core::graph::Edge;
use aletheiadb::core::id::{EdgeId, NodeId, VersionId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::PropertyMap;

#[test]
fn test_edge_connects_mutants() {
    let edge_id = EdgeId::new(1).unwrap();
    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let source_id = NodeId::new(10).unwrap();
    let target_id = NodeId::new(20).unwrap();
    let version = VersionId::new(1).unwrap();

    let edge = Edge::new(
        edge_id,
        label,
        source_id,
        target_id,
        PropertyMap::new(),
        version,
    );

    // Test exact match (returns true)
    assert!(
        edge.connects(source_id, target_id),
        "Edge should connect exact source and target"
    );

    // Test mismatched source (returns false)
    assert!(
        !edge.connects(NodeId::new(99).unwrap(), target_id),
        "Edge should NOT connect with wrong source"
    );

    // Test mismatched target (returns false)
    assert!(
        !edge.connects(source_id, NodeId::new(99).unwrap()),
        "Edge should NOT connect with wrong target"
    );

    // Test both mismatched (returns false)
    assert!(
        !edge.connects(NodeId::new(99).unwrap(), NodeId::new(99).unwrap()),
        "Edge should NOT connect with wrong source and target"
    );
}

#[test]
fn test_edge_has_label_str_mutants() {
    let edge_id = EdgeId::new(1).unwrap();
    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let source_id = NodeId::new(10).unwrap();
    let target_id = NodeId::new(20).unwrap();
    let version = VersionId::new(1).unwrap();

    let edge = Edge::new(
        edge_id,
        label,
        source_id,
        target_id,
        PropertyMap::new(),
        version,
    );

    // Test exact match (returns true)
    assert!(
        edge.has_label_str("KNOWS"),
        "Edge should match exact label string"
    );

    // Test mismatch (returns false)
    assert!(
        !edge.has_label_str("NOT_KNOWS"),
        "Edge should NOT match incorrect label string"
    );
}

#[test]
fn test_edge_fmt_mutants() {
    let edge_id = EdgeId::new(1).unwrap();
    let label = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let source_id = NodeId::new(10).unwrap();
    let target_id = NodeId::new(20).unwrap();
    let version = VersionId::new(1).unwrap();

    let edge = Edge::new(
        edge_id,
        label,
        source_id,
        target_id,
        PropertyMap::new(),
        version,
    );

    let debug_str = format!("{:?}", edge);

    // The mutant replaces the whole body with Ok(Default::default()) which returns ""
    assert!(
        !debug_str.is_empty(),
        "Edge debug string should not be empty"
    );
    assert!(
        debug_str.contains("Edge"),
        "Edge debug string should contain 'Edge'"
    );
    assert!(
        debug_str.contains("KNOWS"),
        "Edge debug string should contain the label string"
    );
}
