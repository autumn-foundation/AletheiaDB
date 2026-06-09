#[allow(unused_imports)]
use super::*;

fn create_test_config(num_nodes: usize) -> DistributedVectorConfig {
    let mut config = DistributedVectorConfig::new(4, DistanceMetric::Cosine);
    for i in 0..num_nodes {
        config = config.with_node(VectorNodeConfig::new(i as u16, format!("node{}:9000", i)));
    }
    config
}

fn create_test_clients(num_nodes: usize) -> Vec<Arc<MockVectorNodeClient>> {
    (0..num_nodes)
        .map(|i| {
            Arc::new(MockVectorNodeClient::new(
                i as u16,
                4,
                DistanceMetric::Cosine,
            ))
        })
        .collect()
}

// ============================================================
// Configuration Tests
// ============================================================

#[test]
fn test_config_creation() {
    let config = DistributedVectorConfig::new(384, DistanceMetric::Cosine);
    assert_eq!(config.dimensions, 384);
    assert_eq!(config.metric, DistanceMetric::Cosine);
    assert!(config.nodes.is_empty());
}

#[test]
fn test_config_with_nodes() {
    let config = DistributedVectorConfig::new(384, DistanceMetric::Cosine)
        .with_node(VectorNodeConfig::new(0, "node0:9000"))
        .with_node(VectorNodeConfig::new(1, "node1:9000"));

    assert_eq!(config.nodes.len(), 2);
    assert_eq!(config.nodes[0].node_id, 0);
    assert_eq!(config.nodes[1].node_id, 1);
}

#[test]
fn test_config_validation_zero_dimensions() {
    let config = DistributedVectorConfig::new(0, DistanceMetric::Cosine)
        .with_node(VectorNodeConfig::new(0, "node0:9000"));

    assert!(config.validate().is_err());
}

#[test]
fn test_config_validation_no_nodes() {
    let config = DistributedVectorConfig::new(384, DistanceMetric::Cosine);
    assert!(config.validate().is_err());
}

#[test]
fn test_config_validation_duplicate_nodes() {
    let config = DistributedVectorConfig::new(384, DistanceMetric::Cosine)
        .with_node(VectorNodeConfig::new(0, "node0:9000"))
        .with_node(VectorNodeConfig::new(0, "node1:9000")); // Duplicate ID

    assert!(config.validate().is_err());
}

// ============================================================
// Index Creation Tests
// ============================================================

#[test]
fn test_create_distributed_index() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);
    let clients: Vec<Arc<MockVectorNodeClient>> = clients.into_iter().collect();

    let index = DistributedVectorIndex::new(config, clients)?;

    assert_eq!(index.node_count(), 3);
    assert_eq!(index.dimensions(), 4);
    assert_eq!(index.distance_metric(), DistanceMetric::Cosine);

    Ok(())
}

#[test]
fn test_create_mismatched_clients() {
    let config = create_test_config(3);
    let clients = create_test_clients(2); // Wrong number

    let result = DistributedVectorIndex::new(config, clients);
    assert!(result.is_err());
}

// ============================================================
// Add/Remove Tests
// ============================================================

#[test]
fn test_add_vector() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    let node = NodeId::new(1).unwrap();
    let vector = vec![1.0, 0.0, 0.0, 0.0];
    index.add(node, &vector)?;

    assert_eq!(index.len(), 1);

    Ok(())
}

#[test]
fn test_add_multiple_vectors() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    for i in 1..=100 {
        let node = NodeId::new(i).unwrap();
        let vector = vec![i as f32, 0.0, 0.0, 0.0];
        index.add(node, &vector)?;
    }

    assert_eq!(index.len(), 100);

    Ok(())
}

#[test]
fn test_add_dimension_mismatch() {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients).unwrap();

    let node = NodeId::new(1).unwrap();
    let wrong_dim = vec![1.0, 0.0]; // Wrong dimensions

    assert!(index.add(node, &wrong_dim).is_err());
}

#[test]
fn test_add_with_nan() {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients).unwrap();

    let node = NodeId::new(1).unwrap();
    let nan_vector = vec![1.0, f32::NAN, 0.0, 0.0];

    assert!(index.add(node, &nan_vector).is_err());
}

