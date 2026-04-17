use std::sync::Arc;
use parking_lot::RwLock;

// Mock structures to allow testing the un-covered methods
struct CurrentStorage;
impl CurrentStorage { fn new() -> Self { CurrentStorage } }
struct HistoricalStorage;
impl HistoricalStorage { fn new() -> Self { HistoricalStorage } }

struct ExecutionConfig {
    max_buffer_size: usize,
    parallel: bool,
    timeout_ms: u64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        ExecutionConfig {
            max_buffer_size: 10_000,
            parallel: false,
            timeout_ms: 0,
        }
    }
}

struct QueryExecutor {
    current: Arc<CurrentStorage>,
    historical: Arc<RwLock<HistoricalStorage>>,
    _config: ExecutionConfig,
}

impl QueryExecutor {
    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: ExecutionConfig::default(),
        }
    }

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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let executor = QueryExecutor::new(current, historical);
        assert_eq!(executor._config.max_buffer_size, 10_000);
    }

    #[test]
    fn test_with_config() {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let config = ExecutionConfig { max_buffer_size: 100, parallel: false, timeout_ms: 0 };
        let executor = QueryExecutor::with_config(current, historical, config);
        assert_eq!(executor._config.max_buffer_size, 100);
    }
}
