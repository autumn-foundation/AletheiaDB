import re

with open('src/query/executor/mod.rs', 'r') as f:
    content = f.read()

# Add Doctests to `QueryExecutor::new` and `QueryExecutor::with_config`
# Make sure to strictly match the starting block to not mess up formatting
bad_new_block = """    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: ExecutionConfig::default(),
        }
    }"""

good_new_block = """    /// Create a new query executor.
    ///
    /// # Why?
    /// Serves as the primary entry point for executing optimized `PhysicalPlan`s.
    /// Requires access to both current and historical storage layers to process
    /// hybrid temporal graphs.
    ///
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

bad_with_config_block = """    /// Create an executor with custom configuration
    pub fn with_config(
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        config: ExecutionConfig,
    ) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: config,
        }
    }"""

good_with_config_block = """    /// Create an executor with custom configuration.
    ///
    /// # Why?
    /// Allows overriding default execution constraints (like buffer sizes and timeouts)
    /// for memory-constrained environments or long-running analytics workloads.
    ///
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
    ) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: config,
        }
    }"""

# Apply Replacements
content = content.replace("    /// Create a new query executor\n" + bad_new_block, good_new_block)
content = content.replace(bad_with_config_block, good_with_config_block)

with open('src/query/executor/mod.rs', 'w') as f:
    f.write(content)