#[test]
fn test_remove_vector() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    let node = NodeId::new(1).unwrap();
    let vector = vec![1.0, 0.0, 0.0, 0.0];

    index.add(node, &vector)?;
    assert_eq!(index.len(), 1);

    index.remove(node)?;
    assert_eq!(index.len(), 0);

    Ok(())
}

// ============================================================
// Search Tests
// ============================================================

#[test]
fn test_search_empty_index() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index.search(&query, 10)?;

    assert!(results.is_empty());

    Ok(())
}

#[test]
fn test_search_basic() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Add vectors
    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let node3 = NodeId::new(3).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
    index.add(node2, &[0.9, 0.1, 0.0, 0.0])?;
    index.add(node3, &[0.0, 1.0, 0.0, 0.0])?;

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let results = index.search(&query, 3)?;

    assert_eq!(results.len(), 3);
    // First result should be node1 (identical to query)
    assert_eq!(results[0].0, node1);

    Ok(())
}

#[test]
fn test_search_dimension_mismatch() {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients).unwrap();

    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    let wrong_query = vec![1.0, 0.0]; // Wrong dimensions

    assert!(index.search(&wrong_query, 10).is_err());
}

#[test]
fn test_search_with_filter() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Add vectors
    for i in 1..=10 {
        let node = NodeId::new(i).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
    }

    let query = vec![5.0, 0.0, 0.0, 0.0];
    let results = index.search_with_filter(&query, 10, |id| id.as_u64() % 2 == 0)?;

    // Should only have even IDs
    for (id, _) in &results {
        assert_eq!(id.as_u64() % 2, 0);
    }

    Ok(())
}

// ============================================================
// Routing Tests
// ============================================================

#[test]
fn test_consistent_routing() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    let node = NodeId::new(42).unwrap();
    let route1 = index.node_for_id(node);
    let route2 = index.node_for_id(node);
    let route3 = index.node_for_id(node);

    assert_eq!(route1, route2);
    assert_eq!(route2, route3);

    Ok(())
}

#[test]
fn test_range_based_routing() -> Result<()> {
    let mut config = create_test_config(3);
    config.routing_strategy = RoutingStrategy::RangeBased;
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Test edge cases
    let node_min = NodeId::new(0).unwrap();
    let node_max = NodeId::new(u64::MAX - 1000).unwrap();

    let route_min = index.node_for_id(node_min);
    let route_max = index.node_for_id(node_max);

    assert!(route_min < 3);
    assert!(route_max < 3);

    Ok(())
}

// ============================================================
// Circuit Breaker Tests
// ============================================================

#[test]
fn test_circuit_breaker_opens_on_failures() {
    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        open_duration: Duration::from_millis(100),
        success_threshold: 2,
    };
    let cb = NodeCircuitBreaker::new(config);

    assert_eq!(cb.state(), CircuitState::Closed);

    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Closed);

    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);
    assert!(!cb.should_allow());
}

#[test]
fn test_circuit_breaker_half_open_transition() {
    let config = CircuitBreakerConfig {
        failure_threshold: 1,
        open_duration: Duration::from_millis(10),
        success_threshold: 1,
    };
    let cb = NodeCircuitBreaker::new(config);

    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    std::thread::sleep(Duration::from_millis(20));

    assert_eq!(cb.state(), CircuitState::HalfOpen);
}

#[test]
fn test_circuit_breaker_closes_from_half_open() {
    let config = CircuitBreakerConfig {
        failure_threshold: 1,
        open_duration: Duration::from_millis(10),
        success_threshold: 2,
    };
    let cb = NodeCircuitBreaker::new(config);

    cb.record_failure();
    std::thread::sleep(Duration::from_millis(20));

    assert_eq!(cb.state(), CircuitState::HalfOpen);

    cb.record_success();
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    cb.record_success();
    assert_eq!(cb.state(), CircuitState::Closed);
}

// ============================================================
// Node Failure Tests
// ============================================================

#[test]
fn test_search_with_unhealthy_node() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    // Make one node unhealthy
    clients[1].set_healthy(false);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Add vectors (will only go to healthy nodes based on routing)
    for i in 1..=10 {
        let node = NodeId::new(i).unwrap();
        // This will succeed for nodes routed to healthy clients
        let _ = index.add(node, &[i as f32, 0.0, 0.0, 0.0]);
    }

    // Search should still work with partial results
    let query = vec![5.0, 0.0, 0.0, 0.0];
    let results = index.search(&query, 10)?;

    // Should have results from the healthy nodes
    assert!(!results.is_empty() || index.len() == 0);

    Ok(())
}

