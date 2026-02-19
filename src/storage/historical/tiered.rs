use super::HistoricalStorage;
use crate::core::error::Result;
use crate::core::id::VersionId;
use crate::core::version::{EdgeVersion, NodeVersion};
use crate::storage::migration;
use crate::storage::tiered_storage;
use std::sync::Arc;

impl HistoricalStorage {
    /// Configure tiered storage for this historical storage.
    ///
    /// When tiered storage is configured, versions not found in hot storage
    /// will be looked up from cold storage via the tiered storage layer.
    ///
    /// # Arguments
    ///
    /// * `tiered` - The tiered storage instance to use
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::storage::historical::HistoricalStorage;
    /// use aletheiadb::storage::tiered_storage::TieredStorage;
    /// use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    ///
    /// let mut historical = HistoricalStorage::new();
    /// let cold = RedbColdStorage::with_default_config("data/cold.redb")?;
    /// let tiered = TieredStorage::with_default_config(Box::new(cold));
    /// historical.set_tiered_storage(Arc::new(tiered));
    /// ```
    pub fn set_tiered_storage(&mut self, tiered: Arc<tiered_storage::TieredStorage>) {
        self.tiered_storage = Some(tiered);
    }

    /// Get the tiered storage instance, if configured.
    pub fn tiered_storage(&self) -> Option<&tiered_storage::TieredStorage> {
        self.tiered_storage.as_deref()
    }

    /// Check if tiered storage is enabled.
    pub fn has_tiered_storage(&self) -> bool {
        self.tiered_storage.is_some()
    }

    /// Get a node version from any tier (hot or cold).
    ///
    /// This method first checks hot storage, then falls back to cold storage
    /// via the tiered storage layer (if configured).
    ///
    /// # Arguments
    ///
    /// * `version_id` - The version ID to retrieve
    ///
    /// # Returns
    ///
    /// Returns `Ok(Some(version))` if found in either tier, `Ok(None)` if not found,
    /// or an error if cold storage access fails.
    pub fn get_node_version_tiered(
        &self,
        version_id: VersionId,
    ) -> Result<Option<Arc<NodeVersion>>> {
        // Check hot storage first
        if let Some(version) = self.node_versions.get(&version_id) {
            if let Some(ref tiered) = self.tiered_storage {
                tiered.record_hot_hit();
            }
            return Ok(Some(Arc::new(version.clone())));
        }

        // Fall back to cold storage if tiered storage is configured
        if let Some(ref tiered) = self.tiered_storage {
            return tiered.get_node_version_cold(version_id);
        }

        Ok(None)
    }

    /// Get an edge version from any tier (hot or cold).
    ///
    /// This method first checks hot storage, then falls back to cold storage
    /// via the tiered storage layer (if configured).
    pub fn get_edge_version_tiered(
        &self,
        version_id: VersionId,
    ) -> Result<Option<Arc<EdgeVersion>>> {
        // Check hot storage first
        if let Some(version) = self.edge_versions.get(&version_id) {
            if let Some(ref tiered) = self.tiered_storage {
                tiered.record_hot_hit();
            }
            return Ok(Some(Arc::new(version.clone())));
        }

        // Fall back to cold storage if tiered storage is configured
        if let Some(ref tiered) = self.tiered_storage {
            return tiered.get_edge_version_cold(version_id);
        }

        Ok(None)
    }

    /// Migrate old versions from hot storage to cold storage.
    ///
    /// This method identifies versions that meet the migration policy criteria
    /// and moves them to cold storage. The migration service handles the actual
    /// transfer, and this method removes migrated versions from hot storage.
    ///
    /// # Arguments
    ///
    /// * `migration_service` - The migration service to use for transferring versions
    ///
    /// # Returns
    ///
    /// Returns the number of versions migrated, or an error if migration fails.
    pub fn migrate_to_cold(
        &mut self,
        migration_service: &migration::MigrationService,
    ) -> Result<usize> {
        use std::time::Instant;

        if self.tiered_storage.is_none() {
            return Ok(0);
        }

        let mut total_migrated = 0;

        // Identify node version candidates
        let node_candidates = migration_service.identify_node_candidates(
            &self.node_versions,
            &self.node_version_heads,
            &self.node_version_counts,
            Instant::now(),
        );

        // Collect versions to migrate
        let node_versions_to_migrate: Vec<NodeVersion> = node_candidates
            .iter()
            .filter_map(|c| self.node_versions.get(&c.version_id).cloned())
            .collect();

        // Migrate to cold storage
        if !node_versions_to_migrate.is_empty() {
            let migrated = migration_service.migrate_node_versions(&node_versions_to_migrate)?;
            total_migrated += migrated;

            // Remove migrated versions from hot storage
            for candidate in &node_candidates[..migrated] {
                if let Some(version) = self.node_versions.remove(&candidate.version_id) {
                    // Update version count
                    if let Some(count) = self.node_version_counts.get_mut(&version.node_id) {
                        *count = count.saturating_sub(1);
                    }
                    // Issue #212: Update cached stats counters when migrating to cold storage
                    if version.is_anchor() {
                        self.cached_node_anchor_count =
                            self.cached_node_anchor_count.saturating_sub(1);
                    } else {
                        self.cached_node_delta_count =
                            self.cached_node_delta_count.saturating_sub(1);
                    }
                }
            }
        }

        // Identify edge version candidates
        let edge_candidates = migration_service.identify_edge_candidates(
            &self.edge_versions,
            &self.edge_version_heads,
            &self.edge_version_counts,
            Instant::now(),
        );

        // Collect versions to migrate
        let edge_versions_to_migrate: Vec<EdgeVersion> = edge_candidates
            .iter()
            .filter_map(|c| self.edge_versions.get(&c.version_id).cloned())
            .collect();

        // Migrate to cold storage
        if !edge_versions_to_migrate.is_empty() {
            let migrated = migration_service.migrate_edge_versions(&edge_versions_to_migrate)?;
            total_migrated += migrated;

            // Remove migrated versions from hot storage
            for candidate in &edge_candidates[..migrated] {
                if let Some(version) = self.edge_versions.remove(&candidate.version_id)
                    && let Some(count) = self.edge_version_counts.get_mut(&version.edge_id)
                {
                    *count = count.saturating_sub(1);
                    // Issue #212: Update cached stats counters when migrating to cold storage
                    if version.is_anchor() {
                        self.cached_edge_anchor_count =
                            self.cached_edge_anchor_count.saturating_sub(1);
                    } else {
                        self.cached_edge_delta_count =
                            self.cached_edge_delta_count.saturating_sub(1);
                    }
                }
            }
        }

        Ok(total_migrated)
    }

    /// Get the total number of versions in hot storage.
    pub fn hot_version_count(&self) -> usize {
        self.node_versions.len() + self.edge_versions.len()
    }

    /// Get the estimated memory usage of hot storage in bytes.
    pub fn hot_memory_usage(&self) -> usize {
        let node_size = self.node_versions.len() * std::mem::size_of::<NodeVersion>();
        let edge_size = self.edge_versions.len() * std::mem::size_of::<EdgeVersion>();
        node_size + edge_size
    }
}
