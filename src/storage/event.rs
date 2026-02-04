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