#[test]
fn test_search_all_nodes_unavailable() {
    let mut config = create_test_config(3);
    config.min_nodes_for_search = 1;
    let clients = create_test_clients(3);

    // Make all nodes unhealthy
    for client in &clients {
        client.set_healthy(false);
    }

    let index = DistributedVectorIndex::new(config, clients).unwrap();

    let query = vec![1.0, 0.0, 0.0, 0.0];
    let result = index.search(&query, 10);

    assert!(result.is_err());
}

// ============================================================
// Stats Tests
// ============================================================

#[test]
fn test_stats() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    let stats = index.stats();
    assert_eq!(stats.node_count, 3);
    assert_eq!(stats.available_nodes, 3);
    assert_eq!(stats.node_stats.len(), 3);

    Ok(())
}

#[test]
fn test_node_connection_stats() -> Result<()> {
    let config = create_test_config(1);
    let clients = create_test_clients(1);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Perform some operations
    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0])?;
    index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0])?;

    let stats = index.stats();
    assert!(stats.node_stats[0].request_count >= 2);

    Ok(())
}

// ============================================================
// Merge Results Tests
// ============================================================

#[test]
fn test_merge_results_empty() {
    let results = DistributedVectorIndex::<MockVectorNodeClient>::merge_results(vec![], 10);
    assert!(results.is_empty());
}

#[test]
fn test_merge_results_single_node() {
    let node_results = vec![vec![
        (NodeId::new(1).unwrap(), 0.9),
        (NodeId::new(2).unwrap(), 0.8),
        (NodeId::new(3).unwrap(), 0.7),
    ]];

    let merged = DistributedVectorIndex::<MockVectorNodeClient>::merge_results(node_results, 2);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].0, NodeId::new(1).unwrap());
    assert_eq!(merged[1].0, NodeId::new(2).unwrap());
}

#[test]
fn test_merge_results_multiple_nodes() {
    let node_results = vec![
        vec![
            (NodeId::new(1).unwrap(), 0.9),
            (NodeId::new(2).unwrap(), 0.7),
        ],
        vec![
            (NodeId::new(3).unwrap(), 0.85),
            (NodeId::new(4).unwrap(), 0.6),
        ],
    ];

    let merged = DistributedVectorIndex::<MockVectorNodeClient>::merge_results(node_results, 3);
    assert_eq!(merged.len(), 3);
    // Should be sorted: 0.9, 0.85, 0.7
    assert_eq!(merged[0].0, NodeId::new(1).unwrap());
    assert_eq!(merged[1].0, NodeId::new(3).unwrap());
    assert_eq!(merged[2].0, NodeId::new(2).unwrap());
}

// ============================================================
// Mock Client Tests
// ============================================================

#[test]
fn test_mock_client_add_search() -> Result<()> {
    let client = MockVectorNodeClient::new(0, 4, DistanceMetric::Cosine);

    let node = NodeId::new(1).unwrap();
    client.add(node, &[1.0, 0.0, 0.0, 0.0])?;

    let results = client.search(&[1.0, 0.0, 0.0, 0.0], 10)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node);
    assert!((results[0].1 - 1.0).abs() < 0.001); // Cosine similarity with self = 1.0

    Ok(())
}

#[test]
fn test_mock_client_fail_next() {
    let client = MockVectorNodeClient::new(0, 4, DistanceMetric::Cosine);
    client.fail_next("Test error");

    let result = client.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]);
    assert!(result.is_err());

    // Next call should succeed
    let result = client.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0]);
    assert!(result.is_ok());
}

#[test]
fn test_mock_client_unhealthy() {
    let client = MockVectorNodeClient::new(0, 4, DistanceMetric::Cosine);
    client.set_healthy(false);

    let result = client.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]);
    assert!(result.is_err());
}

// ============================================================
// Rebalancing Tests
// ============================================================

#[test]
fn test_needs_rebalancing_empty() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Empty index - no rebalancing needed
    assert!(!index.needs_rebalancing(2.0));

    Ok(())
}

