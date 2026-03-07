use aletheiadb::prelude::*;
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
use aletheiadb::index::vector::temporal::{DriftMetric, TemporalVectorConfig, SnapshotStrategy};
use aletheiadb::core::temporal::TimeRange;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
let db = AletheiaDB::new().unwrap();

// Configure vector indexing with frequent snapshots for demonstration
let mut temp_config = TemporalVectorConfig::default();
temp_config.snapshot_strategy = SnapshotStrategy::TransactionInterval(1);

db.vector_index("embedding")
    .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    .temporal(temp_config)
    .enable()?;

// 1. Create node with initial embedding
let embedding1 = vec![0.0f32; 384];
let node_id = db.create_node("Person", properties! {
    "name" => "Alice",
    "embedding" => &embedding1[..],
})?;

// 2. Update node with different embedding (simulating drift)
let mut embedding2 = vec![0.0f32; 384];
embedding2[0] = 1.0; // Changed!
db.write(|tx| {
    tx.update_node(node_id, properties! {
        "embedding" => &embedding2[..],
    })
})?;

// 3. Find drift covering the changes
let start = aletheiadb::time::from_secs(0);
let end = aletheiadb::time::now();
let time_range = TimeRange::new(start, end)?;

let drifted_nodes = db.find_drift_in(
    "embedding",
    0.1,
    time_range,
    DriftMetric::Cosine,
)?;

for (node_id, drift_score) in drifted_nodes {
    println!("Node {} drifted by {:.3}", node_id, drift_score);
}
Ok(())
}
