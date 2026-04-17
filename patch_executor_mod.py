with open('src/query/executor/mod.rs', 'r') as f:
    content = f.read()

# Replace block with duplicates entirely to fix the file state.
bad_block = """    ///
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
    }

    /// Create an executor with custom configuration.
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
    pub fn with_config("""

good_block = """    /// Create a new query executor.
    ///
    /// # Why?
    /// Serves as the primary entry point for executing optimized `PhysicalPlan`s.
    /// Requires access to both current and historical storage layers to process
    /// hybrid temporal graphs.
    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: ExecutionConfig::default(),
        }
    }

    /// Create an executor with custom configuration.
    ///
    /// # Why?
    /// Allows overriding default execution constraints (like buffer sizes and timeouts)
    /// for memory-constrained environments or long-running analytics workloads.
    pub fn with_config("""

content = content.replace(bad_block, good_block)

with open('src/query/executor/mod.rs', 'w') as f:
    f.write(content)
