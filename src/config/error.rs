/// Errors that can occur when loading or saving configuration.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    /// I/O error when reading or writing file.
    #[error("I/O error: {0}")]
    IoError(String),
    /// Error parsing TOML.
    #[error("Parse error: {0}")]
    ParseError(String),
    /// Error serializing to TOML.
    #[error("Serialize error: {0}")]
    SerializeError(String),
    /// Invalid configuration value.
    #[error("Invalid value: {0}")]
    InvalidValue(String),
}
