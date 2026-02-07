//! Production observability infrastructure for AletheiaDB.
//!
//! This module provides comprehensive instrumentation for production deployments,
//! including structured logging, metrics collection, and tracing integration.
//!
//! # Feature Flags
//!
//! Observability is **opt-in** via feature flags to ensure zero runtime cost when disabled:
//!
//! - `observability`: Core observability infrastructure (tracing + metrics)
//! - `observability-tracy`: Tracy profiler integration for CPU profiling
//!
//! # Quick Start
//!
//! ```ignore
//! use aletheiadb::observability;
//!
//! // Initialize observability (call once at application startup)
//! observability::init(observability::Config::default());
//!
//! // Metrics are automatically collected
//! let db = AletheiaDB::new();
//!
//! // Periodically check for critical errors
//! let metrics = aletheiadb::metrics();
//! if metrics.has_critical_errors() {
//!     panic!("Data corruption detected!");
//! }
//! ```
//!
//! # Architecture
//!
//! Observability infrastructure is designed to be:
//!
//! 1. **Zero-cost when disabled**: Feature-gated compilation removes all instrumentation
//! 2. **Low overhead when enabled**: <5% performance impact on critical paths
//! 3. **Pluggable backends**: Support for multiple backends (logs, Prometheus, Tracy)
//! 4. **Library-friendly**: Consumers control configuration via API
//!
//! # Critical Metrics
//!
//! The following metrics should **NEVER** be non-zero in production:
//!
//! - **Lock poisoning** (`lock_poison_count`): Thread panicked while holding lock
//! - **Timestamp violations** (`timestamp_violations`): Transaction time not monotonic
//! - **WAL checksum failures** (`wal_checksum_failures`): Durability log corrupted
//!
//! See [`metrics`](metrics/index.html) module for full documentation.

pub mod backends;
pub mod metrics;

// Re-export key types
pub use metrics::{METRICS, Metrics, MetricsSnapshot};

// Re-export backend configs (conditional on features)
#[cfg(feature = "observability-honeycomb")]
pub use backends::honeycomb::HoneycombConfig;

#[cfg(not(feature = "observability-honeycomb"))]
/// Honeycomb configuration (placeholder when feature is disabled)
#[derive(Debug, Clone)]
pub struct HoneycombConfig {
    /// Honeycomb API key
    pub api_key: String,
    /// Dataset name
    pub dataset: String,
    /// Service name
    pub service_name: String,
}

#[cfg(feature = "observability-prometheus")]
pub use backends::prometheus::PrometheusConfig;

#[cfg(not(feature = "observability-prometheus"))]
/// Prometheus configuration (placeholder when feature is disabled)
#[derive(Debug, Clone)]
pub struct PrometheusConfig {
    /// Bind address for HTTP server
    pub bind_addr: String,
}

use std::sync::{Mutex, Once};

static INIT: Once = Once::new();
#[allow(dead_code)] // Used only when observability-honeycomb or observability-prometheus features are enabled
static WORKER_HANDLES: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

/// Observability configuration.
///
/// This controls how observability data is collected and exported.
///
/// # Backend Support
///
/// - **Stdout logging**: Always available with `observability` feature
/// - **Honeycomb**: Requires `observability-honeycomb` feature + API key
/// - **Prometheus**: Requires `observability-prometheus` feature + bind address
/// - **Tracy**: Requires `observability-tracy` feature
///
/// # Configuration
///
/// Use `Config::from_env()` to auto-configure from environment variables:
/// - `HONEYCOMB_API_KEY`: Enable Honeycomb integration
/// - `HONEYCOMB_DATASET`: Dataset name (default: "aletheiadb")
/// - `PROMETHEUS_BIND_ADDR`: Prometheus HTTP endpoint (e.g., "127.0.0.1:9090")
#[derive(Debug, Clone)]
pub struct Config {
    /// Enable structured logging to stdout.
    ///
    /// Default: `true` when observability feature is enabled.
    pub enable_logging: bool,

    /// Enable Tracy profiler integration.
    ///
    /// Default: `true` when `observability-tracy` feature is enabled.
    pub enable_tracy: bool,

    /// Honeycomb configuration (optional).
    ///
    /// When set, spans will be sent to Honeycomb for distributed tracing.
    /// Requires the `observability-honeycomb` feature.
    pub honeycomb: Option<HoneycombConfig>,

