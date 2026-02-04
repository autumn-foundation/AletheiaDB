//! Observers for reacting to storage events.
//!
//! This module implements the `StorageObserver` adapter for the temporal vector index,
//! enabling automatic snapshot creation when graph anchors are created.

use super::TemporalVectorIndex;
use crate::storage::event::{StorageEvent, StorageObserver};
use crate::utils::Result;
use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

/// Creates an observer that triggers vector snapshot creation on graph anchor events.
///
/// This factory function returns a `StorageObserver` (a thread-safe closure) that listens
/// for `NodeAnchorCreated` events from the historical storage layer. When an anchor is
/// created, this observer ensures a corresponding vector snapshot is created, maintaining
/// alignment between graph history and vector history.
///
/// # Architecture
///
/// This component bridges the gap between the graph storage (HistoricalStorage)
/// and the vector index (TemporalVectorIndex).
///
/// ```text
/// HistoricalStorage -> StorageEvent::NodeAnchorCreated -> Closure -> TemporalVectorIndex::create_snapshot_for_anchor
/// ```
pub fn create_vector_index_observer(index: Arc<TemporalVectorIndex>) -> StorageObserver {
    Arc::new(move |event: &StorageEvent| -> Result<()> {
        // Only interested in anchor creation events for logging/metrics
        // This effectively replaces the `interested_in` method from the old trait
        if !event.is_anchor_event() {
            return Ok(());
        }

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
                let snapshot_id = index.create_snapshot_for_anchor(*timestamp)?;

                #[cfg(feature = "observability")]
                if let Some(id) = snapshot_id {
                    tracing::info!(
                        "Created vector snapshot {} aligned with anchor version {}",
                        id,
                        version_id
                    );
                }

                // Suppress unused variable warnings when observability is disabled
                #[cfg(not(feature = "observability"))]
                {
                    let _ = version_id;
                    let _ = snapshot_id;
                }

                Ok(())
            }
            StorageEvent::EdgeAnchorCreated { timestamp, .. } => {
                // Also trigger for edge anchors if we support edge vectors in the future
                // For now, we sync on edge anchors too to keep time alignment
                index.create_snapshot_for_anchor(*timestamp)?;
                Ok(())
            }
            // Ignore other events
            _ => Ok(()),
        }
    })
}