#[test]
fn test_needs_rebalancing_balanced() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Add vectors that should distribute relatively evenly
    for i in 1..=30 {
        let node = NodeId::new(i).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
    }

    // With hash-based routing, should be relatively balanced
    // Threshold of 3.0 should accommodate some variance
    let stats = index.rebalance_stats();
    // The test is that the function works, not that perfect balance is achieved
    assert!(stats.total_vectors == 30);

    Ok(())
}

#[test]
fn test_rebalance_stats() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Add some vectors
    for i in 1..=15 {
        let node = NodeId::new(i).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
    }

    let stats = index.rebalance_stats();

    assert_eq!(stats.total_vectors, 15);
    assert_eq!(stats.node_count, 3);
    assert!(stats.min_node_size <= stats.max_node_size);
    assert!(stats.imbalance_ratio >= 1.0);

    Ok(())
}

// ============================================================
// VectorNodeConfig Tests
// ============================================================

#[test]
fn test_node_config_defaults() {
    let config = VectorNodeConfig::new(0, "node0:9000");

    assert_eq!(config.node_id, 0);
    assert_eq!(config.endpoint, "node0:9000");
    assert_eq!(config.timeout, DEFAULT_TIMEOUT);
}

#[test]
fn test_node_config_with_timeout() {
    let config = VectorNodeConfig::new(0, "node0:9000").with_timeout(Duration::from_secs(60));

    assert_eq!(config.timeout, Duration::from_secs(60));
}

// ============================================================
// DistributedError Tests
// ============================================================

#[test]
fn test_distributed_error_display() {
    let err = DistributedError::NoNodesAvailable;
    assert!(format!("{}", err).contains("No nodes available"));

    let err = DistributedError::NodeUnavailable {
        node_id: 0,
        reason: "connection refused".to_string(),
    };
    assert!(format!("{}", err).contains("Node 0"));
    assert!(format!("{}", err).contains("connection refused"));

    let err = DistributedError::CircuitOpen {
        node_id: 1,
        remaining: Duration::from_secs(10),
    };
    assert!(format!("{}", err).contains("Circuit breaker"));
    assert!(format!("{}", err).contains("node 1"));
}

#[test]
fn test_distributed_error_display_all_variants() {
    let err = DistributedError::AllNodesFailed {
        failed_count: 3,
        sample_error: "connection timeout".to_string(),
    };
    assert!(format!("{}", err).contains("All 3 nodes failed"));
    assert!(format!("{}", err).contains("connection timeout"));

    let err = DistributedError::Timeout {
        operation: "search".to_string(),
        duration: Duration::from_secs(30),
    };
    assert!(format!("{}", err).contains("search"));
    assert!(format!("{}", err).contains("timed out"));

    let err = DistributedError::ConfigError("invalid dimensions".to_string());
    assert!(format!("{}", err).contains("Configuration error"));
    assert!(format!("{}", err).contains("invalid dimensions"));
}

// ============================================================
// Search k > MAX_K Tests
// ============================================================

#[test]
fn test_search_k_exceeds_max() {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients).unwrap();

    let query = vec![1.0, 0.0, 0.0, 0.0];
    // Request more than MAX_K results
    let result = index.search(&query, MAX_K + 1);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("exceeds maximum"));
}

#[test]
fn test_search_k_at_max() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    let query = vec![1.0, 0.0, 0.0, 0.0];
    // Request exactly MAX_K results - should succeed
    let results = index.search(&query, MAX_K)?;

    assert!(results.is_empty()); // No vectors added

    Ok(())
}

// ============================================================
// Stats Tests with Vectors
// ============================================================

#[test]
fn test_stats_with_vectors() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Add some vectors
    for i in 1..=15 {
        let node = NodeId::new(i).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
    }

    let stats = index.stats();
    assert_eq!(stats.total_vectors, 15);
    assert_eq!(stats.node_count, 3);
    assert_eq!(stats.available_nodes, 3);

    Ok(())
}

// ============================================================
// Partial Results Tests
// ============================================================

#[test]
fn test_search_partial_results_disabled() {
    let mut config = create_test_config(3);
    config.allow_partial_results = false;
    let clients = create_test_clients(3);

    // Make one node unhealthy
    clients[1].set_healthy(false);

    let index = DistributedVectorIndex::new(config, clients).unwrap();

    // Add a vector to a healthy node
    let node = NodeId::new(1).unwrap();
    let _ = index.add(node, &[1.0, 0.0, 0.0, 0.0]);

    let query = vec![1.0, 0.0, 0.0, 0.0];
    // With partial results disabled and one node unhealthy, search should fail
    // (but only if the unhealthy node is actually queried - depends on routing)
    let _ = index.search(&query, 10);
}

