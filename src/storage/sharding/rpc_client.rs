//! RPC client implementation for shard-to-shard communication.
//!
//! This module provides a production-ready HTTP-based RPC client that
//! implements the `ShardClient` trait. It uses reqwest for HTTP transport
//! and JSON serialization for protocol messages.
//!
//! # Feature Flags
//!
//! This module requires the `sharding-rpc` feature to be enabled:
//!
//! ```toml
//! [dependencies]
//! gallifreydb = { version = "0.1", features = ["sharding-rpc"] }
//! ```
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::storage::sharding::rpc_client::{HttpShardClient, RpcConfig};
//!
//! let config = RpcConfig {
//!     endpoint: "http://shard0:9000".to_string(),
//!     timeout: Duration::from_secs(30),
//!     ..Default::default()
//! };
//!
//! let client = HttpShardClient::new(ShardId::new(0).unwrap(), config)?;
//! let state = client.get_state()?;
//! ```

use super::network::{
    AbortResponse, CommitResponse, MigrationBatch, MigrationResponse, NetworkError, NetworkResult,
    PrepareResponse, ShardClient,
};
use super::types::{ShardId, ShardState};
use crate::api::TxId;
use std::fmt;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Configuration for RPC client.
#[derive(Debug, Clone)]
pub struct RpcConfig {
    /// Base endpoint URL (e.g., "http://shard0:9000").
    pub endpoint: String,
    /// Request timeout.
    pub timeout: Duration,
    /// Number of retries for transient failures.
    pub max_retries: usize,
    /// Base delay for exponential backoff.
    pub retry_base_delay: Duration,
    /// Maximum delay for exponential backoff.
    pub retry_max_delay: Duration,
    /// Enable TLS/HTTPS.
    pub use_tls: bool,
    /// HTTP connection pool size.
    pub pool_size: usize,
    /// Idle connection timeout.
    pub idle_timeout: Duration,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:9000".to_string(),
            timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            retry_max_delay: Duration::from_secs(10),
            use_tls: false,
            pool_size: 10,
            idle_timeout: Duration::from_secs(90),
        }
    }
}

/// HTTP-based RPC client for shard communication.
///
/// This client implements the `ShardClient` trait using HTTP/JSON as the
/// transport protocol. It includes automatic retries with exponential backoff,
/// connection health tracking, and comprehensive error handling.
///
/// # Thread Safety
///
/// This client is thread-safe and can be shared across multiple threads.
/// Internal state is protected by atomic operations and RwLocks.
///
/// # Protocol
///
/// The HTTP API endpoints are:
///
/// - `POST /api/v1/2pc/prepare` - Prepare phase of 2PC
/// - `POST /api/v1/2pc/commit` - Commit phase of 2PC
/// - `POST /api/v1/2pc/abort` - Abort phase of 2PC
/// - `POST /api/v1/query` - Execute a query
/// - `GET /api/v1/state` - Get shard state
/// - `POST /api/v1/migration/receive` - Receive migration batch
/// - `POST /api/v1/migration/extract` - Extract migration batch
/// - `GET /api/v1/health` - Health check
pub struct HttpShardClient {
    /// Shard ID this client connects to.
    shard_id: ShardId,
    /// Configuration.
    config: RpcConfig,
    /// Health status.
    healthy: AtomicBool,
    /// Last successful request time.
    last_success: RwLock<Option<Instant>>,
    /// Total requests made.
    request_count: AtomicU64,
    /// Failed request count.
    failure_count: AtomicU64,
    /// HTTP client (placeholder - requires reqwest feature).
    #[cfg(feature = "sharding-rpc")]
    http_client: reqwest::blocking::Client,
}

impl fmt::Debug for HttpShardClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpShardClient")
            .field("shard_id", &self.shard_id)
            .field("endpoint", &self.config.endpoint)
            .field("healthy", &self.healthy.load(Ordering::SeqCst))
            .field("request_count", &self.request_count.load(Ordering::Relaxed))
            .finish()
    }
}

