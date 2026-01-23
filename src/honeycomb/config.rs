//! Configuration types for the Honeycomb client.
//!
//! This module provides configuration structs compatible with the `libhoney-rust` API,
//! allowing drop-in replacement for existing `tracing-honeycomb` integrations.

use std::time::Duration;

/// Default Honeycomb API host.
pub const DEFAULT_API_HOST: &str = "https://api.honeycomb.io";

/// Default maximum batch size (events per batch).
pub const DEFAULT_MAX_BATCH_SIZE: usize = 50;

/// Default maximum concurrent batches.
pub const DEFAULT_MAX_CONCURRENT_BATCHES: usize = 10;

/// Default batch timeout.
pub const DEFAULT_BATCH_TIMEOUT: Duration = Duration::from_millis(100);

/// Default pending work capacity.
pub const DEFAULT_PENDING_WORK_CAPACITY: usize = 10_000;

/// Default maximum batch size in bytes (10 MB).
/// Prevents OOM from malicious or buggy events with excessive field counts.
pub const DEFAULT_MAX_BATCH_BYTES: usize = 10 * 1024 * 1024;

/// Client options for connecting to Honeycomb.
///
/// This struct is compatible with `libhoney::client::Options`.
#[derive(Debug, Clone)]
pub struct Options {
    /// Honeycomb API key for authentication.
    pub api_key: String,

    /// Hostname for the Honeycomb API server.
    /// Defaults to `https://api.honeycomb.io`.
    pub api_host: String,

    /// Name of the Honeycomb dataset to send events to.
    pub dataset: String,

    /// Sample rate for events. A value of 1 means all events are sent,
    /// a value of 10 means 1 in 10 events are sent.
    /// Defaults to 1 (no sampling).
    pub sample_rate: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            api_host: DEFAULT_API_HOST.to_string(),
            dataset: String::new(),
            sample_rate: 1,
        }
    }
}

impl Options {
    /// Create new options with the given API key and dataset.
    pub fn new(api_key: impl Into<String>, dataset: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            dataset: dataset.into(),
            ..Default::default()
        }
    }

    /// Set the API key.
    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    /// Set the API host.
    #[must_use]
    pub fn with_api_host(mut self, api_host: impl Into<String>) -> Self {
        self.api_host = api_host.into();
        self
    }

    /// Set the dataset.
    #[must_use]
    pub fn with_dataset(mut self, dataset: impl Into<String>) -> Self {
        self.dataset = dataset.into();
        self
    }

    /// Set the sample rate.
    #[must_use]
    pub fn with_sample_rate(mut self, sample_rate: usize) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    /// Validate the options.
    ///
    /// Returns an error if required fields are missing or invalid.
    pub fn validate(&self) -> Result<(), super::error::Error> {
        if self.api_key.is_empty() {
            return Err(super::error::Error::Config(
                "API key is required".to_string(),
            ));
        }
        if self.dataset.is_empty() {
            return Err(super::error::Error::Config(
                "Dataset is required".to_string(),
            ));
        }
        if self.sample_rate == 0 {
            return Err(super::error::Error::Config(
                "Sample rate must be greater than 0".to_string(),
            ));
        }
        // Prevent DoS via extremely large sample rates that could cause overflow
        const MAX_SAMPLE_RATE: usize = 1_000_000;
        if self.sample_rate > MAX_SAMPLE_RATE {
            return Err(super::error::Error::Config(format!(
                "Sample rate must not exceed {MAX_SAMPLE_RATE}"
            )));
        }
        Ok(())
    }

    /// Returns the batch endpoint URL.
    pub fn batch_endpoint(&self) -> String {
        format!(
            "{}/1/batch/{}",
            self.api_host.trim_end_matches('/'),
            self.dataset
        )
    }
}

