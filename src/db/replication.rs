//! Public replication API: start streaming, bootstrap from a snapshot, and
//! read progress (Issue #3355, Slice B).
//!
//! [`crate::db::replication_role`] (Slice A) is the always-compiled role
//! atomic + write-rejection core; this module is the engine that actually
//! keeps a replica's role/history in sync, built on
//! [`crate::storage::replication`]. Native-only (see that module's docs).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::core::error::{Error, Result, StorageError};
use crate::db::AletheiaDB;
use crate::storage::replication::applier::ReplicaApplierHandle;
use crate::storage::replication::apply::ReplicaStorageHandles;
use crate::storage::replication::source::ReplicationSource;

pub use crate::storage::replication::ReplicationOptions;

fn lock_poisoned(resource: &str) -> Error {
    Error::Storage(StorageError::LockPoisoned {
        resource: resource.to_string(),
    })
}

impl AletheiaDB {
    /// Start streaming replication from `source`, entering read-only replica
    /// mode.
    ///
    /// Resumes from this database's startup index-manifest LSN (the
    /// coordinate captured when indexes were loaded at `open()`/
    /// `with_unified_config` time), or LSN 1 for a fresh database with no
    /// manifest. Spawns a background applier thread
    /// ([`crate::storage::replication::applier::ReplicaApplierHandle`]) that
    /// fetches, applies complete commit frames, and (when index persistence
    /// is configured) periodically persists indexes stamped with the
    /// replica's own applied-LSN coordinate.
    ///
    /// Calling this again while an applier is already running replaces it
    /// (the previous one is stopped and joined first).
    ///
    /// # Errors
    ///
    /// Returns an error only if the internal replication-runtime lock is
    /// poisoned (a prior panic while holding it).
    pub fn start_replication(
        &self,
        source: Box<dyn ReplicationSource>,
        opts: ReplicationOptions,
    ) -> Result<()> {
        self.enter_replica_mode();

        let resume_lsn = self.startup_manifest_lsn.load(Ordering::Relaxed);
        let start_from = if resume_lsn == 0 { 1 } else { resume_lsn };

        let handles = self.replica_storage_handles();
        let applier = ReplicaApplierHandle::spawn(handles, source, opts, start_from);

        let mut slot = self
            .replication
            .lock()
            .map_err(|_| lock_poisoned("replication"))?;
        *slot = Some(applier);
        Ok(())
    }

    /// Bootstrap a fresh, durable replica at `data_dir` from `source`'s
    /// current snapshot, then start streaming from the snapshot's
    /// `source_lsn`.
    ///
    /// Fetches a `.albk`-shaped snapshot artifact via
    /// [`ReplicationSource::fetch_snapshot`] into a temporary file inside
    /// `data_dir`, restores it via [`AletheiaDB::restore_to_data_dir`] (which
    /// durably records the snapshot's `source_lsn` in the restored index
    /// manifest), then calls [`Self::start_replication`] -- which resumes
    /// from exactly that manifest LSN.
    ///
    /// # Errors
    ///
    /// Propagates any error from the snapshot fetch, the restore, or
    /// directory creation.
    pub fn bootstrap_replica(
        mut source: Box<dyn ReplicationSource>,
        data_dir: &Path,
        opts: ReplicationOptions,
    ) -> Result<AletheiaDB> {
        std::fs::create_dir_all(data_dir)?;
        let snapshot_path = data_dir.join(".replica_bootstrap.albk");

        source.fetch_snapshot(&snapshot_path)?;

        let db = AletheiaDB::restore_to_data_dir(&snapshot_path, data_dir);
        let _ = std::fs::remove_file(&snapshot_path);
        let db = db?;

        db.start_replication(source, opts)?;
        Ok(db)
    }

    /// A live snapshot of this replica's applier progress/lag, or `None` if
    /// no replication applier is currently running (a plain primary, or a
    /// replica on which `start_replication`/`bootstrap_replica` was never
    /// called).
    #[must_use]
    pub fn replication_progress(&self) -> Option<crate::db::ReplicaProgressStats> {
        let guard = self.replication.lock().ok()?;
        guard.as_ref().map(ReplicaApplierHandle::progress_snapshot)
    }

    /// Build the Arc-cloned storage handles the background applier needs,
    /// without holding a `&AletheiaDB` across the thread boundary.
    pub(crate) fn replica_storage_handles(&self) -> ReplicaStorageHandles {
        ReplicaStorageHandles {
            current: Arc::clone(&self.current),
            historical: Arc::clone(&self.historical),
            temporal_indexes: Arc::clone(&self.temporal_indexes),
            wal: Arc::clone(&self.wal),
            node_id_gen: Arc::clone(&self.node_id_gen),
            edge_id_gen: Arc::clone(&self.edge_id_gen),
            version_id_gen: Arc::clone(&self.version_id_gen),
            constraint_registry: Arc::clone(&self.constraint_registry),
            current_timestamp: Arc::clone(&self.current_timestamp),
            persistence: match (&self.persistence_manager, &self.persistence_tracker) {
                (Some(manager), Some(tracker)) => Some((Arc::clone(manager), Arc::clone(tracker))),
                _ => None,
            },
        }
    }
}
