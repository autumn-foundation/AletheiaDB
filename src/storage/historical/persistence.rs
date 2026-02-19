use super::HistoricalStorage;
use crate::core::error::Result;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::temporal::TIMESTAMP_MAX;
use crate::core::version::{EdgeVersion, NodeVersion};
use std::collections::HashMap;

impl HistoricalStorage {
    /// Reserve capacity for batch restoration from persistence.
    ///
    /// Pre-allocating capacity improves restoration performance by reducing
    /// reallocations during bulk insertion. Call this before restoring
    /// persisted versions.
    ///
    /// # Arguments
    ///
    /// * `node_versions` - Expected number of node versions to restore
    /// * `edge_versions` - Expected number of edge versions to restore
    pub(crate) fn reserve_restoration_capacity(
        &mut self,
        node_versions: usize,
        edge_versions: usize,
    ) {
        self.node_versions.reserve(node_versions);
        self.edge_versions.reserve(edge_versions);
        // Conservatively estimate unique entities as half of versions
        // (typical case: each entity has ~2 versions on average)
        self.node_version_heads.reserve(node_versions / 2);
        self.edge_version_heads.reserve(edge_versions / 2);
        self.node_version_counts.reserve(node_versions / 2);
        self.edge_version_counts.reserve(edge_versions / 2);
    }

    /// Insert a restored node version directly into storage.
    ///
    /// This is used during index loading to restore persisted versions.
    /// Unlike normal version insertion, this bypasses transaction processing
    /// since the data comes from a trusted source (our own persistence layer).
    ///
    /// # Errors
    ///
    /// Returns an error if the version ID or node ID is invalid.
    pub(crate) fn insert_restored_node_version(&mut self, version: NodeVersion) -> Result<()> {
        let version_id = version.id;
        let node_id = version.node_id;
        let is_anchor = version.is_anchor();

        // Store the version
        self.node_versions.insert(version_id, version);

        // Update version head
        self.node_version_heads.insert(node_id, version_id);

        // Update version count
        *self.node_version_counts.entry(node_id).or_insert(0) += 1;

        // Issue #212: Update cached stats counters during persistence restore
        if is_anchor {
            self.cached_node_anchor_count += 1;
        } else {
            self.cached_node_delta_count += 1;
        }

        Ok(())
    }

    /// Insert a restored edge version directly into storage.
    ///
    /// This is used during index loading to restore persisted versions.
    /// Unlike normal version insertion, this bypasses transaction processing
    /// since the data comes from a trusted source (our own persistence layer).
    ///
    /// # Errors
    ///
    /// Returns an error if the version ID or edge ID is invalid.
    pub(crate) fn insert_restored_edge_version(&mut self, version: EdgeVersion) -> Result<()> {
        let version_id = version.id;
        let edge_id = version.edge_id;
        let is_anchor = version.is_anchor();

        // Store the version
        self.edge_versions.insert(version_id, version);

        // Update version head
        self.edge_version_heads.insert(edge_id, version_id);

        // Update version count
        *self.edge_version_counts.entry(edge_id).or_insert(0) += 1;

        // Issue #212: Update cached stats counters during persistence restore
        if is_anchor {
            self.cached_edge_anchor_count += 1;
        } else {
            self.cached_edge_delta_count += 1;
        }

        Ok(())
    }

