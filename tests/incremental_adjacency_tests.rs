//! Integration tests for Incremental CSR Adjacency Index
//!
//! This test suite validates the incremental CSR implementation using TDD methodology.
//! Tests are organized by phase following the implementation plan.

use aletheiadb::core::id::{EdgeId, NodeId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::index::adjacency::{AdjacencyEntry, AdjacencyIndex};
use aletheiadb::index::incremental_adjacency::{
    CompactionScheduler, IncrementalAdjacencyIndex, IncrementalConfig,
};
use std::sync::Arc;

// ============================================================================
// Phase 1: Core Data Structure Tests
// ============================================================================

#[cfg(test)]
mod phase1_core_structure {
    use super::*;

    // Step 1.1 RED: Test new() creates empty index
    #[test]
    fn test_new_creates_empty_index() {
        let index = IncrementalAdjacencyIndex::new();
        assert_eq!(index.frozen_edge_count(), 0);
        assert_eq!(index.delta_edge_count(), 0);
        assert_eq!(index.tombstone_count(), 0);
    }

    // Step 1.3 RED: Test insert single edge
    #[test]
    fn test_insert_single_edge() {
        let index = IncrementalAdjacencyIndex::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let source = NodeId::new(0).unwrap();
        let target = NodeId::new(1).unwrap();
        let edge_id = EdgeId::new(0).unwrap();

        let entry = AdjacencyEntry::new(target, edge_id, knows);
        index.insert(source, entry);

        assert_eq!(index.delta_edge_count(), 1);
        assert_eq!(index.frozen_edge_count(), 0);
    }

    // Step 1.5 RED: Test insert multiple edges same node
    #[test]
    fn test_insert_multiple_edges_same_node() {
        let index = IncrementalAdjacencyIndex::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let source = NodeId::new(0).unwrap();

        // Insert 3 edges from same source
        for i in 1..=3 {
            let target = NodeId::new(i).unwrap();
            let edge_id = EdgeId::new(i - 1).unwrap();
            let entry = AdjacencyEntry::new(target, edge_id, knows);
            index.insert(source, entry);
        }

        assert_eq!(index.delta_edge_count(), 3);
    }

    // Step 1.7 RED: Test concurrent insert
    #[test]
    fn test_insert_concurrent() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(IncrementalAdjacencyIndex::new());
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let handles: Vec<_> = (0..8)
            .map(|thread_id| {
                let index_clone = Arc::clone(&index);
                thread::spawn(move || {
                    for i in 0..100 {
                        let source = NodeId::new((thread_id * 100 + i) as u64).unwrap();
                        let target = NodeId::new(source.as_u64() + 1).unwrap();
                        let edge_id = EdgeId::new((thread_id * 100 + i) as u64).unwrap();
                        let entry = AdjacencyEntry::new(target, edge_id, knows);
                        index_clone.insert(source, entry);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // Should have 8 threads * 100 edges = 800 edges
        assert_eq!(index.delta_edge_count(), 800);
    }
}

// ============================================================================
// Phase 2: Read Path Tests (MergedAdjacencyGuard)
// ============================================================================

#[cfg(test)]
mod phase2_read_path {
    use super::*;

    // Step 2.1 RED: Test read empty returns empty
    #[test]
    fn test_read_empty_returns_empty() {
        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(0).unwrap();

        let guard = index.get_adjacency(node);
        assert_eq!(guard.iter().count(), 0);
    }

    // Step 2.3 RED: Test read from delta only
    #[test]
    fn test_read_from_delta_only() {
        let index = IncrementalAdjacencyIndex::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let source = NodeId::new(0).unwrap();
        let target = NodeId::new(1).unwrap();
        let edge_id = EdgeId::new(0).unwrap();

        index.insert(source, AdjacencyEntry::new(target, edge_id, knows));

        let guard = index.get_adjacency(source);
        let edges: Vec<_> = guard.iter().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, target);
        assert_eq!(edges[0].edge_id, edge_id);
    }

    // Step 2.5 RED: Test read from frozen only
    #[test]
    fn test_read_from_frozen_only() {
        // Create index with frozen data
        let frozen_edges = vec![(
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            EdgeId::new(0).unwrap(),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        )];
        let frozen = AdjacencyIndex::build(frozen_edges);

        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        let guard = index.get_adjacency(NodeId::new(0).unwrap());
        let edges: Vec<_> = guard.iter().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, NodeId::new(1).unwrap());
        assert_eq!(edges[0].edge_id, EdgeId::new(0).unwrap());
    }

    // Step 2.7 RED: Test read merges frozen and delta
    #[test]
    fn test_read_merges_frozen_and_delta() {
        // Create frozen with 1 edge
        let frozen_edges = vec![(
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            EdgeId::new(0).unwrap(),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        )];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // Add 1 edge to delta
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        index.insert(
            NodeId::new(0).unwrap(),
            AdjacencyEntry::new(NodeId::new(2).unwrap(), EdgeId::new(1).unwrap(), knows),
        );

        // Should see both edges
        let guard = index.get_adjacency(NodeId::new(0).unwrap());
        let edges: Vec<_> = guard.iter().collect();
        assert_eq!(edges.len(), 2);

        // Verify both edges present
        let targets: Vec<_> = edges.iter().map(|e| e.target).collect();
        assert!(targets.contains(&NodeId::new(1).unwrap()));
        assert!(targets.contains(&NodeId::new(2).unwrap()));
    }

    // Step 2.9 RED: Test fast path no delta
    #[test]
    fn test_fast_path_no_delta() {
        // Create frozen only, no delta
        let frozen_edges = vec![(
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            EdgeId::new(0).unwrap(),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        )];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        let guard = index.get_adjacency(NodeId::new(0).unwrap());

        // Fast path: should return slice directly
        assert!(guard.as_slice().is_some());
        let slice = guard.as_slice().unwrap();
        assert_eq!(slice.len(), 1);
        assert_eq!(slice[0].target, NodeId::new(1).unwrap());
    }
}

// ============================================================================
// Phase 3: Tombstone & Delete Tests
// ============================================================================

#[cfg(test)]
mod phase3_tombstones {
    use super::*;

    // Step 3.1 RED: Test delete marks tombstone
    #[test]
    fn test_delete_marks_tombstone() {
        let index = IncrementalAdjacencyIndex::new();
        let edge_id = EdgeId::new(42).unwrap();

        index.delete(edge_id);

        assert_eq!(index.tombstone_count(), 1);
    }

    // Step 3.3 RED: Test read filters tombstones from frozen
    #[test]
    fn test_read_filters_tombstones_from_frozen() {
        // Create frozen with 2 edges
        let frozen_edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            ),
        ];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // Delete one edge
        index.delete(EdgeId::new(0).unwrap());

        // Should only see 1 edge
        let guard = index.get_adjacency(NodeId::new(0).unwrap());
        let edges: Vec<_> = guard.iter().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].edge_id, EdgeId::new(1).unwrap());
    }

    // Step 3.5 RED: Test read filters tombstones from delta
    #[test]
    fn test_read_filters_tombstones_from_delta() {
        let index = IncrementalAdjacencyIndex::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let source = NodeId::new(0).unwrap();

        // Insert 2 edges into delta
        index.insert(
            source,
            AdjacencyEntry::new(NodeId::new(1).unwrap(), EdgeId::new(0).unwrap(), knows),
        );
        index.insert(
            source,
            AdjacencyEntry::new(NodeId::new(2).unwrap(), EdgeId::new(1).unwrap(), knows),
        );

        // Delete one
        index.delete(EdgeId::new(0).unwrap());

        // Should only see 1 edge
        let guard = index.get_adjacency(source);
        let edges: Vec<_> = guard.iter().collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].edge_id, EdgeId::new(1).unwrap());
    }

    // Step 3.7 RED: Test tombstone tracks metadata
    #[test]
    fn test_tombstone_tracks_metadata() {
        let index = IncrementalAdjacencyIndex::new();
        let edge_id = EdgeId::new(42).unwrap();

        let before_delete = chrono::Utc::now();
        index.delete(edge_id);
        let after_delete = chrono::Utc::now();

        let tombstone = index.get_tombstone(edge_id).unwrap();
        assert_eq!(tombstone.edge_id, edge_id);
        assert!(tombstone.deleted_at >= before_delete);
        assert!(tombstone.deleted_at <= after_delete);
        assert!(tombstone.transaction_time >= before_delete);
        assert!(tombstone.transaction_time <= after_delete);
    }
}

