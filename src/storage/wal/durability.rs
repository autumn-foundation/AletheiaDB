//! Durability mode configuration for WAL operations.
//!
//! This module provides the [`DurabilityMode`] enum which controls when data
//! is synced to disk, and [`WriteOptions`] for per-transaction overrides.

use crate::utils::error::{Result, StorageError};
use std::time::Duration;

/// Durability mode controlling when data is synced to disk.
///
/// GallifreyDB supports four durability modes, each offering different
/// tradeoffs between latency, throughput, and durability guarantees:
///
/// - [`Synchronous`](DurabilityMode::Synchronous): Maximum durability, fsync per commit
/// - [`Async`](DurabilityMode::Async): Maximum throughput, periodic background flush
/// - [`GroupCommit`](DurabilityMode::GroupCommit): ACID durability with high throughput
/// - [`AsyncBatched`](DurabilityMode::AsyncBatched): Low latency with batched fsync (not ACID)
///
/// # Piggybacking Optimization
///
/// All modes benefit from "piggybacking" - when any flush occurs (whether from
/// a Synchronous commit, GroupCommit batch, or Async timer), ALL pending data
/// is flushed. This dynamically reduces the "data at risk" window.
///
/// # Example
///
/// ```ignore
/// use gallifreydb::storage::wal::{WalConfig, DurabilityMode};
///
/// // High-throughput ACID mode with 10ms batching
/// let config = WalConfig::default()
///     .with_durability_mode(DurabilityMode::group_commit(10, 200));
///
/// // Bulk loading mode - fast but not immediately durable
/// let config = WalConfig::default()
///     .with_durability_mode(DurabilityMode::async_mode(100));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
pub enum DurabilityMode {
    /// Fsync after every commit. Maximum durability, minimum throughput.
    ///
    /// Each transaction waits for its data to be durably written to disk
    /// before returning. This is the safest mode but has the highest latency.
    ///
    /// - **Latency**: ~1-5ms per commit (dominated by fsync)
    /// - **Throughput**: Limited by disk IOPS
    /// - **Durability**: Full ACID - no data loss on crash
    /// - **Use case**: Default mode, critical financial data
    Synchronous,

    /// Background thread flushes periodically. High throughput but window of data loss.
    ///
    /// Transactions return immediately after writing to the OS buffer cache.
    /// A background thread periodically fsyncs the WAL. On crash, up to
    /// `flush_interval_ms` worth of commits may be lost.
    ///
    /// - **Latency**: ~10-100µs per commit (no fsync wait)
    /// - **Throughput**: Very high (10,000+ tx/sec)
    /// - **Durability**: NOT ACID - possible data loss window
    /// - **Data Loss Risk**: Up to `flush_interval_ms` of recent commits on crash
    ///   (e.g., 100ms interval = up to 100ms of data at risk)
    /// - **Use case**: Bulk data loading, ETL pipelines, non-critical data
    Async {
        /// How often the background thread flushes (in milliseconds).
        /// Lower values reduce data-at-risk but increase disk I/O.
        flush_interval_ms: u64,
    },

    /// Group commit: multiple transactions share one fsync.
    ///
    /// Transactions wait for a batch flush, but the fsync cost is amortized
    /// across all transactions in the batch. This achieves both ACID durability
    /// AND high throughput - the "holy grail" of database durability.
    ///
    /// - **Latency**: ~2-10ms per commit (wait for batch + fsync)
    /// - **Throughput**: High (1,000-10,000+ tx/sec depending on config)
    /// - **Durability**: Full ACID - no data loss on crash
    /// - **Use case**: High-throughput OLTP workloads
    GroupCommit {
        /// Maximum time to wait for more transactions before flushing.
        /// Higher values batch more transactions but increase latency.
        max_delay_ms: u64,

        /// Maximum transactions to batch before forcing a flush.
        /// When reached, flush happens immediately regardless of delay.
        max_batch_size: usize,
    },

