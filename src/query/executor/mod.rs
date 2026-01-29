//! Query Executor
//!
//! Executes physical query plans using a pull-based iterator model.
//! The executor transforms physical operators into iterators that
//! lazily produce results.

mod iterators;
mod results;

use parking_lot::RwLock;
use std::sync::Arc;

use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::utils::error::Result;

use super::planner::physical::{PhysicalOp, PhysicalPlan};

#[doc(hidden)]
pub use iterators::NodeScanIterator;
pub use iterators::ResultIterator;
pub use iterators::TemporalNodeScanIterator;
pub use results::{EntityId, EntityResult, QueryResults, QueryRow};

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
            } => {
                let results = match (property_key, label_filter) {
                    // Property-specific with label filter
                    (Some(prop), Some(label)) => self
                        .current
                        .find_similar_by_embedding_in_with_label(prop, embedding, label, *k)?,
                    // Property-specific without label filter
                    (Some(prop), None) => self
                        .current
                        .find_similar_by_embedding_in(prop, embedding, *k)?,
                    // Default property with label filter
                    (None, Some(label)) => self
                        .current
                        .find_similar_by_embedding_with_label(embedding, label, *k)?,
                    // Default property without label filter
                    (None, None) => self.current.find_similar_by_embedding(embedding, *k)?,
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

            PhysicalOp::Empty => Ok(Box::new(iterators::EmptyIterator)),
            PhysicalOp::SimilarToNode {
                source_node,
                property_key,
                k,
                label_filter,
            } => {
                // 1. Validate that property_key matches the indexed property
                let indexed_property =
                    self.current.get_indexed_property_name().ok_or_else(|| {
                        crate::utils::error::Error::Query(
                            crate::utils::error::QueryError::ExecutionError {
                                message:
                                    "No vector index is enabled. Call enable_vector_index() first."
                                        .to_string(),
                            },
                        )
                    })?;

                if property_key != &indexed_property {
                    return Err(crate::utils::error::Error::Query(
                        crate::utils::error::QueryError::ExecutionError {
                            message: format!(
                                "Property key '{}' does not match indexed property '{}'. \
                                 Vector index was built on '{}', so similar_to queries must use the same property.",
                                property_key, indexed_property, indexed_property
                            ),
                        },
                    ));
                }

                // 2. Look up the source node
                let node = self.current.get_node(*source_node).map_err(|_| {
                    crate::utils::error::Error::Query(
                        crate::utils::error::QueryError::ExecutionError {
                            message: format!("Source node {:?} not found", source_node),
                        },
                    )
                })?;

                // 3. Extract the embedding from the specified property
                let embedding = node
                    .properties
                    .get(property_key)
                    .and_then(|v: &crate::core::PropertyValue| v.as_vector())
                    .ok_or_else(|| {
                        crate::utils::error::Error::Query(
                            crate::utils::error::QueryError::ExecutionError {
                                message: format!(
                                    "Node {:?} does not have a vector property '{}'",
                                    source_node, property_key
                                ),
                            },
                        )
                    })?;

                // 4. Perform HNSW search with the extracted embedding.
                // Request k+1 results to account for filtering out the source node.
                // Use checked_add to prevent overflow (though k=usize::MAX is extremely unlikely).
                let k_with_source = k.checked_add(1).unwrap_or(*k);
                let mut results = if let Some(label) = label_filter {
                    self.current.find_similar_by_embedding_with_label(
                        embedding,
                        label,
                        k_with_source,
                    )?
                } else {
                    self.current
                        .find_similar_by_embedding(embedding, k_with_source)?
                };

                // Remove source node from results. In vector similarity with cosine distance,
                // a node has similarity 1.0 with itself, so it's always the first result.
                // Filtering it out ensures we return k truly *different* nodes.
                results.retain(|(node_id, _)| node_id != source_node);

                // Trim to requested k results after filtering
                results.truncate(*k);

                Ok(Box::new(iterators::VectorResultIterator::new(
                    results,
                    Arc::clone(&self.current),
                )))
            }

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
        use crate::core::temporal::time;
        let now = time::now();
        let alice_label = crate::core::interning::GLOBAL_INTERNER
            .intern("Person")
            .unwrap();
        let bob_label = alice_label;
        {
            let mut hist = historical.write();
            hist.add_node_version(
                alice,
                crate::core::id::VersionId::new(1).unwrap(),
                now,
                now,
                alice_label,
                alice_props,
            )
            .unwrap();
            hist.add_node_version(
                bob,
                crate::core::id::VersionId::new(2).unwrap(),
                now,
                now,
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
            include_provenance: false,
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
            include_provenance: false,
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
                property_key: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
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
                property_key: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
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
                temporal_context: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
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
            include_provenance: false,
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
                property_key: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
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
            include_provenance: false,
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
            include_provenance: false,
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
            include_provenance: false,
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
            include_provenance: false,
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
                        temporal_context: None,
                    }),
                    predicate: crate::query::Predicate::eq("name", "Bob"),
                }),
                count: 10,
                offset: 0,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(bob));
    }

    // ==================== SimilarTo Tests ====================

    #[test]
    fn test_similar_to_node_execution() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index first
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        // Create test nodes with embeddings
        let embedding1 = vec![1.0, 0.0, 0.0];
        let embedding2 = vec![0.9, 0.1, 0.0]; // Similar to embedding1
        let embedding3 = vec![0.0, 1.0, 0.0]; // Different from embedding1

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("title", "Doc1")
                    .insert_vector("embedding", &embedding1)
                    .build(),
            )
            .unwrap();

        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("title", "Doc2")
                    .insert_vector("embedding", &embedding2)
                    .build(),
            )
            .unwrap();

        let _node3 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("title", "Doc3")
                    .insert_vector("embedding", &embedding3)
                    .build(),
            )
            .unwrap();

        // Execute SimilarTo query
        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        // Should find node2 as most similar (excluding node1 itself)
        assert!(!rows.is_empty());
        assert_eq!(rows[0].entity.node_id(), Some(node2));
    }

    #[test]
    fn test_similar_to_with_label_filter() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index first
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        let embedding = vec![1.0, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();

        // Create similar node with different label
        let _node2 = storage
            .create_node(
                "Other",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        // Create similar node with same label
        let node3 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.95, 0.05, 0.0])
                    .build(),
            )
            .unwrap();

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: Some("Doc".to_string()),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        // Should only find node3 (Doc label), not node2 (Other label)
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity.node_id(), Some(node3));
    }

    #[test]
    fn test_similar_to_source_node_not_found() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();

        // Enable vector index so we can test the "node not found" error
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        let executor = QueryExecutor::new(storage, historical);

        let nonexistent_node = NodeId::new(9999).unwrap();

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: nonexistent_node,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let result = executor.execute(plan);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("Source node"));
        }
    }

    #[test]
    fn test_similar_to_missing_vector_property() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index so we can test the "missing property" error
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        // Create node without vector property
        let node = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert("title", "No embedding")
                    .build(),
            )
            .unwrap();

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let result = executor.execute(plan);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("does not have a vector property"));
        }
    }

    #[test]
    fn test_similar_to_custom_property_key() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index on custom property first
        storage
            .enable_vector_index("custom_vector", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        let embedding1 = vec![1.0, 0.0, 0.0];
        let embedding2 = vec![0.9, 0.1, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("custom_vector", &embedding1)
                    .build(),
            )
            .unwrap();

        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("custom_vector", &embedding2)
                    .build(),
            )
            .unwrap();

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "custom_vector".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert!(!rows.is_empty());
        assert_eq!(rows[0].entity.node_id(), Some(node2));
    }

    #[test]
    fn test_similar_to_no_vector_index() {
        use crate::core::PropertyMapBuilder;

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        let embedding = vec![1.0, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();

        // NOTE: Vector index NOT enabled for "embedding" property

        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "embedding".to_string(),
                k: 5,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let result = executor.execute(plan);
        assert!(
            result.is_err(),
            "Should return error when vector index is not enabled"
        );

        // Verify error message indicates index not found
        if let Err(e) = result {
            let error_msg = format!("{}", e);
            assert!(
                error_msg.contains("index") || error_msg.contains("Index"),
                "Error should mention missing index: {}",
                error_msg
            );
        }
    }

    #[test]
    fn test_similar_to_fewer_results_than_k() {
        use crate::core::PropertyMapBuilder;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let (storage, historical) = create_test_storage();
        let executor = QueryExecutor::new(Arc::clone(&storage), historical);

        // Enable vector index
        storage
            .enable_vector_index("embedding", HnswConfig::new(3, DistanceMetric::Cosine))
            .unwrap();

        // Create only 3 nodes total
        let embedding1 = vec![1.0, 0.0, 0.0];
        let embedding2 = vec![0.9, 0.1, 0.0];
        let embedding3 = vec![0.8, 0.2, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding1)
                    .build(),
            )
            .unwrap();

        let _node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding2)
                    .build(),
            )
            .unwrap();

        let _node3 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &embedding3)
                    .build(),
            )
            .unwrap();

        // Request k=10 similar nodes, but only 2 exist (3 total - 1 source)
        let plan = PhysicalPlan {
            root: PhysicalOp::SimilarToNode {
                source_node: node1,
                property_key: "embedding".to_string(),
                k: 10,
                label_filter: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        // Should return only 2 results (3 nodes - source node), not 10
        assert_eq!(
            rows.len(),
            2,
            "Should return only 2 results when database has fewer nodes than k"
        );
    }

    // ==================== Multi-Property Vector Index Tests ====================

    /// Helper to create storage with multiple vector properties
    fn create_multi_property_vector_storage() -> (
        Arc<CurrentStorage>,
        Arc<RwLock<HistoricalStorage>>,
        NodeId,
        NodeId,
    ) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::with_config(
            AnchorConfig::default(),
        )));

        // Enable TWO different vector indexes with different dimensions
        current
            .enable_vector_index(
                "title_embedding",
                HnswConfig::new(4, DistanceMetric::Cosine),
            )
            .expect("Should enable first vector index");
        current
            .enable_vector_index(
                "content_embedding",
                HnswConfig::new(8, DistanceMetric::Cosine),
            )
            .expect("Should enable second vector index");

        // Create test nodes with DIFFERENT embeddings for each property
        // Node 1: title_embedding is similar to [1,0,0,0], content_embedding is different
        let doc1_props = PropertyMapBuilder::new()
            .insert("title", "Rust Programming")
            .insert_vector("title_embedding", &[1.0f32, 0.0, 0.0, 0.0])
            .insert_vector(
                "content_embedding",
                &[0.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
            )
            .build();
        let doc1 = current.create_node("Document", doc1_props).unwrap();

        // Node 2: title_embedding is different, content_embedding is similar to [0,0,0,0,1,0,0,0]
        let doc2_props = PropertyMapBuilder::new()
            .insert("title", "Python Basics")
            .insert_vector("title_embedding", &[0.0f32, 1.0, 0.0, 0.0])
            .insert_vector(
                "content_embedding",
                &[0.0f32, 0.0, 0.0, 0.0, 0.9, 0.1, 0.0, 0.0],
            )
            .build();
        let doc2 = current.create_node("Document", doc2_props).unwrap();

        (current, historical, doc1, doc2)
    }

    /// Test that HnswSearch uses property_key to query the correct index.
    ///
    /// This test verifies that when querying "title_embedding" vs "content_embedding",
    /// we get different results because the embeddings are different.
    #[test]
    fn test_hnsw_search_multi_property() {
        let (current, historical, doc1, _doc2) = create_multi_property_vector_storage();
        let executor = QueryExecutor::new(current, historical);

        // Query title_embedding with vector similar to doc1's title_embedding
        let plan = PhysicalPlan {
            root: PhysicalOp::HnswSearch {
                embedding: vec![1.0f32, 0.0, 0.0, 0.0].into(),
                k: 1,
                label_filter: None,
                property_key: Some("title_embedding".to_string()),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 1);
        // The top result should be doc1 because its title_embedding is [1,0,0,0]
        match &rows[0].entity {
            EntityResult::NodeId(id) => {
                assert_eq!(*id, doc1, "Should return doc1 for title_embedding query")
            }
            EntityResult::Node(node) => assert_eq!(
                node.id, doc1,
                "Should return doc1 for title_embedding query"
            ),
            _ => panic!("Expected Node or NodeId result"),
        }
    }

    /// Test that VectorRerank uses property_key to rerank by the correct property.
    #[test]
    fn test_vector_rerank_multi_property() {
        let (current, historical, doc1, doc2) = create_multi_property_vector_storage();
        let executor = QueryExecutor::new(current, historical);

        // Start with both nodes, rerank by content_embedding similarity
        // Query embedding is similar to doc2's content_embedding
        let plan = PhysicalPlan {
            root: PhysicalOp::VectorRerank {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![doc1, doc2],
                }),
                embedding: vec![0.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0].into(),
                k: 2,
                property_key: Some("content_embedding".to_string()),
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        assert_eq!(rows.len(), 2);
        // doc1 should be first because its content_embedding [0,0,0,0,1,0,0,0] is most similar
        match &rows[0].entity {
            EntityResult::NodeId(id) => {
                assert_eq!(*id, doc1, "doc1 should rank first by content_embedding")
            }
            EntityResult::Node(node) => {
                assert_eq!(node.id, doc1, "doc1 should rank first by content_embedding")
            }
            _ => panic!("Expected Node or NodeId result"),
        }
    }
}