// ============================================================================
// Phase 4: Compaction Tests
// ============================================================================

#[cfg(test)]
mod phase4_compaction {
    use super::*;

    // Step 4.1 RED: Test should_compact ratio threshold
    #[test]
    fn test_should_compact_ratio_threshold() {
        let config = IncrementalConfig {
            compaction_ratio: 0.1,    // 10% threshold
            max_delta_edges: 100_000, // High so ratio triggers first
            ..Default::default()
        };

        // Create frozen with 100 edges
        let frozen_edges: Vec<_> = (0..100)
            .map(|i| {
                (
                    NodeId::new(i).unwrap(),
                    NodeId::new(i + 1).unwrap(),
                    EdgeId::new(i).unwrap(),
                    GLOBAL_INTERNER.intern("KNOWS").unwrap(),
                )
            })
            .collect();
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::with_config(Arc::new(frozen), config);

        // Add 9 edges to delta (9% of frozen)
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        for i in 100..109 {
            index.insert(
                NodeId::new(i).unwrap(),
                AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
            );
        }
        assert!(!index.should_compact()); // Below threshold

        // Add 1 more edge (10% threshold hit)
        index.insert(
            NodeId::new(109).unwrap(),
            AdjacencyEntry::new(NodeId::new(110).unwrap(), EdgeId::new(109).unwrap(), knows),
        );
        assert!(index.should_compact()); // Should trigger
    }

