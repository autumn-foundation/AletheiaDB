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
    use super::*;
    use crate::core::id::NodeId;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::hnsw::HnswConfig;
    use crate::query::planner::physical::PhysicalOp;
    use crate::storage::version::AnchorConfig;

    fn create_test_storage() -> (Arc<CurrentStorage>, Arc<RwLock<HistoricalStorage>>) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
            AnchorConfig::default(),
        )));
        (current, historical)
    }

    fn create_test_storage_with_data() -> (
        Arc<CurrentStorage>,
        Arc<RwLock<HistoricalStorage>>,
        NodeId,
        NodeId,
    ) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
            AnchorConfig::default(),
        )));

        // Enable vector index
        current
            .enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
            .unwrap();

        // Create test nodes
        let alice_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
            .build();
        let alice = current.create_node("Person", alice_props.clone()).unwrap();

        let bob_props = PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
            .build();
        let bob = current.create_node("Person", bob_props.clone()).unwrap();

        // Create edge
        current
            .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Add versions to historical storage (needed for temporal queries)
        use crate::core::temporal::{BiTemporalInterval, time};
        let now = time::now();
        let alice_label = crate::core::interning::GLOBAL_INTERNER
            .intern("Person")
            .unwrap();
        let bob_label = alice_label;
        {
            let mut hist = historical.write().unwrap();
            hist.add_node_version(
                alice,
                crate::core::id::VersionId::new(1).unwrap(),
                BiTemporalInterval::current(now),
                alice_label,
                alice_props,
            )
            .unwrap();
            hist.add_node_version(
                bob,
                crate::core::id::VersionId::new(2).unwrap(),
                BiTemporalInterval::current(now),
                bob_label,
                bob_props,
            )
            .unwrap();
        }

        (current, historical, alice, bob)
    }

    #[test]
    fn test_execution_config_default() {
        let config = ExecutionConfig::default();

        assert_eq!(config.max_buffer_size, 10_000);
        assert!(!config.parallel);
        assert_eq!(config.timeout_ms, 0);
    }

    #[test]
    fn test_execution_config_custom() {
        let config = ExecutionConfig {
            max_buffer_size: 1000,
            parallel: true,
            timeout_ms: 5000,
        };

        assert_eq!(config.max_buffer_size, 1000);
        assert!(config.parallel);
        assert_eq!(config.timeout_ms, 5000);
    }

    #[test]
    fn test_executor_new() {
        let (current, historical) = create_test_storage();
        let executor = QueryExecutor::new(current, historical);

        // Just verify it was created
        assert_eq!(executor._config.max_buffer_size, 10_000);
    }

    #[test]
    fn test_executor_with_config() {
        let (current, historical) = create_test_storage();
        let config = ExecutionConfig {
            max_buffer_size: 500,
            parallel: true,
            timeout_ms: 1000,
        };
        let executor = QueryExecutor::with_config(current, historical, config);

        assert_eq!(executor._config.max_buffer_size, 500);
        assert!(executor._config.parallel);
    }

    #[test]
    fn test_execute_node_lookup() {
        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::NodeLookup {
                node_ids: vec![alice],
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(alice));
    }

    #[test]
    fn test_execute_node_scan() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::NodeScan {
                label: Some("Person".to_string()),
                estimated_rows: 100,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2); // Alice and Bob
    }

    #[test]
    fn test_execute_hnsw_search() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::HnswSearch {
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 2,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn test_execute_hnsw_search_with_label() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::HnswSearch {
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 2,
                label_filter: Some("Person".to_string()),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn test_execute_indexed_traversal() {
        let (current, historical, alice, bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::IndexedTraversal {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![alice],
                }),
                direction: crate::query::ir::Direction::Outgoing,
                label: Some("KNOWS".to_string()),
                depth: 1,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(bob));
    }

    #[test]
    fn test_execute_filter() {
        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Filter {
                input: Box::new(PhysicalOp::NodeScan {
                    label: Some("Person".to_string()),
                    estimated_rows: 100,
                }),
                predicate: crate::query::Predicate::eq("name", "Alice"),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(alice));
    }

    #[test]
    fn test_execute_vector_rerank() {
        let (current, historical, alice, bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::VectorRerank {
                input: Box::new(PhysicalOp::NodeScan {
                    label: Some("Person".to_string()),
                    estimated_rows: 100,
                }),
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 2,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2);
        // Alice should be first (exact match)
        assert_eq!(rows[0].entity.node_id(), Some(alice));
        assert_eq!(rows[1].entity.node_id(), Some(bob));
    }

    #[test]
    fn test_execute_limit() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Limit {
                input: Box::new(PhysicalOp::NodeScan {
                    label: Some("Person".to_string()),
                    estimated_rows: 100,
                }),
                count: 1,
                offset: 0,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn test_execute_limit_with_offset() {
        let (current, historical, _alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Limit {
                input: Box::new(PhysicalOp::NodeScan {
                    label: Some("Person".to_string()),
                    estimated_rows: 100,
                }),
                count: 10,
                offset: 1,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1); // Skipped 1, only 1 remaining
    }

    #[test]
    fn test_execute_empty() {
        let (current, historical) = create_test_storage();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::Empty,
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert!(rows.is_empty());
    }

    #[test]
    fn test_execute_temporal_node_lookup() {
        let (current, historical, alice, _bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        let now = crate::core::temporal::time::now();
        let plan = PhysicalPlan {
            root: PhysicalOp::TemporalNodeLookup {
                node_ids: vec![alice],
                valid_time: now,
                transaction_time: now,
                use_batch: false,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        // This will return empty since there's no historical data yet
        // but it exercises the code path
        let results = executor.execute(plan).expect("Execution failed");
        let _rows: Vec<_> = results.collect_all().expect("Collection failed");
        // Result may be empty or have current data depending on implementation
    }

    #[test]
    fn test_execute_nested_operations() {
        let (current, historical, alice, bob) = create_test_storage_with_data();
        let executor = QueryExecutor::new(current, historical);

        // Traverse -> Filter -> Limit
        let plan = PhysicalPlan {
            root: PhysicalOp::Limit {
                input: Box::new(PhysicalOp::Filter {
                    input: Box::new(PhysicalOp::IndexedTraversal {
                        input: Box::new(PhysicalOp::NodeLookup {
                            node_ids: vec![alice],
                        }),
                        direction: crate::query::ir::Direction::Outgoing,
                        label: Some("KNOWS".to_string()),
                        depth: 1,
                    }),
                    predicate: crate::query::Predicate::eq("name", "Bob"),
                }),
                count: 10,
                offset: 0,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(bob));
    }
}
