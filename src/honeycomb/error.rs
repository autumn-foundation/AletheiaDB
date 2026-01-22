//! Error types for the Honeycomb client.

use std::fmt;

/// Result type for Honeycomb client operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during Honeycomb client operations.
#[derive(Debug)]
pub enum Error {
    /// HTTP request failed
    Http(String),
    /// JSON serialization/deserialization failed
    Serialization(String),
    /// Configuration error
    Config(String),
    /// Batch buffer error
    Buffer(String),
    /// Channel send error
    Channel(String),
    /// Timeout error
    Timeout(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(msg) => write!(f, "HTTP error: {msg}"),
            Error::Serialization(msg) => write!(f, "Serialization error: {msg}"),
            Error::Config(msg) => write!(f, "Configuration error: {msg}"),
            Error::Buffer(msg) => write!(f, "Buffer error: {msg}"),
            Error::Channel(msg) => write!(f, "Channel error: {msg}"),
            Error::Timeout(msg) => write!(f, "Timeout error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(feature = "honeycomb")]
impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Error::Http(err.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Serialization(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let http_err = Error::Http("connection refused".to_string());
        assert!(http_err.to_string().contains("HTTP error"));
        assert!(http_err.to_string().contains("connection refused"));

        let ser_err = Error::Serialization("invalid JSON".to_string());
        assert!(ser_err.to_string().contains("Serialization error"));

        let config_err = Error::Config("missing API key".to_string());
        assert!(config_err.to_string().contains("Configuration error"));

        let buf_err = Error::Buffer("buffer full".to_string());
        assert!(buf_err.to_string().contains("Buffer error"));

        let chan_err = Error::Channel("receiver dropped".to_string());
        assert!(chan_err.to_string().contains("Channel error"));

        let timeout_err = Error::Timeout("request timed out".to_string());
        assert!(timeout_err.to_string().contains("Timeout error"));
    }

    #[test]
    fn test_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(Error::Http("test".to_string()));
        assert!(err.to_string().contains("HTTP error"));
    }

    #[test]
    fn test_error_debug() {
        let err = Error::Config("test".to_string());
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("Config"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_from_serde_json_error() {
        let json_err: std::result::Result<serde_json::Value, _> =
            serde_json::from_str("invalid json");
        let err: Error = json_err.unwrap_err().into();
        assert!(matches!(err, Error::Serialization(_)));
    }
}