    // Step 4.3 RED: Test compact merges delta into frozen
    #[test]
    fn test_compact_merges_delta_into_frozen() {
        // Create frozen with 2 edges
        let frozen_edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            ),
            (
                NodeId::new(1).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            ),
        ];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // Add 1 edge to delta
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        index.insert(
            NodeId::new(2).unwrap(),
            AdjacencyEntry::new(NodeId::new(3).unwrap(), EdgeId::new(2).unwrap(), knows),
        );

        // Before compaction
        assert_eq!(index.frozen_edge_count(), 2);
        assert_eq!(index.delta_edge_count(), 1);

        // Compact
        index.compact();

        // After compaction
        assert_eq!(index.frozen_edge_count(), 3);
        assert_eq!(index.delta_edge_count(), 0);
    }

    // Step 4.5 RED: Test compact removes tombstoned edges
    #[test]
    fn test_compact_removes_tombstoned_edges() {
        // Create frozen with 3 edges
        let frozen_edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(3).unwrap(),
                EdgeId::new(2).unwrap(),
                GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            ),
        ];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // Delete 1 edge
        index.delete(EdgeId::new(1).unwrap());

        // Compact
        index.compact();

        // Should have 2 edges after compaction (tombstoned edge removed)
        assert_eq!(index.frozen_edge_count(), 2);
        assert_eq!(index.tombstone_count(), 0); // Tombstones cleared
    }

    // Step 4.7 RED: Test compact clears delta and tombstones
    #[test]
    fn test_compact_clears_delta_and_tombstones() {
        let index = IncrementalAdjacencyIndex::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Add edges to delta
        index.insert(
            NodeId::new(0).unwrap(),
            AdjacencyEntry::new(NodeId::new(1).unwrap(), EdgeId::new(0).unwrap(), knows),
        );
        index.insert(
            NodeId::new(0).unwrap(),
            AdjacencyEntry::new(NodeId::new(2).unwrap(), EdgeId::new(1).unwrap(), knows),
        );

        // Add tombstone
        index.delete(EdgeId::new(0).unwrap());

        assert_eq!(index.delta_edge_count(), 2);
        assert_eq!(index.tombstone_count(), 1);

        // Compact
        index.compact();

        // Transient state cleared
        assert_eq!(index.delta_edge_count(), 0);
        assert_eq!(index.tombstone_count(), 0);
    }

    // Step 4.9 GREEN: Test compact atomic swap
    #[test]
    fn test_compact_atomic_swap() {
        // This test verifies readers see consistent state during compaction
        use std::sync::Arc;
        use std::thread;

        let frozen_edges: Vec<_> = (0..100)
            .map(|i| {
                (
                    NodeId::new(i).unwrap(),
                    NodeId::new(i + 1).unwrap(),
                    EdgeId::new(i).unwrap(),
                    GLOBAL_INTERNER.intern("KNOWS").unwrap(),
                )
            })
            .collect();
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = Arc::new(IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen)));

        // Add delta
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        for i in 100..110 {
            index.insert(
                NodeId::new(i).unwrap(),
                AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
            );
        }

        // Spawn reader thread
        let index_clone = Arc::clone(&index);
        let reader = thread::spawn(move || {
            for _ in 0..1000 {
                let guard = index_clone.get_adjacency(NodeId::new(0).unwrap());
                let count = guard.iter().count();
                // Should always see valid state (never partial)
                // Either 1 edge (before compaction) or 1 edge (after compaction, since node 0 has 1 edge)
                assert_eq!(count, 1);
            }
        });

        // Compact in main thread
        index.compact();

        reader.join().unwrap();
    }

    // Step 4.11 GREEN: Test concurrent read during compaction
    #[test]
    fn test_concurrent_read_during_compaction() {
        // Verifies lock-free reads during compaction
        use std::sync::Arc;
        use std::thread;

        let frozen_edges: Vec<_> = (0..1000)
            .map(|i| {
                (
                    NodeId::new(i).unwrap(),
                    NodeId::new(i + 1).unwrap(),
                    EdgeId::new(i).unwrap(),
                    GLOBAL_INTERNER.intern("KNOWS").unwrap(),
                )
            })
            .collect();
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = Arc::new(IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen)));

        // Spawn multiple reader threads
        let readers: Vec<_> = (0..8)
            .map(|_| {
                let index_clone = Arc::clone(&index);
                thread::spawn(move || {
                    for i in 0..1000 {
                        let guard = index_clone.get_adjacency(NodeId::new(i % 1000).unwrap());
                        assert!(guard.iter().count() > 0);
                    }
                })
            })
            .collect();

        // Compact while readers are running
        index.compact();

        for reader in readers {
            reader.join().unwrap();
        }
    }

    // Test deletion followed by compaction correctly removes edges
    #[test]
    fn test_delete_then_compact() {
        // Create frozen with 3 edges from node 0
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let frozen_edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(3).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
        ];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // Delete middle edge
        index.delete(EdgeId::new(1).unwrap());

        // Verify before compaction
        let guard = index.get_adjacency(NodeId::new(0).unwrap());
        let edges: Vec<_> = guard.iter().map(|e| e.edge_id).collect();
        assert_eq!(edges.len(), 2); // Edge 1 filtered out
        drop(guard);

        // Compact
        index.compact();

        // Verify after compaction
        assert_eq!(index.frozen_edge_count(), 2);
        assert_eq!(index.tombstone_count(), 0);

        let guard = index.get_adjacency(NodeId::new(0).unwrap());
        let edges: Vec<_> = guard.iter().map(|e| e.edge_id).collect();
        assert_eq!(edges.len(), 2);
        assert!(edges.contains(&EdgeId::new(0).unwrap()));
        assert!(edges.contains(&EdgeId::new(2).unwrap()));
        assert!(!edges.contains(&EdgeId::new(1).unwrap())); // Deleted
    }

    // Test concurrent delete operations
    #[test]
    fn test_concurrent_delete() {
        use std::sync::Arc;
        use std::thread;

        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create index with edges 0-99
        let frozen_edges: Vec<_> = (0..100)
            .map(|i| {
                (
                    NodeId::new(i).unwrap(),
                    NodeId::new(i + 1).unwrap(),
                    EdgeId::new(i).unwrap(),
                    knows,
                )
            })
            .collect();
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = Arc::new(IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen)));

        // Spawn multiple threads to delete different edges concurrently
        let handles: Vec<_> = (0..4)
            .map(|thread_id| {
                let index_clone = Arc::clone(&index);
                thread::spawn(move || {
                    for i in 0..25 {
                        let edge_id = (thread_id * 25 + i) as u64;
                        index_clone.delete(EdgeId::new(edge_id).unwrap());
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // All 100 edges should be tombstoned
        assert_eq!(index.tombstone_count(), 100);

        // Compact should result in 0 edges
        index.compact();
        assert_eq!(index.frozen_edge_count(), 0);
    }

    // Test delete and reinsert same edge ID
    // Tombstones apply to edge IDs - reinserting with same ID is also filtered
    #[test]
    fn test_delete_reinsert_same_id() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create frozen with edge 0
        let frozen_edges = vec![(
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            EdgeId::new(0).unwrap(),
            knows,
        )];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // Delete edge 0
        index.delete(EdgeId::new(0).unwrap());

        // Verify edge is filtered
        {
            let guard = index.get_adjacency(NodeId::new(0).unwrap());
            assert_eq!(guard.iter().count(), 0);
        }

        // Reinsert with same edge ID but different target
        index.insert(
            NodeId::new(0).unwrap(),
            AdjacencyEntry::new(NodeId::new(99).unwrap(), EdgeId::new(0).unwrap(), knows),
        );

        // Edge should still be filtered (tombstone takes precedence until compaction)
        // Tombstones apply to edge IDs, so the reinserted edge is also filtered
        {
            let guard = index.get_adjacency(NodeId::new(0).unwrap());
            assert_eq!(guard.iter().count(), 0);
        }

        // After compaction, both old and new edges with that ID are gone
        // because tombstone was in effect during edge collection
        index.compact();

        {
            let guard = index.get_adjacency(NodeId::new(0).unwrap());
            assert_eq!(guard.iter().count(), 0);
        }

        // To properly reinsert after delete+compact, use a NEW edge ID
        index.insert(
            NodeId::new(0).unwrap(),
            AdjacencyEntry::new(NodeId::new(99).unwrap(), EdgeId::new(100).unwrap(), knows),
        );

        {
            let guard = index.get_adjacency(NodeId::new(0).unwrap());
            let edges: Vec<_> = guard.iter().collect();
            assert_eq!(edges.len(), 1);
            assert_eq!(edges[0].edge_id, EdgeId::new(100).unwrap());
            assert_eq!(edges[0].target, NodeId::new(99).unwrap());
        }
    }

    // Test compaction with overlapping frozen and delta edges for same node
    #[test]
    fn test_compact_node_with_frozen_and_delta() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create frozen with edges from node 0: 0→1
        let frozen_edges = vec![(
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            EdgeId::new(0).unwrap(),
            knows,
        )];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // Add delta edge from same node 0: 0→2
        index.insert(
            NodeId::new(0).unwrap(),
            AdjacencyEntry::new(NodeId::new(2).unwrap(), EdgeId::new(1).unwrap(), knows),
        );

        // Before compaction, should see both edges
        {
            let guard = index.get_adjacency(NodeId::new(0).unwrap());
            let edges: Vec<_> = guard.iter().map(|e| e.edge_id).collect();
            assert_eq!(edges.len(), 2);
        }

        // Compact
        index.compact();

        // After compaction, should still see exactly 2 edges (no duplicates)
        assert_eq!(index.frozen_edge_count(), 2);
        assert_eq!(index.delta_edge_count(), 0);

        {
            let guard = index.get_adjacency(NodeId::new(0).unwrap());
            let edges: Vec<_> = guard.iter().map(|e| e.edge_id).collect();
            assert_eq!(edges.len(), 2);
            assert!(edges.contains(&EdgeId::new(0).unwrap()));
            assert!(edges.contains(&EdgeId::new(1).unwrap()));
        }
    }

    // Test compaction with concurrent inserts (data loss prevention)
    #[test]
    fn test_compact_concurrent_insert_no_data_loss() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(IncrementalAdjacencyIndex::new());
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Insert initial edges
        for i in 0..1000 {
            index.insert(
                NodeId::new(0).unwrap(),
                AdjacencyEntry::new(NodeId::new(i).unwrap(), EdgeId::new(i).unwrap(), knows),
            );
        }

        // Spawn concurrent writer during compaction
        let index_clone = Arc::clone(&index);
        let handle = thread::spawn(move || {
            for i in 1000..2000 {
                index_clone.insert(
                    NodeId::new(0).unwrap(),
                    AdjacencyEntry::new(NodeId::new(i).unwrap(), EdgeId::new(i).unwrap(), knows),
                );
            }
        });

        // Run compaction while writer is running
        index.compact();

        handle.join().unwrap();

        // Verify NO edges lost (either in frozen or delta)
        let total = index.frozen_edge_count() + index.delta_edge_count();
        assert_eq!(total, 2000);
    }
}

