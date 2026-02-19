use super::HistoricalStorage;
use super::config::{AnchorHookContext, PreAnchorHook};
use crate::core::error::{Result, StorageError};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::observer::{StorageEvent, notify_observers};
use crate::core::property::PropertyMap;
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use crate::core::version::{EdgeVersion, EntityVersion, NodeVersion, TemporalVersion, VersionData};
use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

impl HistoricalStorage {
    /// Add a new version of a node.
    ///
    /// This will automatically determine whether to create an anchor or delta
    /// based on the version chain length.
    /// Returns an error if the version limit for this entity is exceeded (DoS protection).
    #[allow(clippy::too_many_arguments)]
    pub fn add_node_version(
        &mut self,
        node_id: NodeId,
        version_id: VersionId,
        valid_from: Timestamp,
        tx_time: Timestamp,
        label: InternedString,
        properties: PropertyMap,
        is_tombstone: bool,
    ) -> Result<()> {
        // Construct bi-temporal interval from separate dimensions
        let mut temporal = BiTemporalInterval::with_valid_time(valid_from, tx_time);

        // For tombstones, close the valid_time at valid_from to create an empty interval [valid_from, valid_from)
        // This represents "entity is no longer valid starting from this point"
        if is_tombstone {
            temporal = temporal.close_valid_time(valid_from)?;
        }

        // Check capacity limit using cached count (O(1) operation, DoS protection)
        let version_count = self.node_version_counts.get(&node_id).copied().unwrap_or(0);
        if version_count >= self.retention_policy.max_versions_per_entity {
            return Err(StorageError::CapacityExceeded {
                resource: format!("node {} versions", node_id),
                current: version_count,
                limit: self.retention_policy.max_versions_per_entity,
            }
            .into());
        }

        // Check if this node already has versions
        let prev_version_id = self.node_version_heads.get(&node_id).copied();

        // Create version (anchor or delta based on chain length)
        let mut version = if let Some(prev_id) = prev_version_id {
            // Verify previous version exists (properties reconstructed later via reconstruct_node_properties)
            if !self.node_versions.contains_key(&prev_id) {
                return Err(StorageError::VersionNotFound(prev_id).into());
            }

            // Get cached counter and increment (O(1) instead of O(anchor_interval))
            // Issue #208: Use cached counter to avoid walking version chain
            let current_count = self
                .node_versions_since_anchor
                .get(&node_id)
                .copied()
                .unwrap_or(0);
            let versions_since_anchor = current_count + 1;

            if versions_since_anchor >= self.config.anchor_interval as usize {
                // Create anchor with link to previous version
                // Use properties.clone() here as we need original for caching later
                let mut anchor = NodeVersion::new_anchor(
                    version_id,
                    node_id,
                    temporal,
                    label,
                    properties.clone(),
                );
                anchor.prev_version = Some(prev_id);
                // Reset counter to 0 after creating anchor
                self.node_versions_since_anchor.insert(node_id, 0);
                anchor
            } else {
                // Create delta from previous version
                let old_properties = self.reconstruct_node_properties(prev_id)?;
                // Update counter for next iteration
                self.node_versions_since_anchor
                    .insert(node_id, versions_since_anchor);
                NodeVersion::new_delta(
                    version_id,
                    node_id,
                    temporal,
                    label,
                    &old_properties,
                    &properties,
                    prev_id,
                )
            }
        } else {
            // First version is always an anchor
            // Initialize counter to 0
            self.node_versions_since_anchor.insert(node_id, 0);
            NodeVersion::new_anchor(version_id, node_id, temporal, label, properties.clone())
        };

        // Handle pre-anchor hook (BEFORE storing)
        if version.is_anchor() {
            Self::handle_pre_anchor_hook(
                AnchorHookContext {
                    entity_type: "node",
                    entity_id: node_id.as_u64(),
                    timestamp: temporal.transaction_time().start(),
                    properties: &properties,
                },
                &mut version.data,
                &self.pre_node_anchor_hook,
            );
        }

        // Link previous version to this one and close its temporal intervals
        if let Some(prev_id) = prev_version_id
            && let Some(prev) = self.node_versions.get_mut(&prev_id)
        {
            // Capture the intervals before modification for temporal index update
            let old_temporal = *prev.temporal();

            Self::close_previous_version_intervals(prev, version_id, &temporal)?;

            // Update temporal indexes to reflect the closed intervals (Issue #209)
            if let Some(ref indexes) = self.temporal_indexes {
                let new_temporal = *prev.temporal();

                // Update valid time end if it was closed
                if old_temporal.valid_time().end() != new_temporal.valid_time().end() {
                    indexes.update_node_valid_time_end(
                        node_id,
                        prev_id,
                        new_temporal.valid_time().end(),
                    );
                }

                // Update transaction time end if it was closed
                if old_temporal.transaction_time().end() != new_temporal.transaction_time().end() {
                    indexes.update_node_transaction_time_end(
                        node_id,
                        prev_id,
                        new_temporal.transaction_time().end(),
                    );
                }
            }
        }

        // Check if anchor before storing (for notifications and caching)
        let is_anchor = version.is_anchor();

        // Store the version and update indexes
        self.node_versions.insert(version_id, version);
        self.node_version_heads.insert(node_id, version_id);
        *self.node_version_counts.entry(node_id).or_insert(0) += 1;

        // Issue #212: Update cached stats counters for O(1) stats() retrieval
        if is_anchor {
            self.cached_node_anchor_count += 1;
        } else {
            self.cached_node_delta_count += 1;
        }

        // Issue #210: Cache properties for ALL versions (anchors and deltas) to avoid
        // reconstructing properties we just added when creating the next delta.
        //
        // BEFORE: Only anchors were cached, causing delta creation to reconstruct
        //         the previous delta's properties even though we just added them.
        // AFTER:  All versions are cached in the main property cache, eliminating
        //         unnecessary reconstructions during consecutive writes.
        let props_arc = Arc::new(properties);
        self.node_property_cache
            .insert(version_id, props_arc.clone());

        // Anchors are also cached in the dedicated anchor cache for fallback
        if is_anchor {
            self.node_anchor_cache.insert(version_id, props_arc);
        }

        // Notify observers
        let timestamp = temporal.transaction_time().start();
        notify_observers(
            &self.observers,
            &StorageEvent::NodeVersionCreated {
                version_id,
                node_id,
                timestamp,
                is_anchor,
            },
        );
        if is_anchor {
            notify_observers(
                &self.observers,
                &StorageEvent::NodeAnchorCreated {
                    version_id,
                    node_id,
                    timestamp,
                },
            );
        }

        Ok(())
    }

