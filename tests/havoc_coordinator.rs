#[cfg(test)]
mod tests {
    use aletheiadb::storage::sharding::ShardId;
    use aletheiadb::storage::sharding::config::{ShardConfig, ShardDefinition};
    use aletheiadb::storage::sharding::coordinator::ShardCoordinator;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn fuzz_coordinator_concurrency() {
        let config = ShardConfig::new(vec![ShardDefinition::new(
            0,
            "localhost:8080",
            vec!["LabelA"],
        )]);
        let coordinator = Arc::new(ShardCoordinator::new(config));

        let mut handles = vec![];
        for _ in 0..10 {
            let coord = coordinator.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _ = coord.begin_distributed_transaction(vec![ShardId::new(0).unwrap()]);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
