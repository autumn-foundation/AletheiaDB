//! Integration tests for Incremental CSR Adjacency Index
//!
//! This test suite validates the incremental CSR implementation using TDD methodology.
//! Tests are organized by phase following the implementation plan.

use gallifreydb::core::id::{EdgeId, NodeId};
use gallifreydb::core::interning::GLOBAL_INTERNER;
use gallifreydb::index::adjacency::{AdjacencyEntry, AdjacencyIndex};
use gallifreydb::index::incremental_adjacency::IncrementalAdjacencyIndex;
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
                        let target = NodeId::new((source.as_u64() + 1)).unwrap();
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
    #[ignore = "Phase 4.1 - not implemented yet"]
    fn test_should_compact_ratio_threshold() {
        // let mut config = IncrementalConfig::default();
        // config.compaction_ratio = 0.1; // 10% threshold
        // config.max_delta_edges = 100_000; // High so ratio triggers first
        //
        // // Create frozen with 100 edges
        // let frozen_edges: Vec<_> = (0..100)
        //     .map(|i| {
        //         (
        //             NodeId::new(i).unwrap(),
        //             NodeId::new(i + 1).unwrap(),
        //             EdgeId::new(i).unwrap(),
        //             GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        //         )
        //     })
        //     .collect();
        // let frozen = AdjacencyIndex::build(frozen_edges);
        // let index = IncrementalAdjacencyIndex::with_config(Arc::new(frozen), config);
        //
        // // Add 9 edges to delta (9% of frozen)
        // let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        // for i in 100..109 {
        //     index.insert(
        //         NodeId::new(i).unwrap(),
        //         AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
        //     );
        // }
        // assert!(!index.should_compact()); // Below threshold
        //
        // // Add 1 more edge (10% threshold hit)
        // index.insert(
        //     NodeId::new(109).unwrap(),
        //     AdjacencyEntry::new(NodeId::new(110).unwrap(), EdgeId::new(109).unwrap(), knows),
        // );
        // assert!(index.should_compact()); // Should trigger
        todo!("Implement should_compact()");
    }

    // Step 4.3 RED: Test compact merges delta into frozen
    #[test]
    #[ignore = "Phase 4.3 - not implemented yet"]
    fn test_compact_merges_delta_into_frozen() {
        // // Create frozen with 2 edges
        // let frozen_edges = vec![
        //     (
        //         NodeId::new(0).unwrap(),
        //         NodeId::new(1).unwrap(),
        //         EdgeId::new(0).unwrap(),
        //         GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        //     ),
        //     (
        //         NodeId::new(1).unwrap(),
        //         NodeId::new(2).unwrap(),
        //         EdgeId::new(1).unwrap(),
        //         GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        //     ),
        // ];
        // let frozen = AdjacencyIndex::build(frozen_edges);
        // let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));
        //
        // // Add 1 edge to delta
        // let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        // index.insert(
        //     NodeId::new(2).unwrap(),
        //     AdjacencyEntry::new(NodeId::new(3).unwrap(), EdgeId::new(2).unwrap(), knows),
        // );
        //
        // // Before compaction
        // assert_eq!(index.frozen_edge_count(), 2);
        // assert_eq!(index.delta_edge_count(), 1);
        //
        // // Compact
        // index.compact();
        //
        // // After compaction
        // assert_eq!(index.frozen_edge_count(), 3);
        // assert_eq!(index.delta_edge_count(), 0);
        todo!("Implement compact()");
    }

    // Step 4.5 RED: Test compact removes tombstoned edges
    #[test]
    #[ignore = "Phase 4.5 - not implemented yet"]
    fn test_compact_removes_tombstoned_edges() {
        // // Create frozen with 3 edges
        // let frozen_edges = vec![
        //     (
        //         NodeId::new(0).unwrap(),
        //         NodeId::new(1).unwrap(),
        //         EdgeId::new(0).unwrap(),
        //         GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        //     ),
        //     (
        //         NodeId::new(0).unwrap(),
        //         NodeId::new(2).unwrap(),
        //         EdgeId::new(1).unwrap(),
        //         GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        //     ),
        //     (
        //         NodeId::new(0).unwrap(),
        //         NodeId::new(3).unwrap(),
        //         EdgeId::new(2).unwrap(),
        //         GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        //     ),
        // ];
        // let frozen = AdjacencyIndex::build(frozen_edges);
        // let index = IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen));
        //
        // // Delete 1 edge
        // index.delete(EdgeId::new(1).unwrap());
        //
        // // Compact
        // index.compact();
        //
        // // Should have 2 edges after compaction (tombstoned edge removed)
        // assert_eq!(index.frozen_edge_count(), 2);
        // assert_eq!(index.tombstone_count(), 0); // Tombstones cleared
        todo!("Filter tombstones during compaction");
    }

    // Step 4.7 RED: Test compact clears delta and tombstones
    #[test]
    #[ignore = "Phase 4.7 - not implemented yet"]
    fn test_compact_clears_delta_and_tombstones() {
        // let index = IncrementalAdjacencyIndex::new();
        // let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        //
        // // Add edges to delta
        // index.insert(
        //     NodeId::new(0).unwrap(),
        //     AdjacencyEntry::new(NodeId::new(1).unwrap(), EdgeId::new(0).unwrap(), knows),
        // );
        // index.insert(
        //     NodeId::new(0).unwrap(),
        //     AdjacencyEntry::new(NodeId::new(2).unwrap(), EdgeId::new(1).unwrap(), knows),
        // );
        //
        // // Add tombstone
        // index.delete(EdgeId::new(0).unwrap());
        //
        // assert_eq!(index.delta_edge_count(), 2);
        // assert_eq!(index.tombstone_count(), 1);
        //
        // // Compact
        // index.compact();
        //
        // // Transient state cleared
        // assert_eq!(index.delta_edge_count(), 0);
        // assert_eq!(index.tombstone_count(), 0);
        todo!("Clear transient state after compaction");
    }

    // Step 4.9 RED: Test compact atomic swap
    #[test]
    #[ignore = "Phase 4.9 - not implemented yet"]
    fn test_compact_atomic_swap() {
        // // This test verifies readers see consistent state during compaction
        // use std::sync::Arc;
        // use std::thread;
        //
        // let frozen_edges: Vec<_> = (0..100)
        //     .map(|i| {
        //         (
        //             NodeId::new(i).unwrap(),
        //             NodeId::new(i + 1).unwrap(),
        //             EdgeId::new(i).unwrap(),
        //             GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        //         )
        //     })
        //     .collect();
        // let frozen = AdjacencyIndex::build(frozen_edges);
        // let index = Arc::new(IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen)));
        //
        // // Add delta
        // let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        // for i in 100..110 {
        //     index.insert(
        //         NodeId::new(i).unwrap(),
        //         AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
        //     );
        // }
        //
        // // Spawn reader thread
        // let index_clone = Arc::clone(&index);
        // let reader = thread::spawn(move || {
        //     for _ in 0..1000 {
        //         let guard = index_clone.get_adjacency(NodeId::new(0).unwrap());
        //         let count = guard.iter().count();
        //         // Should always see valid state (never partial)
        //         assert!(count == 1 || count == 1); // Before or after compact
        //     }
        // });
        //
        // // Compact in main thread
        // index.compact();
        //
        // reader.join().unwrap();
        todo!("Use ArcSwap for atomic replacement");
    }

    // Step 4.11 RED: Test concurrent read during compaction
    #[test]
    #[ignore = "Phase 4.11 - not implemented yet"]
    fn test_concurrent_read_during_compaction() {
        // // Verifies lock-free reads during compaction
        // use std::sync::Arc;
        // use std::thread;
        //
        // let frozen_edges: Vec<_> = (0..1000)
        //     .map(|i| {
        //         (
        //             NodeId::new(i).unwrap(),
        //             NodeId::new(i + 1).unwrap(),
        //             EdgeId::new(i).unwrap(),
        //             GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        //         )
        //     })
        //     .collect();
        // let frozen = AdjacencyIndex::build(frozen_edges);
        // let index = Arc::new(IncrementalAdjacencyIndex::from_frozen(Arc::new(frozen)));
        //
        // // Spawn multiple reader threads
        // let readers: Vec<_> = (0..8)
        //     .map(|_| {
        //         let index_clone = Arc::clone(&index);
        //         thread::spawn(move || {
        //             for i in 0..1000 {
        //                 let guard = index_clone.get_adjacency(NodeId::new(i % 1000).unwrap());
        //                 assert!(guard.iter().count() > 0);
        //             }
        //         })
        //     })
        //     .collect();
        //
        // // Compact while readers are running
        // index.compact();
        //
        // for reader in readers {
        //     reader.join().unwrap();
        // }
        todo!("Verify lock-free reads during compaction");
    }
}