// ============================================================================
// Phase 5: Background Compaction Thread Tests
// ============================================================================

#[cfg(test)]
#[allow(unused_imports)] // Used by phase 5 tests when uncommented
mod phase5_background_compaction {
    use super::*;

    // Step 5.1 GREEN: Test background compaction starts
    #[test]
    fn test_background_compaction_starts() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let index = Arc::new(IncrementalAdjacencyIndex::new());
        let scheduler = CompactionScheduler::new(Arc::clone(&index));

        let handle = scheduler.start();

        // Verify thread is running
        thread::sleep(Duration::from_millis(100));
        assert!(!handle.is_finished());

        // Shutdown
        scheduler.shutdown();
        handle.join().unwrap();
    }

    // Step 5.3 GREEN: Test background compaction triggers on threshold
    #[test]
    fn test_background_compaction_triggers_on_threshold() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let config = IncrementalConfig {
            max_delta_edges: 10, // Low threshold for testing
            check_interval: Duration::from_millis(50),
            ..Default::default()
        };

        let index = Arc::new(IncrementalAdjacencyIndex::with_config(
            Arc::new(AdjacencyIndex::new()),
            config,
        ));
        let scheduler = CompactionScheduler::new(Arc::clone(&index));
        let handle = scheduler.start();

        // Add edges to trigger threshold
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        for i in 0..15 {
            index.insert(
                NodeId::new(i).unwrap(),
                AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
            );
        }

        // Wait for background compaction (poll)
        // We wait for frozen_edge_count to reach 15, as delta might be drained
        // slightly before frozen is updated (race condition).
        let mut attempts = 0;
        while index.frozen_edge_count() < 15 && attempts < 200 {
            thread::sleep(Duration::from_millis(50));
            attempts += 1;
        }

        // Delta should be cleared by background compaction
        assert_eq!(index.delta_edge_count(), 0);
        assert_eq!(index.frozen_edge_count(), 15);

        scheduler.shutdown();
        handle.join().unwrap();
    }

    // Step 5.5 GREEN: Test pause/resume
    #[test]
    fn test_background_compaction_pause_resume() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let config = IncrementalConfig {
            max_delta_edges: 10,
            check_interval: Duration::from_millis(50),
            ..Default::default()
        };

        let index = Arc::new(IncrementalAdjacencyIndex::with_config(
            Arc::new(AdjacencyIndex::new()),
            config,
        ));
        let scheduler = CompactionScheduler::new(Arc::clone(&index));
        let handle = scheduler.start();

        // Pause compaction
        scheduler.pause();

        // Add edges beyond threshold
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        for i in 0..15 {
            index.insert(
                NodeId::new(i).unwrap(),
                AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
            );
        }

        thread::sleep(Duration::from_millis(200));

        // Should NOT compact while paused
        assert_eq!(index.delta_edge_count(), 15);

        // Resume
        scheduler.resume();

        // Wait for compaction (poll)
        // Wait for frozen count to increase (should be 15)
        let mut attempts = 0;
        while index.frozen_edge_count() < 15 && attempts < 200 {
            thread::sleep(Duration::from_millis(50));
            attempts += 1;
        }

        // Should now compact
        assert_eq!(index.delta_edge_count(), 0);
        assert_eq!(index.frozen_edge_count(), 15);

        scheduler.shutdown();
        handle.join().unwrap();
    }

    // Step 5.7 GREEN: Test graceful shutdown
    #[test]
    fn test_graceful_shutdown() {
        use std::sync::Arc;
        use std::time::Duration;

        let config = IncrementalConfig {
            max_delta_edges: 10,
            check_interval: Duration::from_millis(50),
            ..Default::default()
        };

        let index = Arc::new(IncrementalAdjacencyIndex::with_config(
            Arc::new(AdjacencyIndex::new()),
            config,
        ));
        let scheduler = CompactionScheduler::new(Arc::clone(&index));
        let handle = scheduler.start();

        // Add edges
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        for i in 0..15 {
            index.insert(
                NodeId::new(i).unwrap(),
                AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
            );
        }

        // Give it a moment to start compacting
        std::thread::sleep(Duration::from_millis(100));

        // Shutdown should complete in-flight compaction
        scheduler.shutdown();
        handle.join().unwrap();

        // Final compaction should have run
        assert_eq!(index.delta_edge_count(), 0);
    }

    // Step 5.9 GREEN: Test compaction thread panic recovery
    #[test]
    fn test_compaction_thread_panic_recovery() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let config = IncrementalConfig {
            max_delta_edges: 5, // Low threshold for quick testing
            check_interval: Duration::from_millis(50),
            ..Default::default()
        };

        let index = Arc::new(IncrementalAdjacencyIndex::with_config(
            Arc::new(AdjacencyIndex::new()),
            config,
        ));

        // Inject panic for first compaction
        index.test_inject_panic_on_compact();

        let scheduler = CompactionScheduler::new(Arc::clone(&index));
        let handle = scheduler.start();

        // Add edges to trigger compaction (should panic on first attempt)
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        for i in 0..7 {
            index.insert(
                NodeId::new(i).unwrap(),
                AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
            );
        }

        // Wait for panic to occur (poll instead of fixed sleep for CI reliability)
        let mut attempts = 0;
        while scheduler.panic_count() == 0 && attempts < 200 {
            thread::sleep(Duration::from_millis(50));
            attempts += 1;
        }

        // Verify panic was caught and counted
        assert_eq!(
            scheduler.panic_count(),
            1,
            "Expected panic to be caught after {} attempts",
            attempts
        );

        // Thread should still be running (panic recovered)
        assert!(!handle.is_finished());

        // Add more edges to trigger another compaction (should succeed this time)
        for i in 7..12 {
            index.insert(
                NodeId::new(i).unwrap(),
                AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
            );
        }

        // Wait for successful compaction (poll)
        // Wait for frozen count to reach 12 (7 from first batch + 5 from second)
        attempts = 0;
        while index.frozen_edge_count() < 12 && attempts < 200 {
            thread::sleep(Duration::from_millis(50));
            attempts += 1;
        }

        // Verify normal compaction worked after panic
        assert_eq!(index.delta_edge_count(), 0);
        assert_eq!(index.frozen_edge_count(), 12);

        // Panic count should still be 1 (no new panics)
        assert_eq!(scheduler.panic_count(), 1);

        scheduler.shutdown();
        handle.join().unwrap();
    }
}

