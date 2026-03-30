//! Cypher-specific error types.

use thiserror::Error;

/// Errors that can occur during Cypher parsing and conversion.
#[derive(Debug, Clone, Error)]
pub enum CypherError {
    /// Error from the Cypher lexer.
    #[error("Cypher lexer error at position {position}: {message}")]
    LexError {
        /// Byte position in the input where the error occurred.
        position: usize,
        /// Description of the lexer error.
        message: String,
    },

    /// Error from the Cypher parser.
    #[error("Cypher parse error at position {position}: {message}")]
    ParseError {
        /// Byte position in the input where the error occurred.
        position: usize,
        /// Description of the parse error.
        message: String,
    },

    /// Unsupported Cypher feature or syntax.
    #[error("Unsupported Cypher feature: {0}")]
    UnsupportedFeature(String),

    /// Invalid temporal clause in a Cypher query.
    #[error("Invalid temporal clause: {0}")]
    InvalidTemporalClause(String),

    /// Invalid timestamp format.
    #[error("Invalid timestamp: {0}")]
    InvalidTimestamp(String),

    /// Parameter binding error.
    #[error("Parameter error: {0}")]
    ParameterError(String),

    /// Semantic error detected during analysis.
    #[error("Cypher semantic error: {0}")]
    SemanticError(String),
}