/// Transmission options for controlling batching behavior.
///
/// This struct is compatible with `libhoney::transmission::Options`.
#[derive(Debug, Clone)]
pub struct TransmissionOptions {
    /// Maximum number of events to collect before sending a batch.
    /// Defaults to 50.
    pub max_batch_size: usize,

    /// Maximum number of batches that can be sent concurrently.
    /// Defaults to 10.
    pub max_concurrent_batches: usize,

    /// How often to send batches, even if they're not full.
    /// Defaults to 100ms.
    pub batch_timeout: Duration,

    /// Maximum number of events that can be queued for sending.
    /// Defaults to 10,000.
    pub pending_work_capacity: usize,

    /// Additional string to append to the User-Agent header.
    pub user_agent_addition: Option<String>,

    /// Maximum batch size in bytes.
    /// Prevents OOM from events with excessive field counts.
    /// Defaults to 10 MB.
    pub max_batch_bytes: usize,
}

impl Default for TransmissionOptions {
    fn default() -> Self {
        Self {
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            max_concurrent_batches: DEFAULT_MAX_CONCURRENT_BATCHES,
            batch_timeout: DEFAULT_BATCH_TIMEOUT,
            pending_work_capacity: DEFAULT_PENDING_WORK_CAPACITY,
            user_agent_addition: None,
            max_batch_bytes: DEFAULT_MAX_BATCH_BYTES,
        }
    }
}

impl TransmissionOptions {
    /// Create new transmission options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum batch size.
    #[must_use]
    pub fn with_max_batch_size(mut self, size: usize) -> Self {
        self.max_batch_size = size;
        self
    }

    /// Set the maximum concurrent batches.
    #[must_use]
    pub fn with_max_concurrent_batches(mut self, count: usize) -> Self {
        self.max_concurrent_batches = count;
        self
    }

    /// Set the batch timeout.
    #[must_use]
    pub fn with_batch_timeout(mut self, timeout: Duration) -> Self {
        self.batch_timeout = timeout;
        self
    }

    /// Set the pending work capacity.
    #[must_use]
    pub fn with_pending_work_capacity(mut self, capacity: usize) -> Self {
        self.pending_work_capacity = capacity;
        self
    }

    /// Set the user agent addition.
    #[must_use]
    pub fn with_user_agent_addition(mut self, addition: impl Into<String>) -> Self {
        self.user_agent_addition = Some(addition.into());
        self
    }

    /// Set the maximum batch size in bytes.
    ///
    /// This prevents OOM from events with excessive field counts.
    #[must_use]
    pub fn with_max_batch_bytes(mut self, bytes: usize) -> Self {
        self.max_batch_bytes = bytes;
        self
    }

    /// Validate the transmission options.
    pub fn validate(&self) -> Result<(), super::error::Error> {
        if self.max_batch_size == 0 {
            return Err(super::error::Error::Config(
                "Max batch size must be greater than 0".to_string(),
            ));
        }
        if self.max_concurrent_batches == 0 {
            return Err(super::error::Error::Config(
                "Max concurrent batches must be greater than 0".to_string(),
            ));
        }
        if self.pending_work_capacity == 0 {
            return Err(super::error::Error::Config(
                "Pending work capacity must be greater than 0".to_string(),
            ));
        }
        if self.max_batch_bytes == 0 {
            return Err(super::error::Error::Config(
                "Max batch bytes must be greater than 0".to_string(),
            ));
        }
        Ok(())
    }
}

/// Main configuration struct for the Honeycomb client.
///
/// This struct is compatible with `libhoney::Config`.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Client options (API key, dataset, etc.).
    pub options: Options,

    /// Transmission options (batching behavior).
    pub transmission_options: TransmissionOptions,
}

impl Config {
    /// Create a new config with the given API key and dataset.
    pub fn new(api_key: impl Into<String>, dataset: impl Into<String>) -> Self {
        Self {
            options: Options::new(api_key, dataset),
            transmission_options: TransmissionOptions::default(),
        }
    }

