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
        //
        // Issue #3425: the current-storage snapshot is taken INSIDE the
        // `historical.read()` guard (and under `snapshot_lock`, mirroring
        // `create_checkpoint`'s locking contract) rather than before/outside it.
        // A committing writer holds `historical.write()` across its
        // `commit_timestamp` finalization, so acquiring `historical.read()` here
        // excludes that window: without this, the backup could clone a committed
        // node/edge whose `commit_timestamp` is still `None` (an in-memory
        // finalize-window artifact). Lock order is preserved
        // (`historical` -> `snapshot_lock` -> DashMap shards); `create_snapshot`
        // takes no `historical`/`wal`/`current_timestamp` lock, so no inversion.
        let (current_snapshot, historical_snapshot, tiered_arc) = {
            let hist = self.historical.read();
            let current_snapshot = {
                let _snap_lock = self.current.snapshot_lock.write();
                self.current
                    .create_snapshot(crate::storage::wal::LSN(source_lsn))
            };
            let historical_snapshot = hist.create_snapshot(crate::storage::wal::LSN(source_lsn));
            let tiered = hist.tiered_storage_arc();
            (current_snapshot, historical_snapshot, tiered)
        };

        // Test-only seam (Issue #3425): lets the backup-path regression test inspect
        // the exact current-storage snapshot this backup captured, so it can prove
        // the snapshot never contains a committed node with `commit_timestamp: None`
        // even when a concurrent writer is parked in the finalize window. Production
        // builds compile this away.
        #[cfg(test)]
        backup_test_hooks::run_post_current_snapshot_hook(&current_snapshot);

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

        // Capture declared schema constraints (Issue #3378) and uniqueness
        // constraints (Issue #3218) so both survive the backup→restore
        // round-trip. Uniqueness constraints are otherwise only WAL-persisted,
        // so a fresh-WAL restore would silently drop them; they are captured
        // here and re-declared on restore (see `reapply_unique_constraints`),
        // which rebuilds the reservation index and writes a durable WAL record.
        let schema_constraints = self.constraint_registry.export_schema_constraints();
        let unique_constraints = self.constraint_registry.export_unique_constraints();

        let payload = build_payload(
            current_snapshot,
            historical_snapshot,
            cold_node_versions,
            cold_edge_versions,
            source_lsn,
            created_at_micros,
            schema_constraints,
            unique_constraints,
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

        // Restore schema constraints (Issue #3378) by writing the sidecar into
        // the data dir (parent of the reopen WAL dir = tmp root), so the
        // reopen below loads them through the normal startup sidecar path.
        crate::db::schema_constraint::persist_descriptors_to_dir(
            tmp.path(),
            &payload.schema_constraints,
        )?;

        // Use an isolated WAL dir inside the temp dir to avoid cross-test contamination
        // from the default "aletheiadb/wal" path.
        let config = build_restore_config(tmp.path(), tmp.path().join("wal"));
        let mut db = AletheiaDB::with_unified_config(config)?;

        // Re-declare uniqueness constraints (Issue #3218) after the data is
        // loaded: unlike the schema-constraint sidecar (loaded during startup
        // above), uniqueness constraints are WAL-persisted, so they must be
        // re-declared against the reopened DB — which rebuilds the reservation
        // index from the restored nodes and writes a durable WAL record.
        reapply_unique_constraints(&db, &payload.unique_constraints)?;

        // Keep the temp dir alive for the lifetime of the ephemeral DB.
        db._tempdir = Some(tmp);
        Ok(db)
    }

    /// Restore a backup artifact into a **durable** database at `data_dir`.
    ///
    /// The target directory must be empty (no index manifest present) to
    /// prevent overwriting existing data.
    ///
    /// After restoration, the DB is persisted under `data_dir` in the canonical
    /// durable layout and can be reopened with either
    /// [`AletheiaDB::open`]`(data_dir)` /
    /// [`AletheiaDB::open_from_env`] or the equivalent
    /// `AletheiaDB::with_unified_config(durable_config_for_data_dir(data_dir))`.
    ///
    /// The restored index artifacts are materialised at the same on-disk depth
    /// that [`crate::config::durable_config_for_data_dir`] reads them from: the
    /// index-persistence root is `data_dir/indexes` (the value that config
    /// assigns to `PersistenceConfig::data_dir`), and the
    /// `IndexPersistenceManager` appends its own `indexes/` beneath that base.
    /// Writing at any other depth (e.g. using `data_dir` directly as the base)
    /// leaves the manifest where `open`/`open_from_env` cannot find it, so a
    /// reopen silently comes up empty.
    ///
    /// # Errors
    ///
    /// - `Error::Backup(BackupError::TargetNotEmpty)` — target already has data.
    /// - `Error::Backup(BackupError::BadMagic)` — invalid artifact.
    /// - `Error::Backup(BackupError::IncompatibleVersion { .. })` — format too new.
    pub fn restore_to_data_dir(path: &Path, data_dir: &Path) -> Result<AletheiaDB> {
        // Canonical index-persistence root: the same base that
        // `durable_config_for_data_dir` assigns to `PersistenceConfig::data_dir`
        // (the `IndexPersistenceManager` appends its own `indexes/` under this).
        let index_root = index_persistence_root(data_dir);

        check_target_empty(&index_root).map_err(Error::Backup)?;

        let payload = read_artifact(path).map_err(Error::Backup)?;
        materialize_to_dir(&payload, &index_root).map_err(Error::Backup)?;

        // Restore schema constraints (Issue #3378): the durable reopen derives
        // its data dir as the parent of its WAL dir (`data_dir/wal` → `data_dir`),
        // so the sidecar must land at `data_dir`, not the index root.
        crate::db::schema_constraint::persist_descriptors_to_dir(
            data_dir,
            &payload.schema_constraints,
        )?;

        // Reopen through the canonical durable config so the layout written above
        // is exactly the layout `open`/`open_from_env` will read on restart.
        let config = crate::config::durable_config_for_data_dir(data_dir);
        let db = AletheiaDB::with_unified_config(config)?;

        // Re-declare uniqueness constraints (Issue #3218). Because
        // `enable_unique_constraint` appends a `DeclareUniqueConstraint` WAL
        // record, the constraint is durable in the restored database and is
        // recovered by normal WAL replay on a later reopen.
        reapply_unique_constraints(&db, &payload.unique_constraints)?;
        Ok(db)
    }
}

