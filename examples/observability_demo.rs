//! Observability Demo
//!
//! Demonstrates GallifreyDB's production observability features including:
//! - Honeycomb integration for distributed tracing
//! - Error categorization and metrics
//! - Critical error detection
//!
//! # Running with Honeycomb
//!
//! ```bash
//! export HONEYCOMB_API_KEY="your-api-key"
//! export HONEYCOMB_DATASET="gallifreydb-demo"
//! export RUST_LOG=gallifreydb=info
//! cargo run --example observability_demo --features observability-honeycomb
//! ```
//!
//! # Running with Tracy (CPU profiling)
//!
//! ```bash
//! cargo run --example observability_demo --features observability-tracy
//! ```
//!
//! # Running with basic logging
//!
//! ```bash
//! export RUST_LOG=gallifreydb=info
//! cargo run --example observability_demo --features observability
//! ```

use gallifreydb::{GallifreyDB, PropertyMapBuilder, WriteOps};

#[cfg(feature = "observability")]
use gallifreydb::observability;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize observability (only compiled with 'observability' feature)
    #[cfg(feature = "observability")]
    {
        let config = observability::Config::from_env();
        observability::init(config.clone());
        println!("📊 Observability initialized");

        #[cfg(feature = "observability-honeycomb")]
        {
            if config.honeycomb.is_some() {
                println!("🐝 Honeycomb integration active");
            } else {
                println!("⚠️  HONEYCOMB_API_KEY not set - falling back to stdout logging");
            }
        }

        #[cfg(feature = "observability-prometheus")]
        {
            if let Some(ref prom_config) = config.prometheus {
                println!(
                    "📈 Prometheus metrics available at http://{}/metrics",
                    prom_config.bind_addr
                );
            }
        }
    }

    #[cfg(not(feature = "observability"))]
    println!("ℹ️  Running without observability (zero overhead mode)");

    println!("\n🚀 Creating GallifreyDB instance...");
    let db = GallifreyDB::new()?;

    println!("📝 Creating sample nodes and edges...");

    // Create some nodes
    let alice_id = db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30)
            .build(),
    )?;

    let bob_id = db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("age", 25)
            .build(),
    )?;

    // Create edge
    let edge_id = db.create_edge(
        alice_id,
        bob_id,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2020).build(),
    )?;

    println!(
        "✓ Created nodes: Alice ({:?}), Bob ({:?})",
        alice_id, bob_id
    );
    println!("✓ Created edge: KNOWS ({:?})", edge_id);

    // Demonstrate transactions (these are instrumented)
    println!("\n🔄 Running transaction workload...");
    for i in 0..5 {
        db.write(|tx| {
            let node_id = tx.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", format!("Doc {}", i))
                    .insert("version", i)
                    .build(),
            )?;
            println!("  Transaction {}: Created document {:?}", i, node_id);
            Ok(())
        })?;
    }

    // Check metrics (only available with observability feature)
    #[cfg(feature = "observability")]
    {
        println!("\n📈 Checking metrics...");
        let metrics = observability::metrics();

        println!("  Lock poison count: {}", metrics.lock_poison_count);
        println!("  Timestamp violations: {}", metrics.timestamp_violations);
        println!("  WAL checksum failures: {}", metrics.wal_checksum_failures);
        println!("  Write conflicts: {}", metrics.write_conflicts);

        println!("\n📊 Error categorization:");
        println!("  Storage errors: {}", metrics.error_storage_total);
        println!("  Temporal errors: {}", metrics.error_temporal_total);
        println!("  Query errors: {}", metrics.error_query_total);
        println!("  Transaction errors: {}", metrics.error_transaction_total);
        println!("  Vector errors: {}", metrics.error_vector_total);
        println!("  I/O errors: {}", metrics.error_io_total);
        println!("  Other errors: {}", metrics.error_other_total);

        if metrics.has_critical_errors() {
            println!("\n❌ CRITICAL: Data corruption detected!");
            println!("   This should NEVER happen in production.");
            return Err("Critical errors detected".into());
        } else {
            println!("\n✅ No critical errors detected - database is healthy");
        }
    }

    println!("\n🎉 Demo complete!");

    #[cfg(feature = "observability-honeycomb")]
    println!("\n💡 Check your Honeycomb dashboard to see traces and metrics!");

    Ok(())
}