    /// Rebuild version chains after restoration from persistence.
    ///
    /// This method reconstructs the `prev_version` and `next_version` links for all
    /// versions, and ensures version heads point to the correct (latest tx_time) version.
    /// Must be called after all versions have been inserted via `insert_restored_node_version`
    /// and `insert_restored_edge_version`.
    ///
    /// # Version Chain Semantics
    ///
    /// - Versions are ordered by transaction time (tx_time start)
    /// - `prev_version` points to the temporally previous version (earlier tx_time)
    /// - `next_version` points to the temporally next version (later tx_time)
    /// - Version heads point to the version with the latest tx_time
    pub(crate) fn rebuild_version_chains(&mut self) {
        // === Rebuild node version chains ===

        // Group versions by node ID
        let mut node_versions_by_id: HashMap<NodeId, Vec<VersionId>> = HashMap::new();
        for (vid, version) in &self.node_versions {
            node_versions_by_id
                .entry(version.node_id)
                .or_default()
                .push(*vid);
        }

        // For each node, sort versions by tx_time and link them
        for (node_id, mut version_ids) in node_versions_by_id {
            // Sort by transaction time start (ascending order)
            // Phase 2: Use TIMESTAMP_MAX instead of i64::MAX
            version_ids.sort_by_key(|vid| {
                self.node_versions
                    .get(vid)
                    .map(|v| v.temporal.transaction_time().start())
                    .unwrap_or(TIMESTAMP_MAX)
            });

            // Link prev/next
            for i in 0..version_ids.len() {
                let vid = version_ids[i];

                // Link to previous version (earlier in time)
                let prev = if i > 0 {
                    Some(version_ids[i - 1])
                } else {
                    None
                };

                // Link to next version (later in time)
                let next = if i < version_ids.len() - 1 {
                    Some(version_ids[i + 1])
                } else {
                    None
                };

                if let Some(version) = self.node_versions.get_mut(&vid) {
                    version.prev_version = prev;
                    version.next_version = next;
                }
            }

            // Set head to the latest version (last in sorted order)
            if let Some(&latest_vid) = version_ids.last() {
                self.node_version_heads.insert(node_id, latest_vid);

                // Issue #208: Rebuild counter cache for anchor interval checks
                // Count versions since last anchor by walking backwards from head
                let mut count = 0;
                let mut current_id = latest_vid;

                while let Some(version) = self.node_versions.get(&current_id) {
                    if version.is_anchor() {
                        // Found anchor, counter is 0
                        break;
                    }
                    // Delta version, increment counter and continue
                    count += 1;
                    if let Some(prev_id) = version.prev_version {
                        current_id = prev_id;
                    } else {
                        // No more versions (shouldn't happen, first is always anchor)
                        break;
                    }
                }

                self.node_versions_since_anchor.insert(node_id, count);
            }
        }

        // === Rebuild edge version chains ===

        // Group versions by edge ID
        let mut edge_versions_by_id: HashMap<EdgeId, Vec<VersionId>> = HashMap::new();
        for (vid, version) in &self.edge_versions {
            edge_versions_by_id
                .entry(version.edge_id)
                .or_default()
                .push(*vid);
        }

        // For each edge, sort versions by tx_time and link them
        for (edge_id, mut version_ids) in edge_versions_by_id {
            // Sort by transaction time start (ascending order)
            // Phase 2: Use TIMESTAMP_MAX (already imported above)
            version_ids.sort_by_key(|vid| {
                self.edge_versions
                    .get(vid)
                    .map(|v| v.temporal.transaction_time().start())
                    .unwrap_or(TIMESTAMP_MAX)
            });

            // Link prev/next
            for i in 0..version_ids.len() {
                let vid = version_ids[i];

                // Link to previous version (earlier in time)
                let prev = if i > 0 {
                    Some(version_ids[i - 1])
                } else {
                    None
                };

                // Link to next version (later in time)
                let next = if i < version_ids.len() - 1 {
                    Some(version_ids[i + 1])
                } else {
                    None
                };

                if let Some(version) = self.edge_versions.get_mut(&vid) {
                    version.prev_version = prev;
                    version.next_version = next;
                }
            }

            // Set head to the latest version (last in sorted order)
            if let Some(&latest_vid) = version_ids.last() {
                self.edge_version_heads.insert(edge_id, latest_vid);

                // Issue #208: Rebuild counter cache for anchor interval checks
                // Count versions since last anchor by walking backwards from head
                let mut count = 0;
                let mut current_id = latest_vid;

                while let Some(version) = self.edge_versions.get(&current_id) {
                    if version.is_anchor() {
                        // Found anchor, counter is 0
                        break;
                    }
                    // Delta version, increment counter and continue
                    count += 1;
                    if let Some(prev_id) = version.prev_version {
                        current_id = prev_id;
                    } else {
                        // No more versions (shouldn't happen, first is always anchor)
                        break;
                    }
                }

                self.edge_versions_since_anchor.insert(edge_id, count);
            }
        }
    }

    /// Create an MVCC snapshot of historical storage at the specified LSN.
    ///
    /// This provides snapshot isolation for checkpoint operations, capturing
    /// all node and edge versions at a consistent point in time.
    ///
    /// # Snapshot Isolation
    ///
    /// The snapshot captures Arc references to all versions. Concurrent
    /// modifications after snapshot creation do NOT affect the snapshot's
    /// iteration.
    ///
    /// # Memory Overhead
    ///
    /// - Iterates once over version HashMaps to collect Arc references
    /// - Memory: ~8 bytes per version (just Arc pointers)
    /// - For 10M versions: ~80MB overhead
    ///
    /// # Arguments
    ///
    /// * `lsn` - LSN at which snapshot is taken (for tracking)
    ///
    /// # Returns
    ///
    /// A snapshot that provides isolated iteration over versions.
    pub fn create_snapshot(
        &self,
        lsn: crate::storage::wal::LSN,
    ) -> crate::storage::snapshot::HistoricalStorageSnapshot {
        use crate::storage::snapshot::HistoricalStorageSnapshot;
        use std::sync::Arc;

        // Collect Arc references to all node versions
        let node_versions: Vec<Arc<NodeVersion>> = self
            .node_versions
            .values()
            .map(|version| Arc::new(version.clone()))
            .collect();

        // Collect Arc references to all edge versions
        let edge_versions: Vec<Arc<EdgeVersion>> = self
            .edge_versions
            .values()
            .map(|version| Arc::new(version.clone()))
            .collect();

        HistoricalStorageSnapshot::new(lsn, node_versions, edge_versions)
    }

    /// **Test-only helper**: Remove a node version from hot storage.
    ///
    /// This is used in tests to simulate version migration to cold storage.
    /// In production, versions are migrated by the `MigrationService` which
    /// atomically moves versions from hot to cold storage.
    ///
    /// # Safety
    /// This method directly modifies internal state and should only be used
    /// in tests. It does not update caches or notify observers.
    #[doc(hidden)]
    pub fn __test_remove_node_version(&mut self, version_id: VersionId) {
        self.node_versions.remove(&version_id);
    }

    /// **Test-only helper**: Clear the property reconstruction cache.
    ///
    /// This is used in tests to force actual property reconstruction instead
    /// of returning cached values. This is essential for testing that reconstruction
    /// works correctly when versions are in cold storage.
    ///
    /// # Safety
    /// This method clears caches and should only be used in tests where you
    /// want to verify reconstruction behavior without cache interference.
    #[doc(hidden)]
    pub fn __test_clear_property_cache(&self) {
        self.node_property_cache.clear();
        self.node_anchor_cache.clear();
    }
}
