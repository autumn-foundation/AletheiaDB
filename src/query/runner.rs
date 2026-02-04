//! Query Runner Trait
//!
//! Defines the interface for executing queries, decoupling the query builder
//! from the specific database implementation.

use super::{Query, QueryResults};
use crate::utils::error::Result;
use std::sync::Arc;

/// Trait for executing queries.
///
/// This trait allows the `QueryBuilder` to execute queries without
/// depending directly on `GallifreyDB`.
pub trait QueryRunner {
    /// Execute a compiled query and return results.
    fn execute_query(&self, query: Query) -> Result<QueryResults>;
}

/// Implement QueryRunner for Arc<T> to allow passing Arc<GallifreyDB> directly.
impl<T: QueryRunner + ?Sized> QueryRunner for Arc<T> {
    fn execute_query(&self, query: Query) -> Result<QueryResults> {
        (**self).execute_query(query)
    }
}
