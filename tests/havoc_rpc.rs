#[cfg(test)]
mod tests {
    use aletheiadb::storage::sharding::rpc_client::{HttpShardClient, RpcConfig};
    use aletheiadb::storage::sharding::{ShardClient, ShardId};
    use std::sync::Arc;

    #[tokio::test]
    async fn fuzz_rpc_client_concurrency() {
        let config = RpcConfig::default();
        let client = Arc::new(HttpShardClient::new(ShardId::new(0).unwrap(), config).unwrap());
        let mut handles = vec![];
        for _ in 0..10 {
            let client_clone = client.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    let _ = client_clone.is_healthy();
                }
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
    }
}
