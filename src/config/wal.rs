#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

use crate::config::ConfigError;

/// Configuration for WAL (Write-Ahead Log) system.
///
/// Controls buffer sizes, stripe configuration, flush behavior, and durability settings.
/// This consolidates all WAL-related configuration in one place.
/// Configuration options for the Write-Ahead Log (WAL).
///
/// # The Spark
/// The WAL is the backbone of durability in AletheiaDB. This struct allows you to tune
/// its behavior, such as concurrency (stripes), sync intervals, and directory paths,
/// to balance between latency and throughput.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
#[non_exhaustive]
pub struct WalConfig {
    /// Number of stripes for concurrent appends (must be power of 2).
    /// Higher values improve concurrency but use more memory.
    /// Default: 16
    pub num_stripes: usize,

    /// Ring buffer capacity per stripe.
    /// Larger capacity reduces backpressure but uses more memory.
    /// Default: 1024
    pub stripe_capacity: usize,

    /// Write buffer size for segment files (in bytes).
    /// Affects I/O batching efficiency.
    /// Default: 64KB (65536 bytes)
    pub write_buffer_size: usize,

    /// Maximum segment size before rotation (in bytes).
    /// Default: 64MB (67108864 bytes)
    pub segment_size: usize,

    /// Flush interval in milliseconds for async/group-commit modes.
    /// Lower values reduce latency but increase I/O overhead.
    /// Default: 10ms
    pub flush_interval_ms: u64,

    /// Directory where WAL files are stored.
    /// Default: "aletheiadb/wal"
    pub wal_dir: std::path::PathBuf,

    /// Number of WAL segments to keep for recovery.
    /// Default: 10
    pub segments_to_retain: usize,

    /// Durability mode controlling when data is synced to disk.
    /// This determines the tradeoff between durability guarantees and performance.
    /// Default: GroupCommit (10ms delay, 200 batch size)
    pub durability_mode: crate::storage::wal::DurabilityMode,
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            num_stripes: 16,
            stripe_capacity: 1024,
            write_buffer_size: 64 * 1024,   // 64KB
            segment_size: 64 * 1024 * 1024, // 64MB
            flush_interval_ms: 10,
            wal_dir: std::path::PathBuf::from("aletheiadb/wal"),
            segments_to_retain: 10,
            durability_mode: crate::storage::wal::DurabilityMode::group_commit_default(),
        }
    }
}

/// Builder for WAL configuration.
///
/// Provides a fluent API for constructing WAL configuration with validation.
#[must_use = "builders do nothing unless you call build()"]
#[derive(Debug)]
pub struct WalConfigBuilder {
    config: WalConfig,
}

