//! HTTP server configuration.

/// CORS (Cross-Origin Resource Sharing) configuration.
#[derive(Debug, Clone)]
pub struct CorsConfig {
    /// Allowed origins. Empty means allow any origin (development mode only).
    allowed_origins: Vec<String>,
    /// Allowed HTTP methods.
    allowed_methods: Vec<String>,
    /// Allowed HTTP headers.
    allowed_headers: Vec<String>,
    /// Max age for preflight cache in seconds.
    max_age: u32,
}

impl CorsConfig {
    /// Create a permissive CORS config for development.
    ///
    /// # Security Warning
    ///
    /// This allows any origin and should NOT be used in production.
    /// Use [`CorsConfig::restrictive`] or configure specific origins instead.
    pub fn permissive() -> Self {
        Self {
            allowed_origins: vec![],
            allowed_methods: vec![
                "GET".to_string(),
                "POST".to_string(),
                "PUT".to_string(),
                "DELETE".to_string(),
                "OPTIONS".to_string(),
            ],
            allowed_headers: vec!["Content-Type".to_string(), "Authorization".to_string()],
            max_age: 3600,
        }
    }

    /// Create a restrictive CORS config with no allowed origins.
    ///
    /// You must explicitly add allowed origins using [`CorsConfig::allow_origin`].
    pub fn restrictive() -> Self {
        Self {
            allowed_origins: vec!["http://localhost:3000".to_string()],
            allowed_methods: vec!["GET".to_string(), "POST".to_string(), "OPTIONS".to_string()],
            allowed_headers: vec!["Content-Type".to_string()],
            max_age: 3600,
        }
    }

    /// Add an allowed origin.
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.allowed_origins.push(origin.into());
        self
    }

    /// Set allowed HTTP methods.
    pub fn allowed_methods(mut self, methods: Vec<String>) -> Self {
        self.allowed_methods = methods;
        self
    }

    /// Set allowed HTTP headers.
    pub fn allowed_headers(mut self, headers: Vec<String>) -> Self {
        self.allowed_headers = headers;
        self
    }

    /// Set max age for preflight cache.
    pub fn max_age(mut self, seconds: u32) -> Self {
        self.max_age = seconds;
        self
    }

    /// Check if any origin is allowed (permissive mode).
    pub fn is_permissive(&self) -> bool {
        self.allowed_origins.is_empty()
    }

    /// Get allowed origins.
    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed_origins
    }

    /// Get allowed methods.
    pub fn get_allowed_methods(&self) -> &[String] {
        &self.allowed_methods
    }

    /// Get allowed headers.
    pub fn get_allowed_headers(&self) -> &[String] {
        &self.allowed_headers
    }

    /// Get max age in seconds.
    pub fn get_max_age(&self) -> u32 {
        self.max_age
    }
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self::restrictive()
    }
}

/// Rate limiting configuration.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Number of requests allowed per second per IP.
    requests_per_second: u32,
    /// Maximum burst size (concurrent requests) per IP.
    burst_size: u32,
}

impl RateLimitConfig {
    /// Create a new rate limit configuration.
    pub fn new(requests_per_second: u32, burst_size: u32) -> Self {
        Self {
            requests_per_second,
            burst_size,
        }
    }

    /// Get requests per second.
    pub fn requests_per_second(&self) -> u32 {
        self.requests_per_second
    }

    /// Get burst size.
    pub fn burst_size(&self) -> u32 {
        self.burst_size
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_second: 10,
            burst_size: 20,
        }
    }
}

/// Configuration for the HTTP server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    port: u16,
    host: String,
    cors: CorsConfig,
    rate_limit: RateLimitConfig,
}

impl ServerConfig {
    /// Create a new server config with the specified port.
    ///
    /// Uses default host of "0.0.0.0" (all interfaces), restrictive CORS,
    /// and default rate limiting (10 req/s, 20 burst).
    pub fn new(port: u16) -> Self {
        Self {
            port,
            host: "0.0.0.0".to_string(),
            cors: CorsConfig::default(),
            rate_limit: RateLimitConfig::default(),
        }
    }

    /// Get the configured port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get the configured host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Get the CORS configuration.
    pub fn cors(&self) -> &CorsConfig {
        &self.cors
    }