// ============================================================================
// Phase 5: Background Compaction Thread Tests
// ============================================================================

#[cfg(test)]
mod phase5_background_compaction {
    use super::*;

    // Step 5.1 RED: Test background compaction starts
    #[test]
    #[ignore = "Phase 5.1 - not implemented yet"]
    fn test_background_compaction_starts() {
        // use std::sync::Arc;
        // use std::thread;
        // use std::time::Duration;
        //
        // let index = Arc::new(IncrementalAdjacencyIndex::new());
        // let scheduler = CompactionScheduler::new(Arc::clone(&index));
        //
        // let handle = scheduler.start();
        //
        // // Verify thread is running
        // thread::sleep(Duration::from_millis(100));
        // assert!(!handle.is_finished());
        //
        // // Shutdown
        // scheduler.shutdown();
        // handle.join().unwrap();
        todo!("Implement CompactionScheduler::start()");
    }

    // Step 5.3 RED: Test background compaction triggers on threshold
    #[test]
    #[ignore = "Phase 5.3 - not implemented yet"]
    fn test_background_compaction_triggers_on_threshold() {
        // use std::sync::Arc;
        // use std::thread;
        // use std::time::Duration;
        //
        // let mut config = IncrementalConfig::default();
        // config.max_delta_edges = 10; // Low threshold for testing
        // config.check_interval = Duration::from_millis(50);
        //
        // let index = Arc::new(IncrementalAdjacencyIndex::with_config(
        //     Arc::new(AdjacencyIndex::new()),
        //     config,
        // ));
        // let scheduler = CompactionScheduler::new(Arc::clone(&index));
        // let handle = scheduler.start();
        //
        // // Add edges to trigger threshold
        // let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        // for i in 0..15 {
        //     index.insert(
        //         NodeId::new(i).unwrap(),
        //         AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
        //     );
        // }
        //
        // // Wait for background compaction
        // thread::sleep(Duration::from_millis(200));
        //
        // // Delta should be cleared by background compaction
        // assert_eq!(index.delta_edge_count(), 0);
        // assert_eq!(index.frozen_edge_count(), 15);
        //
        // scheduler.shutdown();
        // handle.join().unwrap();
        todo!("Add threshold monitoring loop");
    }