    /// Async mode with batched fsync and optional durability tracking.
    ///
    /// Combines the low latency of Async mode with the intelligent batching
    /// of GroupCommit mode. Transactions return immediately after writing to
    /// the OS cache (no fsync wait), but fsync is batched using configurable
    /// thresholds for efficiency.
    ///
    /// - **Latency**: ~10-100µs per commit (no fsync wait, like Async)
    /// - **Throughput**: Very high (10,000+ tx/sec)
    /// - **Durability**: NOT ACID - eventually durable with tracking
    /// - **Data Loss Risk**: Up to `max_batch_size` operations OR `max_delay_ms`
    ///   window on crash (similar to Async but with smarter batching)
    /// - **Use case**: High-throughput apps that need batching efficiency but
    ///   don't want to block on fsync. Optionally track durability via epochs.
    ///
    /// # Difference from Other Modes
    ///
    /// - **vs Async**: Smarter batching (size + delay triggers) instead of just timer
    /// - **vs GroupCommit**: Returns immediately (no wait), but same batching logic
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::{GallifreyDB, DurabilityMode};
    ///
    /// let mode = DurabilityMode::async_batched_validated(10, 100)?;
    /// let db = GallifreyDB::with_mode(mode);
    ///
    /// // Writes return in <100µs (no fsync wait)
    /// let node_id = db.write(|tx| {
    ///     tx.create_node("FastWrite", properties)
    /// })?;
    ///
    /// // Node immediately visible (in memory)
    /// let node = db.get_node(node_id)?;
    ///
    /// // Background thread batches fsyncs efficiently
    /// ```
    AsyncBatched {
        /// Maximum time to wait before forcing fsync (1-1000ms).
        /// When elapsed, batch is flushed regardless of size.
        max_delay_ms: u64,

        /// Maximum operations to batch before forcing fsync (1-10000).
        /// When reached, batch is flushed regardless of delay.
        max_batch_size: usize,
    },
}

impl Default for DurabilityMode {
    /// Returns [`DurabilityMode::Synchronous`] as the default.
    ///
    /// This ensures maximum durability out of the box. Users who need
    /// higher throughput should explicitly opt into other modes.
    fn default() -> Self {
        DurabilityMode::Synchronous
    }
}

impl DurabilityMode {
    /// Create a new Async mode with the specified flush interval.
    ///
    /// **INTERNAL USE ONLY** - Use [`async_mode_validated()`](Self::async_mode_validated)
    /// for external calls to ensure validation in release builds.
    ///
    /// # Arguments
    ///
    /// * `flush_interval_ms` - How often to flush in milliseconds.
    ///   Typical values: 100ms for bulk loading, 1000ms for background ETL.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `flush_interval_ms` is 0 or > 60000ms.
    /// In release builds, invalid values are NOT validated (security issue).
    #[allow(dead_code)] // Kept for internal const construction
    pub(crate) const fn async_mode(flush_interval_ms: u64) -> Self {
        // Const assertions - will panic in debug builds only
        assert!(
            flush_interval_ms > 0,
            "flush_interval_ms must be greater than 0"
        );
        assert!(
            flush_interval_ms <= 60_000,
            "flush_interval_ms must be <= 60000ms (1 minute)"
        );
        DurabilityMode::Async { flush_interval_ms }
    }

    /// Create a new Async mode with validation.
    ///
    /// Returns an error if the flush interval is out of valid range (1-60000ms).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::WalError`] if validation fails.
    pub fn async_mode_validated(flush_interval_ms: u64) -> Result<Self> {
        if flush_interval_ms == 0 {
            return Err(StorageError::WalError {
                reason: "flush_interval_ms must be greater than 0".to_string(),
            }
            .into());
        }
        if flush_interval_ms > 60_000 {
            return Err(StorageError::WalError {
                reason: "flush_interval_ms must be <= 60000ms (1 minute)".to_string(),
            }
            .into());
        }
        Ok(DurabilityMode::Async { flush_interval_ms })
    }

