#[cfg(test)]
mod tests {
    use crate::AletheiaDB;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_execute_aql_fuzz(
            query_string in "\\PC*",
        ) {
            let db = AletheiaDB::new().unwrap();

            // Should either succeed or return a clean error, but NEVER panic
            let _ = db.execute_aql(&query_string);
        }
    }
}

#[cfg(test)]
#[cfg(loom)]
mod loom_tests {
    use loom::sync::{Arc, RwLock};
    use loom::thread;

    // Simulate RpcClient state with Loom
    struct MockRpcClient {
        healthy: RwLock<bool>,
        last_success: RwLock<Option<u64>>, // Use u64 instead of Instant for Loom
    }

    impl MockRpcClient {
        fn new() -> Self {
            Self {
                healthy: RwLock::new(true),
                last_success: RwLock::new(None),
            }
        }

        fn check_health(&self) -> bool {
            *self.healthy.read().unwrap()
        }

        fn mark_success(&self) {
            let mut h = self.healthy.write().unwrap();
            *h = true;
            let mut ls = self.last_success.write().unwrap();
            *ls = Some(100);
        }

        fn mark_failure(&self) {
            let mut h = self.healthy.write().unwrap();
            *h = false;
        }
    }

    #[test]
    fn test_rpc_client_concurrency() {
        loom::model(|| {
            let client = Arc::new(MockRpcClient::new());

            let c1 = client.clone();
            let t1 = thread::spawn(move || {
                c1.mark_success();
            });

            let c2 = client.clone();
            let t2 = thread::spawn(move || {
                c2.mark_failure();
            });

            let c3 = client.clone();
            let t3 = thread::spawn(move || {
                c3.check_health();
            });

            t1.join().unwrap();
            t2.join().unwrap();
            t3.join().unwrap();
        });
    }
}

#[cfg(test)]
mod parsing_havoc {
    use crate::AletheiaDB;
    use proptest::prelude::*;

    proptest! {
        // Test parsing with extreme characters specifically focusing on query string parsing
        #[test]
        fn test_aql_parser_extreme_strings(
            query in "\\PC{0, 2048}",
        ) {
            let db = AletheiaDB::new().unwrap();
            let _ = db.execute_aql(&query);
        }
    }
}

#[cfg(test)]
mod graph_havoc {
    use crate::AletheiaDB;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use proptest::prelude::*;

    proptest! {
        // Test edge creation with randomly sized strings, extreme integers and extreme properties
        #[test]
        fn test_create_edge_fuzz(
            label_node1 in "\\PC*",
            label_node2 in "\\PC*",
            label_edge in "\\PC*",
        ) {
            let db = AletheiaDB::new().unwrap();
            let mut tx = db.write_transaction().unwrap();

            let n1 = tx.create_node(&label_node1, PropertyMapBuilder::new().build());
            let n2 = tx.create_node(&label_node2, PropertyMapBuilder::new().build());

            if let (Ok(n1_id), Ok(n2_id)) = (n1, n2) {
                let _ = tx.create_edge(n1_id, n2_id, &label_edge, PropertyMapBuilder::new().build());
            }
            let _ = tx.commit();
        }
    }
}

#[cfg(test)]
mod property_havoc {
    use crate::AletheiaDB;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use proptest::prelude::*;

    proptest! {
        // Deeply nested properties or extremely long vector embeddings
        #[test]
        fn test_extreme_properties(
            key in "\\PC*",
            vec_data in proptest::collection::vec(proptest::num::f32::ANY, 0..10_000), // Max dimensions might be configured
        ) {
            let db = AletheiaDB::new().unwrap();
            let mut tx = db.write_transaction().unwrap();

            let mut props = PropertyMapBuilder::new();
            // Just push any float array as vector
            // But AletheiaDB Vector expects max dimensions (e.g. 1536). Will this overflow buffers?
            props = props.insert_vector(&key, &vec_data);

            // Should not panic, but might return VectorError::DimensionTooLarge
            let _ = tx.create_node("Node", props.build());
            let _ = tx.commit();
        }
    }
}

#[cfg(test)]
mod metric_havoc {
    use crate::core::hlc::HybridTimestamp;
    use crate::core::temporal::time;
    use proptest::prelude::*;

    proptest! {
        // Find crashing values for time::to_iso8601
        #[test]
        fn test_to_iso8601_fuzz(
            timestamp in any::<i64>(),
            counter in any::<u16>(),
        ) {
            let ts = HybridTimestamp::new_unchecked(timestamp, counter.into());
            // This crashes for negative timestamps or very large timestamps because
            // of the naive u64 cast: std::time::Duration::new(secs as u64, nanos)
            let _ = time::to_iso8601(ts);
        }
    }
}