    // Step 5.5 RED: Test pause/resume
    #[test]
    #[ignore = "Phase 5.5 - not implemented yet"]
    fn test_background_compaction_pause_resume() {
        // use std::sync::Arc;
        // use std::thread;
        // use std::time::Duration;
        //
        // let mut config = IncrementalConfig::default();
        // config.max_delta_edges = 10;
        // config.check_interval = Duration::from_millis(50);
        //
        // let index = Arc::new(IncrementalAdjacencyIndex::with_config(
        //     Arc::new(AdjacencyIndex::new()),
        //     config,
        // ));
        // let scheduler = CompactionScheduler::new(Arc::clone(&index));
        // let handle = scheduler.start();
        //
        // // Pause compaction
        // scheduler.pause();
        //
        // // Add edges beyond threshold
        // let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        // for i in 0..15 {
        //     index.insert(
        //         NodeId::new(i).unwrap(),
        //         AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
        //     );
        // }
        //
        // thread::sleep(Duration::from_millis(200));
        //
        // // Should NOT compact while paused
        // assert_eq!(index.delta_edge_count(), 15);
        //
        // // Resume
        // scheduler.resume();
        // thread::sleep(Duration::from_millis(200));
        //
        // // Should now compact
        // assert_eq!(index.delta_edge_count(), 0);
        //
        // scheduler.shutdown();
        // handle.join().unwrap();
        todo!("Implement pause() / resume()");
    }

    // Step 5.7 RED: Test graceful shutdown
    #[test]
    #[ignore = "Phase 5.7 - not implemented yet"]
    fn test_graceful_shutdown() {
        // use std::sync::Arc;
        // use std::time::Duration;
        //
        // let mut config = IncrementalConfig::default();
        // config.max_delta_edges = 10;
        //
        // let index = Arc::new(IncrementalAdjacencyIndex::with_config(
        //     Arc::new(AdjacencyIndex::new()),
        //     config,
        // ));
        // let scheduler = CompactionScheduler::new(Arc::clone(&index));
        // let handle = scheduler.start();
        //
        // // Add edges
        // let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        // for i in 0..15 {
        //     index.insert(
        //         NodeId::new(i).unwrap(),
        //         AdjacencyEntry::new(NodeId::new(i + 1).unwrap(), EdgeId::new(i).unwrap(), knows),
        //     );
        // }
        //
        // // Shutdown should complete in-flight compaction
        // scheduler.shutdown();
        // handle.join().unwrap();
        //
        // // Final compaction should have run
        // assert_eq!(index.delta_edge_count(), 0);
        todo!("Implement shutdown coordination");
    }

    // Step 5.9 RED: Test compaction thread panic recovery
    #[test]
    #[ignore = "Phase 5.9 - not implemented yet"]
    fn test_compaction_thread_panic_recovery() {
        // // This test verifies system continues if background thread panics
        // // (Future enhancement - may add panic catching with fallback)
        todo!("Add panic catching with fallback");
    }
}

// ============================================================================
// Test Utilities
// ============================================================================

// Helper functions for test setup will go here
