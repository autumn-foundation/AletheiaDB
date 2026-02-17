//! Observable pattern for storage events.
//!
//! This module provides an event-driven architecture for components to react to
//! storage operations without tight coupling. Components register a callback
//! to receive notifications about anchors, deletes, checkpoints, and other events.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────┐
//! │ HistoricalStorage   │
//! │  (Event Publisher)  │
//! └──────────┬──────────┘
//!            │ notify()
//!            │
//!     ┌──────┴───────┬──────────────┐
//!     │              │              │
//! ┌───▼────┐   ┌────▼─────┐   ┌───▼──────┐
//! │ Vector │   │ Metrics  │   │  WAL     │
//! │ Index  │   │ System   │   │ Logger   │
//! └────────┘   └──────────┘   └──────────┘
//!   (Callback)   (Callback)     (Callback)
//! ```
//!
//! # Example
//!
//! ```no_run
//! use aletheiadb::core::observer::{StorageEvent};
//! use aletheiadb::storage::historical::HistoricalStorage;
//! use std::sync::Arc;
//!
//! let mut storage = HistoricalStorage::new();
//!
//! storage.add_observer(Arc::new(|event: &StorageEvent| {
//!     match event {
//!         StorageEvent::NodeAnchorCreated { version_id, timestamp, .. } => {
//!             println!("Anchor created: {} at {}", version_id, timestamp);
//!         }
//!         _ => {}
//!     }
//!     Ok(())
//! }));
//! ```
//!
//! # Hooks vs Observers: When to Use Each
//!
//! AletheiaDB uses **both** pre-anchor hooks and post-commit observers as complementary
//! patterns for storage event handling. Understanding when to use each is critical for
//! correct integration.
//!
//! ## Post-Commit Observers (This Module)
//!
//! **When**: AFTER the storage operation completes
//! **Purpose**: Notifications for metrics, logging, monitoring, auditing
//! **Returns**: `Result<()>` - cannot return data to caller
//! **Characteristics**:
//! - ✅ Extensible: Multiple observers can subscribe to same events
//! - ✅ Non-blocking: Observer errors don't fail the storage operation
//! - ✅ Decoupled: Observers don't affect storage logic
//! - ❌ Cannot return data: Can't influence what gets stored
//! - ❌ Post-commit: Data already persisted when observer fires
//!
//! **Use cases**:
//! - Logging anchor creation events
//! - Collecting metrics (anchor count, delta count, etc.)
//! - Triggering background jobs (compaction, backup, etc.)
//! - Notifying monitoring systems
//! - Future: Triggering alerts, webhooks, etc.
//!
//! ## Pre-Anchor Hooks (See `historical::PreAnchorHook`)
//!
//! **When**: BEFORE the storage operation (specifically before anchor creation)
//! **Purpose**: Synchronize related state that must be atomically linked to anchors
//! **Returns**: `Result<Option<T>>` - can return data (e.g., snapshot IDs)
//! **Characteristics**:
//! - ✅ Can return data: Returned values stored with the anchor
//! - ✅ Strong consistency: No consistency window between hook and storage
//! - ✅ Graceful degradation: Hook errors don't block anchor creation
//! - ❌ Single hook per entity type: Not extensible like observers
//! - ❌ Must be fast: Blocks anchor creation until hook completes
//!
//! **Use cases**:
//! - Creating vector snapshots synchronized with anchors (VS-047)
//! - Generating snapshot IDs for provenance tracking
//! - Creating auxiliary indexes that must be 1:1 with anchors
//! - Future: Checkpoint coordination, distributed transaction coordination
//!
//! ## Architecture: Hybrid Pattern (VS-047)
//!
//! For temporal vector integration, we use **both** patterns together:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │        HistoricalStorage.add_node_version()         │
//! └─────────────────────────────────────────────────────┘
//!                         │
//!     ┌───────────────────┴───────────────────┐
//!     │ Is Anchor?                            │
//!     └───────────────────┬───────────────────┘
//!                         │
//!          ┌──────────────▼───────────────┐
//!          │ BEFORE Storage                │
//!          │ 1. Call pre_anchor_hook()     │
//!          │ 2. Get snapshot_id            │
//!          │ 3. Set in anchor data         │
//!          └──────────────┬───────────────┘
//!                         │
//!          ┌──────────────▼───────────────┐
//!          │ Storage                       │
//!          │ Store anchor WITH snapshot_id │
//!          └──────────────┬───────────────┘
//!                         │
//!          ┌──────────────▼───────────────┐
//!          │ AFTER Storage                 │
//!          │ 1. notify_observers()         │
//!          │ 2. Observers react (metrics)  │
//!          └───────────────────────────────┘
//! ```
//!
//! ## Decision Tree: Which Pattern to Use?
//!
//! ```text
//! Does your component need to:
//! ├─ Return data to be stored with the anchor?
//! │  └─ YES → Use Pre-Anchor Hook
//! │     Example: Vector snapshot IDs, checkpoint IDs
//! │
//! └─ Just be notified after storage completes?
//!    └─ YES → Use Post-Commit Observer
//!       Example: Metrics, logging, monitoring, audit trails
//!
//! Need both? → Register both (hybrid pattern)
//!   - Hook for data return (snapshot ID)
//!   - Observer for notifications (metrics)
//! ```
//!
//! ## Key Takeaways
//!
//! 1. **Observers** = Post-commit notifications (this module)
//! 2. **Hooks** = Pre-storage data return (see `historical::PreAnchorHook`)
//! 3. **Both** can coexist for complementary functionality
//! 4. **Observers** are for extensibility, **hooks** are for consistency
//! 5. See **ADR-0018** for complete architecture and design rationale

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::Timestamp;
use crate::utils::Result;
use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

