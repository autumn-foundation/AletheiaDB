use super::TemporalVectorIndex;
use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

/// Observer adapter for TemporalVectorIndex to react to storage events.
///
/// This struct implements `StorageObserver` to enable TemporalVectorIndex to create
/// snapshots when HistoricalStorage creates anchors, maintaining synchronization
/// between graph versioning and vector indexing.
///
/// # Design
///
/// The observer wraps an Arc reference to TemporalVectorIndex, allowing multiple
/// components to share the same index. When node/edge anchors are created, the
/// observer triggers snapshot creation and stores the snapshot ID in the anchor's
/// metadata via the return value.
///
/// # Example
///
/// ```no_run
/// use gallifreydb::index::vector::temporal::{TemporalVectorIndex, VectorIndexObserver, TemporalVectorConfig};
/// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
/// use gallifreydb::storage::historical::HistoricalStorage;
/// use std::sync::Arc;
///
/// # fn example() -> gallifreydb::utils::Result<()> {
/// // Create temporal vector index
/// let config = TemporalVectorConfig::default_with_hnsw(
///     HnswConfig::new(384, DistanceMetric::Cosine)
/// );
/// let index = Arc::new(TemporalVectorIndex::new(config)?);
///
/// // Create observer wrapper
/// let observer = VectorIndexObserver::new(Arc::clone(&index));
///
/// // Register with HistoricalStorage
/// let mut storage = HistoricalStorage::new();
/// storage.add_observer(Arc::new(observer));
///
/// // Now anchors will automatically trigger vector snapshots
/// # Ok(())
/// # }
/// ```
pub struct VectorIndexObserver {
    /// The temporal vector index (reserved for future use in metrics/monitoring)
    #[allow(dead_code)]
    index: Arc<TemporalVectorIndex>,
}

impl VectorIndexObserver {
    /// Creates a new observer for a TemporalVectorIndex.
    ///
    /// # Arguments
    ///
    /// * `index` - Arc reference to the TemporalVectorIndex to observe
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use gallifreydb::index::vector::temporal::{TemporalVectorIndex, VectorIndexObserver, TemporalVectorConfig};
    /// # use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    /// # use std::sync::Arc;
    /// # fn example() -> gallifreydb::utils::Result<()> {
    /// let config = TemporalVectorConfig::default_with_hnsw(
    ///     HnswConfig::new(384, DistanceMetric::Cosine)
    /// );
    /// let index = Arc::new(TemporalVectorIndex::new(config)?);
    /// let observer = VectorIndexObserver::new(index);
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(index: Arc<TemporalVectorIndex>) -> Self {
        VectorIndexObserver { index }
    }
}

impl crate::core::observer::StorageObserver for VectorIndexObserver {
    fn on_event(&self, event: &crate::core::observer::StorageEvent) -> crate::utils::Result<()> {
        use crate::core::observer::StorageEvent;

        match event {
            // Note: The PreAnchorHook already creates the snapshot and links the snapshot_id
            // to the anchor. This observer is only for post-commit actions like logging/metrics.
            StorageEvent::NodeAnchorCreated {
                node_id: _node_id,
                timestamp: _timestamp,
                ..
            } => {
                #[cfg(feature = "observability")]
                tracing::debug!(
                    "VectorIndexObserver: Node anchor created for {} at timestamp {} (snapshot already created by pre-anchor hook)",
                    _node_id,
                    _timestamp
                );
                Ok(())
            }
            StorageEvent::EdgeAnchorCreated {
                edge_id: _edge_id,
                timestamp: _timestamp,
                ..
            } => {
                #[cfg(feature = "observability")]
                tracing::debug!(
                    "VectorIndexObserver: Edge anchor created for {} at timestamp {} (snapshot already created by pre-anchor hook)",
                    _edge_id,
                    _timestamp
                );
                Ok(())
            }
            // Ignore other events
            _ => Ok(()),
        }
    }

    fn interested_in(&self, event: &crate::core::observer::StorageEvent) -> bool {
        // Only interested in anchor creation events for logging/metrics
        event.is_anchor_event()
    }
}
