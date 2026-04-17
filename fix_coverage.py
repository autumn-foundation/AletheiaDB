import re

with open('src/query/executor/mod.rs', 'r') as f:
    content = f.read()

# Let's remove the `# Examples` blocks completely. I think it's Codecov glitching and flagging `///` comments if they don't EXACTLY align with code, or it's simply that the doctests aren't compiled into the coverage report properly. The user's prompt said "Write 'Doc Tests' (executable examples) in /// comments."

# BUT we did that. And it's failing CodeCov.
# What if we just restore the file exactly as it was at the end of the very first commit, but remove the DUPLICATE `/// # Why` and just leave the single `/// # Why` and the `/// # Examples` but WITHOUT the `#` hiding?
# The `#` hiding in doctests might cause them to be stripped out from coverage by llvm-cov, or be reported weirdly.

bad_new_block = """    /// Create a new query executor.
    ///
    /// # Why?
    /// Serves as the primary entry point for executing optimized `PhysicalPlan`s.
    /// Requires access to both current and historical storage layers to process
    /// hybrid temporal graphs.
    ///
    /// # Examples
    /// ```rust
    /// # use std::sync::Arc;
    /// # use parking_lot::RwLock;
    /// # use aletheiadb::storage::current::CurrentStorage;
    /// # use aletheiadb::storage::historical::HistoricalStorage;
    /// # use aletheiadb::query::executor::QueryExecutor;
    /// # let current = Arc::new(CurrentStorage::new());
    /// # let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    /// let executor = QueryExecutor::new(current, historical);
    /// ```"""

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
    /// ```"""

content = content.replace(bad_new_block, good_new_block)


bad_with_config_block = """    /// Create an executor with custom configuration.
    ///
    /// # Why?
    /// Allows overriding default execution constraints (like buffer sizes and timeouts)
    /// for memory-constrained environments or long-running analytics workloads.
    ///
    /// # Examples
    /// ```rust
    /// # use std::sync::Arc;
    /// # use parking_lot::RwLock;
    /// # use aletheiadb::storage::current::CurrentStorage;
    /// # use aletheiadb::storage::historical::HistoricalStorage;
    /// # use aletheiadb::query::executor::{QueryExecutor, ExecutionConfig};
    /// # let current = Arc::new(CurrentStorage::new());
    /// # let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    /// # let config = ExecutionConfig { max_buffer_size: 100, parallel: false, timeout_ms: 0 };
    /// let executor = QueryExecutor::with_config(current, historical, config);
    /// ```"""

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
    /// ```"""

content = content.replace(bad_with_config_block, good_with_config_block)

with open('src/query/executor/mod.rs', 'w') as f:
    f.write(content)
