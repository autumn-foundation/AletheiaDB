use super::*;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMapBuilder;
use crate::core::temporal::TIMESTAMP_MAX;
use std::sync::Arc;

fn make_node_anchor() -> NodeVersion {
    let props = PropertyMapBuilder::new().insert("name", "node").build();
    NodeVersion::new_anchor(
        VersionId::new(10).unwrap(),
        NodeId::new(11).unwrap(),
        BiTemporalInterval::current(1_000.into()),
        GLOBAL_INTERNER.intern("Node").unwrap(),
        props,
    )
}

fn make_edge_anchor() -> EdgeVersion {
    let props = PropertyMapBuilder::new().insert("weight", 1i64).build();
    EdgeVersion::new_anchor(
        VersionId::new(20).unwrap(),
        EdgeId::new(21).unwrap(),
        BiTemporalInterval::current(1_000.into()),
        GLOBAL_INTERNER.intern("EDGE").unwrap(),
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        props,
    )
}

fn make_edge_delta() -> EdgeVersion {
    let old_props = PropertyMapBuilder::new().insert("weight", 1i64).build();
    let new_props = PropertyMapBuilder::new().insert("weight", 2i64).build();
    EdgeVersion::new_delta(
        VersionId::new(22).unwrap(),
        EdgeId::new(23).unwrap(),
        BiTemporalInterval::current(2_000.into()),
        GLOBAL_INTERNER.intern("EDGE").unwrap(),
        NodeId::new(1).unwrap(),
        NodeId::new(2).unwrap(),
        &old_props,
        &new_props,
        VersionId::new(20).unwrap(),
    )
}

#[test]
fn test_vector_delta_from_diff_allows_exact_max_dimensions() {
    let len = MAX_VECTOR_DIMENSIONS;
    let old = vec![0.0f32; len];
    let mut new = old.clone();
    new[0] = 1.0;

    let delta = VectorDelta::from_diff(&old, &new);
    assert!(delta.is_some(), "Max dimension should be allowed");
}

#[test]
fn test_vector_delta_from_diff_threshold_behavior_for_sparse_vs_full() {
    // Boundary: changes * 2 == dimension should be full storage.
    let old_full = vec![0.0f32; 4];
    let mut new_full = old_full.clone();
    new_full[0] = 1.0;
    new_full[1] = 2.0;
    let delta_full = VectorDelta::from_diff(&old_full, &new_full).unwrap();
    assert!(
        matches!(delta_full, VectorDelta::Full(_)),
        "Threshold boundary should choose full storage"
    );

    // One change in 3 dimensions should be sparse.
    let old_sparse = vec![0.0f32; 3];
    let mut new_sparse = old_sparse.clone();
    new_sparse[0] = 1.0;
    let delta_sparse = VectorDelta::from_diff(&old_sparse, &new_sparse).unwrap();
    assert!(
        matches!(delta_sparse, VectorDelta::Sparse { .. }),
        "Few changes should choose sparse storage"
    );
}

#[test]
fn test_vector_delta_apply_ignores_index_equal_to_length() {
    let base = vec![0.0f32, 1.0, 2.0];
    let delta = VectorDelta::Sparse {
        dimension: 3,
        changes: Arc::new(vec![(3, 99.0)]),
    };

    let result = delta.apply(&base);
    assert_eq!(result, base);
}

#[test]
fn test_vector_delta_sparse_estimated_heap_size_matches_formula() {
    let mut changes = Vec::with_capacity(4);
    changes.push((0u32, 1.0f32));
    changes.push((3u32, 2.0f32));
    let delta = VectorDelta::Sparse {
        dimension: 8,
        changes: Arc::new(changes),
    };

    let expected = match &delta {
        VectorDelta::Sparse { changes, .. } => {
            changes.capacity() * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>())
        }
        _ => unreachable!(),
    };
    assert_eq!(delta.estimated_heap_size(), expected);
}

