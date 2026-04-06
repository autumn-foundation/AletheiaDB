import re

with open("src/storage/redb_cold_storage.rs", "r") as f:
    content = f.read()

def replace_or_fail(search, replace):
    global content
    if search not in content:
        raise ValueError(f"Search string not found:\n{search}")
    content = content.replace(search, replace)

replace_or_fail('''    /// Get the zstd compression level for this algorithm.
    pub fn zstd_level(&self) -> Option<i32> {''', '''    /// Get the `zstd` compression level for this algorithm.
    ///
    /// The compression level balances speed and ratio. This method translates
    /// the abstract `CompressionAlgorithm` enum into the specific integer level
    /// expected by the `zstd` crate.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::CompressionAlgorithm;
    ///
    /// assert_eq!(CompressionAlgorithm::Zstd.zstd_level(), Some(3));
    /// assert_eq!(CompressionAlgorithm::Fast.zstd_level(), Some(1));
    /// assert_eq!(CompressionAlgorithm::None.zstd_level(), None);
    /// ```
    pub fn zstd_level(&self) -> Option<i32> {''')

replace_or_fail('''    /// Calculate the compression ratio (raw/compressed).
    pub fn compression_ratio(&self) -> f64 {''', '''    /// Calculate the compression ratio (raw bytes divided by compressed bytes).
    ///
    /// This helps monitor the effectiveness of the chosen compression algorithm
    /// in the cold storage tier. A higher ratio indicates better compression.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::ColdStorageStats;
    ///
    /// let mut stats = ColdStorageStats::default();
    /// stats.bytes_written_raw = 1000;
    /// stats.bytes_written_compressed = 250;
    /// assert_eq!(stats.compression_ratio(), 4.0); // 4x compression!
    /// ```
    pub fn compression_ratio(&self) -> f64 {''')

replace_or_fail('''    /// Create a new atomic statistics tracker.
    pub fn new() -> Self {''', '''    /// Create a new atomic statistics tracker.
    ///
    /// Initializes all atomic counters to zero. This is used by the cold storage
    /// backend to track metrics across multiple concurrent threads safely.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::AtomicColdStorageStats;
    ///
    /// let stats = AtomicColdStorageStats::new();
    /// ```
    pub fn new() -> Self {''')

replace_or_fail('''    /// Create a snapshot of the current statistics.
    pub fn snapshot(&self) -> ColdStorageStats {''', '''    /// Create a point-in-time snapshot of the current statistics.
    ///
    /// Uses relaxed memory ordering to gather all metrics without expensive locking.
    /// Since the counters are independent, the snapshot might be slightly "fuzzy"
    /// during high contention, but it's perfect for observability.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::AtomicColdStorageStats;
    /// use std::sync::atomic::Ordering;
    ///
    /// let atomic_stats = AtomicColdStorageStats::new();
    /// atomic_stats.bytes_written_raw.store(500, Ordering::Relaxed);
    ///
    /// let snapshot = atomic_stats.snapshot();
    /// assert_eq!(snapshot.bytes_written_raw, 500);
    /// ```
    pub fn snapshot(&self) -> ColdStorageStats {''')

replace_or_fail('''    /// Create a new Redb configuration.
    pub fn new() -> Self {''', '''    /// Create a new Redb configuration with default settings.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::RedbConfig;
    ///
    /// let config = RedbConfig::new();
    /// ```
    pub fn new() -> Self {''')

replace_or_fail('''    /// Set the compression algorithm.
    pub fn compression(mut self, compression: CompressionAlgorithm) -> Self {''', '''    /// Set the compression algorithm for stored data.
    ///
    /// This allows overriding the default `Zstd` compression. Use `Fast` if you
    /// prioritize write speed over disk space, or `None` to disable entirely.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::{RedbConfig, CompressionAlgorithm};
    ///
    /// let config = RedbConfig::new().compression(CompressionAlgorithm::Fast);
    /// ```
    pub fn compression(mut self, compression: CompressionAlgorithm) -> Self {''')

replace_or_fail('''    /// Enable or disable checksums.
    pub fn enable_checksums(mut self, enable: bool) -> Self {''', '''    /// Enable or disable CRC32 checksums for data integrity.
    ///
    /// Redb has built-in checksums, but this adds an application-level layer
    /// of verification for compressed payloads. Enabled by default.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::RedbConfig;
    ///
    /// // Disable checksums to squeeze out a tiny bit more performance
    /// let config = RedbConfig::new().enable_checksums(false);
    /// ```
    pub fn enable_checksums(mut self, enable: bool) -> Self {''')

replace_or_fail('''    /// Set the cache size in bytes.
    pub fn cache_size_bytes(mut self, size: usize) -> Self {''', '''    /// Set the Redb internal cache size in bytes.
    ///
    /// A larger cache improves read performance for frequently accessed historical data.
    /// Set to 0 to use Redb's default cache sizing.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::RedbConfig;
    ///
    /// let config = RedbConfig::new().cache_size_bytes(1024 * 1024 * 64); // 64 MB
    /// ```
    pub fn cache_size_bytes(mut self, size: usize) -> Self {''')

replace_or_fail('''    /// Convert to ColdStorageConfig for compression/checksum handling.
    pub fn to_cold_storage_config(&self) -> ColdStorageConfig {''', '''    /// Convert this `RedbConfig` into a standard `ColdStorageConfig`.
    ///
    /// This is an internal adapter to reuse the common compression logic.
    /// Since Redb handles ACID durability itself, `sync_writes` is always forced to `true`.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::storage::redb_cold_storage::RedbConfig;
    ///
    /// let redb_config = RedbConfig::new();
    /// let cold_config = redb_config.to_cold_storage_config();
    /// assert_eq!(cold_config.sync_writes, true);
    /// ```
    pub fn to_cold_storage_config(&self) -> ColdStorageConfig {''')

