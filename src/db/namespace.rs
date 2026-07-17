//! Agent-scoped namespace registry and namespaced entity creation (Issue #3349).
//!
//! The registry maps a namespace name to its `{description, created_at}`
//! metadata so namespaces are **creatable, listable, and describable even when
//! empty** (a namespace need not contain any entity to exist). Writing to an
//! unknown namespace **auto-registers** it (create-on-write memory semantics);
//! there is no strict mode in v1. To mitigate the typo risk that auto-register
//! introduces, [`AletheiaDB::list_namespaces`] surfaces every registered
//! namespace so a caller can discover an accidental `agnet:planner`.
//!
//! # Durability
//!
//! The registry persists as `{data_dir}/namespaces.json` using the same atomic
//! temp→fsync→rename + quarantine-on-corrupt pattern as `snapshots.json`
//! ([`crate::db::snapshot`]): a corrupt or unreadable sidecar never bricks
//! startup — it is quarantined aside (`*.corrupt`) and the registry starts
//! empty. Ephemeral `AletheiaDB::new()` is in-memory only.
//!
//! # The implicit `default` namespace
//!
//! [`Namespace::DEFAULT`] is always present and is never written to the sidecar
//! (so a `default`-only database's file stays empty). It cannot be created
//! explicitly (that is a `CONFLICT`) or deleted (`INVALID_ARGUMENT`), and it is
//! always the first entry in a listing.

use crate::core::error::{Error, Result};
use crate::core::namespace::{Namespace, NamespaceError};
use crate::core::temporal::{Timestamp, time};
use crate::db::AletheiaDB;
use parking_lot::RwLock;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Persisted-format version for the namespace registry sidecar (mirrors the
/// snapshot registry). Bumped only on an incompatible on-disk change.
#[cfg(feature = "serde")]
const PERSIST_FORMAT_VERSION: u32 = 1;

/// Public metadata for a registered namespace (Issue #3349).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceInfo {
    /// The namespace name.
    pub name: String,
    /// Optional human-readable description.
    pub description: Option<String>,
    /// When the namespace was registered (transaction time). The implicit
    /// `default` namespace reports epoch 0.
    pub created_at: Timestamp,
}

/// The on-disk per-entry shape. `created_at` is stored as bare wallclock
/// microseconds — a creation bookmark needs no HLC-logical precision.
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    created_at_micros: i64,
}

#[cfg(feature = "serde")]
impl From<&NamespaceInfo> for PersistedEntry {
    fn from(info: &NamespaceInfo) -> Self {
        PersistedEntry {
            name: info.name.clone(),
            description: info.description.clone(),
            created_at_micros: info.created_at.wallclock(),
        }
    }
}

#[cfg(feature = "serde")]
impl From<PersistedEntry> for NamespaceInfo {
    fn from(e: PersistedEntry) -> Self {
        NamespaceInfo {
            name: e.name,
            description: e.description,
            created_at: Timestamp::from(e.created_at_micros),
        }
    }
}

/// The on-disk registry envelope (versioned, mirrors the snapshot registry).
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
struct PersistedRegistry {
    version: u32,
    namespaces: Vec<PersistedEntry>,
}

/// Synthetic metadata for the always-present implicit `default` namespace.
fn default_info() -> NamespaceInfo {
    NamespaceInfo {
        name: Namespace::DEFAULT.to_string(),
        description: Some("The implicit default namespace".to_string()),
        created_at: Timestamp::from(0),
    }
}

/// In-process registry of namespaces, optionally persisted to a sidecar JSON
/// file. Entirely off the data write path (a leaf; mutating it never touches
/// current/historical storage, the WAL, or `current_timestamp`).
pub(crate) struct NamespaceRegistry {
    entries: RwLock<HashMap<String, NamespaceInfo>>,
    persist_path: Option<PathBuf>,
    /// Serializes concurrent disk saves (the in-memory map has its own
    /// `RwLock`; this guards the temp-file+rename dance and the mutate+save
    /// critical sections).
    save_lock: parking_lot::Mutex<()>,
}