    /// Prometheus configuration (optional).
    ///
    /// When set, an HTTP server will expose metrics at /metrics.
    /// Requires the `observability-prometheus` feature.
    pub prometheus: Option<PrometheusConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enable_logging: true,
            #[cfg(feature = "observability-tracy")]
            enable_tracy: true,
            #[cfg(not(feature = "observability-tracy"))]
            enable_tracy: false,
            honeycomb: None,
            prometheus: None,
        }
    }
}

impl Config {
    /// Create a configuration from environment variables.
    ///
    /// This automatically detects available backends based on environment variables:
    ///
    /// - `HONEYCOMB_API_KEY`: Enables Honeycomb (requires `observability-honeycomb` feature)
    /// - `HONEYCOMB_DATASET`: Dataset name (default: "aletheiadb")
    /// - `PROMETHEUS_BIND_ADDR`: Prometheus HTTP server address (requires `observability-prometheus` feature)
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::observability;
    ///
    /// // Set environment variables
    /// std::env::set_var("HONEYCOMB_API_KEY", "your-api-key");
    /// std::env::set_var("PROMETHEUS_BIND_ADDR", "127.0.0.1:9090");
    ///
    /// // Auto-configure from environment
    /// let config = observability::Config::from_env();
    /// observability::init(config);
    /// ```
    pub fn from_env() -> Self {
        Self {
            enable_logging: true,
            #[cfg(feature = "observability-tracy")]
            enable_tracy: true,
            #[cfg(not(feature = "observability-tracy"))]
            enable_tracy: false,
            honeycomb: std::env::var("HONEYCOMB_API_KEY")
                .ok()
                .map(|api_key| HoneycombConfig {
                    api_key,
                    dataset: std::env::var("HONEYCOMB_DATASET")
                        .unwrap_or_else(|_| "aletheiadb".to_string()),
                    service_name: "aletheiadb".to_string(),
                }),
            prometheus: std::env::var("PROMETHEUS_BIND_ADDR")
                .ok()
                .map(|bind_addr| PrometheusConfig { bind_addr }),
        }
    }
}