/// Events emitted by the storage layer.
///
/// These events allow components to react to storage operations without tight coupling.
/// New event types can be added as needed for observability, indexing, or coordination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageEvent {
    /// A node anchor (full snapshot) was created.
    ///
    /// Anchors are created every N versions (default: 10) and represent complete
    /// property snapshots. This is the key event for temporal vector index synchronization.
    NodeAnchorCreated {
        /// ID of the anchor version
        version_id: VersionId,
        /// ID of the node this anchor belongs to
        node_id: NodeId,
        /// Transaction time when the anchor was created
        timestamp: Timestamp,
    },

    /// An edge anchor (full snapshot) was created.
    EdgeAnchorCreated {
        /// ID of the anchor version
        version_id: VersionId,
        /// ID of the edge this anchor belongs to
        edge_id: EdgeId,
        /// Transaction time when the anchor was created
        timestamp: Timestamp,
    },

    /// A node version (anchor or delta) was created.
    ///
    /// This is a more general event than AnchorCreated, fired for all version types.
    /// Useful for metrics, logging, or audit trails.
    NodeVersionCreated {
        /// ID of the version that was created
        version_id: VersionId,
        /// ID of the node this version belongs to
        node_id: NodeId,
        /// Transaction time when the version was created
        timestamp: Timestamp,
        /// Whether this version is an anchor (true) or delta (false)
        is_anchor: bool,
    },

    /// An edge version (anchor or delta) was created.
    EdgeVersionCreated {
        /// ID of the version that was created
        version_id: VersionId,
        /// ID of the edge this version belongs to
        edge_id: EdgeId,
        /// Transaction time when the version was created
        timestamp: Timestamp,
        /// Whether this version is an anchor (true) or delta (false)
        is_anchor: bool,
    },
}

impl StorageEvent {
    /// Get the timestamp of when this event occurred.
    pub fn timestamp(&self) -> Timestamp {
        match self {
            StorageEvent::NodeAnchorCreated { timestamp, .. }
            | StorageEvent::EdgeAnchorCreated { timestamp, .. }
            | StorageEvent::NodeVersionCreated { timestamp, .. }
            | StorageEvent::EdgeVersionCreated { timestamp, .. } => *timestamp,
        }
    }

    /// Returns true if this is an anchor creation event.
    pub fn is_anchor_event(&self) -> bool {
        matches!(
            self,
            StorageEvent::NodeAnchorCreated { .. } | StorageEvent::EdgeAnchorCreated { .. }
        )
    }
}

