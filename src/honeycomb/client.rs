//! Honeycomb client for event transmission.
//!
//! This module provides the HTTP client for sending events to Honeycomb's
//! batch API with exponential backoff retry logic.

use std::sync::Arc;
use std::time::Duration;

use super::batch::{BatchBuffer, BatchStats};
use super::config::Config;
use super::error::{Error, Result};
use super::event::Event;

/// Default initial retry delay.
pub const DEFAULT_INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Default maximum retry delay.
pub const DEFAULT_MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Default maximum number of retries.
pub const DEFAULT_MAX_RETRIES: u32 = 5;

/// Default total timeout for all retry attempts.
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

/// User-Agent header value.
/// Used when the `honeycomb` feature is enabled for HTTP requests.
#[allow(dead_code)]
pub const USER_AGENT: &str = concat!("aletheiadb-honeycomb/", env!("CARGO_PKG_VERSION"));

/// Response from a batch transmission.
///
/// This struct is populated by the HTTP client when the `honeycomb` feature is enabled.
/// The fields track request status, timing, and any errors.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Response {
    /// HTTP status code (if available).
    pub status_code: Option<u16>,
    /// Response body (if available).
    pub body: Option<String>,
    /// Duration of the request.
    pub duration: Duration,
    /// Error message (if failed).
    pub error: Option<String>,
}

#[allow(dead_code)]
impl Response {
    /// Create a successful response.
    pub fn success(status_code: u16, duration: Duration) -> Self {
        Self {
            status_code: Some(status_code),
            body: None,
            duration,
            error: None,
        }
    }

    /// Create a failed response.
    pub fn error(error: impl Into<String>, duration: Duration) -> Self {
        Self {
            status_code: None,
            body: None,
            duration,
            error: Some(error.into()),
        }
    }

    /// Check if the response indicates success.
    pub fn is_success(&self) -> bool {
        self.error.is_none()
            && self
                .status_code
                .is_some_and(|code| (200..300).contains(&code))
    }

    /// Check if the response indicates a retryable error.
    pub fn is_retryable(&self) -> bool {
        if self.error.is_some() {
            return true; // Network errors are retryable
        }
        match self.status_code {
            Some(code) => {
                // 429 Too Many Requests, 5xx Server errors are retryable
                code == 429 || code >= 500
            }
            None => true,
        }
    }
}

/// Configuration for retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Initial delay before first retry.
    pub initial_delay: Duration,
    /// Maximum delay between retries.
    pub max_delay: Duration,
    /// Maximum number of retry attempts.
    pub max_retries: u32,
    /// Multiplier for exponential backoff.
    pub backoff_multiplier: f64,
    /// Maximum total time for all retry attempts (prevents unbounded blocking).
    pub total_timeout: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            initial_delay: DEFAULT_INITIAL_RETRY_DELAY,
            max_delay: DEFAULT_MAX_RETRY_DELAY,
            max_retries: DEFAULT_MAX_RETRIES,
            backoff_multiplier: 2.0,
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
        }
    }
}

impl RetryConfig {
    /// Create a new retry config.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the initial delay.
    #[must_use]
    pub fn with_initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Set the maximum delay.
    #[must_use]
    pub fn with_max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// Set the maximum retries.
    #[must_use]
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Set the total timeout for all retry attempts.
    ///
    /// This prevents unbounded blocking when the endpoint is unavailable.
    #[must_use]
    pub fn with_total_timeout(mut self, timeout: Duration) -> Self {
        self.total_timeout = timeout;
        self
    }

    /// Calculate the delay for a given retry attempt (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if attempt >= self.max_retries {
            return self.max_delay;
        }

        let delay_ms =
            self.initial_delay.as_millis() as f64 * self.backoff_multiplier.powi(attempt as i32);
        let delay = Duration::from_millis(delay_ms as u64);

        std::cmp::min(delay, self.max_delay)
    }

    /// Check if we should retry after the given attempt.
    pub fn should_retry(&self, attempt: u32) -> bool {
        attempt < self.max_retries
    }
}