    /// Create a new GroupCommit mode with the specified parameters.
    ///
    /// **INTERNAL USE ONLY** - Use [`group_commit_validated()`](Self::group_commit_validated)
    /// for external calls to ensure validation in release builds.
    ///
    /// # Arguments
    ///
    /// * `max_delay_ms` - Maximum wait time for batching (default: 10ms)
    /// * `max_batch_size` - Maximum transactions per batch (default: 200)
    ///
    /// # Panics
    ///
    /// Panics in debug builds if parameters are out of valid range.
    /// In release builds, invalid values are NOT validated (security issue).
    #[allow(dead_code)] // Kept for internal const construction
    pub(crate) const fn group_commit(max_delay_ms: u64, max_batch_size: usize) -> Self {
        // Const assertions - will panic in debug builds only
        assert!(max_delay_ms > 0, "max_delay_ms must be greater than 0");
        assert!(
            max_delay_ms <= 1000,
            "max_delay_ms must be <= 1000ms (1 second)"
        );
        assert!(max_batch_size > 0, "max_batch_size must be greater than 0");
        assert!(max_batch_size <= 10_000, "max_batch_size must be <= 10000");
        DurabilityMode::GroupCommit {
            max_delay_ms,
            max_batch_size,
        }
    }

    /// Create a new GroupCommit mode with validation.
    ///
    /// Returns an error if parameters are out of valid range:
    /// - max_delay_ms: 1-1000ms
    /// - max_batch_size: 1-10000
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::WalError`] if validation fails.
    pub fn group_commit_validated(max_delay_ms: u64, max_batch_size: usize) -> Result<Self> {
        if max_delay_ms == 0 {
            return Err(StorageError::WalError {
                reason: "max_delay_ms must be greater than 0".to_string(),
            }
            .into());
        }
        if max_delay_ms > 1000 {
            return Err(StorageError::WalError {
                reason: "max_delay_ms must be <= 1000ms (1 second)".to_string(),
            }
            .into());
        }
        if max_batch_size == 0 {
            return Err(StorageError::WalError {
                reason: "max_batch_size must be greater than 0".to_string(),
            }
            .into());
        }
        if max_batch_size > 10_000 {
            return Err(StorageError::WalError {
                reason: "max_batch_size must be <= 10000".to_string(),
            }
            .into());
        }
        Ok(DurabilityMode::GroupCommit {
            max_delay_ms,
            max_batch_size,
        })
    }

    /// Returns the default GroupCommit configuration (2ms, 200 transactions).
    ///
    /// This provides a good balance between latency and throughput for
    /// typical OLTP workloads, offering ACID guarantees with much better
    /// performance than Synchronous mode.
    ///
    /// - **Latency**: ~2-4ms per transaction (vs 1-5ms for Synchronous)
    /// - **Throughput**: 10-100x higher than Synchronous
    /// - **ACID**: Fully durable, no data loss on crash
    pub const fn group_commit_default() -> Self {
        DurabilityMode::GroupCommit {
            max_delay_ms: 2,
            max_batch_size: 200,
        }
    }

    /// Create a new AsyncBatched mode with validation.
    ///
    /// Returns an error if parameters are out of valid range:
    /// - max_delay_ms: 1-1000ms
    /// - max_batch_size: 1-10000
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::WalError`] if validation fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::storage::wal::DurabilityMode;
    ///
    /// // Create with custom parameters
    /// let mode = DurabilityMode::async_batched_validated(10, 100)?;
    ///
    /// // 10ms max delay, 100 operations max batch size
    /// ```
    pub fn async_batched_validated(max_delay_ms: u64, max_batch_size: usize) -> Result<Self> {
        // Validate max_delay_ms range
        match max_delay_ms {
            0 => {
                return Err(StorageError::WalError {
                    reason: "max_delay_ms must be greater than 0".to_string(),
                }
                .into());
            }
            1..=1000 => {} // Valid range
            _ => {
                return Err(StorageError::WalError {
                    reason: "max_delay_ms must be <= 1000ms (1 second)".to_string(),
                }
                .into());
            }
        }

        // Validate max_batch_size range
        match max_batch_size {
            0 => {
                return Err(StorageError::WalError {
                    reason: "max_batch_size must be greater than 0".to_string(),
                }
                .into());
            }
            1..=10_000 => {} // Valid range
            _ => {
                return Err(StorageError::WalError {
                    reason: "max_batch_size must be <= 10000".to_string(),
                }
                .into());
            }
        }

        Ok(DurabilityMode::AsyncBatched {
            max_delay_ms,
            max_batch_size,
        })
    }

    /// Returns the default AsyncBatched configuration (10ms, 100 transactions).
    ///
    /// This provides very low latency (<100µs) with efficient batching,
    /// suitable for high-throughput applications that can tolerate a small
    /// data loss window on crash.
    ///
    /// - **Latency**: <100µs per transaction (returns immediately)
    /// - **Throughput**: 10,000+ tx/sec
    /// - **Durability**: Eventually durable (NOT ACID)
    /// - **Data at Risk**: Up to 100 operations or 10ms window
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::{GallifreyDB, WalConfig, DurabilityMode};
    ///
    /// let config = WalConfig {
    ///     durability_mode: DurabilityMode::async_batched_default(),
    ///     ..Default::default()
    /// };
    /// let db = GallifreyDB::with_wal_config(config);
    /// ```
    pub const fn async_batched_default() -> Self {
        DurabilityMode::AsyncBatched {
            max_delay_ms: 10,
            max_batch_size: 100,
        }
    }

    /// Returns a high-throughput GroupCommit configuration (1ms, 500 transactions).
    ///
    /// Optimized for maximum write throughput while maintaining ACID guarantees.
    /// Use this for high-volume workloads where sub-millisecond latency is acceptable.
    ///
    /// - **Latency**: ~1-2ms per transaction
    /// - **Throughput**: Up to 1M+ transactions/sec (depending on hardware)
    /// - **ACID**: Fully durable, no data loss on crash
    /// - **Best for**: Bulk imports, high-frequency updates, analytics ingestion
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::{GallifreyDB, WalConfig, DurabilityMode};
    ///
    /// let wal_config = WalConfig {
    ///     durability_mode: DurabilityMode::fast(),
    ///     ..Default::default()
    /// };
    /// let db = GallifreyDB::with_wal_config(wal_config);
    /// ```
    pub const fn fast() -> Self {
        DurabilityMode::GroupCommit {
            max_delay_ms: 1,
            max_batch_size: 500,
        }
    }

    /// Returns true if this mode requires a background flush thread.
    pub const fn needs_background_thread(&self) -> bool {
        matches!(
            self,
            DurabilityMode::Async { .. }
                | DurabilityMode::GroupCommit { .. }
                | DurabilityMode::AsyncBatched { .. }
        )
    }

    /// Returns the flush interval for background modes, or None for Synchronous.
    pub const fn flush_interval(&self) -> Option<Duration> {
        match self {
            DurabilityMode::Synchronous => None,
            DurabilityMode::Async { flush_interval_ms } => {
                Some(Duration::from_millis(*flush_interval_ms))
            }
            DurabilityMode::GroupCommit { max_delay_ms, .. } => {
                Some(Duration::from_millis(*max_delay_ms))
            }
            DurabilityMode::AsyncBatched { max_delay_ms, .. } => {
                Some(Duration::from_millis(*max_delay_ms))
            }
        }
    }

    /// Returns true if transactions should wait for flush completion.
    ///
    /// - Synchronous: Yes (waits for its own fsync)
    /// - Async: No (returns immediately)
    /// - GroupCommit: Yes (waits for batch fsync)
    pub const fn waits_for_durability(&self) -> bool {
        matches!(
            self,
            DurabilityMode::Synchronous | DurabilityMode::GroupCommit { .. }
        )
    }

    /// Returns true if this mode provides ACID durability guarantees.
    pub const fn is_acid_durable(&self) -> bool {
        matches!(
            self,
            DurabilityMode::Synchronous | DurabilityMode::GroupCommit { .. }
        )
    }
}