/// Initialize the observability system.
///
/// This sets up structured logging and tracing infrastructure with support for
/// multiple backends via Registry-based layer composition. It should be called
/// **once** at application startup before creating any AletheiaDB instances.
///
/// # Backends
///
/// Based on the configuration, the following backends may be enabled:
/// - **Stdout logging**: Structured logs to stdout (when `enable_logging` is true)
/// - **Honeycomb**: Distributed tracing (when `honeycomb` config is set)
/// - **Prometheus**: HTTP metrics endpoint (when `prometheus` config is set)
/// - **Tracy**: CPU profiling (when `enable_tracy` is true)
///
/// # Thread Safety
///
/// This function is idempotent and thread-safe. Subsequent calls are ignored.
///
/// # Example
///
/// ```ignore
/// use aletheiadb::observability;
///
/// fn main() {
///     // Initialize with environment-based config
///     observability::init(observability::Config::from_env());
///
///     // Or customize manually
///     let config = observability::Config {
///         enable_logging: true,
///         enable_tracy: false,
///         honeycomb: Some(HoneycombConfig {
///             api_key: "your-api-key".to_string(),
///             dataset: "aletheiadb".to_string(),
///             service_name: "aletheiadb".to_string(),
///         }),
///         prometheus: None,
///     };
///     observability::init(config);
///
///     // Create database
///     let db = aletheiadb::AletheiaDB::new();
/// }
/// ```
///
/// # Environment Variables
///
/// The following environment variables control observability behavior:
///
/// - `RUST_LOG`: Filter logs by level (e.g., `aletheiadb=trace`, `aletheiadb=warn`)
/// - `HONEYCOMB_API_KEY`: Enable Honeycomb integration
/// - `HONEYCOMB_DATASET`: Honeycomb dataset name (default: "aletheiadb")
/// - `PROMETHEUS_BIND_ADDR`: Prometheus HTTP server address (e.g., "127.0.0.1:9090")
///
/// # Panics
///
/// This function will not panic. If initialization fails, errors are logged
/// but execution continues (fail-open for observability).
pub fn init(config: Config) {
    INIT.call_once(|| {
        #[cfg(feature = "observability")]
        {
            use tracing_subscriber::{EnvFilter, Registry, fmt, layer::SubscriberExt};

            let env_filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("aletheiadb=info"));

            // Build subscriber with Registry and add layers conditionally using Option<Layer>
            let subscriber = Registry::default().with(env_filter);

            // Layer 1: Stdout logging (optional)
            let fmt_layer = if config.enable_logging {
                Some(
                    fmt::layer()
                        .with_target(true)
                        .with_thread_ids(true)
                        .with_file(false)
                        .with_line_number(false)
                        .compact(),
                )
            } else {
                None
            };
            let subscriber = subscriber.with(fmt_layer);

            // Layer 2: Honeycomb (optional, feature-gated)
            #[cfg(feature = "observability-honeycomb")]
            let subscriber = if let Some(ref honeycomb_config) = config.honeycomb {
                match backends::honeycomb::create_layer(honeycomb_config.clone()) {
                    Ok(hc_layer) => subscriber.with(Some(hc_layer)),
                    Err(e) => {
                        eprintln!("Failed to initialize Honeycomb: {}", e);
                        subscriber.with(None)
                    }
                }
            } else {
                subscriber.with(None)
            };

            #[cfg(not(feature = "observability-honeycomb"))]
            let subscriber = {
                if config.honeycomb.is_some() {
                    eprintln!(
                        "Honeycomb config provided but observability-honeycomb feature not enabled"
                    );
                }
                subscriber
            };

            // Layer 3: Tracy (optional, feature-gated)
            #[cfg(feature = "observability-tracy")]
            let subscriber = {
                let tracy_layer = if config.enable_tracy {
                    Some(tracing_tracy::TracyLayer::default())
                } else {
                    None
                };
                subscriber.with(tracy_layer)
            };

            #[cfg(not(feature = "observability-tracy"))]
            let subscriber = {
                if config.enable_tracy {
                    eprintln!(
                        "Tracy enabled in config but observability-tracy feature not enabled"
                    );
                }
                subscriber
            };

            // Set the global subscriber
            if let Err(e) = tracing::subscriber::set_global_default(subscriber) {
                eprintln!("Failed to set global tracing subscriber: {}", e);
            }

            // Start Prometheus HTTP server (separate from tracing layers)
            #[cfg(feature = "observability-prometheus")]
            if let Some(ref prometheus_config) = config.prometheus {
                match backends::prometheus::PrometheusBackend::new(prometheus_config.clone())
                    .start()
                {
                    Ok(handle) => {
                        if let Ok(mut handles) = WORKER_HANDLES.lock() {
                            handles.push(handle);
                        }
                    }
                    Err(e) => eprintln!("Failed to start Prometheus server: {}", e),
                }
            }

            #[cfg(not(feature = "observability-prometheus"))]
            if config.prometheus.is_some() {
                eprintln!(
                    "Prometheus config provided but observability-prometheus feature not enabled"
                );
            }

            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                honeycomb_enabled = config.honeycomb.is_some(),
                prometheus_enabled = config.prometheus.is_some(),
                tracy_enabled = config.enable_tracy,
                "AletheiaDB observability initialized"
            );
        }

        #[cfg(not(feature = "observability"))]
        {
            let _ = config; // Suppress unused variable warning
        }
    });
}

/// Get a snapshot of current metrics.
///
/// This is a convenience function that delegates to `METRICS.snapshot()`.
///
/// # Example
///
/// ```ignore
/// use aletheiadb::observability;
///
/// let metrics = observability::metrics();
/// println!("Lock poisons: {}", metrics.lock_poison_count);
/// println!("Write conflicts: {}", metrics.write_conflicts);
///
/// if metrics.has_critical_errors() {
///     eprintln!("CRITICAL: Data corruption detected!");
/// }
/// ```
pub fn metrics() -> MetricsSnapshot {
    METRICS.snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.enable_logging);
        // Tracy enabled only if feature is enabled
        #[cfg(feature = "observability-tracy")]
        assert!(config.enable_tracy);
        #[cfg(not(feature = "observability-tracy"))]
        assert!(!config.enable_tracy);
    }

    #[test]
    fn test_init_idempotent() {
        // Should be safe to call multiple times
        init(Config::default());
        init(Config::default());
        init(Config::default());
    }

    #[test]
    fn test_metrics_function() {
        let snapshot = metrics();
        // Should return a valid snapshot (values depend on test execution order)
        let _ = snapshot.lock_poison_count;
        let _ = snapshot.write_conflicts;
    }
}
