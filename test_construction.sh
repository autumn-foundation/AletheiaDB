#!/bin/bash
cat > /tmp/test_temporal.rs << 'RUST'
#[cfg(test)]
mod test_temporal_construction {
    use gallifreydb::index::vector::temporal::*;
    use gallifreydb::index::vector::{HnswConfig, DistanceMetric};

    #[test]
    fn test_just_construction() {
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1000),
            retention_policy: RetentionPolicy::KeepN(100),
            max_snapshots: 100,
            hnsw_config: HnswConfig::new(4, DistanceMetric::Cosine),
        };
        let _ = TemporalVectorIndex::new(config).unwrap();
        // Don't add anything, just construct
    }
}
RUST
