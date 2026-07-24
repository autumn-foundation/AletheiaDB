//! Durable tenant-registry sidecar (`{root}/tenants.json`) for [`TenantManager`]
//! (Issue #3365).
//!
//! Mirrors the namespace / snapshot registry persistence convention: an atomic
//! temp → fsync → rename write, and a tolerant load that quarantines a
//! corrupt / unreadable / future-version sidecar aside (`*.corrupt`) and starts
//! with an empty registry rather than bricking startup.
//!
//! The sidecar records only tenant *metadata* (id, creation time, quota) — the
//! tenant data itself lives in each tenant's own `{root}/tenants/{id}` database
//! directory, reopened via the normal durable startup path on load.

use crate::core::error::{Error, Result};
use crate::core::tenant::{TenantError, TenantId, TenantQuota};
use crate::tenant::{Tenant, TenantManager};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

/// Persisted-format version for the tenant registry sidecar. Bumped only on an
/// incompatible on-disk change.
const PERSIST_FORMAT_VERSION: u32 = 1;

/// The on-disk shape of one tenant's metadata.
#[derive(Serialize, Deserialize)]
struct PersistedTenant {
    id: String,
    created_at_micros: i64,
    quota: TenantQuota,
}

/// The on-disk registry envelope (versioned).
#[derive(Serialize, Deserialize)]
struct PersistedRegistry {
    version: u32,
    tenants: Vec<PersistedTenant>,
}

impl TenantManager {
    /// The registry sidecar path, `{root}/tenants.json`. `None` when ephemeral.
    fn registry_path(&self) -> Option<std::path::PathBuf> {
        self.root.as_ref().map(|r| r.join("tenants.json"))
    }

    /// Load the registry sidecar on `open`, reopening each recorded tenant's
    /// database from its directory. A missing sidecar is a clean first run; a
    /// corrupt/unreadable/future-version one is quarantined and startup proceeds
    /// with an empty registry.
    pub(crate) fn load_registry(&self) -> Result<()> {
        let Some(path) = self.registry_path() else {
            return Ok(());
        };
        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                log_registry_warning(&format!(
                    "failed to read tenant registry at {} ({e}); quarantining it and starting empty",
                    path.display()
                ));
                quarantine_corrupt(&path);
                return Ok(());
            }
        };
        let parsed: PersistedRegistry = match serde_json::from_str(&contents) {
            Ok(p) => p,
            Err(e) => {
                log_registry_warning(&format!(
                    "failed to parse tenant registry at {} ({e}); quarantining it and starting empty",
                    path.display()
                ));
                quarantine_corrupt(&path);
                return Ok(());
            }
        };
        if parsed.version > PERSIST_FORMAT_VERSION {
            log_registry_warning(&format!(
                "tenant registry at {} has unsupported future version {} (this build understands \
                 up to {}); quarantining it and starting empty",
                path.display(),
                parsed.version,
                PERSIST_FORMAT_VERSION
            ));
            quarantine_corrupt(&path);
            return Ok(());
        }

        let mut tenants = self.tenants.write();
        for entry in parsed.tenants {
            // A persisted id that fails validation (e.g. a hand-edited sidecar) is
            // skipped with a warning rather than aborting the whole load — one bad
            // row must not deny service to every other tenant.
            let id = match TenantId::new(entry.id.clone()) {
                Ok(id) => id,
                Err(TenantError::InvalidId { id, reason }) => {
                    log_registry_warning(&format!(
                        "skipping tenant registry entry with invalid id {id:?}: {reason}"
                    ));
                    continue;
                }
                Err(_) => continue,
            };
            let (db, data_dir) = self.build_tenant_db(&id)?;
            let tenant = Arc::new(Tenant {
                id: id.clone(),
                db,
                created_at_micros: entry.created_at_micros,
                data_dir,
                quota: RwLock::new(entry.quota),
                nodes: AtomicU64::new(0),
                edges: AtomicU64::new(0),
            });
            tenant.seed_counters();
            tenants.insert(id, tenant);
        }
        Ok(())
    }

    /// Atomically persist the current registry to the sidecar, assuming
    /// `save_lock` is held. A no-op for an ephemeral manager.
    pub(crate) fn save_registry_locked(&self) -> Result<()> {
        let Some(path) = self.registry_path() else {
            return Ok(());
        };
        let mut tenants: Vec<PersistedTenant> = self
            .tenants
            .read()
            .values()
            .map(|t| PersistedTenant {
                id: t.id.as_str().to_string(),
                created_at_micros: t.created_at_micros,
                quota: *t.quota.read(),
            })
            .collect();
        tenants.sort_by(|a, b| a.id.cmp(&b.id));

        let serialized = serde_json::to_vec_pretty(&PersistedRegistry {
            version: PERSIST_FORMAT_VERSION,
            tenants,
        })
        .map_err(|e| Error::Other(format!("failed to serialize tenant registry: {e}")))?;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }

        let tmp_path = path.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp_path);
        {
            use std::io::Write as _;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            let mut file = options.open(&tmp_path).map_err(Error::Io)?;
            file.write_all(&serialized).map_err(Error::Io)?;
            file.sync_all().map_err(Error::Io)?;
        }
        std::fs::rename(&tmp_path, &path).map_err(Error::Io)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::File::open(parent)
                .map_err(Error::Io)?
                .sync_all()
                .map_err(Error::Io)?;
        }
        Ok(())
    }
}

/// Emit a tenant-registry warning under either logging configuration.
fn log_registry_warning(message: &str) {
    #[cfg(feature = "observability")]
    tracing::warn!("{}", message);
    #[cfg(not(feature = "observability"))]
    eprintln!("WARNING: {}", message);
}

/// Move a corrupt/unreadable sidecar aside (`*.corrupt`) so startup proceeds
/// with an empty registry while preserving the bad bytes for inspection.
fn quarantine_corrupt(path: &std::path::Path) {
    let mut corrupt = path.as_os_str().to_owned();
    corrupt.push(".corrupt");
    let corrupt = std::path::PathBuf::from(corrupt);
    if let Err(e) = std::fs::rename(path, &corrupt) {
        log_registry_warning(&format!(
            "could not quarantine corrupt tenant registry {} -> {}: {e}",
            path.display(),
            corrupt.display()
        ));
    }
}