/// Honeycomb client for sending events.
///
/// The client handles:
/// - Event batching via `BatchBuffer`
/// - HTTP transmission to Honeycomb's batch API
/// - Exponential backoff retry logic
///
/// # Example
///
/// ```ignore
/// use aletheiadb::honeycomb::{Client, Config, Event};
///
/// let config = Config::new("api-key", "dataset");
/// let client = Client::new(config).unwrap();
///
/// let mut event = Event::new();
/// event.add_field("message", "Hello!");
///
/// client.send(event)?;
/// client.flush()?;
/// ```
pub struct Client {
    /// Configuration.
    config: Config,
    /// Batch buffer for collecting events.
    buffer: BatchBuffer,
    /// Retry configuration.
    retry_config: RetryConfig,
    /// HTTP client (when the honeycomb feature is enabled).
    #[cfg(feature = "honeycomb")]
    http_client: reqwest::blocking::Client,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("config", &self.config)
            .field("buffer_len", &self.buffer.len())
            .field("retry_config", &self.retry_config)
            .finish()
    }
}

impl Client {
    /// Create a new client with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client fails to initialize (when `honeycomb` feature is enabled).
    #[cfg(feature = "honeycomb")]
    pub fn new(config: Config) -> Result<Self> {
        let buffer = BatchBuffer::new(config.transmission_options.clone());
        let http_client = Self::build_http_client(&config)?;

        Ok(Self {
            config,
            buffer,
            retry_config: RetryConfig::default(),
            http_client,
        })
    }

    /// Create a new client with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client fails to initialize (when `honeycomb` feature is enabled).
    /// Without the feature, this always succeeds.
    #[cfg(not(feature = "honeycomb"))]
    pub fn new(config: Config) -> Result<Self> {
        let buffer = BatchBuffer::new(config.transmission_options.clone());

        Ok(Self {
            config,
            buffer,
            retry_config: RetryConfig::default(),
        })
    }

    /// Create a new client with custom retry configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client fails to initialize (when `honeycomb` feature is enabled).
    /// Without the feature, this always succeeds.
    #[cfg(feature = "honeycomb")]
    pub fn with_retry_config(config: Config, retry_config: RetryConfig) -> Result<Self> {
        let buffer = BatchBuffer::new(config.transmission_options.clone());
        let http_client = Self::build_http_client(&config)?;

        Ok(Self {
            config,
            buffer,
            retry_config,
            http_client,
        })
    }

    /// Create a new client with custom retry configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client fails to initialize (when `honeycomb` feature is enabled).
    /// Without the feature, this always succeeds.
    #[cfg(not(feature = "honeycomb"))]
    pub fn with_retry_config(config: Config, retry_config: RetryConfig) -> Result<Self> {
        let buffer = BatchBuffer::new(config.transmission_options.clone());

        Ok(Self {
            config,
            buffer,
            retry_config,
        })
    }

    /// Build the HTTP client.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client fails to initialize (e.g., TLS backend issues).
    #[cfg(feature = "honeycomb")]
    fn build_http_client(config: &Config) -> Result<reqwest::blocking::Client> {
        let mut user_agent = USER_AGENT.to_string();
        if let Some(ref addition) = config.transmission_options.user_agent_addition {
            user_agent = format!("{user_agent} {addition}");
        }

        reqwest::blocking::Client::builder()
            .user_agent(user_agent)
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(config.transmission_options.max_concurrent_batches)
            .build()
            .map_err(|e| Error::Http(format!("Failed to build HTTP client: {e}")))
    }

    /// Send an event.
    ///
    /// The event is added to the batch buffer. If the buffer becomes full,
    /// the batch is transmitted immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if the buffer is at capacity or transmission fails.
    pub fn send(&self, event: Event) -> Result<()> {
        // Apply sampling
        if event.should_drop() {
            return Ok(());
        }

        // Add to buffer
        if let Some(batch) = self.buffer.add(event)? {
            // Buffer is full, send the batch
            self.send_batch(&batch)?;
        }

        Ok(())
    }

    /// Flush all pending events.
    ///
    /// Forces transmission of all events in the buffer, regardless of
    /// batch size or timeout.
    pub fn flush(&self) -> Result<()> {
        let batch = self.buffer.flush()?;
        if !batch.is_empty() {
            self.send_batch(&batch)?;
        }
        Ok(())
    }

    /// Send a batch of events to Honeycomb.
    fn send_batch(&self, batch: &[Event]) -> Result<Response> {
        if batch.is_empty() {
            return Ok(Response::success(200, Duration::ZERO));
        }

        #[cfg(feature = "honeycomb")]
        {
            self.send_batch_http(batch)
        }

        #[cfg(not(feature = "honeycomb"))]
        {
            // When honeycomb feature is disabled, just record stats
            let _ = batch;
            self.buffer.stats().record_events_sent(batch.len() as u64);
            self.buffer.stats().record_batch_sent();
            Ok(Response::success(200, Duration::ZERO))
        }
    }

