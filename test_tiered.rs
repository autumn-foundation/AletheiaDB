use aletheiadb::{AletheiaDB, config::AletheiaDBConfig};
use aletheiadb::config::HistoricalConfigBuilder;
use std::time::Duration;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
// Configure cold storage via the unified config builder
let config = AletheiaDBConfig::builder()
    .historical(
        HistoricalConfigBuilder::new()
            .enable_cold_storage(true)
            .cold_storage_path("data/cold.redb")
            .migration_age_threshold(Duration::from_secs(3600)) // 1 hour
            .max_hot_versions(1000)
            .build(),
    )
    .build();

// Cold storage automatically initialized!
let _db = AletheiaDB::with_unified_config(config)?;
Ok(())
}
