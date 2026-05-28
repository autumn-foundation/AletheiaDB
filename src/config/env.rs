use crate::config::{AletheiaDBConfig, WalConfigBuilder};

/// Name of the environment variable that, when set, points all exposed binaries
/// (`aletheia-server`, `aletheia-mcp`, `aletheia` CLI) and the Python SDK at a
/// durable data directory.
pub const DATA_DIR_ENV: &str = "ALETHEIADB_DATA_DIR";

/// Name of the environment variable that, when set, points all exposed binaries
/// and the Python SDK at a TOML config file (loaded via
/// [`AletheiaDBConfig::from_toml_file`]). Takes precedence over [`DATA_DIR_ENV`].
pub const CONFIG_ENV: &str = "ALETHEIADB_CONFIG";

/// Read the data directory from [`DATA_DIR_ENV`].
///
/// Returns `Some(path)` when the variable is set to a non-empty value
/// (whitespace is trimmed). Unset or empty resolves to `None`, signalling
/// the caller should fall back to ephemeral storage.
#[must_use]
pub fn data_dir_from_env() -> Option<std::path::PathBuf> {
    std::env::var(DATA_DIR_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
}

/// Read the TOML config path from [`CONFIG_ENV`].
///
/// Same semantics as [`data_dir_from_env`]: unset or empty → `None`.
#[must_use]
pub fn config_path_from_env() -> Option<std::path::PathBuf> {
    std::env::var(CONFIG_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
}

/// Build a canonical durable [`AletheiaDBConfig`] rooted at `data_dir`.
///
/// The shape — `{data_dir}/wal` for the WAL, `{data_dir}/indexes` for index
/// persistence, group-commit durability, and `load_on_startup = true` so a
/// restart replays prior state — is what every exposed binary (HTTP server,
/// MCP server, CLI, Python SDK) uses when `ALETHEIADB_DATA_DIR` is set.
/// Centralised here so the binaries don't drift out of sync.
#[must_use]
pub fn durable_config_for_data_dir(data_dir: impl Into<std::path::PathBuf>) -> AletheiaDBConfig {
    use crate::storage::index_persistence::PersistenceConfig;
    use crate::storage::wal::DurabilityMode;

    let data_dir = data_dir.into();
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(data_dir.join("wal"))
                .durability_mode(DurabilityMode::GroupCommit {
                    max_delay_ms: 10,
                    max_batch_size: 200,
                })
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.join("indexes"),
            load_on_startup: true,
            ..Default::default()
        })
        .build()
}
