//! Recovery with Progress Tracking Example
//!
//! Demonstrates how to build a progress tracker for database recovery.
//!
//! This example shows:
//! - Counting WAL entries before recovery
//! - Building a custom progress tracker
//! - Simulating progress reporting (percentage, operations/sec)
//! - Displaying progress with a visual progress bar
//!
//! **Note:** This example uses internal recovery APIs and simulates progress tracking.
//! In future versions, AletheiaDB will provide a high-level API with progress callbacks:
//! ```ignore
//! let db = AletheiaDB::recover_with_progress("/data/mydb", |progress| {
//!     println!("{}% complete ({}/{})",
//!              progress.percent, progress.completed, progress.total);
//! })?;
//! ```
//! See the project roadmap for the planned high-level recovery API.
//!
//! # Running
//!
//! ```bash
//! cargo run --example recovery_progress
//! ```
//!
//! # Expected Output
//!
//! ```text
//! 📊 Recovery Progress Tracking Example
//! ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
//!
//! Phase 1: Creating large dataset
//! ─────────────────────────────────
//! ✓ Creating 10,000 operations...
//! ✓ WAL flushed to disk
//!
//! Phase 2: Recovery with progress tracking
//! ──────────────────────────────────────────
//! ✓ Scanning WAL to determine total operations...
//! ✓ Found 10,000 WAL entries to replay
//!
//! Recovery Progress:
//! [████████████████████] 100% (10000/10000)
//! ⚡ Speed: 50000 ops/sec
//! ⏱️  Time: 0.20s
//!
//! ✓ Recovery completed!
//!
//! Phase 3: Final Statistics
//! ─────────────────────────
//! Recovery Statistics:
//!   Total Operations: 10,000
//!   Time Elapsed: 0.20s
//!   Average Speed: 50,000 ops/sec
//!   Nodes Recovered: 5,000
//!   Edges Recovered: 5,000
//!
//! ✅ Recovery completed with progress tracking!
//! ```

use aletheiadb::core::error::Result;
use aletheiadb::core::{
    id::{EdgeId, NodeId},
    interning::GLOBAL_INTERNER,
    property::PropertyMapBuilder,
    temporal::time,
};
use aletheiadb::storage::{
    checkpoint::{CheckpointConfig, CheckpointManager},
    wal::{
        LSN, WalOperation,
        concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
    },
};
use std::io::Write;
use std::time::Instant;
use tempfile::TempDir;

/// Progress tracker for recovery operations
struct RecoveryProgress {
    total_operations: usize,
    completed_operations: usize,
    start_time: Instant,
    last_update_time: Instant,
    update_interval_ops: usize,
}

impl RecoveryProgress {
    fn new(total_operations: usize) -> Self {
        let now = Instant::now();
        Self {
            total_operations,
            completed_operations: 0,
            start_time: now,
            last_update_time: now,
            update_interval_ops: (total_operations / 100).max(1), // Update every 1%, minimum 1
        }
    }

    /// Update progress and optionally print status
    fn update(&mut self, operations_done: usize) {
        self.completed_operations += operations_done;

        // Print progress every N operations or at completion
        // Note: clippy suggests is_multiple_of() but it's unstable, so we use % == 0
        #[allow(clippy::manual_is_multiple_of)]
        if self.completed_operations >= self.total_operations
            || self.completed_operations % self.update_interval_ops.max(1) == 0
        {
            self.print_progress();
        }
    }

    /// Print current progress bar and statistics
    fn print_progress(&mut self) {
        let percentage = (self.completed_operations as f64 / self.total_operations as f64) * 100.0;
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let ops_per_sec = if elapsed > 0.0 {
            self.completed_operations as f64 / elapsed
        } else {
            0.0
        };

        // Create progress bar (20 characters wide)
        let bar_width = 20;
        let filled = ((percentage / 100.0) * bar_width as f64) as usize;
        // Note: clippy suggests repeat_n() but it's unstable, so we use repeat().take()
        #[allow(clippy::manual_repeat_n)]
        let bar: String = std::iter::repeat('█')
            .take(filled)
            .chain(std::iter::repeat('░').take(bar_width - filled))
            .collect();

        print!(
            "\r[{}] {:3.0}% ({}/{}) ⚡ {:.0} ops/sec ⏱️  {:.2}s",
            bar, percentage, self.completed_operations, self.total_operations, ops_per_sec, elapsed
        );

        std::io::stdout().flush().ok();

        self.last_update_time = Instant::now();
    }

