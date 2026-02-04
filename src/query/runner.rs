//! Query Runner Trait
//!
//! Defines the interface for executing queries, decoupling the query builder
//! from the specific database implementation.

use super::{Query, QueryResults};
use crate::utils::error::Result;

/// Trait for executing queries.
///
/// This trait allows the `QueryBuilder` to execute queries without
/// depending directly on `GallifreyDB`.
pub trait QueryRunner {
    /// Execute a compiled query and return results.
    fn execute_query(&self, query: Query) -> Result<QueryResults>;
}
