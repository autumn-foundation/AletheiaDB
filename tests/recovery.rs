//! Recovery test suite - unified entry point for all recovery tests
//!
//! This module includes all recovery-related integration tests organized
//! in the tests/recovery/ subdirectory as required by Issue #293.
//!
//! ## Test Organization
//!
//! The recovery tests are organized by operation type, with each module
//! testing a specific aspect of WAL replay during database recovery:
//!
//! - **`replay_create_tests`** - CreateNode/CreateEdge replay operations (Issue #288)
//!   - Basic node/edge creation
//!   - Property preservation (including vector embeddings)
//!   - Temporal interval preservation
//!   - ID tracking during replay
//!
//! - **`replay_update_tests`** - UpdateNode/UpdateEdge replay operations (Issue #289)
//!   - Node/edge updates with property changes
//!   - Label changes
//!   - Version chain creation
//!   - Vector embedding updates
//!
//! - **`replay_delete_tests`** - DeleteNode/DeleteEdge replay operations (Issue #290)
//!   - Node/edge deletion
//!   - Tombstone creation
//!   - Time-travel query support after deletion
//!   - Mixed operation sequences
//!
//! - **`replay_id_tracking_tests`** - ID generator recovery (Issue #291)
//!   - Node ID generator initialization
//!   - Edge ID generator initialization
//!   - Version ID generator initialization
//!   - Handling gaps in ID sequences
//!   - Independent generator state
//!
//! - **`replay_loop_tests`** - WAL replay loop basics (Issue #287)
//!   - Empty WAL handling
//!   - Multiple operation types
//!   - Checkpoint markers
//!   - LSN tracking
//!   - Max ID tracking across operation types
//!
//! - **`large_dataset_recovery`** - Large dataset recovery tests (Issue #294)
//!   - 10,000 nodes and 50,000 edges recovery
//!   - Recovery time benchmarks (target: <10 seconds)
//!   - Throughput measurements
//!
//! - **`checkpoint_recovery_tests`** - CheckpointManager with index persistence
//!   - Full state persistence (graph + temporal indexes)
//!   - Recovery from persisted state + WAL replay
//!   - LSN consistency across checkpoint and recovery
//!   - ID generator initialization from persisted state
//!
//! - **`property_based_tests`** - Property-based recovery invariant tests (Issue #295)
//!   - Temporal consistency (monotonic timestamps, valid time ranges)
//!   - Version chain integrity (unbroken sequences, no gaps)
//!   - Referential integrity (edge references valid nodes)
//!   - ID uniqueness (no duplicate IDs after recovery)
//!   - 1000+ random operation sequences per test
//!
//! ## Running Tests
//!
//! ```bash
//! # Run all recovery tests
//! cargo test --test recovery
//!
//! # Run specific test module
//! cargo test --test recovery replay_create_tests
//!
//! # Run individual test
//! cargo test --test recovery test_replay_create_node_basic
//!
//! # Run large dataset tests (ignored by default)
//! cargo test --test recovery large_dataset -- --ignored
//!
//! # Run with output
//! cargo test --test recovery -- --nocapture
//! ```
//!
//! ## Test Coverage
//!
//! These tests ensure comprehensive coverage of WAL replay operations:
//! - ✅ All operation types (Create/Update/Delete for Nodes/Edges)
//! - ✅ ID generator recovery and conflict prevention
//! - ✅ Temporal semantics preservation
//! - ✅ Vector index recovery (Issue #292)
//! - ✅ Property handling (including vectors, strings, numbers, booleans)
//! - ✅ Error handling and edge cases
//! - ✅ Large dataset recovery performance (Issue #294)

#[path = "recovery/replay_create_tests.rs"]
mod replay_create_tests;

#[path = "recovery/replay_update_tests.rs"]
mod replay_update_tests;

#[path = "recovery/replay_delete_tests.rs"]
mod replay_delete_tests;

#[path = "recovery/replay_id_tracking_tests.rs"]
mod replay_id_tracking_tests;

#[path = "recovery/replay_loop_tests.rs"]
mod replay_loop_tests;

#[path = "recovery/large_dataset_recovery.rs"]
mod large_dataset_recovery;

#[path = "recovery/checkpoint_recovery_tests.rs"]
mod checkpoint_recovery_tests;

#[path = "recovery/property_based_tests.rs"]
mod property_based_tests;