// ============================================================================
// Phase 6: CurrentIndexes Integration Tests
// ============================================================================

#[cfg(test)]
mod phase6_current_indexes_integration {
    use super::*;
    use aletheiadb::PropertyMapBuilder;
    use aletheiadb::core::graph::Edge;
    use aletheiadb::core::id::VersionId;
    use aletheiadb::index::current::CurrentIndexes;

    // Step 6.1: Test insert_edge updates adjacency incrementally (no rebuild)
    #[test]
    fn test_incremental_edge_insertion() {
        let indexes = CurrentIndexes::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Insert first edge
        let edge1 = Edge::new(
            EdgeId::new(1).unwrap(),
            knows,
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );
        indexes.insert_edge(edge1);

        // Get adjacency - should NOT trigger rebuild, should use incremental
        {
            let guard = indexes.get_outgoing(NodeId::new(0).unwrap());
            assert_eq!(guard.len(), 1);
            assert_eq!(guard[0].target, NodeId::new(1).unwrap());
        } // Guard dropped here

        // Insert second edge
        let edge2 = Edge::new(
            EdgeId::new(2).unwrap(),
            knows,
            NodeId::new(0).unwrap(),
            NodeId::new(2).unwrap(),
            PropertyMapBuilder::new().build(),
            VersionId::new(1).unwrap(),
        );
        indexes.insert_edge(edge2);

        // Get adjacency again - should see both edges WITHOUT rebuild
        {
            let guard = indexes.get_outgoing(NodeId::new(0).unwrap());
            assert_eq!(guard.len(), 2);
        } // Guard dropped here
    }

