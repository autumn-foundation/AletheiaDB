//! Durable record of the enabled temporal vector indexes (Issue #3888).
//!
//! Each enable writes the config of each temporal vector index to
//! `{persistence.data_dir}/temporal_vector_indexes.dat` (bitcode + CRC, atomic
//! write). At open, after recovery, the database enables each recorded index
//! again. The enable rebuilds the snapshots from history, so as-of vector
//! search survives a reopen.
//!
//! - A missing file means no index to restore.
//! - A bad file is quarantined (`*.corrupt-<micros>`). Open continues.
//! - Save merges with the file on disk, so an entry that failed to restore is
//!   not deleted.
//! - A config with a custom metric is not stored: a closure cannot be stored.
//! - Ephemeral databases (persistence disabled) have no file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bitcode::{Decode, Encode};

use crate::core::error::{Error, Result, StorageError};
use crate::db::AletheiaDB;
use crate::index::vector::hnsw::HnswConfig;
use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};
use crate::index::vector::{DistanceMetric, Quantization, StorageMode};
use crate::storage::index_persistence::PersistenceConfig;
use crate::storage::index_persistence::common::{load_encoded_with_crc, save_encoded_with_crc};

/// File name of the sidecar in the persistence directory.
pub(crate) const SIDECAR_FILE: &str = "temporal_vector_indexes.dat";

/// Maximum sidecar size accepted on load (DoS guard).
const MAX_SIDECAR_BYTES: u64 = 1024 * 1024;

