use aletheiadb::{AletheiaDB, config::AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::storage::wal::DurabilityMode;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
// Programmatic configuration
let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new()
        .num_stripes(64).unwrap()  // High concurrency
        .durability_mode(DurabilityMode::group_commit_default())
        .build())
    .build();

let _db = AletheiaDB::with_unified_config(config)?;
Ok(())
}
