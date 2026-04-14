//! Prometheus metrics backend
//!
//! This module provides integration with Prometheus for metrics collection.
//! When enabled via the `observability-prometheus` feature, an HTTP server
//! is started that serves metrics in Prometheus format at /metrics.

use crate::Error;

/// Prometheus configuration
#[derive(Debug, Clone)]
pub struct PrometheusConfig {
    /// Bind address for the Prometheus HTTP server (e.g., "127.0.0.1:9090")
    pub bind_addr: String,
}

/// Prometheus metrics backend
///
/// This manages the Prometheus HTTP server and periodic metrics synchronization.
#[cfg(feature = "observability-prometheus")]
pub struct PrometheusBackend {
    bind_addr: String,
}

#[cfg(feature = "observability-prometheus")]
impl PrometheusBackend {
    /// Create a new Prometheus backend
    pub fn new(config: PrometheusConfig) -> Self {
        Self {
            bind_addr: config.bind_addr,
        }
    }

    /// Start the Prometheus HTTP server
    ///
    /// This spawns two things:
    /// 1. An HTTP server on the configured bind address that serves /metrics
    /// 2. A background thread that periodically syncs metrics from our atomic counters
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The bind address is invalid
    /// - The Prometheus exporter cannot be installed
    /// - The HTTP server cannot be started
    pub fn start(self) -> Result<std::thread::JoinHandle<()>, Error> {
        use metrics_exporter_prometheus::PrometheusBuilder;

        let addr: std::net::SocketAddr = self
            .bind_addr
            .parse()
            .map_err(|e| Error::other(format!("Invalid Prometheus bind address: {}", e)))?;

        PrometheusBuilder::new()
            .with_http_listener(addr)
            .install()
            .map_err(|e| Error::other(format!("Prometheus install failed: {}", e)))?;

        // Spawn background thread to sync metrics
        let handle = std::thread::spawn(move || {
            loop {
                sync_metrics_to_prometheus();
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        });

        Ok(handle)
    }
}

/// Synchronize metrics from our atomic counters to Prometheus
///
/// This reads the current snapshot of metrics and updates the Prometheus
/// registry with absolute values.
#[cfg(feature = "observability-prometheus")]
fn sync_metrics_to_prometheus() {
    use metrics::counter;

    let snapshot = crate::observability::METRICS.snapshot();

    // Critical error metrics
    counter!("aletheiadb_lock_poison_total").absolute(snapshot.lock_poison_count);
    counter!("aletheiadb_timestamp_violations_total").absolute(snapshot.timestamp_violations);
    counter!("aletheiadb_wal_checksum_failures_total").absolute(snapshot.wal_checksum_failures);
    counter!("aletheiadb_write_conflicts_total").absolute(snapshot.write_conflicts);

    // Error categorization metrics
    counter!("aletheiadb_errors_total", "category" => "storage")
        .absolute(snapshot.error_storage_total);
    counter!("aletheiadb_errors_total", "category" => "temporal")
        .absolute(snapshot.error_temporal_total);
    counter!("aletheiadb_errors_total", "category" => "query").absolute(snapshot.error_query_total);
    counter!("aletheiadb_errors_total", "category" => "transaction")
        .absolute(snapshot.error_transaction_total);
    counter!("aletheiadb_errors_total", "category" => "vector")
        .absolute(snapshot.error_vector_total);
    counter!("aletheiadb_errors_total", "category" => "io").absolute(snapshot.error_io_total);
    counter!("aletheiadb_errors_total", "category" => "other").absolute(snapshot.error_other_total);
}

#[cfg(not(feature = "observability-prometheus"))]
pub struct PrometheusBackend;

#[cfg(not(feature = "observability-prometheus"))]
impl PrometheusBackend {
    pub fn new(_config: PrometheusConfig) -> Self {
        Self
    }

    pub fn start(self) -> Result<(), Error> {
        Err(Error::other(
            "Prometheus support not compiled in. Enable the 'observability-prometheus' feature.",
        ))
    }
}
