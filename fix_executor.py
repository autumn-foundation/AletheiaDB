with open('src/query/executor/mod.rs', 'r') as f:
    content = f.read()

bad_block = """    /// Create an executor with custom configuration.
    ///
    /// # Why?
    /// Allows overriding default execution constraints (like buffer sizes and timeouts)
    /// for memory-constrained environments or long-running analytics workloads.
    pub fn with_config("""

good_block = """    /// Create an executor with custom configuration.
    ///
    /// # Why?
    /// Allows overriding default execution constraints (like buffer sizes and timeouts)
    /// for memory-constrained environments or long-running analytics workloads.
    #[cfg(test)] // Actually wait, if we can't test the doc block easily, let's just make the codecov satisfied by doing the right thing. No, we shouldn't #cfg(test).
    pub fn with_config("""

# The actual issue is we just need the doctest to count as coverage!
# Codecov is saying lines 137-141 are uncovered. Those lines are:
# 137:     /// Create an executor with custom configuration.
# 138:     ///
# 139:     /// # Why?
# 140:     /// Allows overriding default execution constraints (like buffer sizes and timeouts)
# 141:     /// for memory-constrained environments or long-running analytics workloads.

# To cover docstrings in Rust, you have to write a valid rustdoc test inside the doc block.
# Let's add the # Examples block BACK but fix the compiler error!

bad_block_to_replace = """    /// Create an executor with custom configuration.
    ///
    /// # Why?
    /// Allows overriding default execution constraints (like buffer sizes and timeouts)
    /// for memory-constrained environments or long-running analytics workloads.
    pub fn with_config("""

good_block_with_doctest = """    /// Create an executor with custom configuration.
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

content = content.replace(bad_block_to_replace, good_block_with_doctest)

# Similarly for `new`
bad_new_block = """    /// Create a new query executor.
    ///
    /// # Why?
    /// Serves as the primary entry point for executing optimized `PhysicalPlan`s.
    /// Requires access to both current and historical storage layers to process
    /// hybrid temporal graphs.
    pub fn new("""

good_new_block_with_doctest = """    /// Create a new query executor.
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
    pub fn new("""

content = content.replace(bad_new_block, good_new_block_with_doctest)

with open('src/query/executor/mod.rs', 'w') as f:
    f.write(content)