    /// Add a new version of an edge.
    /// Returns an error if the version limit for this entity is exceeded (DoS protection).
    #[allow(clippy::too_many_arguments)]
    pub fn add_edge_version(
        &mut self,
        edge_id: EdgeId,
        version_id: VersionId,
        valid_from: Timestamp,
        tx_time: Timestamp,
        label: InternedString,
        source: NodeId,
        target: NodeId,
        properties: PropertyMap,
        is_tombstone: bool,
    ) -> Result<()> {
        // Construct bi-temporal interval from separate dimensions
        let mut temporal = BiTemporalInterval::with_valid_time(valid_from, tx_time);

        // For tombstones, close the valid_time at valid_from to create an empty interval [valid_from, valid_from)
        // This represents "entity is no longer valid starting from this point"
        if is_tombstone {
            temporal = temporal.close_valid_time(valid_from)?;
        }

        // Check capacity limit using cached count (O(1) operation, DoS protection)
        let version_count = self.edge_version_counts.get(&edge_id).copied().unwrap_or(0);
        if version_count >= self.retention_policy.max_versions_per_entity {
            return Err(StorageError::CapacityExceeded {
                resource: format!("edge {} versions", edge_id),
                current: version_count,
                limit: self.retention_policy.max_versions_per_entity,
            }
            .into());
        }

        // Check if this edge already has versions
        let prev_version_id = self.edge_version_heads.get(&edge_id).copied();

        // Create version (anchor or delta based on chain length)
        let mut version = if let Some(prev_id) = prev_version_id {
            // Verify previous version exists (properties reconstructed later via reconstruct_edge_properties)
            if !self.edge_versions.contains_key(&prev_id) {
                return Err(StorageError::VersionNotFound(prev_id).into());
            }

            // Get cached counter and increment (O(1) instead of O(anchor_interval))
            // Issue #208: Use cached counter to avoid walking version chain
            let current_count = self
                .edge_versions_since_anchor
                .get(&edge_id)
                .copied()
                .unwrap_or(0);
            let versions_since_anchor = current_count + 1;

            if versions_since_anchor >= self.config.anchor_interval as usize {
                // Create anchor with link to previous version
                // Use properties.clone() here as we need original for caching later
                let mut anchor = EdgeVersion::new_anchor(
                    version_id,
                    edge_id,
                    temporal,
                    label,
                    source,
                    target,
                    properties.clone(),
                );
                anchor.prev_version = Some(prev_id);
                // Reset counter to 0 after creating anchor
                self.edge_versions_since_anchor.insert(edge_id, 0);
                anchor
            } else {
                // Create delta from previous version
                let old_properties = self.reconstruct_edge_properties(prev_id)?;
                // Update counter for next iteration
                self.edge_versions_since_anchor
                    .insert(edge_id, versions_since_anchor);
                EdgeVersion::new_delta(
                    version_id,
                    edge_id,
                    temporal,
                    label,
                    source,
                    target,
                    &old_properties,
                    &properties,
                    prev_id,
                )
            }
        } else {
            // First version is always an anchor
            // Initialize counter to 0
            self.edge_versions_since_anchor.insert(edge_id, 0);
            EdgeVersion::new_anchor(
                version_id,
                edge_id,
                temporal,
                label,
                source,
                target,
                properties.clone(),
            )
        };

        // Handle pre-anchor hook (BEFORE storing)
        if version.is_anchor() {
            Self::handle_pre_anchor_hook(
                AnchorHookContext {
                    entity_type: "edge",
                    entity_id: edge_id.as_u64(),
                    timestamp: temporal.transaction_time().start(),
                    properties: &properties,
                },
                &mut version.data,
                &self.pre_edge_anchor_hook,
            );
        }

        // Link previous version to this one and close its temporal intervals
        if let Some(prev_id) = prev_version_id
            && let Some(prev) = self.edge_versions.get_mut(&prev_id)
        {
            // Capture the intervals before modification for temporal index update
            let old_temporal = *prev.temporal();

            Self::close_previous_version_intervals(prev, version_id, &temporal)?;

            // Update temporal indexes to reflect the closed intervals (Issue #209)
            if let Some(ref indexes) = self.temporal_indexes {
                let new_temporal = *prev.temporal();

                // Update valid time end if it was closed
                if old_temporal.valid_time().end() != new_temporal.valid_time().end() {
                    indexes.update_edge_valid_time_end(
                        edge_id,
                        prev_id,
                        new_temporal.valid_time().end(),
                    );
                }

                // Update transaction time end if it was closed
                if old_temporal.transaction_time().end() != new_temporal.transaction_time().end() {
                    indexes.update_edge_transaction_time_end(
                        edge_id,
                        prev_id,
                        new_temporal.transaction_time().end(),
                    );
                }
            }

            // Update temporal adjacency index to reflect closed valid time
            if let Some(ref adj_index) = self.temporal_adjacency_index {
                let new_temporal = *prev.temporal();
                if old_temporal.valid_time().end() != new_temporal.valid_time().end() {
                    adj_index.close_edge_valid_time(
                        edge_id,
                        source,
                        target,
                        new_temporal.valid_time().end(),
                    );
                }
            }
        }

        // Check if anchor before storing (for notifications and caching)
        let is_anchor = version.is_anchor();

        // Store the version and update indexes
        self.edge_versions.insert(version_id, version);
        self.edge_version_heads.insert(edge_id, version_id);
        *self.edge_version_counts.entry(edge_id).or_insert(0) += 1;

        // Issue #212: Update cached stats counters for O(1) stats() retrieval
        if is_anchor {
            self.cached_edge_anchor_count += 1;
        } else {
            self.cached_edge_delta_count += 1;
        }

        // Issue #210: Cache properties for ALL versions (anchors and deltas) to avoid
        // reconstructing properties we just added when creating the next delta.
        let props_arc = Arc::new(properties);
        self.edge_property_cache
            .insert(version_id, props_arc.clone());

        // Anchors are also cached in the dedicated anchor cache for fallback
        if is_anchor {
            self.edge_anchor_cache.insert(version_id, props_arc);
        }

        // Notify observers
        let timestamp = temporal.transaction_time().start();
        notify_observers(
            &self.observers,
            &StorageEvent::EdgeVersionCreated {
                version_id,
                edge_id,
                timestamp,
                is_anchor,
            },
        );
        if is_anchor {
            notify_observers(
                &self.observers,
                &StorageEvent::EdgeAnchorCreated {
                    version_id,
                    edge_id,
                    timestamp,
                },
            );
        }

        // Update temporal adjacency index if configured
        // Insert after all operations complete so temporal intervals are finalized
        // Skip tombstones - they represent deletions and shouldn't appear in traversal queries
        if !is_tombstone
            && let Some(ref adj_index) = self.temporal_adjacency_index
            && let Err(_e) = adj_index.insert_edge(
                edge_id,
                source,
                target,
                label,
                temporal.valid_time().start(),
                temporal.valid_time().end(),
                temporal.transaction_time().start(),
                temporal.transaction_time().end(),
            )
        {
            #[cfg(feature = "observability")]
            tracing::warn!(
                edge_id = %edge_id,
                source = %source,
                target = %target,
                error = %_e,
                "Failed to insert edge into temporal adjacency index"
            );
        }

        Ok(())
    }