/// Callback type for storage observers.
pub type StorageCallback = Arc<dyn Fn(&StorageEvent) -> Result<()> + Send + Sync>;

/// Helper for notifying multiple observers of an event.
///
/// This function handles error logging and ensures all observers are notified
/// even if some fail. Observer errors do not propagate to the caller.
///
/// # Arguments
/// - `observers`: List of observers to notify
/// - `event`: The event to broadcast
///
/// # Design Note
/// This is a standalone function rather than a method to keep HistoricalStorage
/// focused on storage logic, not observer management.
pub fn notify_observers(observers: &[StorageCallback], event: &StorageEvent) {
    for observer in observers {
        // Notify observer, log errors but don't fail
        if let Err(e) = observer(event) {
            #[cfg(feature = "observability")]
            {
                use crate::utils::error::Error;
                match &e {
                    Error::Vector(ve) => {
                        tracing::warn!("Observer error for event {:?}: {}", event, ve);
                    }
                    _ => {
                        tracing::warn!("Observer error for event {:?}: {:?}", event, e);
                    }
                }
            }

            // Without observability, silently continue (errors don't block storage)
            #[cfg(not(feature = "observability"))]
            {
                let _ = e; // Suppress unused variable warning
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::{NodeId, VersionId};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_notify_observers() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        let observer: StorageCallback = Arc::new(move |_event| {
            count_clone.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let observers = vec![observer];

        let event = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: 1000.into(),
        };

        notify_observers(&observers, &event);

        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_multiple_observers() {
        let count1 = Arc::new(AtomicUsize::new(0));
        let count1_clone = count1.clone();
        let observer1: StorageCallback = Arc::new(move |_event| {
            count1_clone.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let count2 = Arc::new(AtomicUsize::new(0));
        let count2_clone = count2.clone();
        let observer2: StorageCallback = Arc::new(move |_event| {
            count2_clone.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let observers = vec![observer1, observer2];

        let event = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: 1000.into(),
        };

        notify_observers(&observers, &event);

        assert_eq!(count1.load(Ordering::SeqCst), 1);
        assert_eq!(count2.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_filtered_observer() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();

        // Observer only interested in anchors
        let observer: StorageCallback = Arc::new(move |event| {
            if event.is_anchor_event() {
                count_clone.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        });

        let observers = vec![observer];

        // Send anchor event - should be counted
        let anchor_event = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: 1000.into(),
        };
        notify_observers(&observers, &anchor_event);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // Send version event - should be filtered out
        let version_event = StorageEvent::NodeVersionCreated {
            version_id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: 2000.into(),
            is_anchor: false,
        };
        notify_observers(&observers, &version_event);
        assert_eq!(count.load(Ordering::SeqCst), 1); // Still 1
    }

    #[test]
    fn test_event_timestamp() {
        let event = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: 12345.into(),
        };

        assert_eq!(event.timestamp().wallclock(), 12345);
    }

    #[test]
    fn test_event_collection() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let observer: StorageCallback = Arc::new(move |event| {
            events_clone.lock().unwrap().push(event.clone());
            Ok(())
        });

        let observers = vec![observer];

        let event1 = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: 1000.into(),
        };
        let event2 = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(2).unwrap(),
            node_id: NodeId::new(2).unwrap(),
            timestamp: 2000.into(),
        };

        notify_observers(&observers, &event1);
        notify_observers(&observers, &event2);

        let collected = events.lock().unwrap();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0], event1);
        assert_eq!(collected[1], event2);
    }

    #[test]
    #[should_panic(expected = "Observer panic!")]
    fn test_notify_observers_propagates_panic() {
        // 💣 Risk: A panicking observer crashes the storage operation.
        // This test confirms the current behavior (fail-fast).

        let observer: StorageCallback = Arc::new(|_event| {
            panic!("Observer panic!");
        });

        let observers = vec![observer];

        let event = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: 1000.into(),
        };

        notify_observers(&observers, &event);
    }
}
