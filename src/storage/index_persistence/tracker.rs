//! Persistence mutation tracker.
//!
//! This module provides a thread-safe tracker for index mutations and persistence timestamps.
//! It serves as the shared state synchronization point between the main application thread
//! (which records mutations) and the background persistence worker (which checks counters
//! and resets them after saving).
//!
//! # Purpose
//!
//! The `PersistenceTracker` solves the problem of "when should we save indexes?":
//! - Tracks how many changes have occurred since the last save.
//! - Tracks how much time has passed since the last save.
//! - Allows the background worker to atomically check and reset these counters.
//!
//! # Thread Safety
//!
//! All internal counters are atomic (`AtomicU64`, `AtomicBool`), allowing concurrent access
//! without locking. The main thread can increment counters while the background thread
//! reads or resets them.
//!
//! # Usage
//!
//! 1. **Main Thread**: Calls `record_*_mutation()` when modifying data.
//! 2. **Worker Thread**: Calls `get_*_mutations()` and `seconds_since_*_persist()` to check policies.
//! 3. **Worker Thread**: Calls `reset_*_mutations()` after successful persistence.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Tracks persistence state for automatic index persistence.
///
/// This struct maintains mutation counters and last persist timestamps for each index type,
/// enabling policy-based automatic persistence triggers.
#[derive(Debug)]
pub(crate) struct PersistenceTracker {
    /// Vector index mutation counter (total across all vector properties)
    vector_mutations: AtomicU64,
    /// Graph index mutation counter
    graph_mutations: AtomicU64,
    /// Temporal index mutation counter (new versions)
    temporal_mutations: AtomicU64,
    /// String interner mutation counter (new strings)
    string_mutations: AtomicU64,
    /// Last persist timestamp for vector indexes (unix timestamp)
    last_vector_persist: AtomicU64,
    /// Last persist timestamp for graph index
    last_graph_persist: AtomicU64,
    /// Last persist timestamp for temporal index
    last_temporal_persist: AtomicU64,
    /// Last persist timestamp for string interner
    last_string_persist: AtomicU64,

    /// Last persisted LSN for vector indexes
    last_vector_lsn: AtomicU64,
    /// Last persisted LSN for graph index
    last_graph_lsn: AtomicU64,
    /// Last persisted LSN for temporal index
    last_temporal_lsn: AtomicU64,
    /// Last persisted LSN for string interner
    last_string_lsn: AtomicU64,

    /// Shutdown signal for background persistence thread
    shutdown: AtomicBool,
}

impl PersistenceTracker {
    /// Create a new persistence tracker with all counters at zero.
    pub fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();

