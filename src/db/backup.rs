//! Public backup / restore API for AletheiaDB (issue #3217).

use std::path::Path;

use crate::config::AletheiaDBConfig;
use crate::core::error::{Error, Result};
use crate::db::AletheiaDB;
use crate::storage::backup::{
    BackupError, BackupSummary, build_payload, check_target_empty, materialize_to_dir,
    read_artifact, write_artifact,
};
use crate::storage::index_persistence::PersistenceConfig;
use crate::storage::wal::DurabilityMode;

impl AletheiaDB {
    /// Create a portable backup artifact at `path`.
    ///
    /// The artifact is a single self-contained file containing the complete
    /// bi-temporal state: all current nodes/edges, all version history (including
    /// cold-tier versions), and the string interner table.
    ///
    /// The file is written atomically (temp → rename) so an interrupted backup
    /// never leaves a partial artifact at `path`.
    ///
    /// # Consistency
    ///
    /// Takes an Arc-COW snapshot at the current WAL LSN so that the backup
    /// represents a single consistent point in time — no concurrent write
    /// can appear partially in the artifact.
    ///
    /// # Errors
    ///
    /// Returns `Error::Backup` on any serialization or I/O failure.
    pub fn backup(&self, path: &Path) -> Result<BackupSummary> {
        let source_lsn = self.wal.current_lsn().0;

        // Take consistent point-in-time snapshots.
        let current_snapshot = self
            .current
            .create_snapshot(crate::storage::wal::LSN(source_lsn));
        let (historical_snapshot, tiered_arc) = {
            let hist = self.historical.read();
            let snap = hist.create_snapshot(crate::storage::wal::LSN(source_lsn));
            let tiered = hist.tiered_storage_arc();
            (snap, tiered)
        };

        // Scan cold-tier versions outside the historical lock (disk I/O).
        let (cold_node_versions, cold_edge_versions) = if let Some(tiered) = tiered_arc {
            let cold_nodes = tiered.scan_node_versions_cold().map_err(|e| {
                Error::Backup(BackupError::Io(format!("cold node scan failed: {e}")))
            })?;
            let cold_edges = tiered.scan_edge_versions_cold().map_err(|e| {
                Error::Backup(BackupError::Io(format!("cold edge scan failed: {e}")))
            })?;
            (cold_nodes, cold_edges)
        } else {
            (Vec::new(), Vec::new())
        };

        // Timestamp in microseconds — use system clock for metadata only.
        let created_at_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as i64;

        let payload = build_payload(
            current_snapshot,
            historical_snapshot,
            cold_node_versions,
            cold_edge_versions,
            source_lsn,
            created_at_micros,
        )
        .map_err(Error::Backup)?;

        let summary = BackupSummary {
            node_versions: payload.node_version_count,
            edge_versions: payload.edge_version_count,
            current_node_count: payload.current_node_count,
            current_edge_count: payload.current_edge_count,
            bytes_written: 0, // filled in after write
            source_lsn,
        };

        let bytes_written = write_artifact(&payload, path).map_err(Error::Backup)?;

        Ok(BackupSummary {
            bytes_written,
            ..summary
        })
    }

    /// Restore a backup artifact into a fresh **ephemeral** (in-memory) database.
    ///
    /// The restored database behaves exactly like one created with `AletheiaDB::new()`.
    /// All bi-temporal history is reconstructed from the artifact.
    ///
    /// # Errors
    ///
    /// - `Error::Backup(BackupError::BadMagic)` — file is not a valid backup artifact.
    /// - `Error::Backup(BackupError::IncompatibleVersion { .. })` — artifact format too new.
    /// - `Error::Backup(BackupError::Corrupt(_))` — corrupted or truncated data.
    pub fn restore(path: &Path) -> Result<AletheiaDB> {
        let payload = read_artifact(path).map_err(Error::Backup)?;

        // Materialise into a fresh temp dir, then open via the normal startup path.
        let tmp =
            tempfile::TempDir::new().map_err(|e| Error::Backup(BackupError::Io(e.to_string())))?;

        materialize_to_dir(&payload, tmp.path()).map_err(Error::Backup)?;

        // Use an isolated WAL dir inside the temp dir to avoid cross-test contamination
        // from the default "aletheiadb/wal" path.
        let config = build_restore_config(tmp.path(), Some(tmp.path().join("wal")));
        let mut db = AletheiaDB::with_unified_config(config)?;

        // Keep the temp dir alive for the lifetime of the ephemeral DB.
        db._tempdir = Some(tmp);
        Ok(db)
    }

    /// Restore a backup artifact into a **durable** database at `data_dir`.
    ///
    /// The target directory must be empty (no `indexes/manifest.idx` present)
    /// to prevent overwriting existing data.
    ///
    /// After restoration, the DB is persisted to `data_dir` and can be
    /// reopened with `AletheiaDB::with_unified_config(durable_config_for_data_dir(data_dir))`.
    ///
    /// # Errors
    ///
    /// - `Error::Backup(BackupError::TargetNotEmpty)` — target already has data.
    /// - `Error::Backup(BackupError::BadMagic)` — invalid artifact.
    /// - `Error::Backup(BackupError::IncompatibleVersion { .. })` — format too new.
    pub fn restore_to_data_dir(path: &Path, data_dir: &Path) -> Result<AletheiaDB> {
        check_target_empty(data_dir).map_err(Error::Backup)?;

        let payload = read_artifact(path).map_err(Error::Backup)?;
        materialize_to_dir(&payload, data_dir).map_err(Error::Backup)?;

        let config = build_restore_config(data_dir, Some(data_dir.join("wal")));
        AletheiaDB::with_unified_config(config)
    }
}

/// Build an `AletheiaDBConfig` that loads from an existing persistence directory.
///
/// * `persistence_dir` — base dir for `IndexPersistenceManager` (contains `indexes/`).
/// * `wal_dir` — WAL directory; if `None` a fresh in-memory-only WAL is used.
fn build_restore_config(
    persistence_dir: &Path,
    wal_dir: Option<std::path::PathBuf>,
) -> AletheiaDBConfig {
    use crate::config::WalConfigBuilder;

    let wal_builder = if let Some(dir) = wal_dir {
        WalConfigBuilder::new()
            .wal_dir(dir)
            .durability_mode(DurabilityMode::GroupCommit {
                max_delay_ms: 10,
                max_batch_size: 200,
            })
    } else {
        WalConfigBuilder::new()
    };

    AletheiaDBConfig::builder()
        .wal(wal_builder.build())
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: persistence_dir.to_path_buf(),
            load_on_startup: true,
            ..Default::default()
        })
        .build()
}