/// Current sidecar format version.
const SIDECAR_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
struct Sidecar {
    version: u16,
    entries: Vec<PersistedEntry>,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub(crate) struct PersistedEntry {
    property: String,
    config: PersistedConfig,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
struct PersistedConfig {
    strategy: PersistedStrategy,
    retention: PersistedRetention,
    max_snapshots: u64,
    full_snapshot_interval: u64,
    hnsw: PersistedHnsw,
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
enum PersistedStrategy {
    TransactionInterval(u64),
    TimeInterval(u64),
    ChangeThreshold(f64),
    Hybrid {
        transaction_interval: u64,
        time_interval_secs: u64,
        change_threshold: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
enum PersistedRetention {
    KeepAll,
    KeepN(u64),
    KeepDuration { secs: u64, nanos: u32 },
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
struct PersistedHnsw {
    dimensions: u64,
    metric: u8,
    m: u64,
    ef_construction: u64,
    ef_search: u64,
    capacity: u64,
    quantization: u8,
    /// `Some(path)` for `StorageMode::MemoryMapped`.
    mmap_path: Option<String>,
}

/// Convert a `usize` from the file. A value that does not fit is corrupt data.
fn to_usize(value: u64) -> Result<usize> {
    usize::try_from(value).map_err(|_| {
        StorageError::CorruptedData(format!("value {value} does not fit in usize")).into()
    })
}

impl PersistedEntry {
    /// Make an entry from a resolved config. Returns `None` if the config
    /// cannot be stored (no HNSW config, or a custom metric).
    pub(crate) fn new(property: &str, config: &TemporalVectorConfig) -> Option<Self> {
        let hnsw = config.hnsw_config.as_ref()?;
        if hnsw.custom_metric.is_some() {
            return None;
        }
        let strategy = match config.snapshot_strategy {
            SnapshotStrategy::TransactionInterval(n) => {
                PersistedStrategy::TransactionInterval(n as u64)
            }
            SnapshotStrategy::TimeInterval(secs) => PersistedStrategy::TimeInterval(secs),
            SnapshotStrategy::ChangeThreshold(t) => PersistedStrategy::ChangeThreshold(t),
            SnapshotStrategy::Hybrid {
                transaction_interval,
                time_interval_secs,
                change_threshold,
            } => PersistedStrategy::Hybrid {
                transaction_interval: transaction_interval as u64,
                time_interval_secs,
                change_threshold,
            },
        };
        let retention = match config.retention_policy {
            RetentionPolicy::KeepAll => PersistedRetention::KeepAll,
            RetentionPolicy::KeepN(n) => PersistedRetention::KeepN(n as u64),
            RetentionPolicy::KeepDuration(d) => PersistedRetention::KeepDuration {
                secs: d.as_secs(),
                nanos: d.subsec_nanos(),
            },
        };
        let mmap_path = match &hnsw.storage {
            StorageMode::InMemory => None,
            StorageMode::MemoryMapped { path } => Some(path.to_string_lossy().into_owned()),
        };
        Some(Self {
            property: property.to_string(),
            config: PersistedConfig {
                strategy,
                retention,
                max_snapshots: config.max_snapshots as u64,
                full_snapshot_interval: config.full_snapshot_interval as u64,
                hnsw: PersistedHnsw {
                    dimensions: hnsw.dimensions as u64,
                    metric: hnsw.metric.to_u8(),
                    m: hnsw.m as u64,
                    ef_construction: hnsw.ef_construction as u64,
                    ef_search: hnsw.ef_search as u64,
                    capacity: hnsw.capacity as u64,
                    quantization: hnsw.quantization.to_u8(),
                    mmap_path,
                },
            },
        })
    }

    /// The property name of this entry.
    pub(crate) fn property(&self) -> &str {
        &self.property
    }

    /// Convert this entry back to a config.
    ///
    /// # Errors
    ///
    /// Returns an error if a stored value is not valid.
    pub(crate) fn to_config(&self) -> Result<TemporalVectorConfig> {
        let c = &self.config;
        let snapshot_strategy = match c.strategy {
            PersistedStrategy::TransactionInterval(n) => {
                SnapshotStrategy::TransactionInterval(to_usize(n)?)
            }
            PersistedStrategy::TimeInterval(secs) => SnapshotStrategy::TimeInterval(secs),
            PersistedStrategy::ChangeThreshold(t) => SnapshotStrategy::ChangeThreshold(t),
            PersistedStrategy::Hybrid {
                transaction_interval,
                time_interval_secs,
                change_threshold,
            } => SnapshotStrategy::Hybrid {
                transaction_interval: to_usize(transaction_interval)?,
                time_interval_secs,
                change_threshold,
            },
        };
        let retention_policy = match c.retention {
            PersistedRetention::KeepAll => RetentionPolicy::KeepAll,
            PersistedRetention::KeepN(n) => RetentionPolicy::KeepN(to_usize(n)?),
            PersistedRetention::KeepDuration { secs, nanos } => {
                if nanos >= 1_000_000_000 {
                    return Err(StorageError::CorruptedData(format!(
                        "retention nanos {nanos} is not less than one second"
                    ))
                    .into());
                }
                RetentionPolicy::KeepDuration(Duration::new(secs, nanos))
            }
        };
        let h = &c.hnsw;
        let hnsw = HnswConfig {
            dimensions: to_usize(h.dimensions)?,
            metric: DistanceMetric::from_u8(h.metric)?,
            m: to_usize(h.m)?,
            ef_construction: to_usize(h.ef_construction)?,
            ef_search: to_usize(h.ef_search)?,
            capacity: to_usize(h.capacity)?,
            quantization: Quantization::from_u8(h.quantization)?,
            storage: match &h.mmap_path {
                None => StorageMode::InMemory,
                Some(path) => StorageMode::MemoryMapped {
                    path: PathBuf::from(path),
                },
            },
            custom_metric: None,
        };
        Ok(TemporalVectorConfig {
            snapshot_strategy,
            retention_policy,
            max_snapshots: to_usize(c.max_snapshots)?,
            full_snapshot_interval: to_usize(c.full_snapshot_interval)?,
            hnsw_config: Some(hnsw),
        })
    }
}

/// The sidecar path, or `None` if the database is ephemeral.
pub(crate) fn sidecar_path(persistence: &PersistenceConfig) -> Option<PathBuf> {
    persistence
        .enabled
        .then(|| persistence.data_dir.join(SIDECAR_FILE))
}

/// Log a warning with or without the `observability` feature.
pub(crate) fn log_warning(message: &str) {
    #[cfg(feature = "observability")]
    tracing::warn!("{}", message);
    #[cfg(not(feature = "observability"))]
    eprintln!("WARNING: {message}");
}

/// Move a bad sidecar aside, so open continues with no entries.
fn quarantine(path: &Path, reason: &str) {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let aside = path.with_extension(format!("dat.corrupt-{micros}"));
    let moved = std::fs::rename(path, &aside);
    log_warning(&format!(
        "temporal vector sidecar {} is not usable ({reason}); moved to {} ({}). \
         No temporal vector index is restored from it.",
        path.display(),
        aside.display(),
        if moved.is_ok() { "done" } else { "move failed" },
    ));
}

/// Read the sidecar strictly. A missing file gives no entries.
fn read_entries(path: &Path) -> Result<Vec<PersistedEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let sidecar: Sidecar = load_encoded_with_crc(path, MAX_SIDECAR_BYTES, "temporal vector")
        .map_err(|e| StorageError::CorruptedData(e.to_string()))?;
    if sidecar.version != SIDECAR_VERSION {
        return Err(StorageError::CorruptedData(format!(
            "sidecar version {} is not supported (supported: {SIDECAR_VERSION})",
            sidecar.version
        ))
        .into());
    }
    Ok(sidecar.entries)
}

/// Read the sidecar at open. A bad file is quarantined and gives no entries.
pub(crate) fn load_entries(path: &Path) -> Vec<PersistedEntry> {
    match read_entries(path) {
        Ok(entries) => entries,
        Err(e) => {
            quarantine(path, &e.to_string());
            Vec::new()
        }
    }
}

/// Add or replace `entry` in the sidecar. Entries for other properties stay.
///
/// # Errors
///
/// Returns an error if the file cannot be written.
pub(crate) fn upsert_entry(path: &Path, entry: PersistedEntry) -> Result<()> {
    // Keep a bad file for diagnosis, then write a new one.
    let mut entries = match read_entries(path) {
        Ok(entries) => entries,
        Err(e) => {
            quarantine(path, &e.to_string());
            Vec::new()
        }
    };
    entries.retain(|e| e.property != entry.property);
    entries.push(entry);
    entries.sort_by(|a, b| a.property.cmp(&b.property));
    let sidecar = Sidecar {
        version: SIDECAR_VERSION,
        entries,
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    save_encoded_with_crc(&sidecar, path)
        .map_err(|e| Error::Other(format!("failed to write temporal vector sidecar: {e}")))
}

impl AletheiaDB {
    /// Enable again each temporal vector index that the sidecar records.
    ///
    /// Call this at open, after WAL replay and index load. A failed entry
    /// gives a warning and stays in the sidecar.
    pub(crate) fn restore_temporal_vector_indexes(&self) {
        let Some(path) = sidecar_path(&self.persistence_config) else {
            return;
        };
        for entry in load_entries(&path) {
            let restored = entry.to_config().and_then(|config| {
                self.enable_temporal_vector_index_impl(entry.property(), config, false)
            });
            if let Err(e) = restored {
                log_warning(&format!(
                    "could not restore the temporal vector index for property '{}': {e}",
                    entry.property()
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configs() -> Vec<TemporalVectorConfig> {
        let base =
            TemporalVectorConfig::default_with_hnsw(HnswConfig::new(8, DistanceMetric::Euclidean));
        let mut mmap = base.clone();
        if let Some(h) = mmap.hnsw_config.as_mut() {
            h.storage = StorageMode::MemoryMapped {
                path: PathBuf::from("/tmp/x.usearch"),
            };
            h.quantization = Quantization::I8;
            h.capacity = 77;
        }
        vec![
            base.clone(),
            TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TimeInterval(60),
                retention_policy: RetentionPolicy::KeepAll,
                ..base.clone()
            },
            TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::ChangeThreshold(0.25),
                retention_policy: RetentionPolicy::KeepDuration(Duration::new(5, 7)),
                ..base.clone()
            },
            TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::Hybrid {
                    transaction_interval: 3,
                    time_interval_secs: 9,
                    change_threshold: 0.5,
                },
                max_snapshots: 12_345,
                full_snapshot_interval: 4,
                ..base
            },
            mmap,
        ]
    }

    #[test]
    fn every_config_round_trips() {
        for config in configs() {
            let entry = PersistedEntry::new("p", &config).unwrap();
            assert_eq!(entry.to_config().unwrap(), config);
        }
    }

    #[test]
    fn config_without_hnsw_or_with_custom_metric_is_not_stored() {
        assert!(PersistedEntry::new("p", &TemporalVectorConfig::default_temporal_only()).is_none());
        let mut config = configs().remove(0);
        if let Some(h) = config.hnsw_config.as_mut() {
            h.custom_metric = Some(crate::index::vector::CustomMetric {
                name: "zero".to_string(),
                distance_fn: std::sync::Arc::new(|_, _| 0.0),
            });
        }
        assert!(PersistedEntry::new("p", &config).is_none());
    }

    #[test]
    fn upsert_keeps_other_entries_and_replaces_same_property() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SIDECAR_FILE);
        let all = configs();
        upsert_entry(&path, PersistedEntry::new("b", &all[0]).unwrap()).unwrap();
        upsert_entry(&path, PersistedEntry::new("a", &all[1]).unwrap()).unwrap();
        upsert_entry(&path, PersistedEntry::new("b", &all[2]).unwrap()).unwrap();

        let entries = load_entries(&path);
        let names: Vec<&str> = entries.iter().map(PersistedEntry::property).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(entries[1].to_config().unwrap(), all[2]);
    }

    #[test]
    fn large_sidecar_loads() {
        // Many entries with long names: the file is larger than a few KiB
        // but well under the size limit, so it must load.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SIDECAR_FILE);
        let config = configs().remove(0);
        for i in 0..40 {
            let name = format!("{i:03}{}", "p".repeat(200));
            upsert_entry(&path, PersistedEntry::new(&name, &config).unwrap()).unwrap();
        }
        assert!(std::fs::metadata(&path).unwrap().len() > 8 * 1024);
        assert_eq!(load_entries(&path).len(), 40);
    }

    #[test]
    fn missing_file_gives_no_entries() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_entries(&dir.path().join(SIDECAR_FILE)).is_empty());
    }

    #[test]
    fn unknown_version_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SIDECAR_FILE);
        let future = Sidecar {
            version: SIDECAR_VERSION + 1,
            entries: Vec::new(),
        };
        save_encoded_with_crc(&future, &path).unwrap();
        assert!(load_entries(&path).is_empty());
        assert!(!path.exists(), "the file is moved aside");
    }

    #[test]
    fn bad_value_in_entry_is_an_error() {
        let mut entry = PersistedEntry::new("p", &configs()[0]).unwrap();
        entry.config.hnsw.metric = 200;
        assert!(entry.to_config().is_err());
    }

    #[test]
    fn bad_retention_nanos_is_an_error() {
        let mut entry = PersistedEntry::new("p", &configs()[2]).unwrap();
        entry.config.retention = PersistedRetention::KeepDuration {
            secs: u64::MAX,
            nanos: 1_000_000_000,
        };
        assert!(entry.to_config().is_err());
    }

    #[test]
    fn upsert_over_bad_file_keeps_it_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SIDECAR_FILE);
        std::fs::write(&path, b"bad").unwrap();
        upsert_entry(&path, PersistedEntry::new("a", &configs()[0]).unwrap()).unwrap();
        assert_eq!(load_entries(&path).len(), 1);
        let aside = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains(".corrupt-"));
        assert!(aside);
    }

    #[test]
    fn failed_restore_keeps_entry_and_open_continues() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("indexes").join(SIDECAR_FILE);
        {
            let db = AletheiaDB::open(dir.path()).unwrap();
            drop(db);
        }
        // `max_snapshots == 0` fails config validation at restore.
        let bad = TemporalVectorConfig {
            max_snapshots: 0,
            ..configs().remove(0)
        };
        upsert_entry(&sidecar, PersistedEntry::new("p", &bad).unwrap()).unwrap();

        let db = AletheiaDB::open(dir.path()).unwrap();
        assert!(!db.is_temporal_vector_index_enabled());
        assert_eq!(load_entries(&sidecar).len(), 1, "the entry stays");
    }

    #[test]
    fn ephemeral_database_writes_no_sidecar() {
        let db = AletheiaDB::new().unwrap();
        assert!(sidecar_path(&db.persistence_config).is_none());
        db.enable_temporal_vector_index("p", configs().remove(0))
            .unwrap();
    }

    #[test]
    fn ephemeral_database_has_no_sidecar() {
        let config = PersistenceConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(sidecar_path(&config).is_none());
    }
}
