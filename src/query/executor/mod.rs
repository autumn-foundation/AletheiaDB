//! Query Executor
//!
//! Executes physical query plans using a pull-based iterator model.
//! The executor transforms physical operators into iterators that
//! lazily produce results.

mod iterators;
mod results;

use parking_lot::RwLock;
use std::sync::Arc;

use crate::core::error::Result;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;

use super::planner::physical::{PhysicalOp, PhysicalPlan};

#[doc(hidden)]
pub use iterators::NodeScanIterator;
pub use iterators::ResultIterator;
pub use iterators::TemporalNodeScanIterator;
pub use iterators::{
    BatchTemporalNodeIterator, FilterIterator, LimitIterator, ProjectIterator,
    ProvenanceFilterIterator, TemporalNodeIterator, VectorRerankIterator, VectorResultIterator,
};
pub use results::{EntityId, EntityResult, QueryResults, QueryRow};

/// Configuration for query execution.
///
/// # Examples
///
/// ```rust
/// use aletheiadb::query::executor::ExecutionConfig;
///
/// let config = ExecutionConfig {
///     max_buffer_size: 5000,
///     parallel: true,
///     timeout_ms: 1000,
/// };
/// assert_eq!(config.max_buffer_size, 5000);
/// ```
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum number of results to buffer before backpressure is applied.
    /// Default is 10,000.
    pub max_buffer_size: usize,
    /// Enable parallel execution of query operators (where applicable).
    /// Default is false.
    pub parallel: bool,
    /// Execution timeout in milliseconds.
    /// 0 means no timeout. Default is 0.
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
///
/// The executor converts a `PhysicalPlan` into a pipeline of iterators (`ResultIterator`).
/// It manages access to both `CurrentStorage` (for recent data) and `HistoricalStorage`
/// (for temporal data).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use parking_lot::RwLock;
/// use aletheiadb::storage::current::CurrentStorage;
/// use aletheiadb::storage::historical::HistoricalStorage;
/// use aletheiadb::query::{QueryExecutor, PhysicalPlan};
/// use aletheiadb::query::planner::PhysicalOp;
///
/// # fn run() -> Result<(), Box<dyn std::error::Error>> {
/// // 1. Setup storage
/// let current = Arc::new(CurrentStorage::new());
/// let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
///
/// // 2. Create executor
/// let executor = QueryExecutor::new(current, historical);
///
/// // 3. Define a plan (e.g., scan all Person nodes)
/// let plan = PhysicalPlan {
///     root: PhysicalOp::NodeScan {
///         label: Some("Person".to_string()),
///         estimated_rows: 100,
///     },
///     estimated_cost: Default::default(),
///     temporal_context: None,
///     parallel: false,
///     include_provenance: true,
/// };
///
/// // 4. Execute
/// let results = executor.execute(plan)?;
///
/// // 5. Iterate results
/// for row in results {
///     let row = row?;
///     println!("Found entity: {:?}", row.entity);
/// }
/// # Ok(())
/// # }
/// ```
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

    /// Execute a physical plan and return results.
    ///
    /// This method recursively transforms the operator tree starting at `plan.root`
    /// into a chain of iterators. The execution is lazy: no data is fetched until
    /// the returned `QueryResults` iterator is consumed.
    ///
    /// # Arguments
    ///
    /// * `plan` - The physical query plan to execute.
    ///
    /// # Returns
    ///
    /// Returns a `QueryResults` iterator that produces `Result<QueryRow>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use parking_lot::RwLock;
    /// use aletheiadb::storage::current::CurrentStorage;
    /// use aletheiadb::storage::historical::HistoricalStorage;
    /// use aletheiadb::query::{QueryExecutor, PhysicalPlan};
    /// use aletheiadb::query::planner::PhysicalOp;
    ///
    /// // 1. Setup storage and executor
    /// let current = Arc::new(CurrentStorage::new());
    /// let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
    /// let executor = QueryExecutor::new(current, historical);
    ///
    /// // 2. Create a physical plan (usually done by planner)
    /// let plan = PhysicalPlan {
    ///     root: PhysicalOp::Empty, // Using Empty for example simplicity
    ///     estimated_cost: Default::default(),
    ///     temporal_context: None,
    ///     parallel: false,
    ///     include_provenance: true,
    /// };
    ///
    /// // 3. Execute
    /// let results = executor.execute(plan).unwrap();
    ///
    /// // 4. Iterate
    /// for row in results {
    ///     println!("Got row: {:?}", row);
    /// }
    /// ```
    pub fn execute(&self, plan: PhysicalPlan) -> Result<QueryResults> {
        let iterator = self.execute_op(&plan.root)?;
        // Wrap with provenance filter to conditionally strip metadata
        let filtered = Box::new(iterators::ProvenanceFilterIterator::new(
            iterator,
            plan.include_provenance,
        ));
        Ok(QueryResults::new(filtered))
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
                property_key,
            } => self.execute_hnsw_search(
                embedding,
                *k,
                label_filter.as_deref(),
                property_key.as_deref(),
            ),

            PhysicalOp::TemporalNodeLookup {
                node_ids,
                valid_time,
                transaction_time,
                use_batch,
            } => {
                if *use_batch {
                    // Use batch iterator for large queries (holds lock across all iterations)
                    Ok(Box::new(iterators::BatchTemporalNodeIterator::new(
                        node_ids.clone(),
                        *valid_time,
                        *transaction_time,
                        Arc::clone(&self.historical),
                    )?))
                } else {
                    // Use per-node iterator for small queries (lock per node)
                    Ok(Box::new(iterators::TemporalNodeIterator::new(
                        node_ids.clone(),
                        *valid_time,
                        *transaction_time,
                        Arc::clone(&self.historical),
                    )))
                }
            }

            PhysicalOp::TemporalVectorSearch {
                embedding,
                k,
                timestamp,
                property_key,
            } => {
                // Always use the multi-property-aware method with explicit property name.
                // This ensures correctness in multi-property setups and avoids relying on
                // legacy single-property state (which would use the last enabled index).
                let prop = property_key.as_deref().unwrap_or("embedding");
                let results = self
                    .current
                    .find_similar_as_of_in(prop, embedding, *k, *timestamp)?;

                Ok(Box::new(iterators::VectorResultIterator::new(
                    results,
                    Arc::clone(&self.current),
                )))
            }

            PhysicalOp::IndexedTraversal {
                input,
                direction,
                label,
                depth,
                temporal_context,
            } => {
                let input_iter = self.execute_op(input)?;
                Ok(Box::new(iterators::TraversalIterator::new(
                    input_iter,
                    *direction,
                    label.clone(),
                    *depth,
                    Arc::clone(&self.current),
                    Arc::clone(&self.historical),
                    *temporal_context,
                )))
            }

            PhysicalOp::PropertyScan {
                label, key, value, ..
            } => Ok(Box::new(iterators::PropertyScanIterator::new(
                label.clone(),
                key.clone(),
                value,
                Arc::clone(&self.current),
            ))),

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
                property_key,
            } => {
                let input_iter = self.execute_op(input)?;
                Ok(Box::new(iterators::VectorRerankIterator::new(
                    input_iter,
                    embedding.clone(),
                    *k,
                    Arc::clone(&self.current),
                    property_key.clone(),
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

            PhysicalOp::Project { input, properties } => {
                let input_iter = self.execute_op(input)?;
                Ok(Box::new(iterators::ProjectIterator::new(
                    input_iter,
                    properties.clone(),
                )))
            }

            PhysicalOp::Empty => Ok(Box::new(iterators::EmptyIterator)),
            PhysicalOp::SimilarToNode {
                source_node,
                property_key,
                k,
                label_filter,
            } => self.execute_similar_to_node(
                *source_node,
                property_key,
                *k,
                label_filter.as_deref(),
            ),

            // For unsupported operations, return error
            _ => Err(crate::core::error::Error::Query(
                crate::core::error::QueryError::SyntaxError {
                    message: format!("Unsupported physical operator: {:?}", op.name()),
                },
            )),
        }
    }

    fn execute_hnsw_search(
        &self,
        embedding: &[f32],
        k: usize,
        label_filter: Option<&str>,
        property_key: Option<&str>,
    ) -> Result<Box<dyn ResultIterator>> {
        let results = match (property_key, label_filter) {
            // Property-specific with label filter
            (Some(prop), Some(label)) => self
                .current
                .find_similar_by_embedding_in_with_label(prop, embedding, label, k)?,
            // Property-specific without label filter
            (Some(prop), None) => self
                .current
                .find_similar_by_embedding_in(prop, embedding, k)?,
            // Default property with label filter
            (None, Some(label)) => self
                .current
                .find_similar_by_embedding_with_label(embedding, label, k)?,
            // Default property without label filter
            (None, None) => self.current.find_similar_by_embedding(embedding, k)?,
        };

        Ok(Box::new(iterators::VectorResultIterator::new(
            results,
            Arc::clone(&self.current),
        )))
    }

    fn execute_similar_to_node(
        &self,
        source_node: crate::core::NodeId,
        property_key: &str,
        k: usize,
        label_filter: Option<&str>,
    ) -> Result<Box<dyn ResultIterator>> {
        // 1. Validate that property_key matches the indexed property
        let indexed_property = self.current.get_indexed_property_name().ok_or_else(|| {
            crate::core::error::Error::Query(crate::core::error::QueryError::ExecutionError {
                message: "No vector index is enabled. Call db.vector_index(\"...\").hnsw(...).enable() first."
                    .to_string(),
            })
        })?;

        if property_key != indexed_property {
            return Err(crate::core::error::Error::Query(
                crate::core::error::QueryError::ExecutionError {
                    message: format!(
                        "Property key '{}' does not match indexed property '{}'. \
                         Vector index was built on '{}', so similar_to queries must use the same property.",
                        property_key, indexed_property, indexed_property
                    ),
                },
            ));
        }

        // 2. Look up the source node
        let node = self.current.get_node(source_node).map_err(|_| {
            crate::core::error::Error::Query(crate::core::error::QueryError::ExecutionError {
                message: format!("Source node {:?} not found", source_node),
            })
        })?;

        // 3. Extract the embedding from the specified property
        let embedding = node
            .properties
            .get(property_key)
            .and_then(|v: &crate::core::PropertyValue| v.as_vector())
            .ok_or_else(|| {
                crate::core::error::Error::Query(crate::core::error::QueryError::ExecutionError {
                    message: format!(
                        "Node {:?} does not have a vector property '{}'",
                        source_node, property_key
                    ),
                })
            })?;

        // 4. Perform HNSW search with the extracted embedding.
        // Request k+1 results to account for filtering out the source node.
        // Use checked_add to prevent overflow (though k=usize::MAX is extremely unlikely).
        let k_with_source = k.checked_add(1).unwrap_or(k);
        let mut results = if let Some(label) = label_filter {
            self.current
                .find_similar_by_embedding_with_label(embedding, label, k_with_source)?
        } else {
            self.current
                .find_similar_by_embedding(embedding, k_with_source)?
        };

        // Remove source node from results. In vector similarity with cosine distance,
        // a node has similarity 1.0 with itself, so it's always the first result.
        // Filtering it out ensures we return k truly *different* nodes.
        results.retain(|(node_id, _)| node_id != &source_node);

        // Trim to requested k results after filtering
        results.truncate(k);

        Ok(Box::new(iterators::VectorResultIterator::new(
            results,
            Arc::clone(&self.current),
        )))
    }
}

#[cfg(test)]
mod tests;