    /// Print final summary
    fn finish(&self) {
        println!(); // New line after progress bar
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let ops_per_sec = if elapsed > 0.0 {
            self.completed_operations as f64 / elapsed
        } else {
            0.0
        };

        println!("\n✓ Recovery completed!");
        println!("\nRecovery Statistics:");
        println!("  Total Operations: {}", self.total_operations);
        println!("  Time Elapsed: {:.2}s", elapsed);
        println!("  Average Speed: {:.0} ops/sec", ops_per_sec);
    }
}

/// Count total WAL entries (for progress tracking)
fn count_wal_entries(wal: &ConcurrentWalSystem, start_lsn: LSN) -> Result<usize> {
    let entries = wal.read_from(start_lsn)?;
    Ok(entries.len())
}

fn main() -> Result<()> {
    println!("📊 Recovery Progress Tracking Example");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    // Create temporary directory for this example
    let temp_dir = TempDir::new()?;
    let wal_dir = temp_dir.path().join("wal");
    let checkpoint_dir = temp_dir.path().join("checkpoints");

    // ========================================================================
    // Phase 1: Create large dataset
    // ========================================================================
    println!("Phase 1: Creating large dataset");
    println!("─────────────────────────────────");

    let wal_config = ConcurrentWalSystemConfig::new(wal_dir.clone());
    let wal = ConcurrentWalSystem::new(wal_config)?;

    println!("✓ Creating 10,000 operations...");

    // Create 5,000 nodes
    for i in 1..=5000 {
        let node_id = NodeId::new(i)?;
        wal.append(WalOperation::CreateNode {
            node_id,
            label: GLOBAL_INTERNER.intern(format!("Node{}", i)).unwrap(),
            properties: PropertyMapBuilder::new().insert("id", i as i64).build(),
            valid_from: time::now(),
        })?;
    }

    // Create 5,000 edges
    for i in 1..=5000 {
        let source = NodeId::new(((i - 1) % 5000) + 1)?;
        let target = NodeId::new((i % 5000) + 1)?;
        let edge_id = EdgeId::new(i)?;

        wal.append(WalOperation::CreateEdge {
            edge_id,
            source,
            target,
            label: GLOBAL_INTERNER.intern("EDGE").unwrap(),
            properties: PropertyMapBuilder::new().insert("weight", i as i64).build(),
            valid_from: time::now(),
        })?;
    }

    // Flush WAL to ensure all entries are persisted
    wal.flush()?;
    println!("✓ WAL flushed to disk\n");

    // Drop WAL to simulate shutdown
    drop(wal);

    // ========================================================================
    // Phase 2: Recovery with progress tracking
    // ========================================================================
    println!("Phase 2: Recovery with progress tracking");
    println!("──────────────────────────────────────────");

    // Create new WAL instance for recovery
    let wal_config_recovery = ConcurrentWalSystemConfig::new(wal_dir);
    let wal_recovery = ConcurrentWalSystem::new(wal_config_recovery)?;

    // Count total operations to replay
    println!("✓ Scanning WAL to determine total operations...");
    let start_lsn = LSN::initial();
    let total_ops = count_wal_entries(&wal_recovery, start_lsn)?;
    println!("✓ Found {} WAL entries to replay\n", total_ops);

    // Create progress tracker
    let mut progress = RecoveryProgress::new(total_ops);

    println!("Recovery Progress:");

    // Create checkpoint manager
    let checkpoint_config = CheckpointConfig::with_data_dir(checkpoint_dir);
    let mut checkpoint_manager = CheckpointManager::new(checkpoint_config)?;

    // Note: The current CheckpointManager::recover() doesn't support progress callbacks,
    // so we'll simulate progress tracking by performing recovery and then showing completion.
    // In a real implementation, you would modify the recovery loop to call progress.update()
    // after each operation.

    // Simulate incremental progress updates
    // In a real implementation, this would be inside the recovery loop
    for _i in 1..=100 {
        // Simulate processing
        std::thread::sleep(std::time::Duration::from_millis(2));
        progress.update(total_ops / 100);
    }

    // Perform actual recovery (this happens very fast)
    let (current, _historical, _final_lsn) = checkpoint_manager.recover(&wal_recovery)?;

    progress.finish();

    // ========================================================================
    // Phase 3: Final Statistics
    // ========================================================================
    println!("\nPhase 3: Final Statistics");
    println!("─────────────────────────");

    println!("  Nodes Recovered: {}", current.node_count());
    println!("  Edges Recovered: {}", current.edge_count());
    println!();

    println!("✅ Recovery completed with progress tracking!");
    println!("\n💡 Note: This example demonstrates manual progress tracking.");
    println!("   In production, you could extend CheckpointManager::recover()");
    println!("   to accept a progress callback for real-time updates.");

    Ok(())
}
