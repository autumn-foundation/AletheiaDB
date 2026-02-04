//! Storage events and observers.
//!
//! This module defines events emitted by the storage layer and the observer interface
//! for reacting to them.
//!
//! # Architecture
//!
//! The observer pattern here is implemented using closures for simplicity and flexibility.
//! Components can register a callback function to be notified of storage events.

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

/// Observer function type alias.
///
/// An observer is a thread-safe closure that receives storage events.
/// It returns a Result so that errors can be logged, but they generally
/// do not stop the storage operation.
pub type StorageObserver = Arc<dyn Fn(&StorageEvent) -> Result<()> + Send + Sync>;

/// Helper for notifying multiple observers of an event.
///
/// This function handles error logging and ensures all observers are notified
/// even if some fail. Observer errors do not propagate to the caller.
///
/// # Arguments
/// - `observers`: List of observers to notify
/// - `event`: The event to broadcast
pub fn notify_observers(observers: &[StorageObserver], event: &StorageEvent) {
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_storage_event_accessors() {
        let ts_val = 1000i64;
        let timestamp: Timestamp = ts_val.into();
        let version_id = VersionId::new(1).unwrap();
        let node_id = NodeId::new(1).unwrap();
        let edge_id = EdgeId::new(1).unwrap();

        // Test NodeAnchorCreated
        let node_anchor = StorageEvent::NodeAnchorCreated {
            version_id,
            node_id,
            timestamp,
        };
        assert_eq!(node_anchor.timestamp(), timestamp);
        assert!(node_anchor.is_anchor_event());

        // Test EdgeAnchorCreated
        let edge_anchor = StorageEvent::EdgeAnchorCreated {
            version_id,
            edge_id,
            timestamp,
        };
        assert_eq!(edge_anchor.timestamp(), timestamp);
        assert!(edge_anchor.is_anchor_event());

        // Test NodeVersionCreated (anchor)
        let node_ver_anchor = StorageEvent::NodeVersionCreated {
            version_id,
            node_id,
            timestamp,
            is_anchor: true,
        };
        assert_eq!(node_ver_anchor.timestamp(), timestamp);
        assert!(!node_ver_anchor.is_anchor_event()); // Note: is_anchor_event() specifically checks for *AnchorCreated variants

        // Test NodeVersionCreated (delta)
        let node_ver_delta = StorageEvent::NodeVersionCreated {
            version_id,
            node_id,
            timestamp,
            is_anchor: false,
        };
        assert_eq!(node_ver_delta.timestamp(), timestamp);
        assert!(!node_ver_delta.is_anchor_event());

        // Test EdgeVersionCreated (anchor)
        let edge_ver_anchor = StorageEvent::EdgeVersionCreated {
            version_id,
            edge_id,
            timestamp,
            is_anchor: true,
        };
        assert_eq!(edge_ver_anchor.timestamp(), timestamp);
        assert!(!edge_ver_anchor.is_anchor_event());

        // Test EdgeVersionCreated (delta)
        let edge_ver_delta = StorageEvent::EdgeVersionCreated {
            version_id,
            edge_id,
            timestamp,
            is_anchor: false,
        };
        assert_eq!(edge_ver_delta.timestamp(), timestamp);
        assert!(!edge_ver_delta.is_anchor_event());
    }

    #[test]
    fn test_notify_observers() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();

        let observer: StorageObserver = Arc::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
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
    fn test_notify_multiple_observers() {
        let count = Arc::new(AtomicUsize::new(0));
        let c1 = count.clone();
        let c2 = count.clone();

        let obs1: StorageObserver = Arc::new(move |_| {
            c1.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let obs2: StorageObserver = Arc::new(move |_| {
            c2.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let observers = vec![obs1, obs2];
        let event = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: 1000.into(),
        };

        notify_observers(&observers, &event);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_notify_observer_error_handling() {
        // Observer that fails
        let failing: StorageObserver = Arc::new(|_| {
            Err(crate::utils::error::Error::other("test error".to_string()))
        });

        // Observer that succeeds
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let succeeding: StorageObserver = Arc::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let observers = vec![failing, succeeding];
        let event = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: 1000.into(),
        };

        // Should not panic, and succeeding observer should still be called
        notify_observers(&observers, &event);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_storage_event_debug_derive() {
        let event = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(2).unwrap(),
            timestamp: 1000.into(),
        };
        let debug_str = format!("{:?}", event);
        assert!(debug_str.contains("NodeAnchorCreated"));
        assert!(debug_str.contains("version_id"));
        assert!(debug_str.contains("node_id"));
        assert!(debug_str.contains("timestamp"));
    }
}
