//! Honeycomb backend for distributed tracing
//!
//! This module provides integration with Honeycomb.io for sending telemetry events.
//! Enable via the `observability-honeycomb` feature flag.
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::observability::backends::honeycomb::{HoneycombConfig, create_client};
//!
//! let config = HoneycombConfig::new("api-key", "dataset", "my-service");
//! let client = create_client(config)?;
//!
//! let mut event = client.new_event();
//! event.add_field("operation", "query");
//! event.add_field("duration_ms", 42);
//! client.send(event)?;
//! client.flush()?;
//! ```

use crate::Error;

#[cfg(feature = "honeycomb")]
use crate::honeycomb::{Client as HoneycombClient, Config as HoneycombClientConfig};

/// Honeycomb configuration
#[derive(Debug, Clone)]
pub struct HoneycombConfig {
    /// Honeycomb API key
    pub api_key: String,
    /// Honeycomb dataset name
    pub dataset: String,
    /// Service name for identifying this application
    pub service_name: String,
}

impl HoneycombConfig {
    /// Create a new Honeycomb configuration.
    pub fn new(
        api_key: impl Into<String>,
        dataset: impl Into<String>,
        service_name: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            dataset: dataset.into(),
            service_name: service_name.into(),
        }
    }

    /// Create configuration from environment variables.
    ///
    /// Reads from:
    /// - `HONEYCOMB_API_KEY`
    /// - `HONEYCOMB_DATASET`
    /// - `HONEYCOMB_SERVICE_NAME` (defaults to "gallifreydb")
    pub fn from_env() -> Result<Self, Error> {
        let api_key = std::env::var("HONEYCOMB_API_KEY")
            .map_err(|_| Error::other("HONEYCOMB_API_KEY environment variable not set"))?;
        let dataset = std::env::var("HONEYCOMB_DATASET")
            .map_err(|_| Error::other("HONEYCOMB_DATASET environment variable not set"))?;
        let service_name =
            std::env::var("HONEYCOMB_SERVICE_NAME").unwrap_or_else(|_| "gallifreydb".to_string());

        Ok(Self {
            api_key,
            dataset,
            service_name,
        })
    }
}

/// Create a Honeycomb client.
///
/// This provides direct event sending capability with:
/// - Modern `reqwest 0.11+` HTTP client
/// - Exponential backoff retry logic
/// - Event batching
/// - No git dependencies - all crates.io published
///
/// # Example
///
/// ```ignore
/// use gallifreydb::observability::backends::honeycomb::{HoneycombConfig, create_client};
///
/// let config = HoneycombConfig::new("api-key", "dataset", "my-service");
/// let client = create_client(config)?;
///
/// // Send events directly
/// let mut event = client.new_event();
/// event.add_field("operation", "query");
/// event.add_field("duration_ms", 42);
/// client.send(event)?;
///
/// // Flush when done
/// client.flush()?;
/// ```
///
/// # Errors
///
/// Returns an error if the Honeycomb feature is not enabled.
#[cfg(feature = "honeycomb")]
pub fn create_client(config: HoneycombConfig) -> Result<HoneycombClient, Error> {
    let client_config = HoneycombClientConfig::new(config.api_key, config.dataset);
    HoneycombClient::new(client_config).map_err(|e| Error::other(e.to_string()))
}

/// Create a Honeycomb client (stub when feature is disabled)
#[cfg(not(feature = "honeycomb"))]
pub fn create_client(_config: HoneycombConfig) -> Result<(), Error> {
    Err(Error::other(
        "Honeycomb support not compiled in. Enable the 'honeycomb' or 'observability-honeycomb' feature.",
    ))
}

/// Re-export the custom Honeycomb client types when available.
#[cfg(feature = "honeycomb")]
pub use crate::honeycomb::{
    BatchBuffer, Client, Config, Event, Options, Result as HoneycombResult, TransmissionOptions,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_honeycomb_config_new() {
        let config = HoneycombConfig::new("key", "dataset", "service");
        assert_eq!(config.api_key, "key");
        assert_eq!(config.dataset, "dataset");
        assert_eq!(config.service_name, "service");
    }

    #[test]
    fn test_honeycomb_config_clone() {
        let config = HoneycombConfig::new("key", "dataset", "service");
        let cloned = config.clone();
        assert_eq!(config.api_key, cloned.api_key);
    }

    #[test]
    fn test_honeycomb_config_debug() {
        let config = HoneycombConfig::new("key", "dataset", "service");
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("HoneycombConfig"));
    }

    #[cfg(feature = "honeycomb")]
    #[test]
    fn test_create_client() {
        let config = HoneycombConfig::new("key", "dataset", "service");
        // create_client returns Result, and Client::new also returns Result when honeycomb feature is enabled
        let client = create_client(config).expect("Failed to create test client");
        assert_eq!(client.buffered_events(), 0);
    }

    #[cfg(not(feature = "honeycomb"))]
    #[test]
    fn test_create_client_without_feature() {
        let config = HoneycombConfig::new("key", "dataset", "service");
        let result = create_client(config);
        assert!(result.is_err());
    }
}