/// Per-transaction write options that can override database defaults.
///
/// Use this to specify different durability behavior for specific transactions,
/// such as using Async mode for bulk inserts while the database default is
/// Synchronous.
///
/// # Example
///
/// ```ignore
/// use gallifreydb::{GallifreyDB, WriteOptions, DurabilityMode};
///
/// let db = GallifreyDB::new();
///
/// // Use Async mode for this bulk insert
/// let options = WriteOptions::new()
///     .with_durability(DurabilityMode::async_mode(100));
///
/// db.write_with_options(options, |tx| {
///     for item in bulk_data {
///         tx.create_node("Item", item.into())?;
///     }
///     Ok(())
/// })?;
/// ```
#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    /// Override the default durability mode for this transaction.
    /// If None, uses the database's default mode.
    pub durability_mode: Option<DurabilityMode>,
}

impl WriteOptions {
    /// Flush interval for bulk import operations (in milliseconds).
    ///
    /// This value provides a good balance between throughput and data-at-risk
    /// window for typical bulk loading scenarios.
    const BULK_IMPORT_FLUSH_INTERVAL_MS: u64 = 100;

    /// Create new WriteOptions with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the durability mode for this transaction.
    pub fn with_durability(mut self, mode: DurabilityMode) -> Self {
        self.durability_mode = Some(mode);
        self
    }

