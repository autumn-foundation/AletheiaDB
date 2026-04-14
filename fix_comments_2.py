import re

with open("src/storage/redb_cold_storage.rs", "r") as f:
    content = f.read()

def replace_or_fail(search, replace):
    global content
    if search not in content:
        raise ValueError(f"Search string not found:\n{search}")
    content = content.replace(search, replace)


replace_or_fail('''    /// Store a single node version.
    ///
    /// Encodes and compresses the version before writing it to the `node_versions` table.
    pub fn store_node_version(&self, version: &NodeVersion) -> Result<()> {''', '''    /// Store a single node version.
    ///
    /// Encodes and compresses the version before writing it to the `node_versions` table.
    ///
    /// # Performance
    ///
    /// Storing versions one-by-one is slower than batching. Prefer using
    /// [`store_batch_with_lsn`](Self::store_batch_with_lsn) or
    /// [`store_node_versions_batch`](Self::store_node_versions_batch) for bulk data.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::version::NodeVersion;
    /// # use aletheiadb::core::id::{VersionId, NodeId};
    /// # use aletheiadb::core::temporal::BiTemporalInterval;
    /// # use aletheiadb::core::property::PropertyMap;
    /// # use aletheiadb::core::interning::GLOBAL_INTERNER;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp_dir = tempfile::tempdir()?;
    /// let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    ///
    /// let version = NodeVersion::new_anchor(
    ///     VersionId::new(1)?,
    ///     NodeId::new(100)?,
    ///     BiTemporalInterval::current(1000.into()),
    ///     GLOBAL_INTERNER.intern("Person")?,
    ///     PropertyMap::new(),
    /// );
    ///
    /// storage.store_node_version(&version)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn store_node_version(&self, version: &NodeVersion) -> Result<()> {''')


replace_or_fail('''    /// Retrieve a node version by its ID.
    ///
    /// Returns `Ok(None)` if the version does not exist.
    pub fn get_node_version(&self, id: VersionId) -> Result<Option<NodeVersion>> {''', '''    /// Retrieve a node version by its specific `VersionId`.
    ///
    /// Decompresses and deserializes the payload back into a `NodeVersion`.
    /// Returns `Ok(None)` if the version does not exist in the cold storage tier.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// if let Some(version) = storage.get_node_version(id)? {
    ///     println!("Found historical version from transaction {}",
    ///              version.temporal.transaction_time().start().wallclock());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_node_version(&self, id: VersionId) -> Result<Option<NodeVersion>> {''')


replace_or_fail('''    /// Retrieve multiple node versions efficiently.
    pub fn get_node_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<NodeVersion>>> {''', '''    /// Retrieve multiple node versions in a single call.
    ///
    /// Currently, this performs iterative reads, but provides an API surface
    /// for future optimizations (e.g., parallel reads or read-ahead caching).
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let ids = vec![VersionId::new(1)?, VersionId::new(2)?];
    /// let versions = storage.get_node_versions_batch(&ids)?;
    ///
    /// assert_eq!(versions.len(), 2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_node_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<NodeVersion>>> {''')


replace_or_fail('''    /// Store a single edge version.
    pub fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {''', '''    /// Store a single historical edge version.
    ///
    /// Encodes and compresses the edge version before writing it to the `edge_versions` table.
    /// Prefer batch operations for heavy write workloads.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::version::EdgeVersion;
    /// # use aletheiadb::core::id::{VersionId, EdgeId, NodeId};
    /// # use aletheiadb::core::temporal::BiTemporalInterval;
    /// # use aletheiadb::core::property::PropertyMap;
    /// # use aletheiadb::core::interning::GLOBAL_INTERNER;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let version = EdgeVersion::new_anchor(
    ///     VersionId::new(2)?,
    ///     EdgeId::new(500)?,
    ///     BiTemporalInterval::current(1000.into()),
    ///     GLOBAL_INTERNER.intern("KNOWS")?,
    ///     NodeId::new(1)?,
    ///     NodeId::new(2)?,
    ///     PropertyMap::new(),
    /// );
    ///
    /// storage.store_edge_version(&version)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn store_edge_version(&self, version: &EdgeVersion) -> Result<()> {''')


replace_or_fail('''    /// Retrieve an edge version by its ID.
    pub fn get_edge_version(&self, id: VersionId) -> Result<Option<EdgeVersion>> {''', '''    /// Retrieve an edge version by its specific `VersionId`.
    ///
    /// Returns `Ok(None)` if the historical edge version has not been flushed
    /// to cold storage or does not exist.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// let edge_opt = storage.get_edge_version(id)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_edge_version(&self, id: VersionId) -> Result<Option<EdgeVersion>> {''')


replace_or_fail('''    /// Retrieve multiple edge versions efficiently.
    pub fn get_edge_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<EdgeVersion>>> {''', '''    /// Retrieve multiple edge versions in a single API call.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let ids = vec![VersionId::new(1)?];
    /// let versions = storage.get_edge_versions_batch(&ids)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_edge_versions_batch(&self, ids: &[VersionId]) -> Result<Vec<Option<EdgeVersion>>> {''')


replace_or_fail('''    /// Check if a node version exists in cold storage.
    pub fn contains_node_version(&self, id: VersionId) -> Result<bool> {''', '''    /// Quickly check if a node version exists without fetching its payload.
    ///
    /// This is significantly faster than calling [`get_node_version`](Self::get_node_version)
    /// because it skips reading and decompressing the potentially large property payload.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// if storage.contains_node_version(id)? {
    ///     println!("Version exists, but we didn't waste time loading its properties!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn contains_node_version(&self, id: VersionId) -> Result<bool> {''')


replace_or_fail('''    /// Check if an edge version exists in cold storage.
    pub fn contains_edge_version(&self, id: VersionId) -> Result<bool> {''', '''    /// Quickly check if an edge version exists without fetching its payload.
    ///
    /// Avoids decompression overhead if you only need to verify existence.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// assert_eq!(storage.contains_edge_version(id)?, false);
    /// # Ok(())
    /// # }
    /// ```
    pub fn contains_edge_version(&self, id: VersionId) -> Result<bool> {''')


replace_or_fail('''    /// Delete a node version.
    ///
    /// Returns `true` if the version existed and was deleted, `false` otherwise.
    pub fn delete_node_version(&self, id: VersionId) -> Result<bool> {''', '''    /// Permanently delete a node version from cold storage.
    ///
    /// Returns `true` if the version existed and was deleted, `false` if it
    /// was not found. Space won't be recovered immediately; use [`compact`](Self::compact)
    /// to shrink the database file later if needed.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// let did_delete = storage.delete_node_version(id)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn delete_node_version(&self, id: VersionId) -> Result<bool> {''')


replace_or_fail('''    /// Delete an edge version.
    ///
    /// Returns `true` if the version existed and was deleted, `false` otherwise.
    pub fn delete_edge_version(&self, id: VersionId) -> Result<bool> {''', '''    /// Permanently delete an edge version from cold storage.
    ///
    /// Returns `true` if the version existed and was deleted, `false` otherwise.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// # use aletheiadb::storage::redb_cold_storage::RedbColdStorage;
    /// # use aletheiadb::core::id::VersionId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let temp_dir = tempfile::tempdir()?;
    /// # let storage = RedbColdStorage::with_default_config(temp_dir.path().join("db.redb"))?;
    /// let id = VersionId::new(42)?;
    /// let did_delete = storage.delete_edge_version(id)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn delete_edge_version(&self, id: VersionId) -> Result<bool> {''')


with open("src/storage/redb_cold_storage.rs", "w") as f:
    f.write(content)
