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
    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: ExecutionConfig::default(),
        }
    }"""

bad_with_config_block = """    /// Create an executor with custom configuration.
    ///
    /// # Why?
    /// Allows overriding default execution constraints (like buffer sizes and timeouts)
    /// for memory-constrained environments or long-running analytics workloads.
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

test_patch = """
    #[test]
    fn test_executor_with_config() {
        let (current, historical) = create_test_storage();
        let config = ExecutionConfig {
            max_buffer_size: 500,
            parallel: true,
            timeout_ms: 1000,
        };
        let executor = QueryExecutor::with_config(current, historical, config);
        // Just verify it was created
        assert_eq!(executor._config.max_buffer_size, 500);
    }
"""

content = re.sub(r'}\s*$', test_patch + '}\n', content)

with open('src/query/executor/mod.rs', 'w') as f:
    f.write(content)