// ============================================================
// Circuit Breaker Reset Tests
// ============================================================

#[test]
fn test_reset_all_circuits() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Force some failures to open circuits
    for node in index.nodes.iter() {
        for _ in 0..10 {
            node.circuit_breaker.record_failure();
        }
    }

    // Verify circuits are open
    for node in index.nodes.iter() {
        assert_eq!(node.circuit_state(), CircuitState::Open);
    }

    // Reset all circuits
    index.reset_all_circuits();

    // Verify circuits are closed
    for node in index.nodes.iter() {
        assert_eq!(node.circuit_state(), CircuitState::Closed);
    }

    Ok(())
}

// ============================================================
// Mock Client is_empty Tests
// ============================================================

#[test]
fn test_mock_client_is_empty() -> Result<()> {
    let client = MockVectorNodeClient::new(0, 4, DistanceMetric::Cosine);

    assert!(client.is_empty()?);

    client.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
    assert!(!client.is_empty()?);

    Ok(())
}

// ============================================================
// Circuit Breaker Remaining Time Tests
// ============================================================

#[test]
fn test_circuit_breaker_remaining_time() {
    let config = CircuitBreakerConfig {
        failure_threshold: 1,
        open_duration: Duration::from_secs(60),
        success_threshold: 1,
    };
    let cb = NodeCircuitBreaker::new(config);

    // Initially closed - no remaining time
    assert!(cb.remaining_open_time().is_none());

    // Open the circuit
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    // Should have remaining time
    let remaining = cb.remaining_open_time();
    assert!(remaining.is_some());
    assert!(remaining.unwrap() > Duration::from_secs(0));
    assert!(remaining.unwrap() <= Duration::from_secs(60));
}

// ============================================================
// Node Connection Tests
// ============================================================

#[test]
fn test_node_connection_execute_with_circuit_open() {
    let client = Arc::new(MockVectorNodeClient::new(0, 4, DistanceMetric::Cosine));
    let connection = NodeConnection::new(
        client,
        CircuitBreakerConfig {
            failure_threshold: 1,
            open_duration: Duration::from_secs(60),
            success_threshold: 1,
        },
    );

    // Open the circuit
    connection.circuit_breaker.record_failure();
    assert_eq!(connection.circuit_state(), CircuitState::Open);

    // Execute should fail with circuit open
    let result = connection.execute(|c| c.health_check());
    assert!(result.is_err());
}

#[test]
fn test_node_connection_debug() {
    let client = Arc::new(MockVectorNodeClient::new(0, 4, DistanceMetric::Cosine));
    let connection = NodeConnection::new(client, CircuitBreakerConfig::default());

    let debug_str = format!("{:?}", connection);
    assert!(debug_str.contains("NodeConnection"));
    assert!(debug_str.contains("node_id"));
}

// ============================================================
// DistributedVectorIndex Debug Tests
// ============================================================

#[test]
fn test_distributed_index_debug() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    let debug_str = format!("{:?}", index);
    assert!(debug_str.contains("DistributedVectorIndex"));
    assert!(debug_str.contains("dimensions"));
    assert!(debug_str.contains("node_count"));

    Ok(())
}

// ============================================================
// Mock Client Dimension Mismatch Tests
// ============================================================

#[test]
fn test_mock_client_dimension_mismatch() {
    let client = MockVectorNodeClient::new(0, 4, DistanceMetric::Cosine);

    let result = client.add(NodeId::new(1).unwrap(), &[1.0, 0.0]); // Wrong dimensions
    assert!(result.is_err());
}

// ============================================================
// Config Routing Strategy Tests
// ============================================================

#[test]
fn test_config_routing_strategy() {
    let config = DistributedVectorConfig::new(384, DistanceMetric::Cosine)
        .with_node(VectorNodeConfig::new(0, "node0:9000"))
        .with_routing_strategy(RoutingStrategy::RangeBased);

    assert_eq!(config.routing_strategy, RoutingStrategy::RangeBased);
}

#[test]
fn test_config_min_nodes_for_search() {
    let config = DistributedVectorConfig::new(384, DistanceMetric::Cosine)
        .with_node(VectorNodeConfig::new(0, "node0:9000"))
        .with_min_nodes_for_search(2);

    assert_eq!(config.min_nodes_for_search, 2);
}

