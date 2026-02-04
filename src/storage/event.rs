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
