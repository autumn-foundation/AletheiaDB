//! Crash-safe durable file write helper.
//!
//! A single ordered "temp file → `sync_all` → atomic rename → parent-dir fsync"
//! primitive shared by the encryption-state authority (Issue #3616), the
//! rotation ledger (Issue #488 P0.2), and the crypto-shred keyring. Kept in its
//! own always-compiled module so the small set of durable-breadcrumb writers can
//! share the exact same crash-safe ordering without pulling in the (large,
//! disk/encryption-only) rotation engine.

use crate::core::error::{Result, StorageError};

/// Durable, ordered file write: temp file → `sync_all` → atomic rename → fsync
/// of the parent directory. Guarantees the breadcrumb is on stable storage
/// before the caller proceeds (Issue #488 P0.2). Leaves no temp file behind on
/// success.
///
/// `pub(crate)` so the encryption-state authority (Issue #3616) can flip its
/// durable file with the exact same crash-safe ordering as the rotation ledger.
pub(crate) fn write_durable(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| {
        StorageError::io_error("rotation.state path has no parent directory".to_string())
    })?;
    let tmp = path.with_extension("state.tmp");
    // Remove any stale temp file first: the `mode(0o600)` below only applies when
    // the file is *created*, so a pre-existing temp (e.g. from a crashed write)
    // could otherwise retain looser permissions.
    let _ = std::fs::remove_file(&tmp);
    {
        // Owner-only (0600): the rotation.state / encryption.state bodies carry a
        // CMK-useless KMS blob + the (non-secret) KCV, but there is no reason to
        // leave them world-readable. Matches the auth `keys.json` 0600 precedent.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut f = options
            .open(&tmp)
            .map_err(|e| StorageError::io_error(format!("Failed to write rotation.state: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| StorageError::io_error(format!("Failed to write rotation.state: {e}")))?;
        f.sync_all()
            .map_err(|e| StorageError::io_error(format!("Failed to fsync rotation.state: {e}")))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StorageError::io_error(format!("Failed to publish rotation.state: {e}"))
    })?;
    // Make the rename itself durable.
    crate::storage::index_persistence::fsync_dir(parent);
    Ok(())
}
