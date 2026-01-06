//! Production observability infrastructure for GallifreyDB.
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
//! use gallifreydb::observability;
//!
//! // Initialize observability (call once at application startup)
//! observability::init(observability::Config::default());
//!
//! // Metrics are automatically collected
//! let db = GallifreyDB::new();
//!
//! // Periodically check for critical errors
//! let metrics = gallifreydb::metrics();
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

pub mod metrics;

// Re-export key types
pub use metrics::{Metrics, MetricsSnapshot, METRICS};

use std::sync::Once;

static INIT: Once = Once::new();

/// Observability configuration.
///
/// This controls how observability data is collected and exported.
/// Currently, this is a placeholder for future configuration options.
///
/// # Future Features
///
/// - Log level configuration (TRACE, DEBUG, INFO, WARN, ERROR)
/// - Backend selection (stdout, files, Prometheus, Tracy)
/// - Sampling rates for high-frequency events
/// - Custom formatters (JSON, logfmt, etc.)
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enable_logging: true,
            #[cfg(feature = "observability-tracy")]
            enable_tracy: true,
            #[cfg(not(feature = "observability-tracy"))]
            enable_tracy: false,
        }
    }
}

/// Initialize the observability system.
///
/// This sets up structured logging and tracing infrastructure. It should be
/// called **once** at application startup before creating any GallifreyDB instances.
///
/// # Thread Safety
///
/// This function is idempotent and thread-safe. Subsequent calls are ignored.
///
/// # Example
///
/// ```ignore
/// use gallifreydb::observability;
///
/// fn main() {
///     // Initialize with default config
///     observability::init(observability::Config::default());
///
///     // Or customize
///     let config = observability::Config {
///         enable_logging: true,
///         enable_tracy: false,
///     };
///     observability::init(config);
///
///     // Create database
///     let db = gallifreydb::GallifreyDB::new();
/// }
/// ```
///
/// # Environment Variables
///
/// The following environment variables control observability behavior:
///
/// - `RUST_LOG`: Filter logs by level (e.g., `gallifreydb=trace`, `gallifreydb=warn`)
/// - `GALLIFREYDB_METRICS_ADDR`: Prometheus metrics endpoint address (future)
///
/// # Panics
///
/// This function will not panic. If initialization fails, errors are logged
/// but execution continues (fail-open for observability).
pub fn init(config: Config) {
    INIT.call_once(|| {
        #[cfg(feature = "observability")]
        {
            if config.enable_logging {
                init_tracing_subscriber();
            }

            #[cfg(feature = "observability-tracy")]
            if config.enable_tracy {
                init_tracy();
            }

            #[cfg(feature = "observability")]
            tracing::info!(
                version = env!("CARGO_PKG_VERSION"),
                tracy_enabled = config.enable_tracy,
                "GallifreyDB observability initialized"
            );
        }
    });
}

/// Initialize the tracing subscriber for structured logging.
///
/// This sets up a subscriber that outputs to stdout with the following features:
/// - Respects RUST_LOG environment variable for filtering
/// - Includes timestamps, level, target, and structured fields
/// - Uses a compact format suitable for production
#[cfg(feature = "observability")]
fn init_tracing_subscriber() {
    use tracing_subscriber::{fmt, EnvFilter};

    // Create subscriber with env filter
    let subscriber = fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("gallifreydb=info")),
        )
        .with_target(true)
        .with_thread_ids(true)
        .with_file(false) // Disable file/line for performance
        .with_line_number(false)
        .compact();

    // Set as global default, ignoring errors (may already be set)
    let _ = tracing::subscriber::set_global_default(subscriber.finish());
}

/// Initialize Tracy profiler integration.
#[cfg(feature = "observability-tracy")]
fn init_tracy() {
    // Tracy integration is automatic when the feature is enabled.
    // tracing-tracy subscriber will be set up by the tracing infrastructure.
    // This is a placeholder for any Tracy-specific initialization needed in the future.
}

/// Get a snapshot of current metrics.
///
/// This is a convenience function that delegates to [`METRICS.snapshot()`].
///
/// # Example
///
/// ```ignore
/// use gallifreydb::observability;
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