#[test]
fn test_vector_delta_partial_eq_semantics() {
    let sparse_a = VectorDelta::Sparse {
        dimension: 4,
        changes: Arc::new(vec![(1, 0.5), (3, 1.0)]),
    };
    let sparse_b = VectorDelta::Sparse {
        dimension: 4,
        changes: Arc::new(vec![(1, 0.5), (3, 1.0)]),
    };
    assert_eq!(sparse_a, sparse_b);

    let sparse_dim_mismatch = VectorDelta::Sparse {
        dimension: 5,
        changes: Arc::new(vec![(1, 0.5), (3, 1.0)]),
    };
    assert_ne!(sparse_a, sparse_dim_mismatch);

    let sparse_len_mismatch = VectorDelta::Sparse {
        dimension: 4,
        changes: Arc::new(vec![(1, 0.5)]),
    };
    assert_ne!(sparse_a, sparse_len_mismatch);

    let sparse_idx_mismatch = VectorDelta::Sparse {
        dimension: 4,
        changes: Arc::new(vec![(0, 0.5), (3, 1.0)]),
    };
    assert_ne!(sparse_a, sparse_idx_mismatch);

    let sparse_val_mismatch = VectorDelta::Sparse {
        dimension: 4,
        changes: Arc::new(vec![(1, 0.5 + 1e-3), (3, 1.0)]),
    };
    assert_ne!(sparse_a, sparse_val_mismatch);

    let full_a = VectorDelta::Full(Arc::from(vec![1.0f32, 2.0f32]));
    let full_b = VectorDelta::Full(Arc::from(vec![1.0f32, 2.0f32]));
    assert_eq!(full_a, full_b);

    let full_len_mismatch = VectorDelta::Full(Arc::from(vec![1.0f32]));
    assert_ne!(full_a, full_len_mismatch);

    let full_val_mismatch = VectorDelta::Full(Arc::from(vec![1.0f32, 2.5f32]));
    assert_ne!(full_a, full_val_mismatch);

    assert_ne!(sparse_a, full_a);
}

#[test]
fn test_temporal_version_close_transaction_time_updates_tx_dimension() {
    let mut node = make_node_anchor();
    let end = Timestamp::from(2_000);

    node.close_transaction_time(end).unwrap();

    assert_eq!(node.temporal().transaction_time().end(), end);
    assert_eq!(node.temporal().valid_time().end(), TIMESTAMP_MAX);
}

#[test]
fn test_property_delta_is_empty_only_when_all_collections_empty() {
    let key_a = GLOBAL_INTERNER.intern("a").unwrap();
    let key_v = GLOBAL_INTERNER.intern("v").unwrap();
    let key_r = GLOBAL_INTERNER.intern("r").unwrap();

    let mut changed_only = PropertyDelta::new();
    changed_only.changed.insert(key_a, PropertyValue::Int(1));
    assert!(!changed_only.is_empty());

    let mut vector_only = PropertyDelta::new();
    vector_only.vector_deltas.insert(
        key_v,
        VectorDelta::Sparse {
            dimension: 2,
            changes: Arc::new(vec![(0, 1.0)]),
        },
    );
    assert!(!vector_only.is_empty());

    let mut removed_only = PropertyDelta::new();
    removed_only.removed.insert(key_r);
    assert!(!removed_only.is_empty());

    assert!(PropertyDelta::new().is_empty());
}

#[test]
fn test_property_delta_estimated_heap_size_matches_formula() {
    let mut delta = PropertyDelta::new();
    let key_name = GLOBAL_INTERNER.intern("name").unwrap();
    let key_vec = GLOBAL_INTERNER.intern("embedding").unwrap();
    let key_removed = GLOBAL_INTERNER.intern("old").unwrap();

    delta
        .changed
        .insert(key_name, PropertyValue::string("Alice"));
    delta.vector_deltas.insert(
        key_vec,
        VectorDelta::Sparse {
            dimension: 4,
            changes: Arc::new(vec![(1, 2.0)]),
        },
    );
    delta.removed.insert(key_removed);

    let expected_changed_overhead = delta.changed.capacity()
        * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<PropertyValue>() + 8);
    let expected_changed_values: usize = delta
        .changed
        .values()
        .map(PropertyValue::estimated_heap_size)
        .sum();

    let expected_vector_overhead = delta.vector_deltas.capacity()
        * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<VectorDelta>() + 8);
    let expected_vector_values: usize = delta
        .vector_deltas
        .values()
        .map(VectorDelta::estimated_heap_size)
        .sum();

    let expected_removed_overhead =
        delta.removed.capacity() * (std::mem::size_of::<PropertyKey>() + 8);

    let expected = expected_changed_overhead
        + expected_changed_values
        + expected_vector_overhead
        + expected_vector_values
        + expected_removed_overhead;

    assert_eq!(delta.estimated_heap_size(), expected);
}