        Self {
            vector_mutations: AtomicU64::new(0),
            graph_mutations: AtomicU64::new(0),
            temporal_mutations: AtomicU64::new(0),
            string_mutations: AtomicU64::new(0),
            last_vector_persist: AtomicU64::new(now),
            last_graph_persist: AtomicU64::new(now),
            last_temporal_persist: AtomicU64::new(now),
            last_string_persist: AtomicU64::new(now),
            last_vector_lsn: AtomicU64::new(0),
            last_graph_lsn: AtomicU64::new(0),
            last_temporal_lsn: AtomicU64::new(0),
            last_string_lsn: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
        }
    }

    /// Set the starting LSN for all components (used during initialization/recovery).
    pub fn set_start_lsn(&self, lsn: u64) {
        self.last_vector_lsn.store(lsn, Ordering::Relaxed);
        self.last_graph_lsn.store(lsn, Ordering::Relaxed);
        self.last_temporal_lsn.store(lsn, Ordering::Relaxed);
        self.last_string_lsn.store(lsn, Ordering::Relaxed);
    }

    /// Update the last persisted LSN for vector indexes.
    pub fn update_vector_lsn(&self, lsn: u64) {
        self.last_vector_lsn.fetch_max(lsn, Ordering::Relaxed);
    }

    /// Update the last persisted LSN for graph index.
    pub fn update_graph_lsn(&self, lsn: u64) {
        self.last_graph_lsn.fetch_max(lsn, Ordering::Relaxed);
    }

    /// Update the last persisted LSN for temporal index.
    pub fn update_temporal_lsn(&self, lsn: u64) {
        self.last_temporal_lsn.fetch_max(lsn, Ordering::Relaxed);
    }

    /// Update the last persisted LSN for string interner.
    pub fn update_string_lsn(&self, lsn: u64) {
        self.last_string_lsn.fetch_max(lsn, Ordering::Relaxed);
    }

    /// Get the safe manifest LSN (minimum of all component LSNs).
    ///
    /// This LSN represents the point in time up to which ALL persisted components are consistent.
    /// WAL replay should start from this LSN to ensure no operations are missed for components
    /// that might be lagging behind.
    pub fn get_safe_manifest_lsn(&self) -> u64 {
        let vector = self.last_vector_lsn.load(Ordering::Relaxed);
        let graph = self.last_graph_lsn.load(Ordering::Relaxed);
        let temporal = self.last_temporal_lsn.load(Ordering::Relaxed);
        let string = self.last_string_lsn.load(Ordering::Relaxed);

        // Calculate minimum of all components
        vector.min(graph).min(temporal).min(string)
    }

    /// Increment vector mutation counter.
    pub fn record_vector_mutation(&self) {
        self.vector_mutations.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment graph mutation counter.
    pub fn record_graph_mutation(&self) {
        self.graph_mutations.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment temporal mutation counter.
    pub fn record_temporal_mutation(&self) {
        self.temporal_mutations.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment string mutation counter.
    pub fn record_string_mutation(&self) {
        self.string_mutations.fetch_add(1, Ordering::Relaxed);
    }

    /// Get and reset vector mutation counter, updating last persist time.
    pub fn reset_vector_mutations(&self) -> u64 {
        let count = self.vector_mutations.swap(0, Ordering::Relaxed);
        self.last_vector_persist.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(std::time::Duration::from_secs(0))
                .as_secs(),
            Ordering::Relaxed,
        );
        count
    }

    /// Get and reset graph mutation counter, updating last persist time.
    pub fn reset_graph_mutations(&self) -> u64 {
        let count = self.graph_mutations.swap(0, Ordering::Relaxed);
        self.last_graph_persist.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(std::time::Duration::from_secs(0))
                .as_secs(),
            Ordering::Relaxed,
        );
        count
    }

    /// Get and reset temporal mutation counter, updating last persist time.
    pub fn reset_temporal_mutations(&self) -> u64 {
        let count = self.temporal_mutations.swap(0, Ordering::Relaxed);
        self.last_temporal_persist.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(std::time::Duration::from_secs(0))
                .as_secs(),
            Ordering::Relaxed,
        );
        count
    }

    /// Get and reset string mutation counter, updating last persist time.
    pub fn reset_string_mutations(&self) -> u64 {
        let count = self.string_mutations.swap(0, Ordering::Relaxed);
        self.last_string_persist.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(std::time::Duration::from_secs(0))
                .as_secs(),
            Ordering::Relaxed,
        );
        count
    }

    /// Get current vector mutation count without resetting.
    pub fn get_vector_mutations(&self) -> u64 {
        self.vector_mutations.load(Ordering::Relaxed)
    }

    /// Get current graph mutation count without resetting.
    pub fn get_graph_mutations(&self) -> u64 {
        self.graph_mutations.load(Ordering::Relaxed)
    }

    /// Get current temporal mutation count without resetting.
    pub fn get_temporal_mutations(&self) -> u64 {
        self.temporal_mutations.load(Ordering::Relaxed)
    }

    /// Get current string mutation count without resetting.
    pub fn get_string_mutations(&self) -> u64 {
        self.string_mutations.load(Ordering::Relaxed)
    }

    /// Get seconds since last vector persist.
    pub fn seconds_since_vector_persist(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();
        let last = self.last_vector_persist.load(Ordering::Relaxed);
        now.saturating_sub(last)
    }

    /// Get seconds since last graph persist.
    pub fn seconds_since_graph_persist(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();
        let last = self.last_graph_persist.load(Ordering::Relaxed);
        now.saturating_sub(last)
    }

    /// Get seconds since last temporal persist.
    pub fn seconds_since_temporal_persist(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();
        let last = self.last_temporal_persist.load(Ordering::Relaxed);
        now.saturating_sub(last)
    }

    /// Get seconds since last string persist.
    pub fn seconds_since_string_persist(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs();
        let last = self.last_string_persist.load(Ordering::Relaxed);
        now.saturating_sub(last)
    }

    /// Signal shutdown to background thread.
    pub fn signal_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Check if shutdown has been signaled.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
}

#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tests;
