//! Honeycomb backend for distributed tracing
//!
//! This module provides integration with Honeycomb.io for distributed tracing.
//!
//! # Feature Flags
//!
//! - `observability-honeycomb-v2`: Uses the new custom Honeycomb client (recommended)
//! - `observability-honeycomb-legacy`: Uses the legacy `libhoney-rust` git dependency
//!
//! The `observability-honeycomb` feature now defaults to v2.

use crate::Error;

// Legacy imports (tracing-honeycomb + libhoney-rust)
#[cfg(feature = "observability-honeycomb-legacy")]
use {libhoney::Config as LibhoneyConfig, tracing_honeycomb::new_honeycomb_telemetry_layer};

// New v2 imports (custom client)
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

/// Create a Honeycomb tracing layer (legacy implementation)
///
/// The layer automatically handles sending telemetry data to Honeycomb in the background.
///
/// # Errors
///
/// Returns an error if the Honeycomb configuration is invalid or if the layer
/// cannot be created.
///
/// # Note
///
/// This function requires the `observability-honeycomb-legacy` feature.
/// For new projects, consider using `create_client()` with the v2 implementation.
#[cfg(feature = "observability-honeycomb-legacy")]
pub fn create_layer<S>(
    config: HoneycombConfig,
) -> Result<Box<dyn tracing_subscriber::Layer<S> + Send + Sync>, Error>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let libhoney_config = LibhoneyConfig {
        options: libhoney::client::Options {
            api_key: config.api_key,
            dataset: config.dataset,
            ..Default::default()
        },
        transmission_options: Default::default(),
    };

    // Leak the service name to get a 'static lifetime (acceptable for telemetry config)
    let service_name: &'static str = Box::leak(config.service_name.into_boxed_str());
    let layer = new_honeycomb_telemetry_layer(service_name, libhoney_config);

    Ok(Box::new(layer))
}

/// Create a Honeycomb tracing layer (stub when legacy feature is disabled)
#[cfg(not(feature = "observability-honeycomb-legacy"))]
pub fn create_layer(_config: HoneycombConfig) -> Result<(), Error> {
    Err(Error::other(
        "Legacy Honeycomb support not compiled in. \
         Enable 'observability-honeycomb-legacy' feature or use create_client() with v2.",
    ))
}

/// Create a Honeycomb client using the new v2 implementation.
///
/// This provides direct event sending capability with:
/// - Modern `reqwest 0.11+` HTTP client
/// - Exponential backoff retry logic
/// - Event batching
/// - No git dependencies
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
    Ok(HoneycombClient::new(client_config))
}

/// Create a Honeycomb client (stub when feature is disabled)
#[cfg(not(feature = "honeycomb"))]
pub fn create_client(_config: HoneycombConfig) -> Result<(), Error> {
    Err(Error::other(
        "Honeycomb v2 support not compiled in. Enable the 'honeycomb' feature.",
    ))
}

/// Re-export the custom Honeycomb client types when available.
#[cfg(feature = "honeycomb")]
pub mod v2 {
    //! New v2 Honeycomb client types.
    //!
    //! This module re-exports the custom Honeycomb client implementation
    //! that replaces the `libhoney-rust` git dependency.

    pub use crate::honeycomb::{
        BatchBuffer, Client, Config, Event, Options, Result, TransmissionOptions,
    };
}

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

    #[test]
    fn test_create_layer_without_feature() {
        // When the legacy feature is not enabled, create_layer should return an error
        #[cfg(not(feature = "observability-honeycomb-legacy"))]
        {
            let config = HoneycombConfig::new("key", "dataset", "service");
            let result = create_layer(config);
            assert!(result.is_err());
        }
    }

    #[cfg(feature = "honeycomb")]
    #[test]
    fn test_create_client() {
        let config = HoneycombConfig::new("key", "dataset", "service");
        let client = create_client(config).unwrap();
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