    // Step 6.3 RED: Test get_outgoing returns merged frozen + delta
    #[test]
    fn test_get_outgoing_merges_frozen_and_delta() {
        let indexes = CurrentIndexes::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Add 100 edges to build up frozen
        for i in 0..100 {
            let edge = Edge::new(
                EdgeId::new(i).unwrap(),
                knows,
                NodeId::new(0).unwrap(),
                NodeId::new(i + 1).unwrap(),
                PropertyMapBuilder::new().build(),
                VersionId::new(1).unwrap(),
            );
            indexes.insert_edge(edge);
        }

        // Trigger compaction manually (or wait for background)
        // This moves edges to frozen
        indexes.compact_adjacency(); // Will need to add this method

        // Add 10 more edges (these go to delta)
        for i in 100..110 {
            let edge = Edge::new(
                EdgeId::new(i).unwrap(),
                knows,
                NodeId::new(0).unwrap(),
                NodeId::new(i + 1).unwrap(),
                PropertyMapBuilder::new().build(),
                VersionId::new(1).unwrap(),
            );
            indexes.insert_edge(edge);
        }

        // Get adjacency - should see all 110 edges (frozen + delta)
        let guard = indexes.get_outgoing(NodeId::new(0).unwrap());
        assert_eq!(guard.len(), 110);
    }

    // Step 6.5 RED: Test concurrent insert + read works correctly
    #[test]
    fn test_concurrent_insert_and_read() {
        use std::sync::Arc;
        use std::thread;

        let indexes = Arc::new(CurrentIndexes::new());
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Spawn writer thread
        let indexes_writer = Arc::clone(&indexes);
        let writer = thread::spawn(move || {
            for i in 0..1000 {
                let edge = Edge::new(
                    EdgeId::new(i).unwrap(),
                    knows,
                    NodeId::new(0).unwrap(),
                    NodeId::new(i + 1).unwrap(),
                    PropertyMapBuilder::new().build(),
                    VersionId::new(1).unwrap(),
                );
                indexes_writer.insert_edge(edge);
            }
        });

        // Spawn reader threads
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let indexes_reader = Arc::clone(&indexes);
                thread::spawn(move || {
                    for _ in 0..1000 {
                        let guard = indexes_reader.get_outgoing(NodeId::new(0).unwrap());
                        // Should always see valid state (never partial)
                        let _count = guard.len();
                    }
                })
            })
            .collect();

        writer.join().unwrap();
        for reader in readers {
            reader.join().unwrap();
        }

        // Final check - all edges present
        let guard = indexes.get_outgoing(NodeId::new(0).unwrap());
        assert_eq!(guard.len(), 1000);
    }

    // Step 6.7 RED: Test background compaction works with CurrentIndexes
    #[test]
    fn test_background_compaction_with_current_indexes() {
        use std::thread;
        use std::time::Duration;

        let mut indexes = CurrentIndexes::new_with_background_compaction();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Add enough edges to trigger background compaction (default threshold is 10,000)
        for i in 0..10_500 {
            let edge = Edge::new(
                EdgeId::new(i).unwrap(),
                knows,
                NodeId::new(0).unwrap(),
                NodeId::new(i + 1).unwrap(),
                PropertyMapBuilder::new().build(),
                VersionId::new(1).unwrap(),
            );
            indexes.insert_edge(edge);
        }

        // Wait for background compaction to trigger (check interval is 1 second)
        thread::sleep(Duration::from_millis(1500));

        // Check that edges are in frozen (compaction happened)
        // Should have moved edges from delta to frozen
        assert!(indexes.frozen_edge_count() > 0);

        // Cleanup
        indexes
            .shutdown_background_compaction()
            .expect("shutdown should succeed");
    }

    // Step 6.9 RED: Test remove_edge with tombstones
    #[test]
    fn test_remove_edge_with_tombstones() {
        let indexes = CurrentIndexes::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Insert edges
        for i in 0..10 {
            let edge = Edge::new(
                EdgeId::new(i).unwrap(),
                knows,
                NodeId::new(0).unwrap(),
                NodeId::new(i + 1).unwrap(),
                PropertyMapBuilder::new().build(),
                VersionId::new(1).unwrap(),
            );
            indexes.insert_edge(edge);
        }

        // Remove edge 5
        indexes.remove_edge(EdgeId::new(5).unwrap());

        // Get adjacency - should see 9 edges (tombstone filters out edge 5)
        let guard = indexes.get_outgoing(NodeId::new(0).unwrap());
        assert_eq!(guard.len(), 9);

        // Verify edge 5 is not in the list
        for entry in guard.iter() {
            assert_ne!(entry.edge_id, EdgeId::new(5).unwrap());
        }
    }
}

// ============================================================================
// Phase 7: Persistence Integration Tests
// ============================================================================

#[cfg(test)]
mod phase7_persistence_integration {
    use super::*;
    use aletheiadb::PropertyMapBuilder;
    use aletheiadb::core::graph::Edge;
    use aletheiadb::core::id::VersionId;
    use aletheiadb::index::current::CurrentIndexes;

