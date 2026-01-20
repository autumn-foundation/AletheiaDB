//! Recovery test suite - unified entry point for all recovery tests
//!
//! This module includes all recovery-related integration tests organized
//! in the tests/recovery/ subdirectory as required by Issue #293.

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
