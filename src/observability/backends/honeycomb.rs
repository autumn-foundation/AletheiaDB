//! Honeycomb backend for distributed tracing
//!
//! This module provides integration with Honeycomb.io for distributed tracing.
//! When enabled via the `observability-honeycomb` feature, spans are sent to Honeycomb
//! for analysis and visualization.

use crate::Error;

#[cfg(feature = "observability-honeycomb")]
use {libhoney::Config as LibhoneyConfig, tracing_honeycomb::new_honeycomb_telemetry_layer};

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

/// Create a Honeycomb tracing layer
///
/// The layer automatically handles sending telemetry data to Honeycomb in the background.
///
/// # Errors
///
/// Returns an error if the Honeycomb configuration is invalid or if the layer
/// cannot be created.
#[cfg(feature = "observability-honeycomb")]
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

#[cfg(not(feature = "observability-honeycomb"))]
pub fn create_layer(_config: HoneycombConfig) -> Result<(), Error> {
    Err(Error::other(
        "Honeycomb support not compiled in. Enable the 'observability-honeycomb' feature.",
    ))
}
