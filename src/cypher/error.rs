//! Cypher error types.

use crate::utils::error::Error;
use thiserror::Error;

/// Cypher-specific errors.
#[derive(Error, Debug)]
pub enum CypherError {
    /// Feature not implemented yet.
    #[error("Not implemented")]
    NotImplemented,
}

impl From<CypherError> for Error {
    fn from(err: CypherError) -> Self {
        Error::Query(crate::utils::error::QueryError::SyntaxError {
            message: err.to_string(),
        })
    }
}
