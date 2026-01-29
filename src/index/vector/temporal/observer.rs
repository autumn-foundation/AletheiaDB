//! Observers for reacting to storage events.
//!
//! This module implements the `StorageObserver` adapter for the temporal vector index,
//! enabling automatic snapshot creation when graph anchors are created.

use super::TemporalVectorIndex;
use crate::core::observer::{StorageEvent, StorageObserver};
use crate::utils::Result;
use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

/// Observer that triggers vector snapshot creation on graph anchor events.
///
/// This implements the `StorageObserver` trait to listen for `NodeAnchorCreated`
/// events from the historical storage layer. When an anchor is created, this
/// observer ensures a corresponding vector snapshot is created, maintaining
/// alignment between graph history and vector history.
///
/// # Architecture
///
/// This component bridges the gap between the graph storage (HistoricalStorage)
/// and the vector index (TemporalVectorIndex).
///
/// ```text
/// HistoricalStorage -> StorageEvent::NodeAnchorCreated -> VectorIndexObserver -> TemporalVectorIndex::create_snapshot_for_anchor
/// ```
pub struct VectorIndexObserver {
    index: Arc<TemporalVectorIndex>,
}

impl VectorIndexObserver {
    /// Creates a new observer for the given index.
    pub fn new(index: Arc<TemporalVectorIndex>) -> Self {
        Self { index }
    }
}

impl StorageObserver for VectorIndexObserver {
    fn on_event(&self, event: &StorageEvent) -> Result<()> {
        match event {
            StorageEvent::NodeAnchorCreated {
                timestamp,
                version_id,
                ..
            } => {
                #[cfg(feature = "observability")]
                tracing::debug!(
                    "VectorIndexObserver: NodeAnchorCreated at {}, triggering snapshot",
                    timestamp
                );

                // Trigger snapshot creation aligned with this anchor
                let snapshot_id = self.index.create_snapshot_for_anchor(*timestamp)?;

                if let Some(id) = snapshot_id {
                    #[cfg(feature = "observability")]
                    tracing::info!(
                        "Created vector snapshot {} aligned with anchor version {}",
                        id,
                        version_id
                    );
                }

                Ok(())
            }
            StorageEvent::EdgeAnchorCreated { timestamp, .. } => {
                // Also trigger for edge anchors if we support edge vectors in the future
                // For now, we sync on edge anchors too to keep time alignment
                self.index.create_snapshot_for_anchor(*timestamp)?;
                Ok(())
            }
            // Ignore other events
            _ => Ok(()),
        }
    }

    fn interested_in(&self, event: &StorageEvent) -> bool {
        // Only interested in anchor creation events for logging/metrics
        event.is_anchor_event()
    }
}