    /// Get the rate limit configuration.
    pub fn rate_limit(&self) -> &RateLimitConfig {
        &self.rate_limit
    }

    /// Get the bind address as "host:port".
    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Create a builder for more complex configuration.
    pub fn builder() -> ServerConfigBuilder {
        ServerConfigBuilder::default()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            host: "0.0.0.0".to_string(),
            cors: CorsConfig::default(),
            rate_limit: RateLimitConfig::default(),
        }
    }
}

/// Builder for [`ServerConfig`].
#[derive(Debug, Clone, Default)]
pub struct ServerConfigBuilder {
    port: Option<u16>,
    host: Option<String>,
    cors: Option<CorsConfig>,
    rate_limit: Option<RateLimitConfig>,
}

impl ServerConfigBuilder {
    /// Set the port to bind to.
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set the host to bind to.
    ///
    /// # Valid Values
    ///
    /// - IPv4 addresses: "0.0.0.0", "127.0.0.1", etc.
    /// - IPv6 addresses: "::", "::1", etc.
    /// - Hostnames: "localhost", etc.
    ///
    /// The host is validated at bind time by the underlying server.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Set the CORS configuration.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::http::{ServerConfig, CorsConfig};
    ///
    /// let config = ServerConfig::builder()
    ///     .port(8080)
    ///     .cors(CorsConfig::restrictive().allow_origin("https://myapp.com"))
    ///     .build();
    /// ```
    pub fn cors(mut self, cors: CorsConfig) -> Self {
        self.cors = Some(cors);
        self
    }

    /// Set the rate limit configuration.
    pub fn rate_limit(mut self, rate_limit: RateLimitConfig) -> Self {
        self.rate_limit = Some(rate_limit);
        self
    }

    /// Build the server configuration.
    pub fn build(self) -> ServerConfig {
        ServerConfig {
            port: self.port.unwrap_or(8080),
            host: self.host.unwrap_or_else(|| "0.0.0.0".to_string()),
            cors: self.cors.unwrap_or_default(),
            rate_limit: self.rate_limit.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.port(), 8080);
        assert_eq!(config.host(), "0.0.0.0");
        assert!(!config.cors().is_permissive());
    }

    #[test]
    fn test_new_with_port() {
        let config = ServerConfig::new(3000);
        assert_eq!(config.port(), 3000);
        assert_eq!(config.host(), "0.0.0.0");
    }

    #[test]
    fn test_builder() {
        let config = ServerConfig::builder().port(9000).host("127.0.0.1").build();
        assert_eq!(config.port(), 9000);
        assert_eq!(config.host(), "127.0.0.1");
    }

    #[test]
    fn test_bind_address() {
        let config = ServerConfig::builder().port(8080).host("localhost").build();
        assert_eq!(config.bind_address(), "localhost:8080");
    }

    #[test]
    fn test_cors_permissive() {
        let cors = CorsConfig::permissive();
        assert!(cors.is_permissive());
        assert!(cors.allowed_origins().is_empty());
    }

    #[test]
    fn test_cors_restrictive() {
        let cors = CorsConfig::restrictive();
        assert!(!cors.is_permissive());
        assert!(!cors.allowed_origins().is_empty());
    }

    #[test]
    fn test_cors_allow_origin() {
        let cors = CorsConfig::restrictive().allow_origin("https://example.com");
        assert!(
            cors.allowed_origins()
                .contains(&"https://example.com".to_string())
        );
    }

    #[test]
    fn test_builder_with_cors() {
        let config = ServerConfig::builder()
            .port(8080)
            .cors(CorsConfig::permissive())
            .build();
        assert!(config.cors().is_permissive());
    }

    #[test]
    fn test_default_rate_limit() {
        let config = ServerConfig::default();
        assert_eq!(config.rate_limit().requests_per_second(), 10);
        assert_eq!(config.rate_limit().burst_size(), 20);
    }

    #[test]
    fn test_custom_rate_limit() {
        let rate_limit = RateLimitConfig::new(100, 200);
        let config = ServerConfig::builder().rate_limit(rate_limit).build();
        assert_eq!(config.rate_limit().requests_per_second(), 100);
        assert_eq!(config.rate_limit().burst_size(), 200);
    }
}
