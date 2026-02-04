//! Storage events and observers.
//!
//! This module defines events emitted by the storage layer and the observer
//! mechanism used to react to them.

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::Timestamp;
use crate::utils::Result;
use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

/// Events emitted by the storage layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageEvent {
    /// A node anchor (full snapshot) was created.
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

/// Observer callback for receiving storage events.
pub type StorageObserver = Arc<dyn Fn(&StorageEvent) -> Result<()> + Send + Sync>;

/// Helper for notifying multiple observers of an event.
pub fn notify_observers(observers: &[StorageObserver], event: &StorageEvent) {
    for observer in observers {
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
            #[cfg(not(feature = "observability"))]
            {
                let _ = e;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_storage_event_timestamp() {
        let ts = Timestamp::from(100);
        let vid = VersionId::new(1).unwrap();
        let nid = NodeId::new(1).unwrap();
        let eid = EdgeId::new(1).unwrap();

        let event = StorageEvent::NodeAnchorCreated {
            version_id: vid,
            node_id: nid,
            timestamp: ts,
        };
        assert_eq!(event.timestamp(), ts);

        let event = StorageEvent::EdgeAnchorCreated {
            version_id: vid,
            edge_id: eid,
            timestamp: ts,
        };
        assert_eq!(event.timestamp(), ts);

        let event = StorageEvent::NodeVersionCreated {
            version_id: vid,
            node_id: nid,
            timestamp: ts,
            is_anchor: false,
        };
        assert_eq!(event.timestamp(), ts);

        let event = StorageEvent::EdgeVersionCreated {
            version_id: vid,
            edge_id: eid,
            timestamp: ts,
            is_anchor: false,
        };
        assert_eq!(event.timestamp(), ts);
    }

    #[test]
    fn test_storage_event_is_anchor_event() {
        let ts = Timestamp::from(100);
        let vid = VersionId::new(1).unwrap();
        let nid = NodeId::new(1).unwrap();
        let eid = EdgeId::new(1).unwrap();

        let event = StorageEvent::NodeAnchorCreated {
            version_id: vid,
            node_id: nid,
            timestamp: ts,
        };
        assert!(event.is_anchor_event());

        let event = StorageEvent::EdgeAnchorCreated {
            version_id: vid,
            edge_id: eid,
            timestamp: ts,
        };
        assert!(event.is_anchor_event());

        let event = StorageEvent::NodeVersionCreated {
            version_id: vid,
            node_id: nid,
            timestamp: ts,
            is_anchor: false,
        };
        assert!(!event.is_anchor_event());

        // Even if is_anchor is true, it's not an *AnchorCreated* event type
        let event = StorageEvent::NodeVersionCreated {
            version_id: vid,
            node_id: nid,
            timestamp: ts,
            is_anchor: true,
        };
        assert!(!event.is_anchor_event());

        let event = StorageEvent::EdgeVersionCreated {
            version_id: vid,
            edge_id: eid,
            timestamp: ts,
            is_anchor: false,
        };
        assert!(!event.is_anchor_event());
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
            timestamp: Timestamp::from(100),
        };

        notify_observers(&observers, &event);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_notify_observers_error() {
        // Observer that returns error
        let observer: StorageObserver = Arc::new(move |_| {
            Err(crate::utils::Error::other("test error"))
        });

        let observers = vec![observer];
        let event = StorageEvent::NodeAnchorCreated {
            version_id: VersionId::new(1).unwrap(),
            node_id: NodeId::new(1).unwrap(),
            timestamp: Timestamp::from(100),
        };

        // Should not panic or return error
        notify_observers(&observers, &event);
    }
}