impl WalConfigBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            config: WalConfig::default(),
        }
    }

    /// Set all validated parameters at once (single validation point).
    ///
    /// This is a convenience method that sets all parameters requiring validation
    /// in a single call, reducing the need for multiple `.unwrap()` calls.
    ///
    /// # Parameters
    ///
    /// - `num_stripes`: Number of stripes for concurrent appends (will be rounded to next power of 2)
    /// - `stripe_capacity`: Ring buffer capacity per stripe
    /// - `write_buffer_size`: Write buffer size in bytes
    /// - `segment_size`: Maximum segment size before rotation in bytes (minimum 512)
    /// - `segments_to_retain`: Number of WAL segments to keep for recovery
    /// - `flush_interval_ms`: Flush interval in milliseconds for async/group-commit modes
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if any parameter is invalid:
    /// - Any value is 0
    /// - `segment_size` is less than 512 bytes
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::WalConfigBuilder;
    ///
    /// let config = WalConfigBuilder::new()
    ///     .with_validated(
    ///         32,              // num_stripes
    ///         2048,            // stripe_capacity
    ///         128 * 1024,      // write_buffer_size
    ///         64 * 1024 * 1024, // segment_size
    ///         10,              // segments_to_retain
    ///         10,              // flush_interval_ms
    ///     ).unwrap()  // Single unwrap!
    ///     .build();
    /// ```
    pub fn with_validated(
        self,
        num_stripes: usize,
        stripe_capacity: usize,
        write_buffer_size: usize,
        segment_size: usize,
        segments_to_retain: usize,
        flush_interval_ms: u64,
    ) -> Result<Self, ConfigError> {
        self.num_stripes(num_stripes)?
            .stripe_capacity(stripe_capacity)?
            .write_buffer_size(write_buffer_size)?
            .segment_size(segment_size)?
            .segments_to_retain(segments_to_retain)?
            .flush_interval_ms(flush_interval_ms)
    }

    /// Set the number of stripes (will be rounded to next power of 2).
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `num_stripes` is 0.
    pub fn num_stripes(mut self, num_stripes: usize) -> Result<Self, ConfigError> {
        if num_stripes == 0 {
            return Err(ConfigError::InvalidValue(
                "num_stripes must be greater than 0".into(),
            ));
        }
        let rounded = num_stripes.next_power_of_two();
        if rounded != num_stripes {
            #[cfg(feature = "observability")]
            tracing::warn!(
                original = num_stripes,
                rounded = rounded,
                "num_stripes rounded to next power of 2"
            );
        }
        self.config.num_stripes = rounded;
        Ok(self)
    }

    /// Set the stripe capacity.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `capacity` is 0.
    pub fn stripe_capacity(mut self, capacity: usize) -> Result<Self, ConfigError> {
        if capacity == 0 {
            return Err(ConfigError::InvalidValue(
                "stripe_capacity must be greater than 0".into(),
            ));
        }
        self.config.stripe_capacity = capacity;
        Ok(self)
    }

    /// Set the write buffer size in bytes.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `size` is 0.
    pub fn write_buffer_size(mut self, size: usize) -> Result<Self, ConfigError> {
        if size == 0 {
            return Err(ConfigError::InvalidValue(
                "write_buffer_size must be greater than 0".into(),
            ));
        }
        self.config.write_buffer_size = size;
        Ok(self)
    }

    /// Set the segment size in bytes.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `size` is 0 or less than 512 bytes.
    ///
    /// **Note**: While 512 bytes is allowed (for testing), production use should
    /// be at least 1MB for reasonable performance.
    pub fn segment_size(mut self, size: usize) -> Result<Self, ConfigError> {
        const MIN_SEGMENT_SIZE: usize = 512; // Allow small sizes for testing
        if size == 0 {
            return Err(ConfigError::InvalidValue(
                "segment_size must be greater than 0".into(),
            ));
        }
        if size < MIN_SEGMENT_SIZE {
            return Err(ConfigError::InvalidValue(format!(
                "segment_size must be at least {} bytes, got {}",
                MIN_SEGMENT_SIZE, size
            )));
        }
        self.config.segment_size = size;
        Ok(self)
    }

    /// Set the flush interval in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `ms` is 0.
    pub fn flush_interval_ms(mut self, ms: u64) -> Result<Self, ConfigError> {
        if ms == 0 {
            return Err(ConfigError::InvalidValue(
                "flush_interval_ms must be greater than 0".into(),
            ));
        }
        self.config.flush_interval_ms = ms;
        Ok(self)
    }

    /// Set the WAL directory path.
    pub fn wal_dir(mut self, path: std::path::PathBuf) -> Self {
        self.config.wal_dir = path;
        self
    }

    /// Set the number of WAL segments to retain.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidValue` if `segments_to_retain` is 0.
    pub fn segments_to_retain(mut self, segments: usize) -> Result<Self, ConfigError> {
        if segments == 0 {
            return Err(ConfigError::InvalidValue(
                "segments_to_retain must be greater than 0".into(),
            ));
        }
        self.config.segments_to_retain = segments;
        Ok(self)
    }

    /// Set the durability mode.
    pub fn durability_mode(mut self, mode: crate::storage::wal::DurabilityMode) -> Self {
        self.config.durability_mode = mode;
        self
    }

    /// Build the configuration.
    pub fn build(self) -> WalConfig {
        self.config
    }
}

impl Default for WalConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}
