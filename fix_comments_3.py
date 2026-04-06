import re

with open("src/storage/redb_cold_storage.rs", "r") as f:
    content = f.read()

def replace_or_fail(search, replace):
    global content
    if search not in content:
        raise ValueError(f"Search string not found:\n{search}")
    content = content.replace(search, replace)


replace_or_fail('''    /// Store a batch of node versions.
    ///
    /// Encodes and compresses versions in parallel (if batch size exceeds threshold),
    /// then writes them in a single transaction.
    pub fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {''', '''    /// Store a batch of node versions efficiently.
    ///
    /// This method optimizes serialization and compression. If the batch size is
    /// large enough (e.g., > 1024), it uses Rayon to compress payloads in parallel.
    /// It then commits the entire batch in a single atomic database transaction.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::version::NodeVersion;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let versions: Vec<NodeVersion> = vec![]; // populate from WAL
    /// storage.store_node_versions_batch(&versions)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn store_node_versions_batch(&self, versions: &[NodeVersion]) -> Result<()> {''')


replace_or_fail('''    /// Store a batch of edge versions.
    ///
    /// Encodes and compresses versions in parallel (if batch size exceeds threshold),
    /// then writes them in a single transaction.
    pub fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {''', '''    /// Store a batch of edge versions efficiently.
    ///
    /// Parallels `store_node_versions_batch` but for edges. Uses a single
    /// transaction for maximum throughput.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::version::EdgeVersion;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let edges: Vec<EdgeVersion> = vec![]; // populate from WAL
    /// storage.store_edge_versions_batch(&edges)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn store_edge_versions_batch(&self, versions: &[EdgeVersion]) -> Result<()> {''')


replace_or_fail('''    /// Get usage statistics.
    pub fn stats(&self) -> ColdStorageStats {''', '''    /// Get a snapshot of internal usage statistics.
    ///
    /// Provides metrics on bytes read/written and error counts.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let stats = storage.stats();
    /// println!("Written raw bytes: {}", stats.bytes_written_raw);
    /// # Ok(())
    /// # }
    /// ```
    pub fn stats(&self) -> ColdStorageStats {''')


replace_or_fail('''    /// Flush to disk.
    ///
    /// For Redb, this is a no-op as transactions are durable on commit.
    /// This method exists to satisfy the storage trait interface.
    pub fn flush(&self) -> Result<()> {''', '''    /// Flush uncommitted data to disk.
    ///
    /// For `RedbColdStorage`, this is a fast no-op. Redb guarantees that once
    /// a write transaction is committed (which we do eagerly in batch operations),
    /// it is durable on disk.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// storage.flush()?; // Does nothing, returns Ok
    /// # Ok(())
    /// # }
    /// ```
    pub fn flush(&self) -> Result<()> {''')


replace_or_fail('''    /// Close the database connection.
    ///
    /// This releases the file lock and resources.
    /// Note: The actual file lock is released when the inner `redb::Database` is dropped.
    pub fn close(&self) -> Result<()> {''', '''    /// Close the database connection.
    ///
    /// This gives an explicit way to release the file lock before the object
    /// goes out of scope. For Redb, dropping the database instance achieves this.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// storage.close()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn close(&self) -> Result<()> {''')


replace_or_fail('''    /// Compact the database to reclaim space.
    ///
    /// Performs a database-wide compaction. This copies all live data to a new file
    /// and replaces the old file, removing wasted space from deleted or overwritten records.
    ///
    /// # Locking
    ///
    /// This requires a mutable reference (`&mut self`) and will block other operations
    /// until compaction is complete.
    pub fn compact(&mut self) -> Result<()> {''', '''    /// Compact the database file to reclaim free space.
    ///
    /// As records are overwritten or deleted, Redb accumulates fragmented free space.
    /// This method copies all active data into a fresh, tightly-packed file, swapping
    /// it atomically with the old one.
    ///
    /// # Performance
    ///
    /// This operation is I/O heavy and blocks. It requires an exclusive `&mut self`
    /// reference, ensuring no other threads can access the storage during compaction.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let mut storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// // ... after many deletes ...
    /// storage.compact()?; // Space is now reclaimed
    /// # Ok(())
    /// # }
    /// ```
    pub fn compact(&mut self) -> Result<()> {''')


with open("src/storage/redb_cold_storage.rs", "w") as f:
    f.write(content)