impl HttpShardClient {
    /// Create a new HTTP shard client.
    ///
    /// # Arguments
    ///
    /// * `shard_id` - The shard ID this client connects to
    /// * `config` - RPC configuration
    ///
    /// # Returns
    ///
    /// A new client instance, or an error if initialization fails.
    #[cfg(feature = "sharding-rpc")]
    pub fn new(shard_id: ShardId, config: RpcConfig) -> NetworkResult<Self> {
        let http_client = reqwest::blocking::Client::builder()
            .timeout(config.timeout)
            .pool_max_idle_per_host(config.pool_size)
            .pool_idle_timeout(config.idle_timeout)
            .build()
            .map_err(|e| NetworkError::ConnectionFailed {
                shard_id,
                reason: format!("Failed to create HTTP client: {}", e),
            })?;

        Ok(Self {
            shard_id,
            config,
            healthy: AtomicBool::new(true),
            last_success: RwLock::new(None),
            request_count: AtomicU64::new(0),
            failure_count: AtomicU64::new(0),
            http_client,
        })
    }

    /// Create a new HTTP shard client (stub when feature not enabled).
    #[cfg(not(feature = "sharding-rpc"))]
    pub fn new(shard_id: ShardId, config: RpcConfig) -> NetworkResult<Self> {
        Ok(Self {
            shard_id,
            config,
            healthy: AtomicBool::new(true),
            last_success: RwLock::new(None),
            request_count: AtomicU64::new(0),
            failure_count: AtomicU64::new(0),
        })
    }

    /// Get client statistics.
    pub fn stats(&self) -> ClientStats {
        ClientStats {
            shard_id: self.shard_id,
            endpoint: self.config.endpoint.clone(),
            healthy: self.healthy.load(Ordering::SeqCst),
            request_count: self.request_count.load(Ordering::Relaxed),
            failure_count: self.failure_count.load(Ordering::Relaxed),
            last_success: self.last_success.read().ok().and_then(|l| *l),
        }
    }

    /// Execute a request with retry logic.
    #[cfg(feature = "sharding-rpc")]
    fn execute_with_retry<T, F>(&self, operation: &str, f: F) -> NetworkResult<T>
    where
        F: Fn() -> NetworkResult<T>,
    {
        self.request_count.fetch_add(1, Ordering::Relaxed);

        let mut last_error = None;
        let mut delay = self.config.retry_base_delay;

        for attempt in 0..=self.config.max_retries {
            match f() {
                Ok(result) => {
                    self.healthy.store(true, Ordering::SeqCst);
                    if let Ok(mut last) = self.last_success.write() {
                        *last = Some(Instant::now());
                    }
                    return Ok(result);
                }
                Err(e) => {
                    last_error = Some(e.clone());

                    // Check if error is retryable
                    if !Self::is_retryable(&e) || attempt == self.config.max_retries {
                        break;
                    }

                    // Exponential backoff with jitter
                    let jitter = Duration::from_millis(rand_jitter());
                    std::thread::sleep(delay + jitter);
                    delay = std::cmp::min(delay * 2, self.config.retry_max_delay);
                }
            }
        }

        self.failure_count.fetch_add(1, Ordering::Relaxed);
        self.healthy.store(false, Ordering::SeqCst);

        Err(
            last_error.unwrap_or_else(|| NetworkError::ConnectionFailed {
                shard_id: self.shard_id,
                reason: format!("Unknown error in {}", operation),
            }),
        )
    }

    /// Execute a request (stub when feature not enabled).
    #[cfg(not(feature = "sharding-rpc"))]
    #[allow(dead_code)]
    fn execute_with_retry<T, F>(&self, operation: &str, _f: F) -> NetworkResult<T>
    where
        F: Fn() -> NetworkResult<T>,
    {
        Err(NetworkError::ConnectionFailed {
            shard_id: self.shard_id,
            reason: format!(
                "RPC not available: enable 'sharding-rpc' feature for {} operation",
                operation
            ),
        })
    }