    /// Set the API key.
    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.options.api_key = api_key.into();
        self
    }

    /// Set the dataset.
    #[must_use]
    pub fn with_dataset(mut self, dataset: impl Into<String>) -> Self {
        self.options.dataset = dataset.into();
        self
    }

    /// Set the API host.
    #[must_use]
    pub fn with_api_host(mut self, api_host: impl Into<String>) -> Self {
        self.options.api_host = api_host.into();
        self
    }

    /// Set custom client options.
    #[must_use]
    pub fn with_options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    /// Set custom transmission options.
    #[must_use]
    pub fn with_transmission_options(mut self, options: TransmissionOptions) -> Self {
        self.transmission_options = options;
        self
    }

    /// Validate the entire configuration.
    pub fn validate(&self) -> Result<(), super::error::Error> {
        self.options.validate()?;
        self.transmission_options.validate()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =====================================================
    // Options Tests
    // =====================================================

    #[test]
    fn test_options_default() {
        let opts = Options::default();
        assert!(opts.api_key.is_empty());
        assert_eq!(opts.api_host, DEFAULT_API_HOST);
        assert!(opts.dataset.is_empty());
        assert_eq!(opts.sample_rate, 1);
    }

    #[test]
    fn test_options_new() {
        let opts = Options::new("my-api-key", "my-dataset");
        assert_eq!(opts.api_key, "my-api-key");
        assert_eq!(opts.dataset, "my-dataset");
        assert_eq!(opts.api_host, DEFAULT_API_HOST);
        assert_eq!(opts.sample_rate, 1);
    }

    #[test]
    fn test_options_builder_pattern() {
        let opts = Options::default()
            .with_api_key("key123")
            .with_api_host("https://custom.honeycomb.io")
            .with_dataset("test-dataset")
            .with_sample_rate(10);

        assert_eq!(opts.api_key, "key123");
        assert_eq!(opts.api_host, "https://custom.honeycomb.io");
        assert_eq!(opts.dataset, "test-dataset");
        assert_eq!(opts.sample_rate, 10);
    }

    #[test]
    fn test_options_validate_success() {
        let opts = Options::new("api-key", "dataset");
        assert!(opts.validate().is_ok());
    }

    #[test]
    fn test_options_validate_missing_api_key() {
        let opts = Options::default().with_dataset("dataset");
        let err = opts.validate().unwrap_err();
        assert!(err.to_string().contains("API key is required"));
    }

    #[test]
    fn test_options_validate_missing_dataset() {
        let opts = Options::default().with_api_key("key");
        let err = opts.validate().unwrap_err();
        assert!(err.to_string().contains("Dataset is required"));
    }

    #[test]
    fn test_options_validate_zero_sample_rate() {
        let opts = Options::new("key", "dataset").with_sample_rate(0);
        let err = opts.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("Sample rate must be greater than 0")
        );
    }

    #[test]
    fn test_options_validate_excessive_sample_rate() {
        // Sample rate exceeding MAX_SAMPLE_RATE (1,000,000) should be rejected
        let opts = Options::new("key", "dataset").with_sample_rate(1_000_001);
        let err = opts.validate().unwrap_err();
        assert!(err.to_string().contains("must not exceed"));

        // Just at the limit should be fine
        let opts_at_limit = Options::new("key", "dataset").with_sample_rate(1_000_000);
        assert!(opts_at_limit.validate().is_ok());
    }

    #[test]
    fn test_options_batch_endpoint() {
        let opts = Options::new("key", "my-dataset");
        assert_eq!(
            opts.batch_endpoint(),
            "https://api.honeycomb.io/1/batch/my-dataset"
        );

        let opts_custom = opts.with_api_host("https://custom.api/");
        assert_eq!(
            opts_custom.batch_endpoint(),
            "https://custom.api/1/batch/my-dataset"
        );
    }

    #[test]
    fn test_options_clone() {
        let opts1 = Options::new("key", "dataset");
        let opts2 = opts1.clone();
        assert_eq!(opts1.api_key, opts2.api_key);
        assert_eq!(opts1.dataset, opts2.dataset);
    }

    #[test]
    fn test_options_debug() {
        let opts = Options::new("key", "dataset");
        let debug_str = format!("{opts:?}");
        assert!(debug_str.contains("Options"));
        assert!(debug_str.contains("dataset"));
    }

    // =====================================================
    // TransmissionOptions Tests
    // =====================================================

    #[test]
    fn test_transmission_options_default() {
        let opts = TransmissionOptions::default();
        assert_eq!(opts.max_batch_size, DEFAULT_MAX_BATCH_SIZE);
        assert_eq!(opts.max_concurrent_batches, DEFAULT_MAX_CONCURRENT_BATCHES);
        assert_eq!(opts.batch_timeout, DEFAULT_BATCH_TIMEOUT);
        assert_eq!(opts.pending_work_capacity, DEFAULT_PENDING_WORK_CAPACITY);
        assert!(opts.user_agent_addition.is_none());
    }

    #[test]
    fn test_transmission_options_new() {
        let opts = TransmissionOptions::new();
        assert_eq!(opts.max_batch_size, DEFAULT_MAX_BATCH_SIZE);
    }

    #[test]
    fn test_transmission_options_builder_pattern() {
        let opts = TransmissionOptions::new()
            .with_max_batch_size(100)
            .with_max_concurrent_batches(20)
            .with_batch_timeout(Duration::from_millis(200))
            .with_pending_work_capacity(50_000)
            .with_user_agent_addition("my-app/1.0");

        assert_eq!(opts.max_batch_size, 100);
        assert_eq!(opts.max_concurrent_batches, 20);
        assert_eq!(opts.batch_timeout, Duration::from_millis(200));
        assert_eq!(opts.pending_work_capacity, 50_000);
        assert_eq!(opts.user_agent_addition, Some("my-app/1.0".to_string()));
    }

    #[test]
    fn test_transmission_options_validate_success() {
        let opts = TransmissionOptions::default();
        assert!(opts.validate().is_ok());
    }

    #[test]
    fn test_transmission_options_validate_zero_batch_size() {
        let opts = TransmissionOptions::default().with_max_batch_size(0);
        let err = opts.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("Max batch size must be greater than 0")
        );
    }

    #[test]
    fn test_transmission_options_validate_zero_concurrent_batches() {
        let opts = TransmissionOptions::default().with_max_concurrent_batches(0);
        let err = opts.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("Max concurrent batches must be greater than 0")
        );
    }

    #[test]
    fn test_transmission_options_validate_zero_capacity() {
        let opts = TransmissionOptions::default().with_pending_work_capacity(0);
        let err = opts.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("Pending work capacity must be greater than 0")
        );
    }

    #[test]
    fn test_transmission_options_validate_zero_batch_bytes() {
        let opts = TransmissionOptions::default().with_max_batch_bytes(0);
        let err = opts.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("Max batch bytes must be greater than 0")
        );
    }

    #[test]
    fn test_transmission_options_max_batch_bytes() {
        let opts = TransmissionOptions::default().with_max_batch_bytes(5 * 1024 * 1024);
        assert_eq!(opts.max_batch_bytes, 5 * 1024 * 1024);
    }

    #[test]
    fn test_transmission_options_default_max_batch_bytes() {
        let opts = TransmissionOptions::default();
        assert_eq!(opts.max_batch_bytes, DEFAULT_MAX_BATCH_BYTES);
    }

    #[test]
    fn test_transmission_options_clone() {
        let opts1 = TransmissionOptions::new().with_max_batch_size(100);
        let opts2 = opts1.clone();
        assert_eq!(opts1.max_batch_size, opts2.max_batch_size);
    }

    #[test]
    fn test_transmission_options_debug() {
        let opts = TransmissionOptions::new();
        let debug_str = format!("{opts:?}");
        assert!(debug_str.contains("TransmissionOptions"));
        assert!(debug_str.contains("max_batch_size"));
    }

    // =====================================================
    // Config Tests
    // =====================================================

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.options.api_key.is_empty());
        assert!(config.options.dataset.is_empty());
        assert_eq!(
            config.transmission_options.max_batch_size,
            DEFAULT_MAX_BATCH_SIZE
        );
    }

    #[test]
    fn test_config_new() {
        let config = Config::new("api-key", "dataset");
        assert_eq!(config.options.api_key, "api-key");
        assert_eq!(config.options.dataset, "dataset");
    }

    #[test]
    fn test_config_builder_pattern() {
        let config = Config::default()
            .with_api_key("key")
            .with_dataset("data")
            .with_api_host("https://custom.api");

        assert_eq!(config.options.api_key, "key");
        assert_eq!(config.options.dataset, "data");
        assert_eq!(config.options.api_host, "https://custom.api");
    }

    #[test]
    fn test_config_with_custom_options() {
        let options = Options::new("custom-key", "custom-dataset").with_sample_rate(5);

        let config = Config::default().with_options(options);

        assert_eq!(config.options.api_key, "custom-key");
        assert_eq!(config.options.dataset, "custom-dataset");
        assert_eq!(config.options.sample_rate, 5);
    }

    #[test]
    fn test_config_with_transmission_options() {
        let tx_opts = TransmissionOptions::new().with_max_batch_size(200);

        let config = Config::new("key", "dataset").with_transmission_options(tx_opts);

        assert_eq!(config.transmission_options.max_batch_size, 200);
    }

    #[test]
    fn test_config_validate_success() {
        let config = Config::new("key", "dataset");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validate_invalid_options() {
        let config = Config::default().with_dataset("dataset");
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("API key is required"));
    }

    #[test]
    fn test_config_validate_invalid_transmission_options() {
        let tx_opts = TransmissionOptions::new().with_max_batch_size(0);
        let config = Config::new("key", "dataset").with_transmission_options(tx_opts);
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("Max batch size"));
    }

    #[test]
    fn test_config_clone() {
        let config1 = Config::new("key", "dataset");
        let config2 = config1.clone();
        assert_eq!(config1.options.api_key, config2.options.api_key);
    }

    #[test]
    fn test_config_debug() {
        let config = Config::new("key", "dataset");
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("Config"));
        assert!(debug_str.contains("options"));
        assert!(debug_str.contains("transmission_options"));
    }

    // =====================================================
    // API Compatibility Tests (libhoney-rust parity)
    // =====================================================

    #[test]
    fn test_libhoney_api_compatibility() {
        // Test that the API matches libhoney-rust usage patterns
        let config = Config {
            options: Options {
                api_key: "my-key".to_string(),
                dataset: "my-dataset".to_string(),
                ..Default::default()
            },
            transmission_options: Default::default(),
        };

        assert_eq!(config.options.api_key, "my-key");
        assert_eq!(config.options.dataset, "my-dataset");
        assert_eq!(config.options.api_host, DEFAULT_API_HOST);
        assert_eq!(config.options.sample_rate, 1);
    }

    #[test]
    fn test_transmission_module_alias() {
        // Verify the transmission module re-export works
        use super::super::transmission::Options as TransmissionOpts;
        let opts: TransmissionOpts = TransmissionOpts::default();
        assert_eq!(opts.max_batch_size, DEFAULT_MAX_BATCH_SIZE);
    }

    #[test]
    fn test_client_module_alias() {
        // Verify the client_types module re-export works
        use super::super::client_types::Options as ClientOpts;
        let opts: ClientOpts = ClientOpts::new("key", "dataset");
        assert_eq!(opts.api_key, "key");
    }
}