    // Step 7.1: Test delta reconstruction after import
    #[test]
    fn test_delta_reconstruction_after_import() {
        let indexes = CurrentIndexes::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Insert 10 edges
        for i in 0..10 {
            let edge = Edge::new(
                EdgeId::new(i).unwrap(),
                knows,
                NodeId::new(0).unwrap(),
                NodeId::new(i + 1).unwrap(),
                PropertyMapBuilder::new().build(),
                VersionId::new(1).unwrap(),
            );
            indexes.insert_edge(edge);
        }

        // Compact to move edges to frozen
        indexes.compact_adjacency();

        // Add 3 more edges (these go to delta)
        for i in 10..13 {
            let edge = Edge::new(
                EdgeId::new(i).unwrap(),
                knows,
                NodeId::new(0).unwrap(),
                NodeId::new(i + 1).unwrap(),
                PropertyMapBuilder::new().build(),
                VersionId::new(1).unwrap(),
            );
            indexes.insert_edge(edge);
        }

        // Export CSR (only exports frozen, not delta)
        let (out_nodes, out_offsets, out_edges) = indexes.export_outgoing_csr();
        let (in_nodes, in_offsets, in_edges) = indexes.export_incoming_csr();

        // Create new indexes and import
        let new_indexes = CurrentIndexes::new();

        // Copy edges to new indexes (simulating persistence loading)
        // In real persistence, edges would be loaded from disk
        for i in 0..13 {
            let edge_id = EdgeId::new(i).unwrap();
            if let Some(edge) = indexes.get_edge(edge_id) {
                new_indexes.insert_edge(edge);
            }
        }

        // Import CSR - should reconstruct delta from diff
        new_indexes.import_csr(
            out_nodes,
            out_offsets,
            out_edges,
            in_nodes,
            in_offsets,
            in_edges,
        );

        // Verify all 13 edges are visible (10 in frozen + 3 in delta)
        {
            let guard = new_indexes.get_outgoing(NodeId::new(0).unwrap());
            assert_eq!(
                guard.len(),
                13,
                "Should see all 13 edges after delta reconstruction"
            );
        }

        // Verify delta was reconstructed (3 edges)
        assert_eq!(
            new_indexes.delta_edge_count(),
            3,
            "Delta should have 3 edges"
        );
        assert_eq!(
            new_indexes.frozen_edge_count(),
            10,
            "Frozen should have 10 edges"
        );
    }

    // Step 7.3: Test empty delta reconstruction
    #[test]
    fn test_empty_delta_reconstruction() {
        let indexes = CurrentIndexes::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Insert 10 edges and compact (all go to frozen)
        for i in 0..10 {
            let edge = Edge::new(
                EdgeId::new(i).unwrap(),
                knows,
                NodeId::new(0).unwrap(),
                NodeId::new(i + 1).unwrap(),
                PropertyMapBuilder::new().build(),
                VersionId::new(1).unwrap(),
            );
            indexes.insert_edge(edge);
        }
        indexes.compact_adjacency();

        // Export and import
        let (out_nodes, out_offsets, out_edges) = indexes.export_outgoing_csr();
        let (in_nodes, in_offsets, in_edges) = indexes.export_incoming_csr();

        let new_indexes = CurrentIndexes::new();
        // Copy all edges using public API (simulating persistence loading)
        let max_edge_id = 100; // Large enough to cover all tests
        for i in 0..max_edge_id {
            let edge_id = EdgeId::new(i).unwrap();
            if let Some(edge) = indexes.get_edge(edge_id) {
                new_indexes.insert_edge(edge);
            }
        }

        new_indexes.import_csr(
            out_nodes,
            out_offsets,
            out_edges,
            in_nodes,
            in_offsets,
            in_edges,
        );

        // Verify delta is empty
        assert_eq!(new_indexes.delta_edge_count(), 0, "Delta should be empty");
        assert_eq!(
            new_indexes.frozen_edge_count(),
            10,
            "Frozen should have 10 edges"
        );

        // Verify all edges are visible
        {
            let guard = new_indexes.get_outgoing(NodeId::new(0).unwrap());
            assert_eq!(guard.len(), 10, "Should see all 10 edges");
        }
    }

    // Step 7.5: Test delta reconstruction with multiple nodes
    #[test]
    fn test_delta_reconstruction_multiple_nodes() {
        let indexes = CurrentIndexes::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Insert edges for nodes 0, 1, 2
        for source in 0..3 {
            for target in 0..5 {
                let edge_id = source * 5 + target;
                let edge = Edge::new(
                    EdgeId::new(edge_id).unwrap(),
                    knows,
                    NodeId::new(source).unwrap(),
                    NodeId::new(target + 10).unwrap(),
                    PropertyMapBuilder::new().build(),
                    VersionId::new(1).unwrap(),
                );
                indexes.insert_edge(edge);
            }
        }

        // Compact to freeze first 15 edges
        indexes.compact_adjacency();

        // Add more edges for each node (these go to delta)
        for source in 0..3 {
            for target in 5..7 {
                let edge_id = 15 + source * 2 + (target - 5);
                let edge = Edge::new(
                    EdgeId::new(edge_id).unwrap(),
                    knows,
                    NodeId::new(source).unwrap(),
                    NodeId::new(target + 10).unwrap(),
                    PropertyMapBuilder::new().build(),
                    VersionId::new(1).unwrap(),
                );
                indexes.insert_edge(edge);
            }
        }

        // Export and import
        let (out_nodes, out_offsets, out_edges) = indexes.export_outgoing_csr();
        let (in_nodes, in_offsets, in_edges) = indexes.export_incoming_csr();

        let new_indexes = CurrentIndexes::new();
        // Copy all edges using public API (simulating persistence loading)
        let max_edge_id = 100; // Large enough to cover all tests
        for i in 0..max_edge_id {
            let edge_id = EdgeId::new(i).unwrap();
            if let Some(edge) = indexes.get_edge(edge_id) {
                new_indexes.insert_edge(edge);
            }
        }

        new_indexes.import_csr(
            out_nodes,
            out_offsets,
            out_edges,
            in_nodes,
            in_offsets,
            in_edges,
        );

        // Verify each node has correct edge count
        for source in 0..3 {
            let guard = new_indexes.get_outgoing(NodeId::new(source).unwrap());
            assert_eq!(
                guard.len(),
                7,
                "Each node should have 7 edges (5 frozen + 2 delta)"
            );
        }

        // Verify delta was reconstructed correctly
        assert_eq!(
            new_indexes.delta_edge_count(),
            6,
            "Delta should have 6 edges (3 nodes × 2 edges)"
        );
        assert_eq!(
            new_indexes.frozen_edge_count(),
            15,
            "Frozen should have 15 edges"
        );
    }