    /// Send a batch via HTTP with retries.
    ///
    /// Respects both `max_retries` and `total_timeout` to prevent unbounded blocking.
    /// Also enforces `max_batch_bytes` to prevent OOM from oversized batches.
    #[cfg(feature = "honeycomb")]
    fn send_batch_http(&self, batch: &[Event]) -> Result<Response> {
        let endpoint = self.config.options.batch_endpoint();
        let body = serde_json::to_string(batch)?;

        // Check batch size limit to prevent OOM
        let max_bytes = self.config.transmission_options.max_batch_bytes;
        if body.len() > max_bytes {
            self.buffer.stats().record_batch_failed();
            return Err(Error::Buffer(format!(
                "Batch size ({} bytes) exceeds maximum ({} bytes)",
                body.len(),
                max_bytes
            )));
        }

        let mut last_response = Response::error("No attempts made", Duration::ZERO);
        let overall_start = std::time::Instant::now();

        for attempt in 0..=self.retry_config.max_retries {
            // Check total timeout before each attempt
            if overall_start.elapsed() >= self.retry_config.total_timeout {
                self.buffer.stats().record_batch_failed();
                return Err(Error::Timeout(format!(
                    "Total timeout ({:?}) exceeded after {} attempts",
                    self.retry_config.total_timeout, attempt
                )));
            }

            let start = std::time::Instant::now();

            let result = self
                .http_client
                .post(&endpoint)
                .header("X-Honeycomb-Team", &self.config.options.api_key)
                .header("Content-Type", "application/json")
                .body(body.clone())
                .send();

            let duration = start.elapsed();

            match result {
                Ok(response) => {
                    let status = response.status().as_u16();
                    last_response = Response {
                        status_code: Some(status),
                        body: response.text().ok(),
                        duration,
                        error: None,
                    };

                    if last_response.is_success() {
                        self.buffer.stats().record_events_sent(batch.len() as u64);
                        self.buffer.stats().record_batch_sent();
                        return Ok(last_response);
                    }

                    if !last_response.is_retryable() || !self.retry_config.should_retry(attempt) {
                        break;
                    }
                }
                Err(e) => {
                    last_response = Response::error(e.to_string(), duration);

                    if !self.retry_config.should_retry(attempt) {
                        break;
                    }
                }
            }

            // Wait before retrying, but respect total timeout
            if self.retry_config.should_retry(attempt) {
                let delay = self.retry_config.delay_for_attempt(attempt);
                let remaining = self
                    .retry_config
                    .total_timeout
                    .saturating_sub(overall_start.elapsed());
                if remaining.is_zero() {
                    break;
                }
                std::thread::sleep(std::cmp::min(delay, remaining));
            }
        }

        self.buffer.stats().record_batch_failed();
        Err(Error::Http(format!(
            "Failed to send batch after {} retries: {:?}",
            self.retry_config.max_retries, last_response.error
        )))
    }

    /// Get a reference to the client's statistics.
    pub fn stats(&self) -> &Arc<BatchStats> {
        self.buffer.stats()
    }

    /// Get the number of events currently buffered.
    pub fn buffered_events(&self) -> usize {
        self.buffer.len()
    }

    /// Check if the batch timeout has expired.
    ///
    /// If true, you should call `flush()` to send pending events.
    pub fn should_flush(&self) -> bool {
        self.buffer.is_timeout_expired()
    }

