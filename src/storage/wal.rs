//! Write-Ahead Log (WAL) implementation for crash recovery and durability.
//!
//! The WAL provides:
//! - Sequential logging of all mutations
//! - Crash recovery by replaying operations
//! - Point-in-time recovery capabilities
//! - Configurable durability modes for performance tuning
//!
//! # Architecture
//!
//! GallifreyDB uses a **Concurrent WAL with Striped Lock-Free Ring Buffers** for
//! high-throughput write operations while maintaining ACID compliance.
//!
//! ```text
//!                     ┌─────────────────────┐
//!                     │    LSN Allocator    │
//!                     │  AtomicU64::fetch_add
//!                     └──────────┬──────────┘
//!                                │
//!        ┌───────────────────────┼───────────────────────┐
//!        ▼                       ▼                       ▼
//! ┌─────────────┐         ┌─────────────┐         ┌─────────────┐
//! │   Stripe 0  │         │   Stripe 1  │         │  Stripe N   │
//! │ Ring Buffer │         │ Ring Buffer │         │ Ring Buffer │
//! │ (Lock-free) │         │ (Lock-free) │         │ (Lock-free) │
//! └──────┬──────┘         └──────┬──────┘         └──────┬──────┘
//!        └───────────────────────┼───────────────────────┘
//!                                ▼
//!                     ┌─────────────────────┐
//!                     │  Flush Coordinator  │
//!                     │  - Sorts by LSN     │
//!                     │  - Writes segment   │
//!                     └─────────────────────┘
//! ```
//!
//! # Key Design Principles
//!
//! 1. **Lock-free append path**: Multiple threads can append concurrently without mutex contention
//! 2. **Global LSN ordering**: Single atomic counter ensures total ordering of all operations
//! 3. **Sorted flush**: Entries are sorted by LSN before writing to disk
//! 4. **ACID preserved**: Synchronous and GroupCommit modes remain fully ACID compliant
//!
//! # Durability Modes
//!
//! | Mode | Latency | Throughput | ACID |
//! |------|---------|------------|------|
//! | Synchronous | ~1.5ms | ~600/sec | ✅ Full |
//! | GroupCommit | ~10-50ms | ~100K+/sec | ✅ Full |
//! | Async | <100ns | ~500K+/sec | ❌ Eventual |
//!
//! See [`DurabilityMode`] for details.
//!
//! # Usage
//!
//! ## Single Operations
//!
//! ```ignore
//! use gallifreydb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
//!
//! let config = ConcurrentWalSystemConfig::new("data/wal");
//! let wal = ConcurrentWalSystem::new(config)?;
//!
//! // Async append (returns immediately)
//! let lsn = wal.append_async(operation)?;
//!
//! // Commit with configured durability
//! wal.commit()?;
//!
//! // Shutdown gracefully
//! wal.shutdown();
//! ```
//!
//! ## Batch Operations (High-Throughput)
//!
//! For high-throughput workloads with multiple operations, use `append_batch()` for
//! significant performance improvements:
//!
//! ```ignore
//! use gallifreydb::storage::wal::{WalOperation, ConcurrentWalSystem};
//!
//! // Create multiple operations (e.g., from a transaction)
//! let operations = vec![
//!     WalOperation::CreateNode { /* ... */ },
//!     WalOperation::CreateEdge { /* ... */ },
//!     WalOperation::UpdateNode { /* ... */ },
//! ];
//!
//! // Batch append - optimizes LSN allocation and serialization
//! let lsns = wal.append_batch(operations)?;
//! assert_eq!(lsns.len(), 3);
//!
//! // For GroupCommit mode, commit and wait for durability
//! if let Some(epoch) = wal.commit()? {
//!     wal.group_commit_coordinator().unwrap().wait_for_flush(epoch)?;
//! }
//! ```
//!
//! **Performance Benefits:**
//! - Single atomic LSN allocation (vs N atomic operations)
//! - Better CPU cache locality during serialization
//! - Reduced stripe buffer contention
//! - **20-50% throughput improvement** for batch sizes > 10

// Durability mode support
pub mod durability;
pub mod group_commit;

// Concurrent WAL modules
pub mod concurrent;
pub mod concurrent_system;
pub mod flush_coordinator;
pub mod lsn_allocator;
pub mod ring_buffer;
pub mod segment_reader;
pub mod stripe;

// New modules for data structures and serialization
pub mod entry;
pub mod serialization;

// Re-export key types
pub use durability::{DurabilityMode, WriteOptions};
pub use group_commit::GroupCommitCoordinator;

// Re-export types from entry
pub use entry::{LSN, WalEntry, WalOperation};

// Re-export serialization helpers (needed by concurrent.rs via super::)
pub(crate) use serialization::{estimate_entry_capacity, serialize_entry_into};
