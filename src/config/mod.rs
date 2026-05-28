//! Unified configuration for AletheiaDB.
//!
//! This module provides a centralized configuration system that consolidates
//! all previously hardcoded values across WAL, historical storage, and vector indexes.
//!
//! # Features
//!
//! - **`config-toml`** (enabled by default): Adds TOML file support via `from_toml_file()`,
//!   `from_toml_str()`, `to_toml_file()`, and `to_toml_string()` methods.
//!   Disable with `default-features = false` if only using programmatic configuration.

mod db;
mod error;
mod historical;
#[cfg(test)]
mod tests;
mod vector;
mod wal;

pub use db::{AletheiaDBConfig, AletheiaDBConfigBuilder};
pub use error::ConfigError;
pub use historical::{HistoricalConfig, HistoricalConfigBuilder};
pub use vector::{VectorIndexConfig, VectorIndexConfigBuilder};
pub use wal::{WalConfig, WalConfigBuilder};
mod env;
pub use env::*;
