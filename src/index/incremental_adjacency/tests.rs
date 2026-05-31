// use super::*;
    use super::*;

    #[test]
    fn test_new_creates_empty_index() {
        let index = IncrementalAdjacencyIndex::new();
        assert_eq!(index.frozen_edge_count(), 0);
        assert_eq!(index.delta_edge_count(), 0);
        assert_eq!(index.tombstone_count(), 0);
    }

    #[test]
    fn test_guard_capacity_hint_and_fast_len() {
        use crate::core::interning::InternedString;
        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(1).unwrap();

        {
            let guard = index.get_adjacency(node);
            // Empty index
            assert_eq!(guard.capacity_hint(), 0);
            assert_eq!(guard.fast_len(), Some(0));
        }

        // Add to delta
        let entry = AdjacencyEntry::new(
            NodeId::new(2).unwrap(),
            EdgeId::new(1).unwrap(),
            InternedString::from_raw(1),
        );
        index.insert(node, entry);

        {
            let guard = index.get_adjacency(node);
            assert_eq!(guard.capacity_hint(), 1);
            assert_eq!(guard.fast_len(), None); // Not fast path anymore because delta is not empty
        }

        // Compact
        index.compact();

        {
            let guard = index.get_adjacency(node);
            assert_eq!(guard.capacity_hint(), 1);
            assert_eq!(guard.fast_len(), Some(1));
        }

        // Add tombstone
        index.delete(EdgeId::new(1).unwrap());

        {
            let guard = index.get_adjacency(node);
            assert_eq!(guard.capacity_hint(), 1); // capacity_hint is upper bound, doesn't account for tombstones
            assert_eq!(guard.fast_len(), None); // Not fast path anymore because tombstones exist
        }
    }

    #[test]
    fn test_guard_frozen_slice() {
        use crate::core::interning::InternedString;
        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(1).unwrap();

        // Empty index: frozen_slice returns empty slice
        let guard = index.get_adjacency(node);
        assert!(guard.frozen_slice().is_empty());
        drop(guard);

        // Insert and compact so the entry lands in the frozen layer
        let edge = EdgeId::new(1).unwrap();
        let entry = AdjacencyEntry::new(NodeId::new(2).unwrap(), edge, InternedString::from_raw(1));
        index.insert(node, entry);
        index.compact();

        let guard = index.get_adjacency(node);
        assert_eq!(guard.frozen_slice().len(), 1);
        assert_eq!(guard.frozen_slice()[0].edge_id, edge);
    }

    #[test]
    fn test_guard_delta_slice() {
        use crate::core::interning::InternedString;
        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(1).unwrap();

        // No delta yet: delta_slice returns empty slice (unwrap_or path)
        let guard = index.get_adjacency(node);
        assert!(guard.delta_slice().is_empty());
        drop(guard);

        // After insert (before compact): delta_slice returns the entry
        let edge = EdgeId::new(1).unwrap();
        let entry = AdjacencyEntry::new(NodeId::new(2).unwrap(), edge, InternedString::from_raw(1));
        index.insert(node, entry);

        let guard = index.get_adjacency(node);
        assert_eq!(guard.delta_slice().len(), 1);
        assert_eq!(guard.delta_slice()[0].edge_id, edge);
    }

    #[test]
    fn test_guard_is_tombstoned() {
        use crate::core::interning::InternedString;
        let index = IncrementalAdjacencyIndex::new();
        let node = NodeId::new(1).unwrap();
        let edge = EdgeId::new(1).unwrap();
        let other_edge = EdgeId::new(2).unwrap();

        let entry = AdjacencyEntry::new(NodeId::new(2).unwrap(), edge, InternedString::from_raw(1));
        index.insert(node, entry);
        index.compact();

        // Fast path: no tombstones, is_tombstoned always returns false
        let guard = index.get_adjacency(node);
        assert!(!guard.is_tombstoned(edge));
        drop(guard);

        // Delete the edge: now is_tombstoned returns true for that edge
        index.delete(edge);

        let guard = index.get_adjacency(node);
        assert!(guard.is_tombstoned(edge));
        assert!(!guard.is_tombstoned(other_edge));
    }
