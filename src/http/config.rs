//! HTTP server configuration.

/// Configuration for the HTTP server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    port: u16,
    host: String,
}

impl ServerConfig {
    /// Create a new server config with the specified port.
    ///
    /// Uses default host of "0.0.0.0" (all interfaces).
    pub fn new(port: u16) -> Self {
        Self {
            port,
            host: "0.0.0.0".to_string(),
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
        }
    }
}

/// Builder for [`ServerConfig`].
#[derive(Debug, Clone, Default)]
pub struct ServerConfigBuilder {
    port: Option<u16>,
    host: Option<String>,
}

impl ServerConfigBuilder {
    /// Set the port to bind to.
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set the host to bind to.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Build the server configuration.
    pub fn build(self) -> ServerConfig {
        ServerConfig {
            port: self.port.unwrap_or(8080),
            host: self.host.unwrap_or_else(|| "0.0.0.0".to_string()),
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
}