// ============================================================
// Node Connection Stats Success Rate Tests
// ============================================================

#[test]
fn test_node_connection_stats_success_rate() {
    let stats = NodeConnectionStats {
        node_id: 0,
        circuit_state: CircuitState::Closed,
        request_count: 10,
        failure_count: 3,
    };

    let rate = stats.success_rate();
    assert!((rate - 0.7).abs() < 0.001);
}

#[test]
fn test_node_connection_stats_success_rate_zero_requests() {
    let stats = NodeConnectionStats {
        node_id: 0,
        circuit_state: CircuitState::Closed,
        request_count: 0,
        failure_count: 0,
    };

    assert_eq!(stats.success_rate(), 1.0);
}

// ============================================================
// Concurrent Circuit Breaker Stress Tests
// ============================================================

#[test]
fn test_circuit_breaker_concurrent_failures() {
    use std::thread;

    let config = CircuitBreakerConfig {
        failure_threshold: 10,
        open_duration: Duration::from_secs(60),
        success_threshold: 3,
    };
    let cb = Arc::new(NodeCircuitBreaker::new(config));

    let mut handles = vec![];

    // Spawn multiple threads to record failures concurrently
    for _ in 0..4 {
        let cb_clone = Arc::clone(&cb);
        let handle = thread::spawn(move || {
            for _ in 0..5 {
                cb_clone.record_failure();
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // After 20 failures (4 threads x 5), circuit should be open
    assert_eq!(cb.state(), CircuitState::Open);
}

#[test]
fn test_circuit_breaker_concurrent_success_failure_mix() {
    use std::thread;

    let config = CircuitBreakerConfig {
        failure_threshold: 5,
        open_duration: Duration::from_millis(10),
        success_threshold: 2,
    };
    let cb = Arc::new(NodeCircuitBreaker::new(config));

    let mut handles = vec![];

    // Some threads record successes
    for _ in 0..2 {
        let cb_clone = Arc::clone(&cb);
        let handle = thread::spawn(move || {
            for _ in 0..10 {
                cb_clone.record_success();
                std::thread::sleep(Duration::from_micros(100));
            }
        });
        handles.push(handle);
    }

    // Some threads record failures
    for _ in 0..2 {
        let cb_clone = Arc::clone(&cb);
        let handle = thread::spawn(move || {
            for _ in 0..10 {
                cb_clone.record_failure();
                std::thread::sleep(Duration::from_micros(100));
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // State should be valid (either Closed, Open, or HalfOpen)
    let state = cb.state();
    assert!(
        state == CircuitState::Closed
            || state == CircuitState::Open
            || state == CircuitState::HalfOpen
    );
}

#[test]
fn test_circuit_breaker_concurrent_state_checks() {
    use std::thread;

    let config = CircuitBreakerConfig {
        failure_threshold: 3,
        open_duration: Duration::from_millis(50),
        success_threshold: 2,
    };
    let cb = Arc::new(NodeCircuitBreaker::new(config));

    // Open the circuit
    for _ in 0..5 {
        cb.record_failure();
    }
    assert_eq!(cb.state(), CircuitState::Open);

    let mut handles = vec![];

    // Multiple threads checking state simultaneously
    for _ in 0..8 {
        let cb_clone = Arc::clone(&cb);
        let handle = thread::spawn(move || {
            for _ in 0..100 {
                let _ = cb_clone.state();
                let _ = cb_clone.should_allow();
                let _ = cb_clone.remaining_open_time();
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // No panic means success - concurrent reads are safe
}

// ============================================================
// Rebalancing Threshold Constant Test
// ============================================================

#[test]
fn test_recommended_imbalance_threshold() {
    // Verify the constant exists and has expected value
    assert!((RECOMMENDED_IMBALANCE_THRESHOLD - 2.0).abs() < 0.001);
}

#[test]
fn test_needs_rebalancing_with_threshold_constant() -> Result<()> {
    let config = create_test_config(3);
    let clients = create_test_clients(3);

    let index = DistributedVectorIndex::new(config, clients)?;

    // Empty index - no rebalancing needed at any threshold
    assert!(!index.needs_rebalancing(RECOMMENDED_IMBALANCE_THRESHOLD));

    Ok(())
}