/// Re-declare each restored uniqueness constraint (Issue #3218) against a
/// freshly-reopened database.
///
/// `enable_unique_constraint` runs the duplicate pre-flight scan (a safety net
/// — a consistent backup enforced uniqueness, so no duplicates should exist),
/// rebuilds the reservation index from the restored nodes, and appends a
/// durable `DeclareUniqueConstraint` WAL record. A dangling/duplicate
/// declaration surfaces as a normal error rather than silently dropping the
/// constraint.
fn reapply_unique_constraints(
    db: &AletheiaDB,
    descriptors: &[crate::core::constraint::UniqueConstraintDescriptor],
) -> Result<()> {
    for d in descriptors {
        db.enable_unique_constraint(&d.label, &d.property)?;
    }
    Ok(())
}

/// The index-persistence base directory under a durable data root.
///
/// This mirrors [`crate::config::durable_config_for_data_dir`], which sets
/// `PersistenceConfig::data_dir = data_dir/indexes`. Keeping the join in one
/// place ensures restore materialises artifacts at the exact depth the durable
/// reopen path reads them from.
fn index_persistence_root(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("indexes")
}

/// Build an `AletheiaDBConfig` that loads from an existing persistence directory.
///
/// * `persistence_dir` — base dir for `IndexPersistenceManager` (contains `indexes/`).
/// * `wal_dir` — WAL directory; if `None` a fresh in-memory-only WAL is used.
fn build_restore_config(persistence_dir: &Path, wal_dir: std::path::PathBuf) -> AletheiaDBConfig {
    use crate::config::WalConfigBuilder;

    let wal_builder =
        WalConfigBuilder::new()
            .wal_dir(wal_dir)
            .durability_mode(DurabilityMode::GroupCommit {
                max_delay_ms: 10,
                max_batch_size: 200,
            });

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

/// Test-only interleaving seam for the Issue #3425 backup-path regression test.
///
/// Lets a test observe the exact current-storage snapshot that a real `backup()`
/// call captured. The hook fires immediately after the snapshot is taken (inside
/// the `historical.read()` guard block), so a test can drive the real backup path
/// concurrently with a writer parked in the finalize window and assert the
/// captured snapshot never contains a committed node with `commit_timestamp: None`.
/// Compiled away in non-test builds.
#[cfg(test)]
pub(crate) mod backup_test_hooks {
    use crate::storage::snapshot::CurrentStorageSnapshot;
    use std::sync::{Arc, Mutex};

    type Hook = Arc<dyn Fn(&CurrentStorageSnapshot) + Send + Sync>;

    static POST_CURRENT_SNAPSHOT_HOOK: Mutex<Option<Hook>> = Mutex::new(None);

    /// Install a hook fired just after `backup()` captures its current snapshot.
    pub(crate) fn set_post_current_snapshot_hook(hook: Hook) {
        *POST_CURRENT_SNAPSHOT_HOOK
            .lock()
            .expect("backup post-snapshot hook mutex poisoned") = Some(hook);
    }

    /// Remove any installed post-snapshot hook.
    pub(crate) fn clear_post_current_snapshot_hook() {
        *POST_CURRENT_SNAPSHOT_HOOK
            .lock()
            .expect("backup post-snapshot hook mutex poisoned") = None;
    }

    /// Invoke the installed post-snapshot hook, if any.
    ///
    /// The `Arc` is cloned out from under the mutex so the hook body never runs
    /// while holding the registry lock.
    pub(crate) fn run_post_current_snapshot_hook(snapshot: &CurrentStorageSnapshot) {
        let hook = POST_CURRENT_SNAPSHOT_HOOK
            .lock()
            .expect("backup post-snapshot hook mutex poisoned")
            .clone();
        if let Some(hook) = hook {
            hook(snapshot);
        }
    }
}