replace_or_fail('''    /// Create a new Redb cold storage at the given path.
    ///
    /// # Arguments
    ///
    /// * `path` - Directory or file path for the Redb database file.
    /// * `config` - Configuration for compression and caching.
    ///
    /// # Errors
    ///
    /// Returns an error if the database file cannot be created or opened.
    pub fn new<P: AsRef<Path>>(path: P, config: RedbConfig) -> Result<Self> {''', '''    /// Create a new Redb cold storage at the given path.
    ///
    /// This initializes the database and creates the necessary internal tables
    /// (`node_versions`, `edge_versions`, `metadata`) if they do not exist.
    ///
    /// # Usage
    ///
    /// Use this to explicitly configure the cold storage backend. If you don't need
    /// custom configuration, use [`with_default_config`](Self::with_default_config).
    ///
    /// # Details
    ///
    /// If the parent directories do not exist, this method will attempt to create them.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("my_cold_data.redb");
    ///
    /// let config = RedbConfig::new();
    /// let storage = RedbColdStorage::new(&path, config)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new<P: AsRef<Path>>(path: P, config: RedbConfig) -> Result<Self> {''')

replace_or_fail('''    /// Create with default configuration.
    ///
    /// Equivalent to `RedbColdStorage::new(path, RedbConfig::default())`.
    pub fn with_default_config<P: AsRef<Path>>(path: P) -> Result<Self> {''', '''    /// Create a new Redb cold storage using the default configuration.
    ///
    /// This is a convenience wrapper around [`new`](Self::new) for when you want
    /// standard Zstd compression and default cache sizes.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("default_cold.redb");
    ///
    /// let storage = RedbColdStorage::with_default_config(&path)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_default_config<P: AsRef<Path>>(path: P) -> Result<Self> {''')

replace_or_fail('''    /// Set the encryption cipher for at-rest encryption of stored data.
    ///
    /// When a cipher is set, all stored version data is encrypted after
    /// compression and decrypted before decompression. Metadata (LSN, table
    /// structure) remains unencrypted.
    #[must_use]
    pub fn with_cipher(mut self, cipher: Arc<dyn crate::encryption::cipher::Cipher>) -> Self {''', '''    /// Set the encryption cipher for at-rest encryption of stored data.
    ///
    /// This enables transparent encryption for all historical versions. The data is
    /// compressed *before* being encrypted to preserve compression effectiveness.
    /// Note that table structure and the `flushed_lsn` metadata are NOT encrypted.
    ///
    /// # Usage
    ///
    /// Use this as part of a builder pattern after constructing the storage instance.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::encryption::Aes256GcmCipher;
    /// # use zeroize::Zeroizing;
    /// # use std::sync::Arc;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("secure_cold.redb");
    ///
    /// // Normally you would load this key securely!
    /// let key = Zeroizing::new([0u8; 32]);
    /// let cipher = Arc::new(Aes256GcmCipher::new(&key));
    ///
    /// let storage = RedbColdStorage::with_default_config(&path)?
    ///     .with_cipher(cipher);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_cipher(mut self, cipher: Arc<dyn crate::encryption::cipher::Cipher>) -> Self {''')

replace_or_fail('''    /// Get the database file path.
    pub fn path(&self) -> &Path {''', '''    /// Get the absolute or relative path to the Redb database file.
    ///
    /// This is useful for logging or debugging storage locations.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use std::path::Path;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("my_db.redb");
    /// let storage = RedbColdStorage::with_default_config(&path)?;
    ///
    /// assert_eq!(storage.path(), path.as_path());
    /// # Ok(())
    /// # }
    /// ```
    pub fn path(&self) -> &Path {''')

replace_or_fail('''    /// Set fault injection flag for write operations (Test only).
    #[cfg(test)]
    pub fn set_fail_writes(&self, fail: bool) {''', '''    /// Set the fault injection flag for write operations.
    ///
    /// When set to `true`, the next write operation (like `store_node_version`) will
    /// immediately return an IO error. This is exclusively used to test database
    /// recovery and failure handling.
    ///
    /// #[doc(hidden)]
    #[cfg(test)]
    pub fn set_fail_writes(&self, fail: bool) {''')

replace_or_fail('''    /// Check if a write failure was injected and attempted (Test only).
    #[cfg(test)]
    pub fn was_write_attempted(&self) -> bool {''', '''    /// Check if a write operation was attempted while fault injection was active.
    ///
    /// This allows tests to verify that the code under test actually tried to
    /// write to the database during the failure scenario.
    ///
    /// #[doc(hidden)]
    #[cfg(test)]
    pub fn was_write_attempted(&self) -> bool {''')

replace_or_fail('''    /// Get the flushed LSN from the metadata table.
    pub fn get_flushed_lsn(&self) -> Result<Option<LSN>> {''', '''    /// Get the Log Sequence Number (LSN) of the last safely flushed transaction.
    ///
    /// The Write-Ahead Log uses this value to determine which segments can be
    /// safely truncated and deleted. If a transaction's LSN is less than or equal
    /// to this `flushed_lsn`, it is durably persisted in cold storage.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let path = temp_dir.path().join("db.redb");
    /// let storage = RedbColdStorage::with_default_config(&path)?;
    ///
    /// if let Some(lsn) = storage.get_flushed_lsn()? {
    ///     println!("Safe to truncate WAL up to LSN: {:?}", lsn);
    /// } else {
    ///     println!("No data flushed yet.");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_flushed_lsn(&self) -> Result<Option<LSN>> {''')

with open("src/storage/redb_cold_storage.rs", "w") as f:
    f.write(content)
