use aletheiadb::storage::sharding::network::{
    CircuitBreakerConfig, ConnectionPool, MockShardClient, PoolConfig,
};
use aletheiadb::storage::sharding::types::ShardId;
use std::sync::Arc;

#[test]
fn havoc_connection_leak_repro() {
    let shard_id = ShardId::new(0).unwrap();
    let pool_config = PoolConfig {
        max_connections: 10,
        ..Default::default()
    };
    let pool = ConnectionPool::new(shard_id, pool_config, CircuitBreakerConfig::default());

    // 1. Fill the pool with one healthy connection
    let client = Arc::new(MockShardClient::new(shard_id));
    pool.add(client.clone());

    assert_eq!(pool.stats().total_connections, 1);

    // 2. Mark it unhealthy
    client.set_healthy(false);

    // 3. Simulate failure loop
    // In a real scenario, if get() fails, the caller (RPC client) creates a new connection
    // and adds it to the pool for future reuse.
    for _ in 0..100 {
        // get() returns Error because existing connection is unhealthy
        assert!(pool.get().is_err());

        // Caller creates new connection (thinking it helps)
        let new_client = Arc::new(MockShardClient::new(shard_id));
        // But if the network is down, this new client might also be unhealthy/unused
        // Or even if healthy, we add it.
        new_client.set_healthy(false); // Simulate persistent failure
        pool.add(new_client);
    }

    // 4. Assert leak
    let stats = pool.stats();
    println!("Pool Stats: {:?}", stats);

    // We expect 101 connections.
    // The pool never cleaned up the unhealthy ones.
    // And it allowed adding beyond max_connections (10).
    assert!(
        stats.total_connections <= 10,
        "Memory Leak: Pool size {} exceeded max_connections 10",
        stats.total_connections
    );
}

#[test]
fn havoc_max_connections_enforcement() {
    let shard_id = ShardId::new(1).unwrap();
    let max_connections = 5;
    let pool_config = PoolConfig {
        max_connections,
        ..Default::default()
    };
    let pool = ConnectionPool::new(shard_id, pool_config, CircuitBreakerConfig::default());

    // 1. Fill the pool to max capacity with healthy connections
    for _ in 0..max_connections {
        let client = Arc::new(MockShardClient::new(shard_id));
        client.set_healthy(true);
        pool.add(client);
    }

    let stats = pool.stats();
    assert_eq!(stats.total_connections, max_connections, "Should fill to capacity");

    // 2. Try to add one more healthy connection
    let extra_client = Arc::new(MockShardClient::new(shard_id));
    extra_client.set_healthy(true);
    pool.add(extra_client);

    // 3. Assert that the pool did NOT grow
    // This kills the mutation where `len < max` becomes `len <= max`
    let final_stats = pool.stats();
    assert_eq!(
        final_stats.total_connections,
        max_connections,
        "Pool size should strictly adhere to max_connections (mutant check: < vs <=)"
    );
}