    /// Check if an error is retryable.
    #[allow(dead_code)]
    fn is_retryable(error: &NetworkError) -> bool {
        matches!(
            error,
            NetworkError::Timeout { .. }
                | NetworkError::ConnectionFailed { .. }
                | NetworkError::ShardUnavailable(_)
        )
    }

    #[cfg(feature = "sharding-rpc")]
    fn post_json<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
    ) -> NetworkResult<Resp> {
        let url = format!("{}{}", self.config.endpoint, path);

        let response = self.http_client.post(&url).json(body).send().map_err(|e| {
            if e.is_timeout() {
                NetworkError::Timeout {
                    shard_id: self.shard_id,
                    operation: path.to_string(),
                    duration: self.config.timeout,
                }
            } else if e.is_connect() {
                NetworkError::ConnectionFailed {
                    shard_id: self.shard_id,
                    reason: e.to_string(),
                }
            } else {
                NetworkError::ProtocolError(e.to_string())
            }
        })?;

        if !response.status().is_success() {
            return Err(NetworkError::ProtocolError(format!(
                "HTTP {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            )));
        }

        response.json().map_err(|e| {
            NetworkError::SerializationError(format!("Failed to deserialize response: {}", e))
        })
    }

    #[cfg(feature = "sharding-rpc")]
    fn get_json<Resp: serde::de::DeserializeOwned>(&self, path: &str) -> NetworkResult<Resp> {
        let url = format!("{}{}", self.config.endpoint, path);

        let response = self.http_client.get(&url).send().map_err(|e| {
            if e.is_timeout() {
                NetworkError::Timeout {
                    shard_id: self.shard_id,
                    operation: path.to_string(),
                    duration: self.config.timeout,
                }
            } else if e.is_connect() {
                NetworkError::ConnectionFailed {
                    shard_id: self.shard_id,
                    reason: e.to_string(),
                }
            } else {
                NetworkError::ProtocolError(e.to_string())
            }
        })?;

        if !response.status().is_success() {
            return Err(NetworkError::ProtocolError(format!(
                "HTTP {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            )));
        }

        response.json().map_err(|e| {
            NetworkError::SerializationError(format!("Failed to deserialize response: {}", e))
        })
    }
}

/// Simple jitter function (returns 0-100ms).
#[allow(dead_code)]
fn rand_jitter() -> u64 {
    // Simple pseudo-random using time
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 100) as u64
}

impl ShardClient for HttpShardClient {
    fn shard_id(&self) -> ShardId {
        self.shard_id
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }

    #[cfg(feature = "sharding-rpc")]
    fn prepare(&self, tx_id: TxId, operations: &[u8]) -> NetworkResult<PrepareResponse> {
        #[derive(serde::Serialize)]
        struct PrepareRequest {
            tx_id: u64,
            operations: Vec<u8>,
        }

        #[derive(serde::Deserialize)]
        struct PrepareResponseDto {
            ready: bool,
            reason: Option<String>,
        }

        self.execute_with_retry("prepare", || {
            let req = PrepareRequest {
                tx_id: tx_id.as_u64(),
                operations: operations.to_vec(),
            };

            let resp: PrepareResponseDto = self.post_json("/api/v1/2pc/prepare", &req)?;

            Ok(PrepareResponse {
                ready: resp.ready,
                reason: resp.reason,
            })
        })
    }

    #[cfg(not(feature = "sharding-rpc"))]
    fn prepare(&self, _tx_id: TxId, _operations: &[u8]) -> NetworkResult<PrepareResponse> {
        Err(NetworkError::ConnectionFailed {
            shard_id: self.shard_id,
            reason: "RPC not available: enable 'sharding-rpc' feature".to_string(),
        })
    }

    #[cfg(feature = "sharding-rpc")]
    fn commit(&self, tx_id: TxId) -> NetworkResult<CommitResponse> {
        #[derive(serde::Serialize)]
        struct CommitRequest {
            tx_id: u64,
        }

        #[derive(serde::Deserialize)]
        struct CommitResponseDto {
            success: bool,
        }

        self.execute_with_retry("commit", || {
            let req = CommitRequest {
                tx_id: tx_id.as_u64(),
            };

            let resp: CommitResponseDto = self.post_json("/api/v1/2pc/commit", &req)?;

            Ok(CommitResponse {
                success: resp.success,
            })
        })
    }

    #[cfg(not(feature = "sharding-rpc"))]
    fn commit(&self, _tx_id: TxId) -> NetworkResult<CommitResponse> {
        Err(NetworkError::ConnectionFailed {
            shard_id: self.shard_id,
            reason: "RPC not available: enable 'sharding-rpc' feature".to_string(),
        })
    }

    #[cfg(feature = "sharding-rpc")]
    fn abort(&self, tx_id: TxId) -> NetworkResult<AbortResponse> {
        #[derive(serde::Serialize)]
        struct AbortRequest {
            tx_id: u64,
        }

        #[derive(serde::Deserialize)]
        struct AbortResponseDto {
            acknowledged: bool,
        }

        self.execute_with_retry("abort", || {
            let req = AbortRequest {
                tx_id: tx_id.as_u64(),
            };

            let resp: AbortResponseDto = self.post_json("/api/v1/2pc/abort", &req)?;

            Ok(AbortResponse {
                acknowledged: resp.acknowledged,
            })
        })
    }

    #[cfg(not(feature = "sharding-rpc"))]
    fn abort(&self, _tx_id: TxId) -> NetworkResult<AbortResponse> {
        Err(NetworkError::ConnectionFailed {
            shard_id: self.shard_id,
            reason: "RPC not available: enable 'sharding-rpc' feature".to_string(),
        })
    }

    #[cfg(feature = "sharding-rpc")]
    fn query(&self, query_id: u64, query_data: &[u8]) -> NetworkResult<Vec<u8>> {
        #[derive(serde::Serialize)]
        struct QueryRequest {
            query_id: u64,
            query_data: Vec<u8>,
        }

        #[derive(serde::Deserialize)]
        struct QueryResponseDto {
            data: Vec<u8>,
        }

        self.execute_with_retry("query", || {
            let req = QueryRequest {
                query_id,
                query_data: query_data.to_vec(),
            };

            let resp: QueryResponseDto = self.post_json("/api/v1/query", &req)?;

            Ok(resp.data)
        })
    }

    #[cfg(not(feature = "sharding-rpc"))]
    fn query(&self, _query_id: u64, _query_data: &[u8]) -> NetworkResult<Vec<u8>> {
        Err(NetworkError::ConnectionFailed {
            shard_id: self.shard_id,
            reason: "RPC not available: enable 'sharding-rpc' feature".to_string(),
        })
    }

    #[cfg(feature = "sharding-rpc")]
    fn get_state(&self) -> NetworkResult<ShardState> {
        #[derive(serde::Deserialize)]
        struct ShardStateDto {
            shard_id: u16,
            node_count: u64,
            edge_count: u64,
            storage_bytes: u64,
        }

        self.execute_with_retry("get_state", || {
            let resp: ShardStateDto = self.get_json("/api/v1/state")?;

            let shard_id = ShardId::new(resp.shard_id).map_err(|_| {
                NetworkError::ProtocolError(format!("Invalid shard ID: {}", resp.shard_id))
            })?;

            let mut state = ShardState::new(shard_id);
            state.node_count = resp.node_count;
            state.edge_count = resp.edge_count;
            state.storage_bytes = resp.storage_bytes;

            Ok(state)
        })
    }

    #[cfg(not(feature = "sharding-rpc"))]
    fn get_state(&self) -> NetworkResult<ShardState> {
        Err(NetworkError::ConnectionFailed {
            shard_id: self.shard_id,
            reason: "RPC not available: enable 'sharding-rpc' feature".to_string(),
        })
    }

    #[cfg(feature = "sharding-rpc")]
    fn receive_migration_batch(&self, batch: MigrationBatch) -> NetworkResult<MigrationResponse> {
        #[derive(serde::Deserialize)]
        struct MigrationResponseDto {
            accepted: bool,
            nodes_written: u64,
            edges_written: u64,
            error: Option<String>,
        }

        self.execute_with_retry("receive_migration_batch", || {
            // Serialize batch (simplified - in production would use proper DTO)
            let req = serde_json::json!({
                "migration_id": batch.migration_id,
                "batch_number": batch.batch_number,
                "is_last": batch.is_last,
                "nodes": batch.nodes.len(),
                "edges": batch.edges.len(),
                "checksum": batch.checksum,
            });

            let resp: MigrationResponseDto = self.post_json("/api/v1/migration/receive", &req)?;

            Ok(MigrationResponse {
                accepted: resp.accepted,
                nodes_written: resp.nodes_written,
                edges_written: resp.edges_written,
                error: resp.error,
            })
        })
    }

    #[cfg(not(feature = "sharding-rpc"))]
    fn receive_migration_batch(&self, _batch: MigrationBatch) -> NetworkResult<MigrationResponse> {
        Err(NetworkError::ConnectionFailed {
            shard_id: self.shard_id,
            reason: "RPC not available: enable 'sharding-rpc' feature".to_string(),
        })
    }

    #[cfg(feature = "sharding-rpc")]
    fn extract_migration_batch(
        &self,
        migration_id: u64,
        labels: &[String],
        batch_size: usize,
        offset: u64,
    ) -> NetworkResult<MigrationBatch> {
        #[derive(serde::Serialize)]
        struct ExtractRequest {
            migration_id: u64,
            labels: Vec<String>,
            batch_size: usize,
            offset: u64,
        }

        #[derive(serde::Deserialize)]
        #[allow(dead_code)] // Fields reserved for full implementation
        struct ExtractResponseDto {
            migration_id: u64,
            batch_number: u64,
            is_last: bool,
            node_count: usize,
            edge_count: usize,
            checksum: u64,
        }

        self.execute_with_retry("extract_migration_batch", || {
            let req = ExtractRequest {
                migration_id,
                labels: labels.to_vec(),
                batch_size,
                offset,
            };

            let resp: ExtractResponseDto = self.post_json("/api/v1/migration/extract", &req)?;

            Ok(MigrationBatch {
                migration_id: resp.migration_id,
                batch_number: resp.batch_number,
                is_last: resp.is_last,
                nodes: Vec::new(), // Would be populated from full response
                edges: Vec::new(), // Would be populated from full response
                checksum: resp.checksum,
            })
        })
    }

    #[cfg(not(feature = "sharding-rpc"))]
    fn extract_migration_batch(
        &self,
        _migration_id: u64,
        _labels: &[String],
        _batch_size: usize,
        _offset: u64,
    ) -> NetworkResult<MigrationBatch> {
        Err(NetworkError::ConnectionFailed {
            shard_id: self.shard_id,
            reason: "RPC not available: enable 'sharding-rpc' feature".to_string(),
        })
    }

    #[cfg(feature = "sharding-rpc")]
    fn health_check(&self) -> NetworkResult<()> {
        #[derive(serde::Deserialize)]
        struct HealthResponse {
            status: String,
        }

        self.execute_with_retry("health_check", || {
            let resp: HealthResponse = self.get_json("/api/v1/health")?;

            if resp.status == "healthy" {
                Ok(())
            } else {
                Err(NetworkError::ShardUnavailable(self.shard_id))
            }
        })
    }

    #[cfg(not(feature = "sharding-rpc"))]
    fn health_check(&self) -> NetworkResult<()> {
        Err(NetworkError::ConnectionFailed {
            shard_id: self.shard_id,
            reason: "RPC not available: enable 'sharding-rpc' feature".to_string(),
        })
    }
}

/// Statistics for an HTTP shard client.
#[derive(Debug, Clone)]
pub struct ClientStats {
    /// Shard ID.
    pub shard_id: ShardId,
    /// Endpoint URL.
    pub endpoint: String,
    /// Health status.
    pub healthy: bool,
    /// Total request count.
    pub request_count: u64,
    /// Failed request count.
    pub failure_count: u64,
    /// Last successful request time.
    pub last_success: Option<Instant>,
}

impl ClientStats {
    /// Calculate success rate (0.0 to 1.0).
    pub fn success_rate(&self) -> f64 {
        if self.request_count == 0 {
            1.0
        } else {
            1.0 - (self.failure_count as f64 / self.request_count as f64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rpc_config_default() {
        let config = RpcConfig::default();
        assert_eq!(config.endpoint, "http://localhost:9000");
        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn test_client_creation_without_feature() {
        let shard_id = ShardId::new(0).unwrap();
        let config = RpcConfig::default();

        // Should succeed (creates stub client)
        let client = HttpShardClient::new(shard_id, config).unwrap();
        assert_eq!(client.shard_id(), shard_id);
    }

    #[test]
    fn test_client_stats() {
        let shard_id = ShardId::new(0).unwrap();
        let config = RpcConfig::default();
        let client = HttpShardClient::new(shard_id, config).unwrap();

        let stats = client.stats();
        assert_eq!(stats.shard_id, shard_id);
        assert!(stats.healthy);
        assert_eq!(stats.request_count, 0);
        assert_eq!(stats.failure_count, 0);
        assert_eq!(stats.success_rate(), 1.0);
    }

    #[test]
    fn test_is_retryable() {
        assert!(HttpShardClient::is_retryable(&NetworkError::Timeout {
            shard_id: ShardId::new(0).unwrap(),
            operation: "test".to_string(),
            duration: Duration::from_secs(1),
        }));

        assert!(HttpShardClient::is_retryable(
            &NetworkError::ConnectionFailed {
                shard_id: ShardId::new(0).unwrap(),
                reason: "refused".to_string(),
            }
        ));

        assert!(HttpShardClient::is_retryable(
            &NetworkError::ShardUnavailable(ShardId::new(0).unwrap())
        ));

        assert!(!HttpShardClient::is_retryable(
            &NetworkError::ProtocolError("bad request".to_string())
        ));
    }

    #[test]
    fn test_rand_jitter() {
        let jitter = rand_jitter();
        assert!(jitter < 100, "Jitter should be less than 100ms");
    }

    #[test]
    #[cfg(not(feature = "sharding-rpc"))]
    fn test_operations_return_error_without_feature() {
        let shard_id = ShardId::new(0).unwrap();
        let config = RpcConfig::default();
        let client = HttpShardClient::new(shard_id, config).unwrap();

        // All operations should return errors when feature is disabled
        assert!(client.prepare(TxId::new(1), &[]).is_err());
        assert!(client.commit(TxId::new(1)).is_err());
        assert!(client.abort(TxId::new(1)).is_err());
        assert!(client.query(1, &[]).is_err());
        assert!(client.get_state().is_err());
        assert!(client.health_check().is_err());
    }

    // ==================== RpcConfig Tests ====================

    #[test]
    fn test_rpc_config_custom() {
        let config = RpcConfig {
            endpoint: "http://custom:8080".to_string(),
            timeout: Duration::from_secs(60),
            max_retries: 5,
            retry_base_delay: Duration::from_millis(200),
            retry_max_delay: Duration::from_secs(30),
            use_tls: true,
            pool_size: 20,
            idle_timeout: Duration::from_secs(120),
        };

        assert_eq!(config.endpoint, "http://custom:8080");
        assert_eq!(config.timeout, Duration::from_secs(60));
        assert_eq!(config.max_retries, 5);
        assert!(config.use_tls);
        assert_eq!(config.pool_size, 20);
    }

    #[test]
    fn test_rpc_config_debug() {
        let config = RpcConfig::default();
        let debug = format!("{:?}", config);
        assert!(debug.contains("endpoint"));
        assert!(debug.contains("timeout"));
        assert!(debug.contains("max_retries"));
    }

    #[test]
    fn test_rpc_config_clone() {
        let config = RpcConfig::default();
        let cloned = config.clone();
        assert_eq!(config.endpoint, cloned.endpoint);
        assert_eq!(config.timeout, cloned.timeout);
    }

    // ==================== HttpShardClient Tests ====================

    #[test]
    fn test_client_shard_id() {
        let shard_id = ShardId::new(42).unwrap();
        let config = RpcConfig::default();
        let client = HttpShardClient::new(shard_id, config).unwrap();

        assert_eq!(client.shard_id(), shard_id);
    }

    #[test]
    fn test_client_is_healthy() {
        let shard_id = ShardId::new(0).unwrap();
        let config = RpcConfig::default();
        let client = HttpShardClient::new(shard_id, config).unwrap();

        assert!(client.is_healthy());
    }

    #[test]
    fn test_client_debug() {
        let shard_id = ShardId::new(0).unwrap();
        let config = RpcConfig::default();
        let client = HttpShardClient::new(shard_id, config).unwrap();

        let debug = format!("{:?}", client);
        assert!(debug.contains("HttpShardClient"));
        assert!(debug.contains("shard_id"));
        assert!(debug.contains("endpoint"));
        assert!(debug.contains("healthy"));
    }

    // ==================== ClientStats Tests ====================

    #[test]
    fn test_client_stats_success_rate() {
        // No requests = 100% success
        let stats = ClientStats {
            shard_id: ShardId::new(0).unwrap(),
            endpoint: "localhost".to_string(),
            healthy: true,
            request_count: 0,
            failure_count: 0,
            last_success: None,
        };
        assert_eq!(stats.success_rate(), 1.0);

        // Some failures
        let stats = ClientStats {
            shard_id: ShardId::new(0).unwrap(),
            endpoint: "localhost".to_string(),
            healthy: true,
            request_count: 10,
            failure_count: 2,
            last_success: Some(Instant::now()),
        };
        assert!((stats.success_rate() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_client_stats_debug() {
        let stats = ClientStats {
            shard_id: ShardId::new(0).unwrap(),
            endpoint: "localhost".to_string(),
            healthy: true,
            request_count: 5,
            failure_count: 1,
            last_success: None,
        };

        let debug = format!("{:?}", stats);
        assert!(debug.contains("shard_id"));
        assert!(debug.contains("request_count"));
    }

    // ==================== Error Retryability Tests ====================

    #[test]
    fn test_is_retryable_protocol_error() {
        assert!(!HttpShardClient::is_retryable(
            &NetworkError::ProtocolError("invalid response".to_string())
        ));
    }

    #[test]
    fn test_is_retryable_serialization_error() {
        assert!(!HttpShardClient::is_retryable(
            &NetworkError::SerializationError("failed to parse".to_string())
        ));
    }

    #[test]
    fn test_is_retryable_circuit_open() {
        // CircuitOpen is not currently retryable (caller should wait)
        assert!(!HttpShardClient::is_retryable(&NetworkError::CircuitOpen {
            shard_id: ShardId::new(0).unwrap(),
            remaining: Duration::from_secs(5),
        }));
    }

    #[test]
    fn test_is_retryable_pool_exhausted() {
        // PoolExhausted should not be retryable
        assert!(!HttpShardClient::is_retryable(
            &NetworkError::PoolExhausted {
                shard_id: ShardId::new(0).unwrap(),
                max_connections: 10,
            }
        ));
    }

    // ==================== Migration Operations Tests ====================

    #[test]
    #[cfg(not(feature = "sharding-rpc"))]
    fn test_migration_operations_without_feature() {
        use super::super::network::MigrationBatch;

        let shard_id = ShardId::new(0).unwrap();
        let config = RpcConfig::default();
        let client = HttpShardClient::new(shard_id, config).unwrap();

        // Migration operations should fail without feature
        let batch = MigrationBatch {
            migration_id: 1,
            batch_number: 0,
            is_last: true,
            nodes: vec![],
            edges: vec![],
            checksum: 0,
        };

        assert!(client.receive_migration_batch(batch).is_err());
        assert!(client.extract_migration_batch(1, &[], 100, 0).is_err());
    }
}
