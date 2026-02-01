//! persistence controller
//!
//! This module provides the `PersistenceController` struct which encapsulates
//! the logic for managing index persistence, including the background thread,
//! mutation tracking, and manual persistence operations.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use parking_lot::RwLock;

use crate::core::id::IdGenerator;
use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::utils::error::Result;

use super::PersistenceConfig;
use super::loader::IndexPersistenceManager;
use super::operations::{load_indexes_startup, persist_all_indexes};
use super::tracker::PersistenceTracker;
use super::worker::spawn_background_persistence_thread;

/// Controller for index persistence.
///
/// Encapsulates the persistence manager, tracker, and background thread.
/// Handles lifecycle management (start/stop) and delegates operations.
pub struct PersistenceController {
    manager: Arc<IndexPersistenceManager>,
    tracker: Arc<PersistenceTracker>,
    config: PersistenceConfig,
    thread_stopped: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
}

impl PersistenceController {
    /// Create a new persistence controller.
    pub fn new(config: PersistenceConfig) -> Self {
        let manager = Arc::new(IndexPersistenceManager::new(&config.data_dir));
        let tracker = Arc::new(PersistenceTracker::new());
        let thread_stopped = Arc::new(AtomicBool::new(false));

        Self {
            manager,
            tracker,
            config,
            thread_stopped,
            thread_handle: None,
        }
    }

    /// Load indexes on startup.
    pub fn load_indexes_startup(
        &self,
        current: &Arc<CurrentStorage>,
        historical: &Arc<RwLock<HistoricalStorage>>,
        node_id_gen: &Arc<Mutex<IdGenerator>>,
        edge_id_gen: &Arc<Mutex<IdGenerator>>,
        version_id_gen: &Arc<Mutex<IdGenerator>>,
    ) {
        if self.config.load_on_startup {
            load_indexes_startup(
                &self.manager,
                current,
                historical,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
            );
        }
    }

    /// Start the background persistence thread.
    pub fn start_background_thread(
        &mut self,
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
    ) {
        let handle = spawn_background_persistence_thread(
            current,
            historical,
            temporal_indexes,
            wal,
            Arc::clone(&self.manager),
            Arc::clone(&self.tracker),
            self.config.policies.clone(),
            Arc::clone(&self.thread_stopped),
        );
        self.thread_handle = Some(handle);
    }

    /// Persist all indexes manually.
    pub fn persist_indexes(
        &self,
        current: &Arc<CurrentStorage>,
        historical: &Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: &Arc<TemporalIndexes>,
        wal: &Arc<ConcurrentWalSystem>,
    ) -> Result<()> {
        // Warn if background persistence thread has stopped
        if self
            .thread_stopped
            .load(std::sync::atomic::Ordering::Acquire)
        {
            eprintln!(
                "Warning: Background persistence thread has stopped. \
                 Automatic persistence is disabled. Manual persist_indexes() calls will still work."
            );
        }

        persist_all_indexes(
            current,
            historical,
            temporal_indexes,
            wal,
            &self.manager,
            &self.tracker,
        )
    }

    /// Record graph mutations (nodes/edges added/modified/deleted).
    pub fn record_graph_mutation(&self) {
        self.tracker.record_graph_mutation();
    }

    /// Record temporal mutations (versions added).
    pub fn record_temporal_mutation(&self) {
        self.tracker.record_temporal_mutation();
    }

    /// Record string mutations (new strings interned).
    pub fn record_string_mutation(&self) {
        self.tracker.record_string_mutation();
    }

    /// Record vector mutations (vectors added/modified).
    pub fn record_vector_mutation(&self) {
        self.tracker.record_vector_mutation();
    }

    /// Signal shutdown and wait for the background thread to exit.
    pub fn shutdown(&mut self) {
        self.tracker.signal_shutdown();

        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PersistenceController {
    fn drop(&mut self) {
        self.shutdown();
    }
}
