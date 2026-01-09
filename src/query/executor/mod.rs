//! Query Executor
//!
//! Executes physical query plans using a pull-based iterator model.
//! The executor transforms physical operators into iterators that
//! lazily produce results.

mod iterators;
mod results;

// TODO: Consider migrating to parking_lot::RwLock for consistency with CurrentStorage
// and other performance-critical components. Currently using std::sync::RwLock to match
// the type used in db.rs for HistoricalStorage. A dedicated PR should migrate all
// historical storage access (db.rs, read_tx.rs, write_tx.rs) to parking_lot::RwLock.
// See: CurrentStorage, HnswIndex, TemporalVectorIndex which already use parking_lot.
use std::sync::{Arc, RwLock};

use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::utils::error::Result;

use super::planner::physical::{PhysicalOp, PhysicalPlan};

pub use iterators::ResultIterator;
pub use results::{EntityResult, QueryResults, QueryRow};

/// Configuration for query execution.
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum number of results to buffer
    pub max_buffer_size: usize,
    /// Enable parallel execution
    pub parallel: bool,
    /// Timeout in milliseconds (0 = no timeout)
    pub timeout_ms: u64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        ExecutionConfig {
            max_buffer_size: 10_000,
            parallel: false,
            timeout_ms: 0,
        }
    }
}

/// Query executor that runs physical plans against storage.
pub struct QueryExecutor {
    /// Reference to current storage
    current: Arc<CurrentStorage>,
    /// Reference to historical storage
    historical: Arc<RwLock<HistoricalStorage>>,
    /// Execution configuration (used for timeout/parallelism in future)
    _config: ExecutionConfig,
}

impl QueryExecutor {
    /// Create a new query executor
    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: ExecutionConfig::default(),
        }
    }

    /// Create an executor with custom configuration
    pub fn with_config(
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        config: ExecutionConfig,
    ) -> Self {
        QueryExecutor {
            current,
            historical,
            _config: config,
        }
    }

    /// Execute a physical plan and return results
    pub fn execute(&self, plan: PhysicalPlan) -> Result<QueryResults> {
        let iterator = self.execute_op(&plan.root)?;
        Ok(QueryResults::new(iterator))
    }

    /// Execute a physical operator, returning an iterator
    fn execute_op(&self, op: &PhysicalOp) -> Result<Box<dyn ResultIterator>> {
        match op {
            PhysicalOp::NodeLookup { node_ids } => Ok(Box::new(
                iterators::NodeLookupIterator::new(node_ids.clone(), Arc::clone(&self.current)),
            )),

            PhysicalOp::NodeScan { label, .. } => Ok(Box::new(iterators::NodeScanIterator::new(
                label.clone(),
                Arc::clone(&self.current),
            ))),

            PhysicalOp::HnswSearch {
                embedding,
                k,
                label_filter,
            } => {
                let results = if let Some(label) = label_filter {
                    self.current
                        .find_similar_by_embedding_with_label(embedding, label, *k)?
                } else {
                    self.current.find_similar_by_embedding(embedding, *k)?
                };

                Ok(Box::new(iterators::VectorResultIterator::new(
                    results,
                    Arc::clone(&self.current),
                )))
            }

            PhysicalOp::TemporalNodeLookup {
                node_ids,
                valid_time,
                transaction_time,
            } => Ok(Box::new(iterators::TemporalNodeIterator::new(
                node_ids.clone(),
                *valid_time,
                *transaction_time,
                Arc::clone(&self.current),
                Arc::clone(&self.historical),
            ))),

            PhysicalOp::IndexedTraversal {
                input,
                direction,
                label,
                depth,
            } => {
                let input_iter = self.execute_op(input)?;
                Ok(Box::new(iterators::TraversalIterator::new(
                    input_iter,
                    *direction,
                    label.clone(),
                    *depth,
                    Arc::clone(&self.current),
                )))
            }

            PhysicalOp::Filter { input, predicate } => {
                let input_iter = self.execute_op(input)?;
                Ok(Box::new(iterators::FilterIterator::new(
                    input_iter,
                    predicate.clone(),
                )))
            }

            PhysicalOp::VectorRerank {
                input,
                embedding,
                k,
            } => {
                let input_iter = self.execute_op(input)?;
                Ok(Box::new(iterators::VectorRerankIterator::new(
                    input_iter,
                    embedding.clone(),
                    *k,
                    Arc::clone(&self.current),
                )))
            }

            PhysicalOp::Limit {
                input,
                count,
                offset,
            } => {
                let input_iter = self.execute_op(input)?;
                Ok(Box::new(iterators::LimitIterator::new(
                    input_iter, *offset, *count,
                )))
            }

            PhysicalOp::Empty => Ok(Box::new(iterators::EmptyIterator)),

            // For unsupported operations, return error
            _ => Err(crate::utils::error::Error::Query(
                crate::utils::error::QueryError::SyntaxError {
                    message: format!("Unsupported physical operator: {:?}", op.name()),
                },
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests will be added when we have a test database setup
}