    /// Get the effective durability mode, falling back to the provided default.
    pub fn effective_durability(&self, default: DurabilityMode) -> DurabilityMode {
        self.durability_mode.unwrap_or(default)
    }

    /// Preset for bulk imports - uses Async mode for maximum throughput.
    ///
    /// This preset configures the transaction to use asynchronous durability
    /// with a 100ms flush interval, providing high write throughput at the
    /// cost of a small data-at-risk window on crash.
    ///
    /// # Use Cases
    ///
    /// - Bulk data loading from external sources
    /// - ETL pipelines where source data can be replayed
    /// - Initial database population
    /// - Non-critical data ingestion
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::{GallifreyDB, WriteOptions};
    ///
    /// let db = GallifreyDB::new();
    ///
    /// // Use bulk_import preset for high-throughput loading
    /// db.write_with_options(WriteOptions::bulk_import(), |tx| {
    ///     for record in million_records {
    ///         tx.create_node("Event", record.into())?;
    ///     }
    ///     Ok(())
    /// })?;
    /// ```
    pub fn bulk_import() -> Self {
        Self {
            durability_mode: Some(DurabilityMode::Async {
                flush_interval_ms: Self::BULK_IMPORT_FLUSH_INTERVAL_MS,
            }),
        }
    }

    /// Preset for critical operations - uses Synchronous mode for maximum durability.
    ///
    /// This preset configures the transaction to use synchronous durability,
    /// waiting for fsync to complete before returning. This ensures that data
    /// is durably written to disk before the transaction completes, providing
    /// full ACID guarantees.
    ///
    /// # Use Cases
    ///
    /// - Financial transactions
    /// - Critical business data
    /// - Audit log entries
    /// - Any data that cannot be lost or replayed
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::{GallifreyDB, WriteOptions};
    ///
    /// let db = GallifreyDB::new();
    ///
    /// // Use critical preset for important data
    /// db.write_with_options(WriteOptions::critical(), |tx| {
    ///     tx.create_node("Payment", payment_data)?;
    ///     tx.create_edge(user_id, payment_id, "MADE_PAYMENT", props)?;
    ///     Ok(())
    /// })?;
    /// ```
    pub fn critical() -> Self {
        Self {
            durability_mode: Some(DurabilityMode::Synchronous),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_synchronous() {
        assert_eq!(DurabilityMode::default(), DurabilityMode::Synchronous);
    }

    #[test]
    fn test_async_mode_constructor() {
        let mode = DurabilityMode::async_mode(100);
        assert_eq!(
            mode,
            DurabilityMode::Async {
                flush_interval_ms: 100
            }
        );
    }

    #[test]
    fn test_group_commit_constructor() {
        let mode = DurabilityMode::group_commit(10, 200);
        assert_eq!(
            mode,
            DurabilityMode::GroupCommit {
                max_delay_ms: 10,
                max_batch_size: 200
            }
        );
    }

    #[test]
    fn test_group_commit_default() {
        let mode = DurabilityMode::group_commit_default();
        assert_eq!(
            mode,
            DurabilityMode::GroupCommit {
                max_delay_ms: 2,
                max_batch_size: 200
            }
        );
    }

    #[test]
    fn test_fast() {
        let mode = DurabilityMode::fast();
        assert_eq!(
            mode,
            DurabilityMode::GroupCommit {
                max_delay_ms: 1,
                max_batch_size: 500
            }
        );
    }

    #[test]
    fn test_needs_background_thread() {
        assert!(!DurabilityMode::Synchronous.needs_background_thread());
        assert!(DurabilityMode::async_mode(100).needs_background_thread());
        assert!(DurabilityMode::group_commit_default().needs_background_thread());
    }

    #[test]
    fn test_flush_interval() {
        assert_eq!(DurabilityMode::Synchronous.flush_interval(), None);
        assert_eq!(
            DurabilityMode::async_mode(100).flush_interval(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            DurabilityMode::group_commit(10, 200).flush_interval(),
            Some(Duration::from_millis(10))
        );
    }

    #[test]
    fn test_waits_for_durability() {
        assert!(DurabilityMode::Synchronous.waits_for_durability());
        assert!(!DurabilityMode::async_mode(100).waits_for_durability());
        assert!(DurabilityMode::group_commit_default().waits_for_durability());
    }

    #[test]
    fn test_is_acid_durable() {
        assert!(DurabilityMode::Synchronous.is_acid_durable());
        assert!(!DurabilityMode::async_mode(100).is_acid_durable());
        assert!(DurabilityMode::group_commit_default().is_acid_durable());
    }

    #[test]
    fn test_write_options_default() {
        let opts = WriteOptions::default();
        assert!(opts.durability_mode.is_none());
    }

    #[test]
    fn test_write_options_with_durability() {
        let opts = WriteOptions::new().with_durability(DurabilityMode::async_mode(50));
        assert_eq!(
            opts.durability_mode,
            Some(DurabilityMode::Async {
                flush_interval_ms: 50
            })
        );
    }

    #[test]
    fn test_effective_durability_uses_override() {
        let opts = WriteOptions::new().with_durability(DurabilityMode::async_mode(50));
        let effective = opts.effective_durability(DurabilityMode::Synchronous);
        assert_eq!(
            effective,
            DurabilityMode::Async {
                flush_interval_ms: 50
            }
        );
    }

    #[test]
    fn test_effective_durability_uses_default_when_none() {
        let opts = WriteOptions::new();
        let effective = opts.effective_durability(DurabilityMode::Synchronous);
        assert_eq!(effective, DurabilityMode::Synchronous);
    }

    #[test]
    fn test_write_options_bulk_import_preset() {
        let opts = WriteOptions::bulk_import();
        assert!(opts.durability_mode.is_some());
        let mode = opts.durability_mode.unwrap();

        // bulk_import should use Async mode with exact 100ms flush interval
        match mode {
            DurabilityMode::Async { flush_interval_ms } => {
                assert_eq!(
                    flush_interval_ms,
                    WriteOptions::BULK_IMPORT_FLUSH_INTERVAL_MS,
                    "bulk_import preset should use the defined flush interval constant"
                );
            }
            _ => panic!("bulk_import should return Async mode, got {:?}", mode),
        }
    }

    #[test]
    fn test_write_options_critical_preset() {
        let opts = WriteOptions::critical();
        assert!(opts.durability_mode.is_some());
        let mode = opts.durability_mode.unwrap();

        // critical should use Synchronous mode for maximum durability
        assert_eq!(
            mode,
            DurabilityMode::Synchronous,
            "critical should return Synchronous mode"
        );
    }

    #[test]
    fn test_preset_methods_can_chain_with_other_methods() {
        // Test that presets can be further customized if needed
        let opts = WriteOptions::bulk_import();

        // Verify the preset is set
        assert!(opts.durability_mode.is_some());

        // Could be chained with other future WriteOptions fields
        // (currently only durability_mode exists, but this tests API design)
        let effective = opts.effective_durability(DurabilityMode::Synchronous);
        assert!(matches!(effective, DurabilityMode::Async { .. }));
    }

    #[test]
    fn test_async_mode_validated_success() {
        let mode = DurabilityMode::async_mode_validated(100).unwrap();
        assert_eq!(
            mode,
            DurabilityMode::Async {
                flush_interval_ms: 100
            }
        );
    }

    #[test]
    fn test_async_mode_validated_zero_fails() {
        let result = DurabilityMode::async_mode_validated(0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("flush_interval_ms"));
        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn test_async_mode_validated_too_large_fails() {
        let result = DurabilityMode::async_mode_validated(60_001);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("flush_interval_ms"));
        assert!(err.to_string().contains("60000ms"));
    }

    #[test]
    fn test_async_mode_validated_boundary_values() {
        // Min valid value
        assert!(DurabilityMode::async_mode_validated(1).is_ok());
        // Max valid value
        assert!(DurabilityMode::async_mode_validated(60_000).is_ok());
    }

    #[test]
    fn test_group_commit_validated_success() {
        let mode = DurabilityMode::group_commit_validated(10, 200).unwrap();
        assert_eq!(
            mode,
            DurabilityMode::GroupCommit {
                max_delay_ms: 10,
                max_batch_size: 200
            }
        );
    }

    #[test]
    fn test_group_commit_validated_zero_delay_fails() {
        let result = DurabilityMode::group_commit_validated(0, 200);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("max_delay_ms"));
        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn test_group_commit_validated_delay_too_large_fails() {
        let result = DurabilityMode::group_commit_validated(1001, 200);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("max_delay_ms"));
        assert!(err.to_string().contains("1000ms"));
    }

    #[test]
    fn test_group_commit_validated_zero_batch_fails() {
        let result = DurabilityMode::group_commit_validated(10, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("max_batch_size"));
        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn test_group_commit_validated_batch_too_large_fails() {
        let result = DurabilityMode::group_commit_validated(10, 10_001);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("max_batch_size"));
        assert!(err.to_string().contains("10000"));
    }

    #[test]
    fn test_group_commit_validated_boundary_values() {
        // Min valid values
        assert!(DurabilityMode::group_commit_validated(1, 1).is_ok());
        // Max valid values
        assert!(DurabilityMode::group_commit_validated(1000, 10_000).is_ok());
    }

    // AsyncBatched mode tests
    #[test]
    fn test_async_batched_constructor() {
        let mode = DurabilityMode::async_batched_validated(10, 100).unwrap();
        assert!(matches!(
            mode,
            DurabilityMode::AsyncBatched {
                max_delay_ms: 10,
                max_batch_size: 100
            }
        ));
    }

    #[test]
    fn test_async_batched_validation_zero_delay_fails() {
        let result = DurabilityMode::async_batched_validated(0, 200);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("max_delay_ms"));
        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn test_async_batched_validation_zero_batch_fails() {
        let result = DurabilityMode::async_batched_validated(10, 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("max_batch_size"));
        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn test_async_batched_validation_boundary_values() {
        // Min valid values
        assert!(DurabilityMode::async_batched_validated(1, 1).is_ok());
        // Max valid values
        assert!(DurabilityMode::async_batched_validated(1000, 10_000).is_ok());
        // Just beyond max
        assert!(DurabilityMode::async_batched_validated(1001, 10).is_err());
        assert!(DurabilityMode::async_batched_validated(10, 10_001).is_err());
    }

    #[test]
    fn test_async_batched_default() {
        let mode = DurabilityMode::async_batched_default();
        assert!(matches!(
            mode,
            DurabilityMode::AsyncBatched {
                max_delay_ms: 10,
                max_batch_size: 100
            }
        ));
    }

    #[test]
    fn test_async_batched_needs_background_thread() {
        let mode = DurabilityMode::async_batched_validated(10, 100).unwrap();
        assert!(mode.needs_background_thread());
    }

    #[test]
    fn test_async_batched_does_not_wait_for_durability() {
        let mode = DurabilityMode::async_batched_validated(10, 100).unwrap();
        // Key difference from GroupCommit - AsyncBatched returns immediately!
        assert!(!mode.waits_for_durability());
    }

    #[test]
    fn test_async_batched_not_acid_durable() {
        let mode = DurabilityMode::async_batched_validated(10, 100).unwrap();
        // Eventually durable, but not ACID (data loss window possible)
        assert!(!mode.is_acid_durable());
    }

    #[test]
    fn test_async_batched_flush_interval() {
        let mode = DurabilityMode::async_batched_validated(42, 100).unwrap();
        assert_eq!(mode.flush_interval(), Some(Duration::from_millis(42)));
    }
}
