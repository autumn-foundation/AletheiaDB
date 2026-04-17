with open('src/query/executor/mod.rs', 'r') as f:
    content = f.read()

# Let's write the # Examples in a way that codecov WILL pick up.
# Actually, the issue is likely that we DID NOT HAVE # Examples for with_config, so it was "added lines were not covered"
# BUT wait! My previous commit removed the duplicate test, and left NO # Examples at all!
# The diff for the previous commit removed the # Examples! Let's put back the # Examples!

bad_new_block = """    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: ExecutionConfig::default(),
        }
    }"""

good_new_block = """    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use parking_lot::RwLock;
    /// use aletheiadb::storage::current::CurrentStorage;
    /// use aletheiadb::storage::historical::HistoricalStorage;
    /// use aletheiadb::query::executor::QueryExecutor;
    ///
    /// let current = Arc::new(CurrentStorage::new());
    /// let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    /// let executor = QueryExecutor::new(current, historical);
    /// ```
    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: ExecutionConfig::default(),
        }
    }"""

bad_with_config_block = """    pub fn with_config(
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        config: ExecutionConfig,
    ) -> Self {"""

good_with_config_block = """    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use parking_lot::RwLock;
    /// use aletheiadb::storage::current::CurrentStorage;
    /// use aletheiadb::storage::historical::HistoricalStorage;
    /// use aletheiadb::query::executor::{QueryExecutor, ExecutionConfig};
    ///
    /// let current = Arc::new(CurrentStorage::new());
    /// let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    /// let config = ExecutionConfig { max_buffer_size: 100, parallel: false, timeout_ms: 0 };
    /// let executor = QueryExecutor::with_config(current, historical, config);
    /// ```
    pub fn with_config(
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        config: ExecutionConfig,
    ) -> Self {"""

content = content.replace(bad_new_block, good_new_block)
content = content.replace(bad_with_config_block, good_with_config_block)

with open('src/query/executor/mod.rs', 'w') as f:
    f.write(content)
