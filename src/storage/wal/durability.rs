//! Durability mode configuration for WAL operations.
//!
//! This module provides the [`DurabilityMode`] enum which controls when data
//! is synced to disk, and [`WriteOptions`] for per-transaction overrides.

use std::time::Duration;

/// Durability mode controlling when data is synced to disk.
///
/// GallifreyDB supports three durability modes, each offering different
/// tradeoffs between latency, throughput, and durability guarantees:
///
/// - [`Synchronous`](DurabilityMode::Synchronous): Maximum durability, fsync per commit
/// - [`Async`](DurabilityMode::Async): Maximum throughput, periodic background flush
/// - [`GroupCommit`](DurabilityMode::GroupCommit): ACID durability with high throughput
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
    /// # Arguments
    ///
    /// * `flush_interval_ms` - How often to flush in milliseconds.
    ///   Typical values: 100ms for bulk loading, 1000ms for background ETL.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mode = DurabilityMode::async_mode(100); // Flush every 100ms
    /// ```
    pub const fn async_mode(flush_interval_ms: u64) -> Self {
        DurabilityMode::Async { flush_interval_ms }
    }

    /// Create a new GroupCommit mode with the specified parameters.
    ///
    /// # Arguments
    ///
    /// * `max_delay_ms` - Maximum wait time for batching (default: 10ms)
    /// * `max_batch_size` - Maximum transactions per batch (default: 200)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Low latency: 2ms delay, small batches
    /// let mode = DurabilityMode::group_commit(2, 50);
    ///
    /// // High throughput: 10ms delay, large batches
    /// let mode = DurabilityMode::group_commit(10, 200);
    /// ```
    pub const fn group_commit(max_delay_ms: u64, max_batch_size: usize) -> Self {
        DurabilityMode::GroupCommit {
            max_delay_ms,
            max_batch_size,
        }
    }

    /// Returns the default GroupCommit configuration (10ms, 200 transactions).
    ///
    /// This provides a good balance between latency and throughput for
    /// typical OLTP workloads.
    pub const fn group_commit_default() -> Self {
        DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 200,
        }
    }

    /// Returns true if this mode requires a background flush thread.
    pub const fn needs_background_thread(&self) -> bool {
        matches!(
            self,
            DurabilityMode::Async { .. } | DurabilityMode::GroupCommit { .. }
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
                max_delay_ms: 10,
                max_batch_size: 200
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
}