    // Step 7.7: Test no delta when all edges in frozen
    #[test]
    fn test_no_delta_when_all_edges_frozen() {
        let indexes = CurrentIndexes::new();
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Insert and compact (all edges frozen, no delta)
        for i in 0..20 {
            let edge = Edge::new(
                EdgeId::new(i).unwrap(),
                knows,
                NodeId::new(i / 5).unwrap(),
                NodeId::new(i + 10).unwrap(),
                PropertyMapBuilder::new().build(),
                VersionId::new(1).unwrap(),
            );
            indexes.insert_edge(edge);
        }
        indexes.compact_adjacency();

        // Export and import
        let (out_nodes, out_offsets, out_edges) = indexes.export_outgoing_csr();
        let (in_nodes, in_offsets, in_edges) = indexes.export_incoming_csr();

        let new_indexes = CurrentIndexes::new();
        // Copy all edges using public API (simulating persistence loading)
        let max_edge_id = 100; // Large enough to cover all tests
        for i in 0..max_edge_id {
            let edge_id = EdgeId::new(i).unwrap();
            if let Some(edge) = indexes.get_edge(edge_id) {
                new_indexes.insert_edge(edge);
            }
        }

        new_indexes.import_csr(
            out_nodes,
            out_offsets,
            out_edges,
            in_nodes,
            in_offsets,
            in_edges,
        );

        // Delta should be empty
        assert_eq!(
            new_indexes.delta_edge_count(),
            0,
            "Delta should be empty when all edges are in frozen"
        );
        assert_eq!(
            new_indexes.frozen_edge_count(),
            20,
            "Frozen should have all 20 edges"
        );
    }
}

// ============================================================================
// Phase 9: FrozenAdjacencyView for Read Transactions (Hot Path)
// ============================================================================

#[cfg(test)]
mod phase9_frozen_view {
    use super::*;

    // Step 9.1 RED: Test FrozenAdjacencyView creation when delta/tombstones empty
    #[test]
    fn test_frozen_view_available_when_clean() {
        // Create frozen with edges
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let frozen_edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
        ];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // No delta, no tombstones - should get view
        let view = index.frozen_view();
        assert!(view.is_some(), "Should get frozen view when clean");

        // View should provide direct slice access
        let view = view.unwrap();
        let slice = view.get_adjacency(NodeId::new(0).unwrap());
        assert_eq!(slice.len(), 2);
    }

    // Step 9.2 RED: Test FrozenAdjacencyView unavailable when delta exists
    #[test]
    fn test_frozen_view_unavailable_with_delta() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let frozen_edges = vec![(
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            EdgeId::new(0).unwrap(),
            knows,
        )];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // Add edge to delta
        index.insert(
            NodeId::new(1).unwrap(),
            AdjacencyEntry::new(NodeId::new(2).unwrap(), EdgeId::new(1).unwrap(), knows),
        );

        // Should NOT get view when delta has edges
        let view = index.frozen_view();
        assert!(view.is_none(), "Should NOT get frozen view with delta");
    }

    // Step 9.3 RED: Test FrozenAdjacencyView unavailable when tombstones exist
    #[test]
    fn test_frozen_view_unavailable_with_tombstones() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let frozen_edges = vec![(
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            EdgeId::new(0).unwrap(),
            knows,
        )];
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        // Add tombstone
        index.delete(EdgeId::new(0).unwrap());

        // Should NOT get view when tombstones exist
        let view = index.frozen_view();
        assert!(view.is_none(), "Should NOT get frozen view with tombstones");
    }

    // Step 9.4 RED: Test FrozenAdjacencyView available after compaction
    #[test]
    fn test_frozen_view_available_after_compaction() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let index = IncrementalAdjacencyIndex::new();

        // Add edges to delta
        for i in 0..5 {
            index.insert(
                NodeId::new(0).unwrap(),
                AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
            );
        }

        // Before compaction - delta has edges
        assert!(index.frozen_view().is_none());

        // Compact moves delta to frozen
        index.compact();

        // After compaction - should get view
        let view = index.frozen_view();
        assert!(view.is_some(), "Should get frozen view after compaction");

        let view = view.unwrap();
        let slice = view.get_adjacency(NodeId::new(0).unwrap());
        assert_eq!(slice.len(), 5);
    }

    // Step 9.5 RED: Test FrozenAdjacencyView provides direct slice (no iterator overhead)
    #[test]
    fn test_frozen_view_returns_slice_not_iterator() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let frozen_edges: Vec<_> = (0..100)
            .map(|i| {
                (
                    NodeId::new(0).unwrap(),
                    NodeId::new(i + 1).unwrap(),
                    EdgeId::new(i).unwrap(),
                    knows,
                )
            })
            .collect();
        let frozen = AdjacencyIndex::build(frozen_edges);
        let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));

        let view = index.frozen_view().expect("should get view");

        // get_adjacency returns &[AdjacencyEntry], not an iterator
        let slice: &[AdjacencyEntry] = view.get_adjacency(NodeId::new(0).unwrap());
        assert_eq!(slice.len(), 100);

        // Can index directly like a slice
        assert_eq!(slice[0].edge_id, EdgeId::new(0).unwrap());
        assert_eq!(slice[99].edge_id, EdgeId::new(99).unwrap());
    }
}

// ============================================================================
// Test Utilities
// ============================================================================

// Helper functions for test setup will go here
