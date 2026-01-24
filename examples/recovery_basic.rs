//! Basic Recovery Example
//!
//! Demonstrates automatic database recovery when reopening after a crash.
//!
//! This example shows:
//! - Creating a database and writing data
//! - Simulating a crash (database instance dropped)
//! - Automatic recovery on next startup
//! - Verification that all data is preserved
//!
//! # Running
//!
//! ```bash
//! cargo run --example recovery_basic
//! ```
//!
//! # Expected Output
//!
//! ```text
//! 🚀 Basic Recovery Example
//! ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
//!
//! Phase 1: Initial Database Operations
//! ─────────────────────────────────────
//! ✓ Created database in temporary directory
//! ✓ Created 50 nodes
//! ✓ Created 49 edges (chain: 1→2→3→...→50)
//! ✓ Updated node 1 properties
//! ✓ Deleted node 25
//! ✓ Flushed WAL to disk
//!
//! 💥 Simulating crash (dropping database)...
//!
//! Phase 2: Recovery on Restart
//! ─────────────────────────────
//! ✓ Recovering database from WAL...
//! ✓ Recovery complete!
//! ✓ Recovered LSN: 103
//! ✓ Recovery statistics:
//!   - Nodes recovered: 49 (1 deleted)
//!   - Edges recovered: 49
//!   - Operations replayed: 100+
//!
//! Phase 3: Data Verification
//! ─────────────────────────────
//! ✓ Node 1 exists with updated properties
//! ✓ Node 25 correctly deleted
//! ✓ All other nodes exist (2-24, 26-50)
//! ✓ All edges intact (1→2, 2→3, ...)
//!
//! ✅ SUCCESS: All data recovered correctly!
//! ```

use gallifreydb::core::{
    id::{EdgeId, NodeId},
    property::{PropertyMapBuilder, PropertyValue},
    temporal::{BiTemporalInterval, time},
};
use gallifreydb::storage::{
    persistence::{CheckpointConfig, PersistenceManager},
    wal::{
        WalOperation,
        concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
    },
};
use gallifreydb::utils::error::Result;
use tempfile::TempDir;

fn main() -> Result<()> {
    println!("🚀 Basic Recovery Example");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    // Create temporary directory for this example
    let temp_dir = TempDir::new()?;
    let wal_dir = temp_dir.path().join("wal");
    let checkpoint_dir = temp_dir.path().join("checkpoints");

    // ========================================================================
    // Phase 1: Initial Database Operations
    // ========================================================================
    println!("Phase 1: Initial Database Operations");
    println!("─────────────────────────────────────");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    println!("✓ Created database in temporary directory");

    // Create 50 nodes
    for i in 1..=50 {
        let node_id = NodeId::new(i)?;
        wal.append(WalOperation::CreateNode {
            node_id,
            label: format!("Person{}", i),
            properties: PropertyMapBuilder::new()
                .insert("name", format!("Alice{}", i))
                .insert("age", (20 + i) as i64)
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    println!("✓ Created 50 nodes");

    // Create edges forming a chain: 1→2, 2→3, ..., 49→50
    for i in 1..=49 {
        let edge_id = EdgeId::new(i)?;
        let source = NodeId::new(i)?;
        let target = NodeId::new(i + 1)?;
        wal.append(WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label: "KNOWS".to_string(),
            properties: PropertyMapBuilder::new()
                .insert("since", 2020 + (i as i64))
                .build(),
            temporal: BiTemporalInterval::current(time::now()),
        })?;
    }
    println!("✓ Created 49 edges (chain: 1→2→3→...→50)");

    // Update node 1
    let version_id_51 = gallifreydb::core::id::VersionId::new(51)?;
    let node_id_1 = NodeId::new(1)?;
    wal.append(WalOperation::UpdateNode {
        node_id: node_id_1,
        version_id: version_id_51,
        label: "Person1Updated".to_string(),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice1 (updated)")
            .insert("age", 21)
            .insert("updated", true)
            .build(),
        temporal: BiTemporalInterval::current(time::now()),
    })?;
    println!("✓ Updated node 1 properties");

    // Delete node 25
    let node_id_25 = NodeId::new(25)?;
    wal.append(WalOperation::DeleteNode {
        node_id: node_id_25,
        temporal: BiTemporalInterval::current(time::now()),
    })?;
    println!("✓ Deleted node 25");

    // Flush WAL to ensure all entries are persisted
    wal.flush()?;
    println!("✓ Flushed WAL to disk\n");

    // Simulate crash by dropping the WAL instance
    println!("💥 Simulating crash (dropping database)...\n");
    drop(wal);

    // ========================================================================
    // Phase 2: Recovery on Restart
    // ========================================================================
    println!("Phase 2: Recovery on Restart");
    println!("─────────────────────────────");

    // Create new WAL and PersistenceManager instances
    let wal_config_recovery = ConcurrentWalSystemConfig::new(wal_dir);
    let wal_recovery = ConcurrentWalSystem::new(wal_config_recovery)?;

    let checkpoint_config = CheckpointConfig {
        checkpoint_dir,
        ..Default::default()
    };
    let mut persistence_manager = PersistenceManager::new(checkpoint_config)?;

    println!("✓ Recovering database from WAL...");

    // Perform recovery
    let (current, historical, final_lsn) = persistence_manager.recover(&wal_recovery)?;

    println!("✓ Recovery complete!");
    println!("✓ Recovered LSN: {}", final_lsn.0);
    println!("✓ Recovery statistics:");
    println!("  - Nodes recovered: {} (1 deleted)", current.node_count());
    println!("  - Edges recovered: {}", current.edge_count());
    println!(
        "  - Historical versions: {}",
        historical.stats().total_node_versions
    );
    println!();

    // ========================================================================
    // Phase 3: Data Verification
    // ========================================================================
    println!("Phase 3: Data Verification");
    println!("─────────────────────────────");

    // Verify node 1 was updated
    let node_1 = current.get_node(NodeId::new(1)?)?;
    assert!(node_1.has_label_str("Person1Updated"));
    assert!(matches!(
        node_1.properties.get("name"),
        Some(PropertyValue::String(s)) if s.as_ref() == "Alice1 (updated)"
    ));
    assert!(matches!(
        node_1.properties.get("updated"),
        Some(PropertyValue::Bool(true))
    ));
    println!("✓ Node 1 exists with updated properties");

    // Verify node 25 was deleted
    assert!(current.get_node(NodeId::new(25)?).is_err());
    println!("✓ Node 25 correctly deleted");

    // Verify other nodes exist
    for i in 2..=50 {
        if i == 25 {
            continue; // Skip deleted node
        }
        let node = current.get_node(NodeId::new(i)?)?;
        assert!(node.has_label_str(&format!("Person{}", i)));
    }
    println!("✓ All other nodes exist (2-24, 26-50)");

    // Verify edges
    for i in 1..=49 {
        let edge = current.get_edge(EdgeId::new(i)?)?;
        assert_eq!(edge.source, NodeId::new(i)?);
        assert_eq!(edge.target, NodeId::new(i + 1)?);
        assert!(edge.has_label_str("KNOWS"));
    }
    println!("✓ All edges intact (1→2, 2→3, ...)\n");

    println!("✅ SUCCESS: All data recovered correctly!");

    Ok(())
}
