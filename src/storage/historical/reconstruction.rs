use super::HistoricalStorage;
use super::config::MAX_RECONSTRUCTION_DEPTH;
use crate::core::error::{Result, StorageError, TemporalError};
use crate::core::id::VersionId;
use crate::core::property::PropertyMap;
use crate::core::version::{EdgeVersion, NodeVersion, VersionData};
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl HistoricalStorage {
    /// Reconstruct the properties of a node version.
    ///
    /// This walks backward to find the nearest anchor, then applies all deltas
    /// forward to reconstruct the full property state.
    ///
    /// **Cache Behavior**: Properties are cached by VersionId. Since properties are
    /// immutable per version and temporal visibility is checked separately in
    /// `find_node_version_at_time()`, cached properties are always valid and don't
    /// require invalidation when temporal intervals are modified.
    ///
    /// **Depth Limit**: Returns `TemporalError::MaxDepthExceeded` if the delta
    /// chain exceeds `MAX_RECONSTRUCTION_DEPTH` (100). This protects against
    /// stack overflow from corrupted version chains or cycles.
    pub fn reconstruct_node_properties(&self, version_id: VersionId) -> Result<PropertyMap> {
        self.reconstruct_node_properties_with_depth(version_id, 0)
    }

    /// Get a node version from hot or cold storage.
    ///
    /// This is a helper for reconstruction that checks hot storage first (fast path),
    /// then falls back to tiered storage for cold data access.
    ///
    /// Returns `Err(VersionNotFound)` if the version doesn't exist in any tier.
    #[inline]
    pub(crate) fn get_node_version_any_tier(
        &self,
        version_id: VersionId,
    ) -> Result<Arc<NodeVersion>> {
        if let Some(v) = self.node_versions.get(&version_id) {
            // Fast path: version in hot storage
            Ok(Arc::new(v.clone()))
        } else {
            // Slow path: check cold storage via tiered layer
            self.get_node_version_tiered(version_id)?
                .ok_or(StorageError::VersionNotFound(version_id).into())
        }
    }

    /// Get an edge version from hot or cold storage.
    ///
    /// This is a helper for reconstruction that checks hot storage first (fast path),
    /// then falls back to tiered storage for cold data access.
    ///
    /// Returns `Err(VersionNotFound)` if the version doesn't exist in any tier.
    #[inline]
    pub(crate) fn get_edge_version_any_tier(
        &self,
        version_id: VersionId,
    ) -> Result<Arc<EdgeVersion>> {
        if let Some(v) = self.edge_versions.get(&version_id) {
            // Fast path: version in hot storage
            Ok(Arc::new(v.clone()))
        } else {
            // Slow path: check cold storage via tiered layer
            self.get_edge_version_tiered(version_id)?
                .ok_or(StorageError::VersionNotFound(version_id).into())
        }
    }

    /// Iterative property reconstruction helper for nodes (Issue #211).
    ///
    /// This function implements the core iterative reconstruction algorithm.
    /// It eliminates intermediate PropertyMap allocations and stack overflow risks.
    ///
    /// # Algorithm
    /// 1. Collect version IDs backwards from target to anchor (O(anchor_interval) IDs)
    /// 2. Extract anchor properties as base state
    /// 3. Apply deltas in forward order (O(anchor_interval) delta applications)
    ///
    /// # Arguments
    /// * `version_id` - The version to reconstruct properties for
    ///
    /// # Returns
    /// * `Ok(PropertyMap)` - Reconstructed properties
    /// * `Err(TemporalError::MaxDepthExceeded)` - Delta chain too deep (DoS protection)
    /// * `Err(StorageError::VersionNotFound)` - Version not found
    /// * `Err(TemporalError::CorruptedVersionChain)` - Invalid chain structure
    pub(crate) fn reconstruct_node_properties_iterative(
        &self,
        version_id: VersionId,
    ) -> Result<PropertyMap> {
        // Collect version IDs backwards from target to anchor
        // Pre-allocate with anchor_interval capacity to avoid reallocations
        let mut version_ids: Vec<VersionId> =
            Vec::with_capacity(self.config.anchor_interval as usize);
        let mut current_id = version_id;
        let mut chain_length = 0;

        // Walk backwards until we find an anchor or hit depth limit
        loop {
            // Check depth limit for DoS protection
            if chain_length >= self.max_reconstruction_depth {
                let entity_id = self
                    .node_versions
                    .get(&version_id)
                    .map(|v| v.node_id.to_string())
                    .unwrap_or_else(|| format!("version {}", version_id));
                return Err(TemporalError::MaxDepthExceeded {
                    max_depth: MAX_RECONSTRUCTION_DEPTH,
                    entity_id,
                }
                .into());
            }

            let version = self.get_node_version_any_tier(current_id)?;

            let is_anchor = version.is_anchor();
            let prev_id = version.prev_version;

            // Store version ID (we'll process these in reverse)
            version_ids.push(current_id);

            // If we found an anchor, we're done collecting
            if is_anchor {
                break;
            }

            // Get previous version for delta chain traversal
            current_id = prev_id.ok_or_else(|| TemporalError::CorruptedVersionChain {
                entity_id: version.node_id.to_string(),
                reason: "Delta version has no previous version".to_string(),
            })?;

            chain_length += 1;
        }

        // Now reconstruct properties by applying deltas in forward order
        // The last element in version_ids is the anchor (base state)
        let anchor_id =
            version_ids
                .last()
                .copied()
                .ok_or_else(|| TemporalError::CorruptedVersionChain {
                    entity_id: format!("version {}", version_id),
                    reason: "Empty version chain during reconstruction".to_string(),
                })?;

        let anchor_version = self.get_node_version_any_tier(anchor_id)?;

        let mut properties = match &anchor_version.data {
            VersionData::Anchor { properties, .. } => properties.clone(),
            VersionData::Delta { .. } => {
                // This should never happen due to the is_anchor() check above
                return Err(TemporalError::CorruptedVersionChain {
                    entity_id: anchor_version.node_id.to_string(),
                    reason: "Expected anchor at base of version chain".to_string(),
                }
                .into());
            }
        };

        // Apply deltas in forward order (reverse of collection order)
        // Skip the last element (anchor) since we already have its properties
        for &vid in version_ids.iter().rev().skip(1) {
            let version = self.get_node_version_any_tier(vid)?;

            match &version.data {
                VersionData::Delta { delta } => {
                    properties = delta.apply(&properties);
                }
                VersionData::Anchor { .. } => {
                    // This should never happen - only the last element should be an anchor
                    return Err(TemporalError::CorruptedVersionChain {
                        entity_id: version.node_id.to_string(),
                        reason: "Found anchor in middle of delta chain".to_string(),
                    }
                    .into());
                }
            }
        }

        Ok(properties)
    }

    /// Iterative property reconstruction helper for edges (Issue #211).
    ///
    /// Mirrors the node reconstruction algorithm for consistency. See
    /// `reconstruct_node_properties_iterative` for algorithm details.
    pub(crate) fn reconstruct_edge_properties_iterative(
        &self,
        version_id: VersionId,
    ) -> Result<PropertyMap> {
        // Collect version IDs backwards from target to anchor
        // Pre-allocate with anchor_interval capacity to avoid reallocations
        let mut version_ids: Vec<VersionId> =
            Vec::with_capacity(self.config.anchor_interval as usize);
        let mut current_id = version_id;
        let mut chain_length = 0;

        // Walk backwards until we find an anchor or hit depth limit
        loop {
            // Check depth limit for DoS protection
            if chain_length >= self.max_reconstruction_depth {
                let entity_id = self
                    .edge_versions
                    .get(&version_id)
                    .map(|v| v.edge_id.to_string())
                    .unwrap_or_else(|| format!("version {}", version_id));
                return Err(TemporalError::MaxDepthExceeded {
                    max_depth: MAX_RECONSTRUCTION_DEPTH,
                    entity_id,
                }
                .into());
            }

            let version = self.get_edge_version_any_tier(current_id)?;

            let is_anchor = version.is_anchor();
            let prev_id = version.prev_version;

            // Store version ID (we'll process these in reverse)
            version_ids.push(current_id);

            // If we found an anchor, we're done collecting
            if is_anchor {
                break;
            }

            // Get previous version for delta chain traversal
            current_id = prev_id.ok_or_else(|| TemporalError::CorruptedVersionChain {
                entity_id: version.edge_id.to_string(),
                reason: "Delta version has no previous version".to_string(),
            })?;

            chain_length += 1;
        }

        // Now reconstruct properties by applying deltas in forward order
        // The last element in version_ids is the anchor (base state)
        let anchor_id =
            version_ids
                .last()
                .copied()
                .ok_or_else(|| TemporalError::CorruptedVersionChain {
                    entity_id: format!("version {}", version_id),
                    reason: "Empty version chain during reconstruction".to_string(),
                })?;

        let anchor_version = self.get_edge_version_any_tier(anchor_id)?;

        let mut properties = match &anchor_version.data {
            VersionData::Anchor { properties, .. } => properties.clone(),
            VersionData::Delta { .. } => {
                // This should never happen due to the is_anchor() check above
                return Err(TemporalError::CorruptedVersionChain {
                    entity_id: anchor_version.edge_id.to_string(),
                    reason: "Expected anchor at base of version chain".to_string(),
                }
                .into());
            }
        };

        // Apply deltas in forward order (reverse of collection order)
        // Skip the last element (anchor) since we already have its properties
        for &vid in version_ids.iter().rev().skip(1) {
            let version = self.get_edge_version_any_tier(vid)?;

            match &version.data {
                VersionData::Delta { delta } => {
                    properties = delta.apply(&properties);
                }
                VersionData::Anchor { .. } => {
                    // This should never happen - only the last element should be an anchor
                    return Err(TemporalError::CorruptedVersionChain {
                        entity_id: version.edge_id.to_string(),
                        reason: "Found anchor in middle of delta chain".to_string(),
                    }
                    .into());
                }
            }
        }

        Ok(properties)
    }

    /// Internal helper for node property reconstruction with depth tracking.
    ///
    /// Note (Issue #211): The iterative implementation only caches the final
    /// reconstructed PropertyMap, not intermediate versions. This reduces memory
    /// allocations at the cost of slightly lower cache hit rates compared to
    /// the previous recursive approach.
    ///
    /// The depth parameter is kept for API compatibility but unused in the
    /// iterative implementation.
    pub(crate) fn reconstruct_node_properties_with_depth(
        &self,
        version_id: VersionId,
        _depth: usize, // Kept for API compatibility but unused in iterative implementation
    ) -> Result<PropertyMap> {
        // Dual-cache lookup strategy (Improvement #1 & #2: Issue #338)
        //
        // 1. Check regular cache first (holds all versions: anchors + deltas)
        // 2. If not found, check dedicated anchor cache (holds only anchors)
        //
        // Anchors are stored in BOTH caches during pre-population for redundancy:
        // - Regular cache provides fast access when anchor is still in LRU window
        // - Anchor cache acts as fallback when regular cache evicts due to delta pressure
        //
        // This fallback only triggers after regular cache eviction, providing
        // guaranteed O(1) anchor access even under heavy cache pressure.
        if let Some(cached) = self.node_property_cache.get(&version_id) {
            self.primary_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(cached.as_ref().clone());
        }

        // Fallback to dedicated anchor cache (survives delta cache pressure)
        if let Some(cached) = self.node_anchor_cache.get(&version_id) {
            self.anchor_cache_hits.fetch_add(1, Ordering::Relaxed);
            // Re-populate main cache to make this anchor "hot" again
            // This prevents repeatedly falling back to anchor cache for frequently accessed anchors
            self.node_property_cache.insert(version_id, cached.clone());
            return Ok(cached.as_ref().clone());
        }

        // Cache miss - reconstruct properties using iterative helper
        self.full_reconstructions.fetch_add(1, Ordering::Relaxed);

        let properties = self.reconstruct_node_properties_iterative(version_id)?;

        // Populate cache for future reads
        self.node_property_cache
            .insert(version_id, Arc::new(properties.clone()));

        Ok(properties)
    }

    /// Reconstruct the properties of an edge version.
    ///
    /// **Cache Behavior**: Same as `reconstruct_node_properties()` - properties are
    /// immutable per VersionId, so caching doesn't require invalidation.
    ///
    /// **Depth Limit**: Returns `TemporalError::MaxDepthExceeded` if the delta
    /// chain exceeds `MAX_RECONSTRUCTION_DEPTH` (100). This protects against
    /// stack overflow from corrupted version chains or cycles.
    pub fn reconstruct_edge_properties(&self, version_id: VersionId) -> Result<PropertyMap> {
        self.reconstruct_edge_properties_with_depth(version_id, 0)
    }

    /// Internal helper for edge property reconstruction with depth tracking.
    ///
    /// Note (Issue #211): The iterative implementation only caches the final
    /// reconstructed PropertyMap, not intermediate versions. This reduces memory
    /// allocations at the cost of slightly lower cache hit rates compared to
    /// the previous recursive approach.
    ///
    /// The depth parameter is kept for API compatibility but unused in the
    /// iterative implementation.
    pub(crate) fn reconstruct_edge_properties_with_depth(
        &self,
        version_id: VersionId,
        _depth: usize, // Kept for API compatibility but unused in iterative implementation
    ) -> Result<PropertyMap> {
        // Dual-cache lookup strategy (Improvement #1 & #2: Issue #338)
        //
        // 1. Check regular cache first (holds all versions: anchors + deltas)
        // 2. If not found, check dedicated anchor cache (holds only anchors)
        //
        // Anchors are stored in BOTH caches during pre-population for redundancy:
        // - Regular cache provides fast access when anchor is still in LRU window
        // - Anchor cache acts as fallback when regular cache evicts due to delta pressure
        //
        // This fallback only triggers after regular cache eviction, providing
        // guaranteed O(1) anchor access even under heavy cache pressure.
        if let Some(cached) = self.edge_property_cache.get(&version_id) {
            self.primary_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(cached.as_ref().clone());
        }

        // Fallback to dedicated anchor cache (survives delta cache pressure)
        if let Some(cached) = self.edge_anchor_cache.get(&version_id) {
            self.anchor_cache_hits.fetch_add(1, Ordering::Relaxed);
            // Re-populate main cache to make this anchor "hot" again
            // This prevents repeatedly falling back to anchor cache for frequently accessed anchors
            self.edge_property_cache.insert(version_id, cached.clone());
            return Ok(cached.as_ref().clone());
        }

        // Cache miss - reconstruct properties using iterative helper
        self.full_reconstructions.fetch_add(1, Ordering::Relaxed);

        let properties = self.reconstruct_edge_properties_iterative(version_id)?;

        // Populate cache for future reads
        self.edge_property_cache
            .insert(version_id, Arc::new(properties.clone()));

        Ok(properties)
    }
}
