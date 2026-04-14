use aletheiadb::core::temporal::time;
use aletheiadb::core::{EdgeId, InternedString, NodeId, TIMESTAMP_MAX};
use aletheiadb::index::temporal_adjacency::{TemporalAdjacencyConfig, TemporalAdjacencyIndex};

#[test]
fn test_insert_single_edge() {
    let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());

    let edge_id = EdgeId::new(1).unwrap();
    let source = NodeId::new(100).unwrap();
    let target = NodeId::new(200).unwrap();
    let label = InternedString::from_raw(1);

    let valid_from = time::now();
    let valid_to = TIMESTAMP_MAX;
    let tx_from = time::now();
    let tx_to = TIMESTAMP_MAX;

    // Insert edge into index
    index
        .insert_edge(
            edge_id, source, target, label, valid_from, valid_to, tx_from, tx_to,
        )
        .unwrap();

    // Query outgoing edges from source
    let edges = index.get_outgoing_at_time(source, valid_from, tx_from);

    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0], edge_id);
}

#[test]
fn test_query_incoming_edges() {
    let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());

    let edge_id = EdgeId::new(1).unwrap();
    let source = NodeId::new(100).unwrap();
    let target = NodeId::new(200).unwrap();
    let label = InternedString::from_raw(1);

    let valid_from = time::now();
    let valid_to = TIMESTAMP_MAX;
    let tx_from = time::now();
    let tx_to = TIMESTAMP_MAX;

    index
        .insert_edge(
            edge_id, source, target, label, valid_from, valid_to, tx_from, tx_to,
        )
        .unwrap();

    // Query incoming edges to target
    let edges = index.get_incoming_at_time(target, valid_from, tx_from);

    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0], edge_id);
}

#[test]
fn test_query_deleted_edge_at_past_time() {
    let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());

    let edge_id = EdgeId::new(1).unwrap();
    let source = NodeId::new(100).unwrap();
    let target = NodeId::new(200).unwrap();
    let label = InternedString::from_raw(1);

    let t0 = time::now();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let t1 = time::now();

    // Edge valid from t0 to t1, then deleted
    index
        .insert_edge(
            edge_id,
            source,
            target,
            label,
            t0,
            t1, // Edge ends at t1
            t0,
            TIMESTAMP_MAX,
        )
        .unwrap();

    // Query at t0 - should find edge
    let edges_at_t0 = index.get_outgoing_at_time(source, t0, t0);
    assert_eq!(edges_at_t0.len(), 1);
    assert_eq!(edges_at_t0[0], edge_id);

    // Query at t1 - should NOT find edge (deleted)
    let edges_at_t1 = index.get_outgoing_at_time(source, t1, t1);
    assert_eq!(edges_at_t1.len(), 0);
}

#[test]
fn test_query_with_label_filter() {
    let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());

    let edge1 = EdgeId::new(1).unwrap();
    let edge2 = EdgeId::new(2).unwrap();
    let source = NodeId::new(100).unwrap();
    let target1 = NodeId::new(200).unwrap();
    let target2 = NodeId::new(300).unwrap();

    let label_knows = InternedString::from_raw(1);
    let label_likes = InternedString::from_raw(2);

    let t = time::now();

    // Insert two edges with different labels
    index
        .insert_edge(
            edge1,
            source,
            target1,
            label_knows,
            t,
            TIMESTAMP_MAX,
            t,
            TIMESTAMP_MAX,
        )
        .unwrap();
    index
        .insert_edge(
            edge2,
            source,
            target2,
            label_likes,
            t,
            TIMESTAMP_MAX,
            t,
            TIMESTAMP_MAX,
        )
        .unwrap();

    // Query for KNOWS label only
    let knows_edges = index.get_outgoing_with_label_at_time(source, label_knows, t, t);
    assert_eq!(knows_edges.len(), 1);
    assert_eq!(knows_edges[0], edge1);

    // Query for LIKES label only
    let likes_edges = index.get_outgoing_with_label_at_time(source, label_likes, t, t);
    assert_eq!(likes_edges.len(), 1);
    assert_eq!(likes_edges[0], edge2);
}

#[test]
fn test_dos_protection_max_entries() {
    let config = TemporalAdjacencyConfig {
        max_entries_per_node: 10,
    };
    let index = TemporalAdjacencyIndex::new(config);

    let source = NodeId::new(100).unwrap();
    let label = InternedString::from_raw(1);
    let t = time::now();

    // Insert 10 edges - should succeed
    for i in 0..10 {
        let edge_id = EdgeId::new(i + 1).unwrap();
        let target = NodeId::new(200 + i).unwrap();
        index
            .insert_edge(
                edge_id,
                source,
                target,
                label,
                t,
                TIMESTAMP_MAX,
                t,
                TIMESTAMP_MAX,
            )
            .unwrap();
    }

    // 11th edge should fail
    let edge_11 = EdgeId::new(11).unwrap();
    let target_11 = NodeId::new(211).unwrap();
    let result = index.insert_edge(
        edge_11,
        source,
        target_11,
        label,
        t,
        TIMESTAMP_MAX,
        t,
        TIMESTAMP_MAX,
    );

    assert!(result.is_err());
}
