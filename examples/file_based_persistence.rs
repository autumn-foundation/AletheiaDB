//! File-Based Persistence Example
//!
//! This example demonstrates the TWO main persistence systems in AletheiaDB:
//!
//! 1. **WAL** - Transaction durability (crash recovery)
//! 2. **Index Persistence** - Fast restarts (6-30x faster than WAL replay)
//!
//! Note: Cold storage (Redb) exists but requires manual setup post-initialization.
//! See docs/guides/tiered-storage-guide.md for cold storage configuration.
//!
//! # What This Example Shows
//!
//! 1. **Custom data directory** - Store database files in a specific location
//! 2. **WAL persistence** - Transaction log for durability
//! 3. **Index persistence** - Fast restarts by loading indexes from disk
//! 4. **Vector search** - Optional HNSW index with persistence
//! 5. **Restart survival** - All data persists across program restarts
//!
//! # Usage
//!
//! First run (creates database):
//! ```bash
//! cargo run --example file_based_persistence
//! ```
//!
//! Second run (loads from disk):
//! ```bash
//! cargo run --example file_based_persistence
//! # Notice: Existing database detected, loading...
//! # Notice: Much faster startup!
//! ```
//!
//! Clean up:
//! ```bash
//! rm -rf ./example-db-data
//! ```

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, AletheiaDBConfig};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Configure database with file-based persistence
    let db_path = std::env::current_dir()?.join("example-db-data");

    println!("📁 Database location: {}", db_path.display());

    // Check if database already exists
    let is_existing =
        db_path.exists() && db_path.join("wal").exists() && db_path.join("indexes").exists();

    if is_existing {
        println!("✅ Existing database detected, loading...");
    } else {
        println!("🆕 Creating new database...");
    }

    let config = AletheiaDBConfig::builder()
        // 1. WAL for transaction durability
        .wal(
            WalConfigBuilder::new()
                .wal_dir(db_path.join("wal"))
                .durability_mode(DurabilityMode::GroupCommit {
                    max_delay_ms: 10,
                    max_batch_size: 200,
                })
                .build(),
        )
        // 2. Index persistence for fast restarts
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: db_path.join("indexes"),
            load_on_startup: true, // Load existing indexes on startup
            ..Default::default()
        })
        .build();

    // 2. Initialize database (creates directories automatically)
    let db = Arc::new(AletheiaDB::with_unified_config(config)?);

    println!("✅ Database initialized");

    // 3. Enable vector search (optional)
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
        .enable()?;

    println!("✅ Vector index enabled");

    // 4. Create some data
    if !is_existing {
        println!("\n📝 Creating sample data...");

        let alice = db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .insert_vector(
                    "embedding",
                    &[0.1, 0.2, 0.3], // Dummy 3D vector (normally 384D)
                )
                .build(),
        )?;

        let bob = db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 25i64)
                .insert_vector("embedding", &[0.15, 0.25, 0.35])
                .build(),
        )?;

        db.create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )?;

        println!("  ✓ Created Alice (age 30)");
        println!("  ✓ Created Bob (age 25)");
        println!("  ✓ Created KNOWS relationship");
    }

    // 5. Query the data
    println!("\n🔍 Querying data...");

    let node_count = db.node_count();
    let edge_count = db.edge_count();

    println!("  📊 Nodes: {}", node_count);
    println!("  📊 Edges: {}", edge_count);

    if node_count > 0 {
        println!("  ✓ Data successfully persisted!");
    }

    // 6. Show file structure
    println!("\n📂 Database file structure:");
    println!("  {}/", db_path.display());
    println!("  ├── wal/                # WAL (transaction durability)");
    println!("  │   ├── 00000X.log     # WAL segments");
    println!("  │   └── manifest.json");
    println!("  └── indexes/            # Index persistence (fast restart)");
    println!("      ├── manifest.idx");
    println!("      ├── graph/");
    println!("      │   └── adjacency.idx.zst");
    println!("      ├── temporal/");
    println!("      │   └── versions.idx.zst");
    println!("      └── vector/");
    println!("          └── embedding/");
    println!("              └── current.usearch");
    println!("\n💡 Note: Cold storage (Redb) exists but requires manual setup.");
    println!("   See docs/guides/tiered-storage-guide.md for configuration.");

    println!("\n✅ Done! Data persisted to disk.");
    println!("   Run this example again to see fast loading from disk.");
    println!("\n💡 Tip: Delete ./example-db-data/ to start fresh");

    Ok(())
}
