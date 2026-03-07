// ⚠️ REQUIRES FEATURE: sharding-rpc
// [dependencies]
// aletheiadb = { version = "0.1", features = ["sharding-rpc"] }

use aletheiadb::storage::sharding::{
    ShardConfig, ShardDefinition, ShardCoordinator,
};

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
// Define shard topology
let config = ShardConfig::new(vec![
    ShardDefinition::new(0, "shard0:9000", vec!["Person", "User"]),
    ShardDefinition::new(1, "shard1:9000", vec!["Place", "Location"]),
    ShardDefinition::new(2, "shard2:9000", vec!["Event", "Activity"]),
]);

// Create coordinator
let coordinator = ShardCoordinator::new(config);

// Route queries to appropriate shards
let _shard = coordinator.router().route_node("Person");
Ok(())
}