    /// Close the transaction time of a node version.
    ///
    /// This marks the version as no longer being the "current knowledge" after
    /// the specified timestamp. Used when a node is deleted or superseded.
    ///
    /// # Arguments
    /// * `version_id` - The version to close
    /// * `end_timestamp` - The timestamp at which this version is no longer valid
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err` if version not found
    pub fn close_node_version_transaction_time(
        &mut self,
        version_id: VersionId,
        end_timestamp: Timestamp,
    ) -> Result<()> {
        let version = self
            .node_versions
            .get_mut(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        // Get the node ID before closing (needed for temporal index update)
        let node_id = version.node_id;

        // Use TemporalVersion trait method
        version.close_transaction_time(end_timestamp)?;

        // Update temporal index to reflect the closed interval (Issue #209)
        if let Some(ref indexes) = self.temporal_indexes {
            indexes.update_node_transaction_time_end(node_id, version_id, end_timestamp);
        }

        Ok(())
    }

    /// Close the transaction time of an edge version.
    ///
    /// This marks the version as no longer being the "current knowledge" after
    /// the specified timestamp. Used when an edge is deleted or superseded.
    pub fn close_edge_version_transaction_time(
        &mut self,
        version_id: VersionId,
        end_timestamp: Timestamp,
    ) -> Result<()> {
        let version = self
            .edge_versions
            .get_mut(&version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        // Get the edge ID and node IDs before closing (needed for index updates)
        let edge_id = version.edge_id;
        let source = version.source;
        let target = version.target;

        // Use TemporalVersion trait method
        version.close_transaction_time(end_timestamp)?;

        // Update temporal index to reflect the closed interval (Issue #209)
        if let Some(ref indexes) = self.temporal_indexes {
            indexes.update_edge_transaction_time_end(edge_id, version_id, end_timestamp);
        }

        // Update temporal adjacency index to reflect the closed transaction time
        if let Some(ref adj_index) = self.temporal_adjacency_index {
            adj_index.close_edge_transaction_time(edge_id, source, target, end_timestamp);
        }

        Ok(())
    }

    /// Handle pre-anchor hook invocation with proper logging.
    ///
    /// This helper method encapsulates the common pattern of calling pre-anchor hooks
    /// and handling their results (success with snapshot ID, success without snapshot,
    /// or graceful degradation on failure).
    pub(crate) fn handle_pre_anchor_hook(
        context: AnchorHookContext<'_>,
        version_data: &mut VersionData,
        hook: &Option<PreAnchorHook>,
    ) {
        if let Some(hook_fn) = hook {
            match hook_fn(
                context.entity_type,
                context.entity_id,
                context.timestamp,
                context.properties,
            ) {
                Ok(Some(snapshot_id)) => {
                    version_data.set_vector_snapshot_id(snapshot_id);
                    #[cfg(feature = "observability")]
                    tracing::debug!(
                        "Pre-anchor hook returned snapshot ID {} for {} {}",
                        snapshot_id,
                        context.entity_type,
                        context.entity_id
                    );
                }
                Ok(None) => {
                    #[cfg(feature = "observability")]
                    tracing::debug!(
                        "Pre-anchor hook returned None for {} {} (no snapshot needed)",
                        context.entity_type,
                        context.entity_id
                    );
                }
                Err(_e) => {
                    #[cfg(feature = "observability")]
                    tracing::warn!(
                        "Pre-anchor hook failed for {} {} at timestamp {}: {} (anchor will still be created)",
                        context.entity_type,
                        context.entity_id,
                        context.timestamp,
                        _e
                    );
                }
            }
        }
    }

    /// Close the temporal intervals of a previous version when a new version is created.
    ///
    /// This helper handles the common logic of linking versions together and closing
    /// the temporal intervals of the previous version at the new version's start time.
    pub(crate) fn close_previous_version_intervals<V: EntityVersion>(
        prev_version: &mut V,
        new_version_id: VersionId,
        new_temporal: &BiTemporalInterval,
    ) -> Result<()> {
        prev_version.set_next_version(Some(new_version_id));

        // Work on a local copy, apply modifications, then write back
        let mut prev_temporal = *prev_version.temporal();

        if prev_temporal.is_currently_valid()
            && new_temporal.valid_time().start() > prev_temporal.valid_time().start()
        {
            prev_temporal = prev_temporal.close_valid_time(new_temporal.valid_time().start())?;
        }

        if prev_temporal.is_currently_recorded()
            && new_temporal.transaction_time().start() > prev_temporal.transaction_time().start()
        {
            prev_temporal =
                prev_temporal.close_transaction_time(new_temporal.transaction_time().start())?;
        }

        *prev_version.temporal_mut() = prev_temporal;
        Ok(())
    }
}
