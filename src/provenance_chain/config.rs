//! Configuration for the opt-in provenance hash chain (Issue #3351).
//!
//! Mirrors the shape of the other unified-config structs: `#[derive(Default)]`
//! with the feature **disabled** by default so enabling the chain is an explicit
//! opt-in and a disabled database keeps byte-identical behavior.

use std::path::PathBuf;

/// When the sidecar chain log is flushed to durable storage.
///
/// The default (`Batched`) keeps hashing/fsync off the commit critical path;
/// `PerTransaction` fsyncs every appended transaction record for the strongest
/// durability, and `Never` leaves flushing to the OS (fastest, weakest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum ChainFsyncMode {
    /// fsync after every appended transaction record.
    PerTransaction,
    /// Do not fsync per append; rely on an explicit `flush()` / OS writeback.
    #[default]
    Batched,
    /// Never fsync from the store; durability is entirely the OS's decision.
    Never,
}

/// Opt-in configuration for the tamper-evident provenance hash chain.
///
/// Disabled by default. When `enabled` is `false`, no chain directory is touched
/// and no hashing work is performed, so behavior and on-disk formats are
/// byte-identical to a build without the feature.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(default)
)]
pub struct ChainConfig {
    /// Whether the provenance hash chain is active.
    pub enabled: bool,
    /// Flush policy for the sidecar log.
    pub fsync: ChainFsyncMode,
    /// Override for the chain directory. When `None`, the default
    /// `<data_dir>/chain` is used.
    pub dir: Option<PathBuf>,
}

impl ChainConfig {
    /// Construct an enabled config with the default flush policy and directory.
    #[must_use]
    pub fn enabled() -> Self {
        ChainConfig {
            enabled: true,
            ..Default::default()
        }
    }

    /// Resolve the chain directory against a data directory: the explicit `dir`
    /// override if set, otherwise `<data_dir>/chain`.
    #[must_use]
    pub fn resolve_dir(&self, data_dir: &std::path::Path) -> PathBuf {
        self.dir.clone().unwrap_or_else(|| data_dir.join("chain"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_and_batched() {
        let c = ChainConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.fsync, ChainFsyncMode::Batched);
        assert!(c.dir.is_none());
    }

    #[test]
    fn resolve_dir_defaults_under_data_dir() {
        let c = ChainConfig::default();
        let resolved = c.resolve_dir(std::path::Path::new("/data"));
        assert_eq!(resolved, PathBuf::from("/data/chain"));
    }

    #[test]
    fn resolve_dir_honors_override() {
        let c = ChainConfig {
            enabled: true,
            fsync: ChainFsyncMode::PerTransaction,
            dir: Some(PathBuf::from("/custom/place")),
        };
        assert_eq!(
            c.resolve_dir(std::path::Path::new("/data")),
            PathBuf::from("/custom/place")
        );
    }
}
