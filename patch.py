with open('src/query/executor/mod.rs', 'r') as f:
    content = f.read()

# We want to add it to the test module. Let's find the bottom of the test module.
patch = """
    #[test]
    fn test_executor_with_config() {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
            crate::core::version::AnchorConfig::default(),
        )));
        let config = ExecutionConfig {
            max_buffer_size: 100,
            parallel: false,
            timeout_ms: 0,
        };
        let executor = QueryExecutor::with_config(current, historical, config);
        // Verify it was constructed successfully
        assert_eq!(executor._config.max_buffer_size, 100);
    }
}
"""

with open('src/query/executor/mod.rs', 'w') as f:
    f.write(content.replace("    }\n}", "    }\n" + patch))