impl NamespaceRegistry {
    /// An empty, memory-only registry (no file is ever written).
    pub(crate) fn in_memory() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            persist_path: None,
            save_lock: parking_lot::Mutex::new(()),
        }
    }

    /// Open a registry, loading any existing sidecar at `path`.
    ///
    /// Mirrors [`SnapshotRegistry::open`](crate::db::snapshot): a missing file
    /// yields an empty registry (first run); a corrupt / unparseable /
    /// unknown-future-version / unreadable file is quarantined aside
    /// (`*.corrupt`) and startup proceeds with an empty registry rather than
    /// bricking (a namespace registry is a non-critical bookmark, unlike the
    /// security-critical auth key store).
    pub(crate) fn open(path: Option<PathBuf>) -> Result<Self> {
        let registry = Self {
            entries: RwLock::new(HashMap::new()),
            persist_path: path.clone(),
            save_lock: parking_lot::Mutex::new(()),
        };
        #[cfg(feature = "serde")]
        if let Some(path) = path {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str::<PersistedRegistry>(&contents) {
                    Ok(parsed) if parsed.version <= PERSIST_FORMAT_VERSION => {
                        let mut entries = registry.entries.write();
                        for entry in parsed.namespaces {
                            let info: NamespaceInfo = entry.into();
                            // Never let a persisted `default` shadow the synthetic one.
                            if info.name != Namespace::DEFAULT {
                                entries.insert(info.name.clone(), info);
                            }
                        }
                    }
                    Ok(parsed) => {
                        log_registry_warning(&format!(
                            "namespace registry at {} has unsupported future version {} (this \
                             build understands up to {}); quarantining it and starting empty",
                            path.display(),
                            parsed.version,
                            PERSIST_FORMAT_VERSION
                        ));
                        quarantine_corrupt_registry(&path);
                    }
                    Err(e) => {
                        log_registry_warning(&format!(
                            "failed to parse namespace registry at {} ({e}); quarantining it and \
                             starting empty",
                            path.display()
                        ));
                        quarantine_corrupt_registry(&path);
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    log_registry_warning(&format!(
                        "failed to read namespace registry at {} ({e}); quarantining it and \
                         starting empty",
                        path.display()
                    ));
                    quarantine_corrupt_registry(&path);
                }
            }
        }
        Ok(registry)
    }

    /// Explicitly create a namespace with an optional description.
    ///
    /// # Errors
    ///
    /// [`NamespaceError::AlreadyExists`] (→ `CONFLICT`) if the name is already
    /// registered or is the implicit `default`.
    pub(crate) fn create(
        &self,
        namespace: &Namespace,
        description: Option<String>,
    ) -> Result<NamespaceInfo> {
        if namespace.is_default() {
            return Err(NamespaceError::AlreadyExists {
                namespace: namespace.as_str().to_string(),
            }
            .into());
        }
        let info = NamespaceInfo {
            name: namespace.as_str().to_string(),
            description,
            created_at: time::now(),
        };
        let _guard = self.save_lock.lock();
        {
            let mut entries = self.entries.write();
            if entries.contains_key(&info.name) {
                return Err(NamespaceError::AlreadyExists {
                    namespace: info.name.clone(),
                }
                .into());
            }
            entries.insert(info.name.clone(), info.clone());
        }
        if let Err(e) = self.save_locked() {
            self.entries.write().remove(&info.name);
            return Err(e);
        }
        Ok(info)
    }

    /// Idempotently ensure a namespace is registered (auto-register on write).
    ///
    /// A no-op for the implicit `default` namespace and for an
    /// already-registered name. A newly-registered name is persisted; a persist
    /// failure rolls back the in-memory insert and surfaces as `Err`.
    pub(crate) fn ensure_registered(&self, namespace: &Namespace) -> Result<()> {
        if namespace.is_default() {
            return Ok(());
        }
        let name = namespace.as_str();
        // Fast path: already present (common case) — no lock churn on the write path.
        if self.entries.read().contains_key(name) {
            return Ok(());
        }
        let _guard = self.save_lock.lock();
        let inserted = {
            let mut entries = self.entries.write();
            if entries.contains_key(name) {
                false
            } else {
                entries.insert(
                    name.to_string(),
                    NamespaceInfo {
                        name: name.to_string(),
                        description: None,
                        created_at: time::now(),
                    },
                );
                true
            }
        };
        if inserted && let Err(e) = self.save_locked() {
            self.entries.write().remove(name);
            return Err(e);
        }
        Ok(())
    }

    /// Fetch a namespace's metadata by name, or the synthetic `default` entry.
    pub(crate) fn get(&self, name: &str) -> Option<NamespaceInfo> {
        if name == Namespace::DEFAULT {
            return Some(default_info());
        }
        self.entries.read().get(name).cloned()
    }

    /// List all namespaces: the implicit `default` first, then stored entries
    /// in a stable order (created_at, then name).
    pub(crate) fn list(&self) -> Vec<NamespaceInfo> {
        let mut all: Vec<NamespaceInfo> = self.entries.read().values().cloned().collect();
        all.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.name.cmp(&b.name))
        });
        let mut out = Vec::with_capacity(all.len() + 1);
        out.push(default_info());
        out.extend(all);
        out
    }

    /// Delete a namespace registration.
    ///
    /// This removes only the **registry entry**, not any entities that carry
    /// the namespace (v1 has no cross-namespace move/purge). It is intended for
    /// cleaning up an empty or mistyped namespace.
    ///
    /// # Errors
    ///
    /// - [`NamespaceError::InvalidName`] (→ `INVALID_ARGUMENT`) when asked to
    ///   delete the implicit `default`.
    /// - [`NamespaceError::NotFound`] (→ `NOT_FOUND`) when the name is absent.
    pub(crate) fn remove(&self, namespace: &Namespace) -> Result<()> {
        if namespace.is_default() {
            return Err(NamespaceError::InvalidName {
                name: namespace.as_str().to_string(),
                reason: "the default namespace cannot be deleted".to_string(),
            }
            .into());
        }
        let name = namespace.as_str();
        let _guard = self.save_lock.lock();
        let removed = {
            let mut entries = self.entries.write();
            match entries.remove(name) {
                Some(removed) => removed,
                None => {
                    return Err(NamespaceError::NotFound {
                        namespace: name.to_string(),
                    }
                    .into());
                }
            }
        };
        if let Err(e) = self.save_locked() {
            self.entries.write().insert(removed.name.clone(), removed);
            return Err(e);
        }
        Ok(())
    }

    /// The durable-write body, assuming `save_lock` is held. A no-op for
    /// in-memory-only registries.
    fn save_locked(&self) -> Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        #[cfg(not(feature = "serde"))]
        {
            let _ = path;
            return Ok(());
        }
        #[cfg(feature = "serde")]
        {
            self.save_serialized(path)
        }
    }

    #[cfg(feature = "serde")]
    fn save_serialized(&self, path: &std::path::Path) -> Result<()> {
        let mut infos: Vec<NamespaceInfo> = self.entries.read().values().cloned().collect();
        infos.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.name.cmp(&b.name))
        });
        let namespaces: Vec<PersistedEntry> = infos.iter().map(PersistedEntry::from).collect();

        let serialized = serde_json::to_vec_pretty(&PersistedRegistry {
            version: PERSIST_FORMAT_VERSION,
            namespaces,
        })
        .map_err(|e| Error::Other(format!("failed to serialize namespace registry: {e}")))?;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let tmp_path = path.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp_path);
        {
            use std::io::Write as _;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            let mut file = options.open(&tmp_path)?;
            file.write_all(&serialized)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

/// Emit a namespace-registry warning under either logging configuration
/// (mirrors the snapshot registry).
#[cfg(feature = "serde")]
fn log_registry_warning(message: &str) {
    #[cfg(feature = "observability")]
    tracing::warn!("{}", message);
    #[cfg(not(feature = "observability"))]
    eprintln!("WARNING: {}", message);
}

/// Move a corrupt/unreadable sidecar aside (`*.corrupt`) so startup proceeds
/// with an empty registry while preserving the bad bytes for inspection.
#[cfg(feature = "serde")]
fn quarantine_corrupt_registry(path: &std::path::Path) {
    let mut corrupt = path.as_os_str().to_owned();
    corrupt.push(".corrupt");
    let corrupt = PathBuf::from(corrupt);
    if let Err(e) = std::fs::rename(path, &corrupt) {
        log_registry_warning(&format!(
            "could not quarantine corrupt namespace registry {} -> {}: {e}",
            path.display(),
            corrupt.display()
        ));
    }
}

/// Build the sidecar path for a database's namespace registry, or `None` when
/// the database is ephemeral (persistence disabled). Lives **inside** the
/// configured persistence directory at `{data_dir}/namespaces.json`, exactly
/// like the snapshot registry.
pub(crate) fn registry_path_for(
    persistence: &crate::storage::index_persistence::PersistenceConfig,
) -> Option<PathBuf> {
    if !persistence.enabled {
        return None;
    }
    Some(persistence.data_dir.join("namespaces.json"))
}

impl AletheiaDB {
    /// Create a namespace with an optional description (Issue #3349).
    ///
    /// Namespaces need not contain any entity to exist; this makes an empty one
    /// listable/describable up front. Writing to an unknown namespace also
    /// auto-registers it, so this call is optional for most workflows.
    ///
    /// # Errors
    ///
    /// - `INVALID_ARGUMENT` if `name` fails validation (empty, too long, bad
    ///   charset, or the reserved `all` selector).
    /// - `CONFLICT` if the namespace already exists (including `default`).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_namespace(
        &self,
        name: impl AsRef<str>,
        description: Option<String>,
    ) -> Result<NamespaceInfo> {
        let ns = Namespace::new(name.as_ref())?;
        self.namespaces.create(&ns, description)
    }

    /// List all namespaces (the implicit `default` first, then registered ones
    /// in creation order). Use this to catch a mistyped auto-registered
    /// namespace.
    #[must_use]
    pub fn list_namespaces(&self) -> Vec<NamespaceInfo> {
        self.namespaces.list()
    }

    /// Fetch a namespace's metadata by name.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` (with the name in the error) if the namespace is not
    /// registered. The implicit `default` namespace always resolves.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_namespace(&self, name: impl AsRef<str>) -> Result<NamespaceInfo> {
        let name = name.as_ref();
        self.namespaces.get(name).ok_or_else(|| {
            Error::Namespace(NamespaceError::NotFound {
                namespace: name.to_string(),
            })
        })
    }

    /// Alias for [`get_namespace`](Self::get_namespace).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn describe_namespace(&self, name: impl AsRef<str>) -> Result<NamespaceInfo> {
        self.get_namespace(name)
    }

    /// Delete a namespace registration (not its entities).
    ///
    /// # Errors
    ///
    /// - `INVALID_ARGUMENT` when asked to delete the implicit `default`.
    /// - `NOT_FOUND` (with the name in the error) if the namespace is absent.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn delete_namespace(&self, name: impl AsRef<str>) -> Result<()> {
        // Validate the name shape first so a malformed name is INVALID_ARGUMENT,
        // not a confusing NOT_FOUND.
        let ns = Namespace::new(name.as_ref())?;
        self.namespaces.remove(&ns)
    }

    /// Create a node in the given namespace (Issue #3349).
    ///
    /// The namespace is validated, auto-registered if new, and stamped onto the
    /// node as an immutable, engine-owned ride-along property. Passing
    /// [`Namespace::DEFAULT`] is equivalent to plain
    /// [`create_node`](Self::create_node).
    ///
    /// # Errors
    ///
    /// - `INVALID_ARGUMENT` if `namespace` fails validation or `properties`
    ///   carries an engine-reserved key.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_node_in_namespace(
        &self,
        label: &str,
        properties: crate::core::property::PropertyMap,
        namespace: impl AsRef<str>,
    ) -> Result<crate::core::id::NodeId> {
        use crate::api::transaction::{WriteOps, WriteRequestOptions};
        let ns = Namespace::new(namespace.as_ref())?;
        let options = WriteRequestOptions::new().with_namespace(ns.clone());
        // Register the namespace only AFTER the write commits successfully.
        // `ensure_registered` fsyncs a new registry entry; running it first would
        // leave a durable empty namespace behind if the write then failed (e.g.
        // a reserved-key rejection or constraint violation), even though the call
        // returns Err (#3349). Preferring data-without-registry over
        // registry-without-data keeps the registry from accumulating phantom
        // namespaces; the membership index (PR2) rebuilds registry state from the
        // data at load, so the surviving divergence is self-healing.
        let node_id = self.write(|tx| tx.create_node_with_options(label, properties, options))?;
        self.namespaces.ensure_registered(&ns)?;
        Ok(node_id)
    }

    /// Create an edge in the given namespace (Issue #3349). See
    /// [`create_node_in_namespace`](Self::create_node_in_namespace).
    ///
    /// # Errors
    ///
    /// - `INVALID_ARGUMENT` if `namespace` fails validation or `properties`
    ///   carries an engine-reserved key.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_edge_in_namespace(
        &self,
        source: crate::core::id::NodeId,
        target: crate::core::id::NodeId,
        label: &str,
        properties: crate::core::property::PropertyMap,
        namespace: impl AsRef<str>,
    ) -> Result<crate::core::id::EdgeId> {
        use crate::api::transaction::{WriteOps, WriteRequestOptions};
        let ns = Namespace::new(namespace.as_ref())?;
        let options = WriteRequestOptions::new().with_namespace(ns.clone());
        // Register only AFTER a successful commit; see `create_node_in_namespace`
        // for why (a failed write must not leave a durable empty namespace).
        let edge_id = self
            .write(|tx| tx.create_edge_with_options(source, target, label, properties, options))?;
        self.namespaces.ensure_registered(&ns)?;
        Ok(edge_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_path_none_when_persistence_disabled() {
        let cfg = crate::storage::index_persistence::PersistenceConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(registry_path_for(&cfg), None);
    }

    #[test]
    fn registry_path_inside_data_dir() {
        let cfg = crate::storage::index_persistence::PersistenceConfig {
            enabled: true,
            data_dir: PathBuf::from("/var/lib/aletheia/indexes"),
            ..Default::default()
        };
        assert_eq!(
            registry_path_for(&cfg),
            Some(PathBuf::from("/var/lib/aletheia/indexes/namespaces.json"))
        );
    }

    #[test]
    fn in_memory_create_get_list_conflict_remove() {
        let reg = NamespaceRegistry::in_memory();
        let ns = Namespace::new("agent:planner").unwrap();

        // default is always present but not stored.
        assert!(reg.get("default").is_some());
        assert_eq!(reg.list().len(), 1);

        let info = reg.create(&ns, Some("planner scope".to_string())).unwrap();
        assert_eq!(info.name, "agent:planner");
        assert_eq!(reg.get("agent:planner").unwrap().name, "agent:planner");

        // list: default first, then the created one.
        let list = reg.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "default");
        assert_eq!(list[1].name, "agent:planner");

        // Duplicate create -> CONFLICT (AlreadyExists).
        assert!(matches!(
            reg.create(&ns, None),
            Err(Error::Namespace(NamespaceError::AlreadyExists { .. }))
        ));

        // Creating default -> CONFLICT.
        assert!(matches!(
            reg.create(&Namespace::default(), None),
            Err(Error::Namespace(NamespaceError::AlreadyExists { .. }))
        ));

        // Remove -> gone; second remove -> NOT_FOUND.
        reg.remove(&ns).unwrap();
        assert!(reg.get("agent:planner").is_none());
        assert!(matches!(
            reg.remove(&ns),
            Err(Error::Namespace(NamespaceError::NotFound { .. }))
        ));

        // Removing default -> INVALID_ARGUMENT.
        assert!(matches!(
            reg.remove(&Namespace::default()),
            Err(Error::Namespace(NamespaceError::InvalidName { .. }))
        ));
    }

    #[test]
    fn ensure_registered_is_idempotent_and_skips_default() {
        let reg = NamespaceRegistry::in_memory();
        reg.ensure_registered(&Namespace::default()).unwrap();
        assert_eq!(reg.list().len(), 1, "default is never stored");

        let ns = Namespace::new("session:1").unwrap();
        reg.ensure_registered(&ns).unwrap();
        reg.ensure_registered(&ns).unwrap();
        assert_eq!(reg.list().len(), 2, "auto-register is idempotent");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn open_round_trips_and_quarantines_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("namespaces.json");

        let reg = NamespaceRegistry::open(Some(path.clone())).unwrap();
        reg.create(&Namespace::new("agent:a").unwrap(), Some("a".to_string()))
            .unwrap();
        reg.create(&Namespace::new("agent:b").unwrap(), None)
            .unwrap();

        // Reload -> identical (default + the two stored).
        let reloaded = NamespaceRegistry::open(Some(path.clone())).unwrap();
        let names: Vec<String> = reloaded.list().into_iter().map(|i| i.name).collect();
        assert_eq!(names, vec!["default", "agent:a", "agent:b"]);
        assert_eq!(
            reloaded.get("agent:a").unwrap().description,
            Some("a".to_string())
        );

        // Corrupt the file -> quarantine + empty on next open.
        std::fs::write(&path, b"not json {{{").unwrap();
        let after = NamespaceRegistry::open(Some(path.clone())).unwrap();
        assert_eq!(after.list().len(), 1, "corrupt file -> only default");
        assert!(!path.exists());
        assert!(dir.path().join("namespaces.json.corrupt").exists());
    }
}
