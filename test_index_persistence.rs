use aletheiadb::{AletheiaDB, config::AletheiaDBConfig};
use aletheiadb::storage::index_persistence::PersistenceConfig;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
// Enable index persistence for 6-30x faster startup
let config = AletheiaDBConfig::builder()
    .persistence(PersistenceConfig {
        enabled: true,
        data_dir: "data/my-database".into(),
        load_on_startup: true,  // Load indexes on startup
        use_mmap: true,         // Memory-map large indexes
        ..Default::default()
    })
    .build();

let _db = AletheiaDB::with_unified_config(config);
Ok(())
}