#[test]
fn test_node_and_edge_estimated_size_match_formula() {
    let node = make_node_anchor();
    assert_eq!(
        node.estimated_size(),
        std::mem::size_of::<NodeVersion>() + node.data.estimated_heap_size()
    );

    let edge = make_edge_anchor();
    assert_eq!(
        edge.estimated_size(),
        std::mem::size_of::<EdgeVersion>() + edge.data.estimated_heap_size()
    );
}

#[test]
fn test_edge_anchor_reports_not_delta() {
    let edge = make_edge_anchor();
    assert!(!edge.is_delta());
}

#[test]
fn test_entity_version_trait_round_trip_links_for_node_and_edge() {
    fn set_links<V: EntityVersion>(v: &mut V, prev: Option<VersionId>, next: Option<VersionId>) {
        v.set_prev_version(prev);
        v.set_next_version(next);
    }
    fn links<V: EntityVersion>(v: &V) -> (Option<VersionId>, Option<VersionId>) {
        (v.prev_version(), v.next_version())
    }

    let mut node = make_node_anchor();
    let node_prev = Some(VersionId::new(99).unwrap());
    let node_next = Some(VersionId::new(100).unwrap());
    set_links(&mut node, node_prev, node_next);
    assert_eq!(links(&node), (node_prev, node_next));
    set_links(&mut node, None, None);
    assert_eq!(links(&node), (None, None));

    let mut edge = make_edge_anchor();
    let edge_prev = Some(VersionId::new(199).unwrap());
    let edge_next = Some(VersionId::new(200).unwrap());
    set_links(&mut edge, edge_prev, edge_next);
    assert_eq!(links(&edge), (edge_prev, edge_next));
    set_links(&mut edge, None, None);
    assert_eq!(links(&edge), (None, None));
}

#[test]
fn test_entity_version_trait_is_anchor_for_edge_variants() {
    fn trait_is_anchor<V: EntityVersion>(v: &V) -> bool {
        v.is_anchor()
    }

    let edge_anchor = make_edge_anchor();
    let edge_delta = make_edge_delta();

    assert!(trait_is_anchor(&edge_anchor));
    assert!(!trait_is_anchor(&edge_delta));
}

#[test]
fn test_vector_delta_epsilon_boundary() {
    // 🛡️ Sentinel Test: Verify exact epsilon handling in VectorDelta.
    // This targets mutants that replace `<=` with `<` in floats_approx_equal.
    // (a - b).abs() <= VECTOR_EPSILON

    // A small offset to test boundaries around VECTOR_EPSILON.
    const SMALL_OFFSET: f32 = 1e-9;

    // Use 0.0 as base to ensure EPSILON is exactly representable and addition
    // doesn't lose precision (which happens around 1.0 due to f32::EPSILON > VECTOR_EPSILON).
    let v1 = vec![0.0f32];

    // Case 1: Difference exactly equal to EPSILON
    // 0.0 + 1e-7. abs(diff) = 1e-7.
    // Original (<=): Equal -> No Delta (None)
    // Mutant (<): Not Equal -> Delta (Some)
    let v2 = vec![VECTOR_EPSILON];
    assert!(
        VectorDelta::from_diff(&v1, &v2).is_none(),
        "Difference of exactly EPSILON should be considered equal (no delta)"
    );

    // Case 2: Difference slightly larger than EPSILON
    // Should be detected as different
    let v3 = vec![VECTOR_EPSILON + SMALL_OFFSET];
    assert!(
        VectorDelta::from_diff(&v1, &v3).is_some(),
        "Difference > EPSILON should be detected"
    );

    // Case 3: Difference slightly smaller than EPSILON
    // Should be considered equal
    let v4 = vec![VECTOR_EPSILON - SMALL_OFFSET];
    assert!(
        VectorDelta::from_diff(&v1, &v4).is_none(),
        "Difference < EPSILON should be considered equal"
    );
}
