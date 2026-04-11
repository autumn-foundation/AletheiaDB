import re

with open("src/query/executor/mod.rs", "r") as f:
    content = f.read()

# ExecutionConfig
content = content.replace(
    "pub struct ExecutionConfig {",
    "/// Configuration for the query executor.\n///\n/// # Details\n///\n/// Holds settings such as target batch size, parallel execution flags,\n/// and maximum hop depth.\npub struct ExecutionConfig {"
)

# QueryExecutor
content = content.replace(
    "pub struct QueryExecutor {",
    "/// Core engine for executing physical query plans.\n///\n/// # Details\n///\n/// Uses standard iterators to pull records out of current and historical\n/// storage seamlessly based on the execution plan logic.\npub struct QueryExecutor {"
)

# QueryExecutor methods
content = content.replace(
    "    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {",
    "    /// Create a new query executor with default config.\n    ///\n    /// # Details\n    ///\n    /// The default configuration prioritizes minimal memory usage but does not\n    /// enable parallel execution.\n    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {"
)

content = content.replace(
    "    pub fn with_config(\n        current: Arc<CurrentStorage>,\n        historical: Arc<RwLock<HistoricalStorage>>,\n        config: ExecutionConfig,\n    ) -> Self {",
    "    /// Create a new query executor with custom config.\n    ///\n    /// # Details\n    ///\n    /// Use this to modify default batch sizes or concurrency targets before\n    /// executing queries.\n    pub fn with_config(\n        current: Arc<CurrentStorage>,\n        historical: Arc<RwLock<HistoricalStorage>>,\n        config: ExecutionConfig,\n    ) -> Self {"
)

content = content.replace(
    "    pub fn execute(&self, plan: PhysicalPlan) -> Result<QueryResults> {",
    "    /// Execute a physical query plan.\n    ///\n    /// # Details\n    ///\n    /// Evaluates the sequence of operators defined in the plan against the underlying\n    /// storage engines. Materializes the results.\n    ///\n    /// # Examples\n    ///\n    /// ```rust\n    /// # use aletheiadb::query::executor::QueryExecutor;\n    /// # use aletheiadb::query::planner::physical::PhysicalPlan;\n    /// # use std::sync::{Arc, RwLock};\n    /// // let executor = QueryExecutor::new(current, historical);\n    /// // let results = executor.execute(plan).unwrap();\n    /// ```\n    pub fn execute(&self, plan: PhysicalPlan) -> Result<QueryResults> {"
)

with open("src/query/executor/mod.rs", "w") as f:
    f.write(content)
