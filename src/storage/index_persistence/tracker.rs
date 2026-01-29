//! Persistence mutation tracking.
//!
//! This module provides the `PersistenceTracker` which monitors database mutations
//! (vector, graph, temporal) to determine when indexes should be persisted to disk.
//! It serves as the input signal for the background persistence worker.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Tracks persistence state for automatic index persistence.
///
/// This struct maintains atomic mutation counters and last persist timestamps for each
/// index type (Vector, Graph, Temporal, Strings). It is shared between the main database
/// operations (which increment counters) and the background persistence thread (which
/// reads counters and resets them upon successful persistence).
///
/// # Concurrency
///
/// All fields are atomic, allowing lock-free tracking of high-frequency operations.
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
            shutdown: AtomicBool::new(false),
        }
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