    /// Get a reference to the client configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Get a reference to the retry configuration.
    pub fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }

    /// Create a new event.
    ///
    /// This is a convenience method that creates an event with the
    /// client's sample rate.
    pub fn new_event(&self) -> Event {
        let mut event = Event::new();
        event.set_sample_rate(self.config.options.sample_rate);
        event
    }

    /// Close the client, flushing any pending events.
    pub fn close(self) -> Result<()> {
        // Consume self to prevent further use
        let batch = self.buffer.flush()?;
        if !batch.is_empty() {
            self.send_batch(&batch)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::honeycomb::config::{Options, TransmissionOptions};

    // =====================================================
    // Response Tests
    // =====================================================

    #[test]
    fn test_response_success() {
        let response = Response::success(200, Duration::from_millis(50));
        assert!(response.is_success());
        assert!(!response.is_retryable());
        assert_eq!(response.status_code, Some(200));
        assert!(response.error.is_none());
    }

    #[test]
    fn test_response_error() {
        let response = Response::error("connection failed", Duration::from_millis(10));
        assert!(!response.is_success());
        assert!(response.is_retryable());
        assert!(response.status_code.is_none());
        assert_eq!(response.error, Some("connection failed".to_string()));
    }

    #[test]
    fn test_response_is_success_various_codes() {
        assert!(Response::success(200, Duration::ZERO).is_success());
        assert!(Response::success(201, Duration::ZERO).is_success());
        assert!(Response::success(202, Duration::ZERO).is_success());
        assert!(!Response::success(400, Duration::ZERO).is_success());
        assert!(!Response::success(500, Duration::ZERO).is_success());
    }

    #[test]
    fn test_response_is_retryable() {
        // Network errors are retryable
        assert!(Response::error("timeout", Duration::ZERO).is_retryable());

        // 429 Too Many Requests is retryable
        assert!(Response::success(429, Duration::ZERO).is_retryable());

        // 5xx errors are retryable
        assert!(Response::success(500, Duration::ZERO).is_retryable());
        assert!(Response::success(502, Duration::ZERO).is_retryable());
        assert!(Response::success(503, Duration::ZERO).is_retryable());

        // 4xx errors (except 429) are not retryable
        assert!(!Response::success(400, Duration::ZERO).is_retryable());
        assert!(!Response::success(401, Duration::ZERO).is_retryable());
        assert!(!Response::success(403, Duration::ZERO).is_retryable());
        assert!(!Response::success(404, Duration::ZERO).is_retryable());
    }

    #[test]
    fn test_response_clone() {
        let response = Response::success(200, Duration::from_millis(100));
        let cloned = response.clone();
        assert_eq!(response.status_code, cloned.status_code);
        assert_eq!(response.duration, cloned.duration);
    }

    #[test]
    fn test_response_debug() {
        let response = Response::success(200, Duration::ZERO);
        let debug_str = format!("{response:?}");
        assert!(debug_str.contains("Response"));
    }

    // =====================================================
    // RetryConfig Tests
    // =====================================================

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.initial_delay, DEFAULT_INITIAL_RETRY_DELAY);
        assert_eq!(config.max_delay, DEFAULT_MAX_RETRY_DELAY);
        assert_eq!(config.max_retries, DEFAULT_MAX_RETRIES);
        assert!((config.backoff_multiplier - 2.0).abs() < 0.001);
    }

    #[test]
    fn test_retry_config_new() {
        let config = RetryConfig::new();
        assert_eq!(config.initial_delay, DEFAULT_INITIAL_RETRY_DELAY);
    }

    #[test]
    fn test_retry_config_builder() {
        let config = RetryConfig::new()
            .with_initial_delay(Duration::from_millis(200))
            .with_max_delay(Duration::from_secs(10))
            .with_max_retries(3);

        assert_eq!(config.initial_delay, Duration::from_millis(200));
        assert_eq!(config.max_delay, Duration::from_secs(10));
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn test_retry_config_delay_for_attempt() {
        let config = RetryConfig::new()
            .with_initial_delay(Duration::from_millis(100))
            .with_max_delay(Duration::from_secs(5));

        // Attempt 0: 100ms
        assert_eq!(config.delay_for_attempt(0), Duration::from_millis(100));

        // Attempt 1: 200ms (100 * 2)
        assert_eq!(config.delay_for_attempt(1), Duration::from_millis(200));

        // Attempt 2: 400ms (100 * 4)
        assert_eq!(config.delay_for_attempt(2), Duration::from_millis(400));

        // Attempt 3: 800ms (100 * 8)
        assert_eq!(config.delay_for_attempt(3), Duration::from_millis(800));

        // Attempt 4: 1600ms (100 * 16)
        assert_eq!(config.delay_for_attempt(4), Duration::from_millis(1600));
    }

    #[test]
    fn test_retry_config_delay_capped_at_max() {
        let config = RetryConfig::new()
            .with_initial_delay(Duration::from_millis(1000))
            .with_max_delay(Duration::from_secs(2))
            .with_max_retries(10);

        // Attempt 0: 1000ms
        assert_eq!(config.delay_for_attempt(0), Duration::from_millis(1000));

        // Attempt 1: 2000ms (1000 * 2) - capped at max
        assert_eq!(config.delay_for_attempt(1), Duration::from_secs(2));

        // Attempt 2: would be 4000ms, but capped at 2000ms
        assert_eq!(config.delay_for_attempt(2), Duration::from_secs(2));
    }

    #[test]
    fn test_retry_config_should_retry() {
        let config = RetryConfig::new().with_max_retries(3);

        assert!(config.should_retry(0));
        assert!(config.should_retry(1));
        assert!(config.should_retry(2));
        assert!(!config.should_retry(3));
        assert!(!config.should_retry(4));
    }

    #[test]
    fn test_retry_config_clone() {
        let config = RetryConfig::new().with_max_retries(10);
        let cloned = config.clone();
        assert_eq!(config.max_retries, cloned.max_retries);
    }

    #[test]
    fn test_retry_config_debug() {
        let config = RetryConfig::new();
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("RetryConfig"));
    }

    // =====================================================
    // Client Creation Tests
    // =====================================================

    #[test]
    fn test_client_new() {
        let config = Config::new("test-key", "test-dataset");
        let client = Client::new(config).unwrap();

        assert_eq!(client.buffered_events(), 0);
        assert_eq!(client.config().options.api_key, "test-key");
        assert_eq!(client.config().options.dataset, "test-dataset");
    }

    #[test]
    fn test_client_with_retry_config() {
        let config = Config::new("key", "dataset");
        let retry_config = RetryConfig::new().with_max_retries(10);

        let client = Client::with_retry_config(config, retry_config).unwrap();

        assert_eq!(client.retry_config().max_retries, 10);
    }

    #[test]
    fn test_client_new_event() {
        let config = Config::new("key", "dataset")
            .with_options(Options::new("key", "dataset").with_sample_rate(5));

        let client = Client::new(config).unwrap();
        let event = client.new_event();

        assert_eq!(event.sample_rate, 5);
    }

    // =====================================================
    // Client Send Tests
    // =====================================================

    #[test]
    fn test_client_send_buffers_event() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        let event = Event::new();
        client.send(event).unwrap();

        assert_eq!(client.buffered_events(), 1);
    }

    #[test]
    fn test_client_send_multiple_events() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        for _ in 0..10 {
            client.send(Event::new()).unwrap();
        }

        assert_eq!(client.buffered_events(), 10);
    }

    #[test]
    fn test_client_send_with_sampling() {
        let config = Config::new("key", "dataset")
            .with_options(Options::new("key", "dataset").with_sample_rate(100));

        let client = Client::new(config).unwrap();

        // Send many events with high sample rate
        for i in 0..1000 {
            let mut event = client.new_event();
            event.add_field("index", i);
            client.send(event).unwrap();
        }

        // With sample rate 100, roughly 1% should be kept
        // Allow for statistical variation
        let buffered = client.buffered_events();
        assert!(buffered < 100, "Expected ~10 events, got {buffered}");
    }

    // =====================================================
    // Client Flush Tests
    // =====================================================

    #[test]
    fn test_client_flush_empty() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        client.flush().unwrap();
        assert_eq!(client.buffered_events(), 0);
    }

    // This test triggers HTTP transmission when honeycomb feature is enabled,
    // so we only run it in mock mode.
    #[cfg(not(feature = "honeycomb"))]
    #[test]
    fn test_client_flush_clears_buffer() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        client.send(Event::new()).unwrap();
        client.send(Event::new()).unwrap();
        assert_eq!(client.buffered_events(), 2);

        client.flush().unwrap();
        assert_eq!(client.buffered_events(), 0);
    }

    // This test triggers HTTP transmission when honeycomb feature is enabled,
    // so we only run it in mock mode.
    #[cfg(not(feature = "honeycomb"))]
    #[test]
    fn test_client_flush_updates_stats() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        client.send(Event::new()).unwrap();
        client.send(Event::new()).unwrap();
        client.flush().unwrap();

        let stats = client.stats().snapshot();
        assert_eq!(stats.events_added, 2);
        assert_eq!(stats.events_sent, 2);
        assert_eq!(stats.batches_sent, 1);
    }

    // =====================================================
    // Client Should Flush Tests
    // =====================================================

    #[test]
    fn test_client_should_flush_empty() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        assert!(!client.should_flush());
    }

    #[test]
    fn test_client_should_flush_timeout() {
        let config = Config::new("key", "dataset").with_transmission_options(
            TransmissionOptions::default().with_batch_timeout(Duration::from_millis(1)),
        );
        let client = Client::new(config).unwrap();

        client.send(Event::new()).unwrap();

        std::thread::sleep(Duration::from_millis(10));

        assert!(client.should_flush());
    }

    // =====================================================
    // Client Close Tests
    // =====================================================

    // This test triggers HTTP transmission when honeycomb feature is enabled,
    // so we only run it in mock mode.
    #[cfg(not(feature = "honeycomb"))]
    #[test]
    fn test_client_close() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        client.send(Event::new()).unwrap();
        client.close().unwrap();
        // Client is consumed, cannot use after close
    }

    // =====================================================
    // Client Accessors Tests
    // =====================================================

    #[test]
    fn test_client_config_accessor() {
        let config = Config::new("test-key", "test-dataset");
        let client = Client::new(config).unwrap();

        assert_eq!(client.config().options.api_key, "test-key");
        assert_eq!(client.config().options.dataset, "test-dataset");
    }

    #[test]
    fn test_client_retry_config_accessor() {
        let config = Config::new("key", "dataset");
        let retry_config = RetryConfig::new().with_max_retries(7);
        let client = Client::with_retry_config(config, retry_config).unwrap();

        assert_eq!(client.retry_config().max_retries, 7);
    }

    #[test]
    fn test_client_stats_accessor() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        let stats = client.stats();
        assert_eq!(stats.snapshot().events_added, 0);

        client.send(Event::new()).unwrap();

        assert_eq!(stats.snapshot().events_added, 1);
    }

    // =====================================================
    // Client Debug Tests
    // =====================================================

    #[test]
    fn test_client_debug() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        let debug_str = format!("{client:?}");
        assert!(debug_str.contains("Client"));
        assert!(debug_str.contains("config"));
        assert!(debug_str.contains("retry_config"));
    }

    // =====================================================
    // Integration Tests
    // =====================================================

    // This test triggers HTTP transmission when honeycomb feature is enabled,
    // so we only run it in mock mode.
    #[cfg(not(feature = "honeycomb"))]
    #[test]
    fn test_client_batch_auto_flush() {
        // Create a client with small batch size
        let config = Config::new("key", "dataset")
            .with_transmission_options(TransmissionOptions::default().with_max_batch_size(3));
        let client = Client::new(config).unwrap();

        // Add events up to batch size
        client.send(Event::new()).unwrap();
        assert_eq!(client.buffered_events(), 1);

        client.send(Event::new()).unwrap();
        assert_eq!(client.buffered_events(), 2);

        // Third event should trigger batch send
        client.send(Event::new()).unwrap();
        // After batch is sent, buffer should be empty
        assert_eq!(client.buffered_events(), 0);
    }

    #[test]
    fn test_client_preserves_event_data() {
        let config = Config::new("key", "dataset");
        let client = Client::new(config).unwrap();

        let mut event = Event::new();
        event.add_field("test_key", "test_value");
        event.add_field("count", 42);

        client.send(event).unwrap();

        // Verify the event is in the buffer
        assert_eq!(client.buffered_events(), 1);
    }

    #[test]
    fn test_client_concurrent_sends() {
        use std::sync::Arc;
        use std::thread;

        let config = Config::new("key", "dataset").with_transmission_options(
            TransmissionOptions::default()
                .with_max_batch_size(1000)
                .with_pending_work_capacity(10000),
        );
        let client = Arc::new(Client::new(config).unwrap());

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let client = Arc::clone(&client);
                thread::spawn(move || {
                    for _ in 0..100 {
                        let _ = client.send(Event::new());
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        let stats = client.stats().snapshot();
        assert_eq!(stats.events_added, 1000);
    }

    // =====================================================
    // User Agent Tests
    // =====================================================

    #[test]
    fn test_user_agent_constant() {
        assert!(USER_AGENT.starts_with("aletheiadb-honeycomb/"));
    }

    // =====================================================
    // Constants Tests
    // =====================================================

    #[test]
    fn test_default_constants() {
        assert_eq!(DEFAULT_INITIAL_RETRY_DELAY, Duration::from_millis(100));
        assert_eq!(DEFAULT_MAX_RETRY_DELAY, Duration::from_secs(5));
        assert_eq!(DEFAULT_MAX_RETRIES, 5);
        assert_eq!(DEFAULT_TOTAL_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn test_retry_config_total_timeout() {
        let config = RetryConfig::new().with_total_timeout(Duration::from_secs(60));

        assert_eq!(config.total_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_retry_config_default_total_timeout() {
        let config = RetryConfig::default();

        assert_eq!(config.total_timeout, DEFAULT_TOTAL_TIMEOUT);
    }
}
