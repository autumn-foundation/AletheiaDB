//! Unified Concurrent WAL System.
//!
//! This module provides [`ConcurrentWalSystem`], which combines the concurrent
//! WAL striped architecture with the flush coordinator into a single, cohesive
//! component that can be used as a drop-in replacement for the old `WriteAheadLog`.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    ConcurrentWalSystem                           │
//! │                                                                  │
//! │  ┌──────────────────────┐    ┌─────────────────────────────┐   │
//! │  │    ConcurrentWal     │    │     FlushCoordinator        │   │
//! │  │  (Striped Buffers)   │───▶│   (Segment Management)      │   │
//! │  └──────────────────────┘    └─────────────────────────────┘   │
//! │                                                                  │
//! │  ┌──────────────────────────────────────────────────────────┐  │
//! │  │                   Background Flush Thread                  │  │
//! │  │  - Drains stripes periodically                            │  │
//! │  │  - Writes to segment files                                │  │
//! │  │  - Notifies completion handles                            │  │
//! │  └──────────────────────────────────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
//!
//! let config = ConcurrentWalSystemConfig::new("data/wal");
//! let wal = ConcurrentWalSystem::new(config)?;
//!
//! // Async append (returns immediately)
//! let lsn = wal.append_async(operation)?;
//!
//! // Sync append (waits for durability)
//! let lsn = wal.append_sync(operation)?;
//!
//! // Shutdown gracefully
//! wal.shutdown();
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use arc_swap::ArcSwapOption;

use super::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use super::flush_coordinator::{FlushCoordinator, FlushCoordinatorConfig, FlushStats};
use super::group_commit::GroupCommitCoordinator;
use super::{LSN, WalOperation};
use crate::core::error::{Error, Result, StorageError};
use crate::storage::wal::DurabilityMode;

/// Configuration for the concurrent WAL system.
#[derive(Clone)]
pub struct ConcurrentWalSystemConfig {
    /// WAL directory path.
    pub wal_dir: PathBuf,
    /// Number of stripes (should be power of 2).
    pub num_stripes: usize,
    /// Ring buffer capacity per stripe.
    pub stripe_capacity: usize,
    /// Maximum segment size in bytes before rotation.
    pub segment_size: usize,
    /// Number of segments to retain.
    pub segments_to_retain: usize,
    /// Flush interval in milliseconds.
    pub flush_interval_ms: u64,
    /// Durability mode.
    pub durability_mode: DurabilityMode,
    /// Write buffer size for segment files.
    pub write_buffer_size: usize,
    /// Optional cipher for WAL entry encryption.
    ///
    /// When set, entries are encrypted before writing to disk and segments
    /// use version 2 format. Passed through to `FlushCoordinatorConfig`.
    pub wal_cipher: Option<Arc<dyn crate::encryption::cipher::Cipher>>,
    /// Optional provisioned WAL key version (Issue #488 version-provisioning).
    ///
    /// When set alongside `wal_cipher`, the WAL keyring is built at this version
    /// (via [`WalKeyring::single_versioned`]) instead of the hard-coded
    /// [`INITIAL_WAL_KEY_VERSION`], so a rotate-then-reopen stamps and reports
    /// the real on-disk version. `None` (the default) reproduces prior behavior
    /// exactly. Ignored when `wal_cipher` is `None` (a plaintext WAL has no
    /// keyring).
    pub wal_key_version: Option<u32>,
    /// Recovery policy for a crash-torn trailing entry (Issue #3433).
    ///
    /// When `true` (the default), [`ConcurrentWalSystem::read_from`] tolerates
    /// an undecodable trailing entry in the final WAL segment during replay;
    /// when `false`, any parse failure hard-errors (fail-stop recovery). See
    /// [`crate::config::WalConfig::tolerate_torn_tail`].
    pub tolerate_torn_tail: bool,
}

impl std::fmt::Debug for ConcurrentWalSystemConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConcurrentWalSystemConfig")
            .field("wal_dir", &self.wal_dir)
            .field("num_stripes", &self.num_stripes)
            .field("stripe_capacity", &self.stripe_capacity)
            .field("segment_size", &self.segment_size)
            .field("segments_to_retain", &self.segments_to_retain)
            .field("flush_interval_ms", &self.flush_interval_ms)
            .field("durability_mode", &self.durability_mode)
            .field("write_buffer_size", &self.write_buffer_size)
            .field(
                "wal_cipher",
                &self.wal_cipher.as_ref().map(|c| c.algorithm_name()),
            )
            .field("wal_key_version", &self.wal_key_version)
            .field("tolerate_torn_tail", &self.tolerate_torn_tail)
            .finish()
    }
}

impl Default for ConcurrentWalSystemConfig {
    fn default() -> Self {
        Self {
            wal_dir: PathBuf::from("data/wal"),
            num_stripes: 16,
            stripe_capacity: 1024,
            segment_size: 64 * 1024 * 1024, // 64 MB
            segments_to_retain: 10,
            flush_interval_ms: 10,
            durability_mode: DurabilityMode::Synchronous,
            write_buffer_size: 64 * 1024, // 64 KB
            wal_cipher: None,
            wal_key_version: None,
            tolerate_torn_tail: true,
        }
    }
}

impl ConcurrentWalSystemConfig {
    /// Create a new config with the specified WAL directory.
    pub fn new(wal_dir: impl Into<PathBuf>) -> Self {
        Self {
            wal_dir: wal_dir.into(),
            ..Default::default()
        }
    }

    /// Set the durability mode.
    pub fn with_durability_mode(mut self, mode: DurabilityMode) -> Self {
        self.durability_mode = mode;
        self
    }

    /// Set the number of stripes.
    pub fn with_num_stripes(mut self, num_stripes: usize) -> Self {
        self.num_stripes = num_stripes.next_power_of_two();
        self
    }

    /// Set the flush interval in milliseconds.
    pub fn with_flush_interval_ms(mut self, ms: u64) -> Self {
        self.flush_interval_ms = ms;
        self
    }
}

/// Signal for waking up the flush thread when batch is full.
struct FlushNotifier {
    /// Lock for condvar.
    lock: Mutex<bool>,
    /// Condvar to signal immediate flush.
    condvar: Condvar,
}

impl FlushNotifier {
    fn new() -> Self {
        Self {
            lock: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    /// Signal the flush thread to wake up immediately.
    fn notify(&self) {
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *guard = true;
        self.condvar.notify_one();
    }

    /// Wait for a signal or timeout, returns true if signaled.
    fn wait_timeout(&self, duration: Duration) -> bool {
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());

        // Check if already signaled before waiting.
        // This handles the race where notify() is called before we enter wait_timeout().
        if *guard {
            *guard = false; // Reset signal
            return true;
        }

        let (new_guard, result) = self
            .condvar
            .wait_timeout(guard, duration)
            .unwrap_or_else(|e| e.into_inner());
        guard = new_guard;

        let was_signaled = *guard && !result.timed_out();
        *guard = false; // Reset signal
        was_signaled
    }
}

/// Threshold for consecutive flush errors before logging a critical warning.
const FLUSH_ERROR_WARNING_THRESHOLD: u64 = 3;

/// Helper struct to encapsulate background flush logic.
struct BackgroundFlusher {
    wal: Arc<ConcurrentWal>,
    coordinator: Arc<FlushCoordinator>,
    shutdown: Arc<AtomicBool>,
    flush_notifier: Arc<FlushNotifier>,
    group_commit: Option<Arc<GroupCommitCoordinator>>,
    error_counter: Arc<AtomicU64>,
    interval: Duration,
    sync_on_flush: bool,
}

impl BackgroundFlusher {
    fn run(&self) {
        while !self.shutdown.load(Ordering::Relaxed) {
            self.perform_flush_cycle();
            // Wait for flush interval OR immediate signal (batch full)
            self.flush_notifier.wait_timeout(self.interval);
        }
        self.perform_final_flush();
    }

    fn perform_flush_cycle(&self) {
        let entries = self.wal.drain_all();

        // Always try to advance the epoch when there are entries OR when
        // group commit has pending transactions.
        //
        // LOCK POISONING: If current_batch_size() fails, the coordinator lock is
        // poisoned and the system is in an unrecoverable state. Panicking is correct
        // here - continuing would leave waiting transactions hanging indefinitely.
        let should_mark_flushed = !entries.is_empty()
            || self.group_commit.as_ref().is_some_and(|gc| {
                gc.current_batch_size()
                    .expect("GroupCommitCoordinator lock poisoned - flush thread cannot continue")
                    > 0
            });

        if !entries.is_empty() {
            // Flush to coordinator
            let result = self.coordinator.flush(entries, self.sync_on_flush);
            self.handle_flush_result(result.map(|_| ()));
        } else if should_mark_flushed {
            // No entries but there are pending transactions - advance epoch anyway
            self.handle_flush_result(Ok(()));
        }
    }

    fn perform_final_flush(&self) {
        let entries = self.wal.drain_all();
        if !entries.is_empty() {
            let result = self.coordinator.flush(entries, true);
            self.handle_flush_result(result.map(|_| ()));
        }
    }

    fn handle_flush_result(&self, result: Result<()>) {
        match result {
            Ok(_) => {
                // Reset error counter on success
                self.error_counter.store(0, Ordering::Relaxed);
                if let Some(ref gc) = self.group_commit {
                    gc.mark_flushed(Ok(())).expect(
                        "GroupCommitCoordinator lock poisoned - flush thread cannot continue",
                    );
                }
            }
            Err(e) => {
                // Track consecutive errors for health monitoring
                let errors = self.error_counter.fetch_add(1, Ordering::Relaxed) + 1;
                if errors == FLUSH_ERROR_WARNING_THRESHOLD {
                    eprintln!(
                        "CRITICAL: WAL flush failed {} consecutive times. \
                         Data durability may be compromised. Last error: {}",
                        errors, e
                    );
                } else {
                    eprintln!("WAL flush error: {}", e);
                }

                if let Some(ref gc) = self.group_commit {
                    // Create a new error from the string representation
                    gc.mark_flushed(Err(crate::core::error::Error::other(e.to_string())))
                        .expect(
                            "GroupCommitCoordinator lock poisoned - flush thread cannot continue",
                        );
                }
            }
        }
    }
}

/// Unified concurrent WAL system.
///
/// This combines the striped concurrent WAL with the flush coordinator
/// and a background flush thread to provide a complete WAL solution.
pub struct ConcurrentWalSystem {
    /// The concurrent WAL with striped buffers.
    wal: Arc<ConcurrentWal>,
    /// The flush coordinator for segment management.
    coordinator: Arc<FlushCoordinator>,
    /// Handle to the background flush thread.
    flush_thread: Option<JoinHandle<()>>,
    /// Signal to stop the flush thread.
    shutdown_signal: Arc<AtomicBool>,
    /// Signal to wake up flush thread immediately (batch full).
    flush_notifier: Arc<FlushNotifier>,
    /// Durability mode.
    durability_mode: DurabilityMode,
    /// Group commit coordinator for epoch-based waiting (GroupCommit mode only).
    group_commit: Option<Arc<GroupCommitCoordinator>>,
    /// Counter for consecutive flush errors (for health monitoring).
    consecutive_flush_errors: Arc<AtomicU64>,
    /// Crash-torn-tail recovery policy applied by [`Self::read_from`]
    /// (Issue #3433).
    tolerate_torn_tail: bool,
    /// Optional WAL DEK keyring for encryption at rest, held in a runtime-swappable
    /// presence cell (Issues #3617, #3616). Retained so [`Self::read_from`] can
    /// decrypt encrypted segments during recovery replay — dispatching per segment
    /// on its `key_version` so a mixed old-DEK/new-DEK directory (an in-flight
    /// full-MEK rotation) replays correctly.
    ///
    /// The cell is a single `Arc<ArcSwapOption<..>>` **shared** (Arc-cloned) with
    /// the flush coordinator's config, so a runtime install (`None` → `Some`, Issue
    /// #3616 PR2) or a rotation's inner-generation advance (Issue #3617) is observed
    /// by both the write path (flush) and the recovery/`is_encrypted` path at once.
    /// Off-lock readers (`read_from`, `is_encrypted`, `wal_keyring`) `load()` it
    /// lock-free; the write path's reads stay serialized under the coordinator
    /// `writer` mutex, and the install stores into it inside the seal→reopen
    /// hand-off (see [`Self::install_wal_keyring`]).
    wal_keyring: Arc<ArcSwapOption<crate::encryption::wal_encryption::WalKeyring>>,
    /// Serializes runtime keyring installs (Issue #3616 PR2) so two concurrent
    /// installers cannot both observe the cell as `None`, both pass the presence
    /// check, and both run seal->store->reopen -- the second silently replacing
    /// the first keyring and producing an undecryptable segment. This mutex is
    /// held for the entire [`Self::install_wal_keyring`] body (presence check +
    /// seal + store + reopen) so a second concurrent installer blocks, then
    /// observes `Some`, and returns the existing rejection `Err`.
    ///
    /// Why a dedicated field: it is a private LEAF, taken only at the very top of
    /// install. The hot path (`append`/`flush`) never touches it, so it adds zero
    /// steady-state overhead; and because install itself never calls into
    /// `historical`/`current_timestamp`/cold, it is never held across any lock
    /// ordered after `wal` -- it merely wraps the presence check and the
    /// coordinator-`writer`-guarded seal->store->reopen hand-off.
    #[cfg(not(target_arch = "wasm32"))]
    install_lock: Mutex<()>,
}

impl ConcurrentWalSystem {
    /// Create a new concurrent WAL system.
    pub fn new(config: ConcurrentWalSystemConfig) -> Result<Self> {
        // Create ConcurrentWal config
        let wal_config = ConcurrentWalConfig {
            wal_dir: config.wal_dir.clone(),
            num_stripes: config.num_stripes,
            stripe_capacity: config.stripe_capacity,
            segment_size: config.segment_size,
            segments_to_retain: config.segments_to_retain,
        };

        // Build the WAL DEK keyring from the configured cipher (Issue #3617). A
        // never-rotated encrypted WAL starts as a single-generation keyring
        // (stamps INITIAL_WAL_KEY_VERSION, decrypts any segment); a full-MEK
        // rotation later advances it to a second generation. Plaintext WALs have
        // no keyring.
        //
        // Issue #488 version-provisioning: when the durable `open()` path
        // resolves a provisioned key version from durable on-disk state, build
        // the keyring at that version (still `match_any`, so mixed/legacy
        // segments decrypt exactly as before) so new segments stamp the real
        // version and `current_version` reports it. `None` reproduces prior
        // behavior exactly.
        let initial_keyring = config.wal_cipher.clone().map(|cipher| {
            use crate::encryption::wal_encryption::WalKeyring;
            match config.wal_key_version {
                Some(v) => WalKeyring::single_versioned(cipher, v),
                None => WalKeyring::single(cipher),
            }
        });
        // One shared presence cell, cloned (Arc-cloned, same underlying cell) into
        // both the system and the coordinator so a runtime install or rotation is
        // observed by both the flush and recovery paths (Issues #3616, #3617).
        let wal_keyring = Arc::new(ArcSwapOption::from(initial_keyring.map(Arc::new)));

        // Create FlushCoordinator config
        let coordinator_config = FlushCoordinatorConfig {
            wal_dir: config.wal_dir,
            segment_size: config.segment_size,
            segments_to_retain: config.segments_to_retain,
            flush_interval_ms: config.flush_interval_ms,
            sync_on_flush: matches!(
                config.durability_mode,
                DurabilityMode::Synchronous | DurabilityMode::GroupCommit { .. }
            ),
            write_buffer_size: config.write_buffer_size,
            wal_keyring: Arc::clone(&wal_keyring),
        };

        let wal = Arc::new(ConcurrentWal::new(wal_config)?);
        let coordinator = Arc::new(FlushCoordinator::new(coordinator_config)?);
        let shutdown_signal = Arc::new(AtomicBool::new(false));

        // Create group commit coordinator for modes that need epoch tracking
        let group_commit = match config.durability_mode {
            DurabilityMode::GroupCommit {
                max_batch_size,
                max_delay_ms,
            } => Some(Arc::new(GroupCommitCoordinator::new(
                max_delay_ms,
                max_batch_size,
            ))),
            DurabilityMode::AsyncBatched {
                max_batch_size,
                max_delay_ms,
                ..
            } => Some(Arc::new(GroupCommitCoordinator::new(
                max_delay_ms,
                max_batch_size,
            ))),
            _ => None,
        };

        // Create flush notifier for batch-size-triggered flushes
        let flush_notifier = Arc::new(FlushNotifier::new());

        // Create error counter for health monitoring
        let consecutive_flush_errors = Arc::new(AtomicU64::new(0));

        // Start background flush thread for async/group-commit modes
        let flush_thread = if matches!(
            config.durability_mode,
            DurabilityMode::Async { .. }
                | DurabilityMode::GroupCommit { .. }
                | DurabilityMode::AsyncBatched { .. }
        ) {
            let wal_clone = Arc::clone(&wal);
            let coordinator_clone = Arc::clone(&coordinator);
            let shutdown_clone = Arc::clone(&shutdown_signal);
            let flush_notifier_clone = Arc::clone(&flush_notifier);
            let group_commit_clone = group_commit.clone();
            let error_counter_clone = Arc::clone(&consecutive_flush_errors);
            let flush_interval = Duration::from_millis(config.flush_interval_ms);
            let sync_on_flush =
                matches!(config.durability_mode, DurabilityMode::GroupCommit { .. });

            Some(thread::spawn(move || {
                Self::flush_loop(
                    wal_clone,
                    coordinator_clone,
                    shutdown_clone,
                    flush_notifier_clone,
                    group_commit_clone,
                    error_counter_clone,
                    flush_interval,
                    sync_on_flush,
                );
            }))
        } else {
            None
        };

        Ok(Self {
            wal,
            coordinator,
            flush_thread,
            shutdown_signal,
            flush_notifier,
            durability_mode: config.durability_mode,
            group_commit,
            consecutive_flush_errors,
            tolerate_torn_tail: config.tolerate_torn_tail,
            wal_keyring,
            #[cfg(not(target_arch = "wasm32"))]
            install_lock: Mutex::new(()),
        })
    }

    /// Background flush loop.
    ///
    /// Wakes up either when:
    /// - The flush interval expires (normal periodic flush)
    /// - The flush_notifier is signaled (batch size reached)
    /// - Shutdown is requested
    #[allow(clippy::too_many_arguments)]
    fn flush_loop(
        wal: Arc<ConcurrentWal>,
        coordinator: Arc<FlushCoordinator>,
        shutdown: Arc<AtomicBool>,
        flush_notifier: Arc<FlushNotifier>,
        group_commit: Option<Arc<GroupCommitCoordinator>>,
        error_counter: Arc<AtomicU64>,
        interval: Duration,
        sync_on_flush: bool,
    ) {
        let flusher = BackgroundFlusher {
            wal,
            coordinator,
            shutdown,
            flush_notifier,
            group_commit,
            error_counter,
            interval,
            sync_on_flush,
        };
        flusher.run();
    }

    /// Append an operation asynchronously (fire and forget).
    ///
    /// For `DurabilityMode::Synchronous`, this blocks until durable.
    /// For other modes, this returns immediately.
    pub fn append(&self, operation: WalOperation) -> Result<LSN> {
        match self.durability_mode {
            DurabilityMode::Synchronous => self.append_sync(operation),
            DurabilityMode::Async { .. }
            | DurabilityMode::GroupCommit { .. }
            | DurabilityMode::AsyncBatched { .. } => self.append_async(operation),
        }
    }

    /// Append an operation asynchronously (returns immediately).
    ///
    /// The entry is buffered and will be flushed by the background thread.
    pub fn append_async(&self, operation: WalOperation) -> Result<LSN> {
        self.wal.append_async(operation)
    }

    /// Append a batch of operations to the buffer without flushing (returns immediately).
    ///
    /// This is the batch counterpart of [`append_async`](Self::append_async): it buffers
    /// every operation under a single atomic LSN allocation (the Issue #219 win) and lets
    /// the configured durability path flush them later. Like `append_async`, it performs
    /// **no** durability-mode branching — callers that need a flush still call
    /// [`commit`](Self::commit) afterwards, which honors the active `DurabilityMode`. This
    /// keeps the transaction commit path's behavior identical across Sync/Async/GroupCommit
    /// while routing multi-operation transactions through the efficient batch append.
    pub fn append_batch_async(&self, operations: Vec<WalOperation>) -> Result<Vec<LSN>> {
        self.wal.append_batch(operations)
    }

    /// Append an operation synchronously (waits for durability).
    ///
    /// This flushes immediately and waits for fsync.
    pub fn append_sync(&self, operation: WalOperation) -> Result<LSN> {
        let (lsn, handle) = self.wal.append_with_handle(operation)?;

        // Drain and flush immediately for sync mode
        let entries = self.wal.drain_all();
        if !entries.is_empty() {
            self.coordinator.flush(entries, true)?;
        }

        // Wait for durability
        handle.wait().map_err(|e| {
            Error::Storage(StorageError::WalError {
                reason: format!("WAL flush failed: {}", e),
            })
        })?;

        Ok(lsn)
    }

    /// Append a batch of operations efficiently.
    ///
    /// This method provides significant performance improvements for high-throughput
    /// workloads by batching multiple operations into fewer I/O operations.
    ///
    /// # Performance Benefits
    ///
    /// Compared to calling `append()` multiple times:
    /// - Single atomic LSN allocation for all operations (vs N atomic operations)
    /// - Better CPU cache locality during serialization
    /// - Reduced stripe buffer contention
    ///
    /// # Durability Behavior
    ///
    /// The durability semantics follow the configured `DurabilityMode`:
    /// - **Synchronous**: All operations are flushed and synced before returning
    /// - **Async**: Operations are buffered and flushed by background thread (eventual consistency)
    /// - **GroupCommit**: Operations are buffered and flushed by background thread (caller must wait on epoch)
    /// - **AsyncBatched**: Same as GroupCommit (operations buffered, background flush)
    ///
    /// # Arguments
    ///
    /// * `operations` - Vector of operations to append
    ///
    /// # Returns
    ///
    /// Vector of allocated LSNs in the same order as the operations.
    /// Returns an empty vector if `operations` is empty.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::storage::wal::{WalOperation, ConcurrentWalSystem};
    ///
    /// let ops = vec![
    ///     WalOperation::CreateNode { /* ... */ },
    ///     WalOperation::CreateEdge { /* ... */ },
    ///     WalOperation::UpdateNode { /* ... */ },
    /// ];
    ///
    /// // Efficient batch append
    /// let lsns = wal.append_batch(ops)?;
    /// assert_eq!(lsns.len(), 3);
    ///
    /// // For GroupCommit mode, commit and wait
    /// if let Some(epoch) = wal.commit()? {
    ///     wal.group_commit_coordinator().unwrap().wait_for_flush(epoch)?;
    /// }
    /// ```
    pub fn append_batch(&self, operations: Vec<WalOperation>) -> Result<Vec<LSN>> {
        // Handle empty batch early
        if operations.is_empty() {
            return Ok(Vec::new());
        }

        // Use the underlying WAL's batch append for async modes
        match self.durability_mode {
            DurabilityMode::Synchronous => {
                // For synchronous mode, append batch then flush all
                let (lsns, handles) = self.wal.append_batch_with_handles(operations)?;

                // Drain and flush immediately for sync mode
                let entries = self.wal.drain_all();
                if !entries.is_empty() {
                    self.coordinator.flush(entries, true).map_err(|e| {
                        Error::Storage(StorageError::WalError {
                            reason: format!("Failed to flush batch after drain: {}", e),
                        })
                    })?;
                }

                // Wait for all handles to ensure durability.
                // Note: Since flush coordinator preserves LSN order, waiting for the last one
                // technically implies all previous ones are done, but waiting for all is safer
                // against future changes and handles errors correctly.
                if let Some(last_handle) = handles.into_iter().last() {
                    last_handle.wait().map_err(|e| {
                        Error::Storage(StorageError::WalError {
                            reason: format!("WAL flush failed: {}", e),
                        })
                    })?;
                }

                Ok(lsns)
            }
            DurabilityMode::Async { .. }
            | DurabilityMode::GroupCommit { .. }
            | DurabilityMode::AsyncBatched { .. } => {
                // For async modes, just batch append (background thread handles flush)
                self.wal.append_batch(operations)
            }
        }
    }

    /// Force a flush of all pending entries.
    pub fn flush(&self) -> Result<FlushStats> {
        let entries = self.wal.drain_all();
        if entries.is_empty() {
            return Ok(FlushStats::default());
        }

        let should_sync = !matches!(self.durability_mode, DurabilityMode::Async { .. });
        self.coordinator.flush(entries, should_sync)
    }

    /// Commit with the configured durability mode.
    ///
    /// # Usage
    ///
    /// **Important**: All `append_async()` calls for a transaction MUST complete
    /// before calling `commit()`. The typical pattern is:
    ///
    /// ```ignore
    /// wal.append_async(op1)?;
    /// wal.append_async(op2)?;
    /// let epoch = wal.commit()?;  // Register for durability
    /// if let Some(epoch) = epoch {
    ///     wal.group_commit_coordinator().unwrap().wait_for_flush(epoch)?;
    /// }
    /// ```
    ///
    /// # Returns
    ///
    /// Returns an epoch number for GroupCommit/AsyncBatched modes that the
    /// caller should wait on using `group_commit_coordinator().wait_for_flush(epoch)`.
    ///
    /// For other modes, returns `None`:
    /// - Synchronous: Data is already durable when this returns
    /// - Async: No waiting needed (fire-and-forget)
    ///
    /// # Race Condition Handling
    ///
    /// In GroupCommit mode, there's an intentional race between:
    /// 1. The flush thread draining entries
    /// 2. Transactions calling `register_transaction()`
    ///
    /// This is handled safely: if entries are drained before registration,
    /// the epoch will still advance (with no entries), ensuring waiters
    /// are notified. The data durability is guaranteed because entries
    /// must be in the ring buffer before this method is called.
    pub fn commit(&self) -> Result<Option<u64>> {
        match self.durability_mode {
            DurabilityMode::Synchronous => {
                // Drain and flush immediately with fsync
                let entries = self.wal.drain_all();
                if !entries.is_empty() {
                    self.coordinator.flush(entries, true)?;
                }
                Ok(None)
            }
            DurabilityMode::Async { .. } => {
                // Just let background thread handle it
                Ok(None)
            }
            DurabilityMode::GroupCommit { .. } | DurabilityMode::AsyncBatched { .. } => {
                // Register with coordinator and return epoch to wait for
                if let Some(ref gc) = self.group_commit {
                    let (epoch, should_trigger) = gc.register_transaction()?;

                    // If batch is full, signal flush thread to wake up immediately
                    if should_trigger {
                        self.flush_notifier.notify();
                    }

                    Ok(Some(epoch))
                } else {
                    // Fallback to sync if no coordinator (shouldn't happen)
                    let entries = self.wal.drain_all();
                    if !entries.is_empty() {
                        self.coordinator.flush(entries, true)?;
                    }
                    Ok(None)
                }
            }
        }
    }

    /// Get the group commit coordinator for waiting on epochs.
    ///
    /// Returns `None` for modes that don't use group commit.
    pub fn group_commit_coordinator(&self) -> Option<&Arc<GroupCommitCoordinator>> {
        self.group_commit.as_ref()
    }

    /// Get the current (next to be allocated) LSN.
    pub fn current_lsn(&self) -> LSN {
        self.wal.current_lsn()
    }

    /// Set the next LSN to allocate.
    ///
    /// **Warning**: Recovery-only (Issue #3420). Call this during startup —
    /// before any write is accepted — to seed the allocator past every LSN
    /// already durable on disk (WAL segments and/or index manifest). Calling
    /// it during normal operation will cause duplicate LSNs.
    ///
    /// Hardened after review (PR #3428): `pub(crate)` so external users
    /// cannot corrupt allocator state, a no-op when it would move the
    /// allocator BACKWARDS (the underlying allocator uses `fetch_max`
    /// semantics), and a debug assertion that no appends have happened yet.
    pub(crate) fn set_next_lsn(&self, lsn: LSN) {
        debug_assert_eq!(
            self.total_appends(),
            0,
            "set_next_lsn is recovery-only and must run before any append"
        );
        self.wal.set_next_lsn(lsn);
    }

    /// Get total entries appended.
    pub fn total_appends(&self) -> u64 {
        self.wal.total_appends()
    }

    /// Get total entries flushed to disk.
    pub fn total_flushed(&self) -> u64 {
        self.coordinator.total_entries_flushed()
    }

    /// Get the durability mode.
    pub fn durability_mode(&self) -> DurabilityMode {
        self.durability_mode
    }

    /// Get the number of consecutive flush errors.
    ///
    /// This can be used for health monitoring. A value > 0 indicates
    /// that the last flush(es) failed. A value >= 3 indicates a critical
    /// condition where data durability may be compromised.
    ///
    /// The counter resets to 0 after a successful flush.
    pub fn consecutive_flush_errors(&self) -> u64 {
        self.consecutive_flush_errors.load(Ordering::Relaxed)
    }

    /// Check if the WAL is healthy (no consecutive flush errors).
    ///
    /// Returns `true` if the last flush succeeded, `false` if there are
    /// outstanding errors that haven't been cleared by a successful flush.
    pub fn is_healthy(&self) -> bool {
        self.consecutive_flush_errors() == 0
    }

    /// Get the WAL directory path.
    pub fn wal_dir(&self) -> &std::path::Path {
        self.coordinator.wal_dir()
    }

    /// Whether this WAL is encrypted at rest (a WAL cipher is configured).
    ///
    /// Used by the index key-rotation cross-layer guard (Issue #488): an
    /// index-only key rotation to a new MEK must refuse while any *other* layer
    /// (WAL/checkpoint/cold) is still encrypted under the current MEK, because
    /// switching the key provider afterward would render those un-rotated files
    /// undecryptable. Never exposes the cipher or any key material.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn is_encrypted(&self) -> bool {
        self.wal_keyring.load().is_some()
    }

    /// The WAL DEK keyring, if this WAL is encrypted (Issues #3617, #3616).
    ///
    /// Exposed so the full-MEK rotation driver can advance the WAL DEK generation
    /// before force-rolling the active segment. Returns a clone of the shared
    /// keyring handle (a cheap `Arc` clone that shares the same inner
    /// generations), so a caller's `add_generation` is observed by every holder.
    /// Never returns key material directly — only the redacting-`Debug` handle.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn wal_keyring(&self) -> Option<crate::encryption::wal_encryption::WalKeyring> {
        self.wal_keyring.load_full().map(|k| (*k).clone())
    }

    /// Install a WAL DEK keyring at runtime, flipping a live plaintext WAL to
    /// encrypted (Issue #3616 PR2). This is the structural seam the plaintext →
    /// encrypted enable engine drives.
    ///
    /// # Transition
    ///
    /// Only the `None` → `Some` transition is supported here. Installing when a
    /// keyring is already present is **rejected** with a structured error (never
    /// a silent replace, so no data is lost): rotation (`Some` → `Some'`) is the
    /// job of the keyring's own [`add_generation`], not this seam.
    ///
    /// # Crash-consistency / concurrency
    ///
    /// You cannot append encrypted (v16) frames into an already-open plaintext
    /// (v13) segment, so the install runs as the `advance` closure inside the
    /// existing seal→reopen hand-off ([`Self::seal_active_segment_for_rotation`]):
    /// it drains + fsyncs in-flight ring-buffer entries into the current plaintext
    /// segment, seals it, stores the keyring into the shared presence cell, then
    /// opens a fresh segment which is now written in the encrypted keyversioned
    /// format. Because the store happens **between** seal and reopen while holding
    /// the coordinator `writer` mutex, no `flush()` can interleave: every segment's
    /// header and frames use one consistent keyring state, and every acknowledged
    /// append is preserved across the flip.
    ///
    /// The store closure obeys the same lock-order contract as a rotation
    /// `advance`: it only mutates the in-memory presence cell and must not acquire
    /// any lock ordered after `wal` (no cold flush, no `historical`,
    /// no `current_timestamp`).
    ///
    /// [`add_generation`]: crate::encryption::wal_encryption::WalKeyring::add_generation
    // PR2 (#3616) ships this structural install seam; its production consumer is
    // the plaintext → encrypted enable engine (#3616 PR3), which drives it from
    // `enable_encryption` and the startup `install_pending_enable_wal_keyring` hook.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn install_wal_keyring(
        &self,
        keyring: crate::encryption::wal_encryption::WalKeyring,
    ) -> Result<()> {
        // Serialize the ENTIRE install — presence check, seal, store, reopen —
        // under a dedicated leaf mutex. Without it, the presence check below runs
        // outside the coordinator `writer` mutex while the store happens inside the
        // seal→reopen hand-off, so two concurrent installers could both observe
        // `None`, both pass the check, and both run seal→store→reopen — the second
        // silently replacing the first keyring and producing an undecryptable
        // segment. Holding this guard for the whole body makes a second concurrent
        // installer block here, then observe `Some`, and take the rejection path.
        let _install_guard = self.install_lock.lock().unwrap_or_else(|e| e.into_inner());

        // Reject a double-install: presence is a one-way None → Some transition.
        // Checked (under the install lock) before the seal so an already-encrypted
        // WAL is never re-rolled.
        if self.wal_keyring.load().is_some() {
            // A DISTINGUISHABLE precondition variant (not a generic `WalError`) so
            // the enable engine's `map_wal_install_err` maps ONLY this to
            // FAILED_PRECONDITION and leaves genuine WAL I/O / seal faults as
            // INTERNAL (Issue #3616 PR3). MCP classifies it as FAILED_PRECONDITION.
            return Err(Error::Storage(StorageError::WalKeyringAlreadyInstalled {
                reason: "a WAL keyring is already installed; runtime install only \
                         supports the plaintext → encrypted (None → Some) transition \
                         (key rotation is a separate operation)"
                    .to_string(),
            }));
        }

        let cell = Arc::clone(&self.wal_keyring);
        let new_keyring = Arc::new(keyring);
        // Store into the shared cell strictly between sealing the plaintext
        // segment and opening the fresh (now encrypted) one, under the writer
        // mutex — the atomic hand-off point.
        self.seal_active_segment_for_rotation(move || {
            cell.store(Some(new_keyring));
        })
    }

    /// Uninstall the WAL DEK keyring at runtime, flipping a live encrypted WAL
    /// back to plaintext (Issue #3616 PR4). This is the structural seam the
    /// encrypted → plaintext DISABLE engine drives — the exact inverse of
    /// [`Self::install_wal_keyring`].
    ///
    /// # Transition
    ///
    /// Only the `Some` → `None` transition is supported here. Uninstalling when
    /// NO keyring is present is **rejected** with a structured error (never a
    /// silent no-op or a spurious segment roll): a caller cannot disable
    /// encryption on an already-plaintext WAL.
    ///
    /// # Crash-consistency / concurrency
    ///
    /// Symmetric to install: you cannot append plaintext (v13) frames into an
    /// already-open encrypted (v16) segment, so the uninstall runs as the
    /// `advance` closure inside the same seal→reopen hand-off
    /// ([`Self::seal_active_segment_for_rotation`]): it drains + fsyncs in-flight
    /// ring-buffer entries into the current encrypted segment, seals it, stores
    /// `None` into the shared presence cell, then opens a fresh segment which is
    /// now written in the plaintext string-label format. Because the store
    /// happens **between** seal and reopen while holding the coordinator `writer`
    /// mutex, no `flush()` can interleave: every segment's header and frames use
    /// one consistent keyring state, and every acknowledged append is preserved
    /// across the flip.
    ///
    /// # Read capability after uninstall
    ///
    /// Dropping the keyring removes read-decrypt capability from the live cell,
    /// so [`Self::read_from`] (which snapshots the cell) can no longer decode the
    /// pre-uninstall encrypted segments still on disk. This is expected and
    /// mirrors the enable engine's inverse concern (its retired plaintext
    /// segments must be *deleted* for security): the DISABLE driver captures a
    /// plaintext index snapshot of all pre-uninstall state, then RETIRES those
    /// encrypted segments, so a reopen reconstructs from the plaintext snapshot
    /// and replays only the fresh plaintext segment. The retire is a separate
    /// driver step (mirroring [`install_wal_keyring`]'s retire being separate),
    /// not part of this seam.
    ///
    /// The store closure obeys the same lock-order contract as a rotation
    /// `advance`: it only mutates the in-memory presence cell and must not
    /// acquire any lock ordered after `wal`.
    ///
    /// [`install_wal_keyring`]: Self::install_wal_keyring
    // PR4 (#3616) ships this structural uninstall seam; its production consumer is
    // the encrypted → plaintext disable engine (#3616 PR4), which drives it from
    // `disable_encryption` and the startup disable-resume hook.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn uninstall_wal_keyring(&self) -> Result<()> {
        // Serialize the ENTIRE uninstall — presence check, seal, store, reopen —
        // under the same dedicated leaf mutex install uses. Without it, two
        // concurrent uninstallers could both observe `Some`, both pass the check,
        // and both run seal→store→reopen — the second sealing an already-plaintext
        // segment spuriously. Holding this guard for the whole body makes a second
        // concurrent uninstaller block here, then observe `None`, and take the
        // rejection path. It also serializes install against uninstall (both take
        // this same leaf), so the presence transition is never torn.
        let _install_guard = self.install_lock.lock().unwrap_or_else(|e| e.into_inner());

        // Reject when there is nothing to uninstall: presence must be `Some` for
        // this one-way Some → None transition. Checked (under the install lock)
        // before the seal so an already-plaintext WAL is never re-rolled.
        if self.wal_keyring.load().is_none() {
            // A DISTINGUISHABLE precondition variant (not a generic `WalError`) so
            // the disable engine maps ONLY this to FAILED_PRECONDITION and leaves
            // genuine WAL I/O / seal faults as INTERNAL (Issue #3616 PR4). MCP
            // classifies it as FAILED_PRECONDITION.
            return Err(Error::Storage(StorageError::WalKeyringNotInstalled {
                reason: "no WAL keyring is installed; runtime uninstall only supports the \
                         encrypted → plaintext (Some → None) transition (a plaintext WAL is \
                         already un-encrypted)"
                    .to_string(),
            }));
        }

        let cell = Arc::clone(&self.wal_keyring);
        // Store `None` into the shared cell strictly between sealing the encrypted
        // segment and opening the fresh (now plaintext) one, under the writer
        // mutex — the atomic hand-off point, the exact inverse of install.
        self.seal_active_segment_for_rotation(move || {
            cell.store(None);
        })
    }

    /// Whether every on-disk WAL segment is stamped with `key_version` — the
    /// rotation driver's "old generation fully retired" signal (Issue #3617).
    /// See [`FlushCoordinator::all_segments_use_key_version`].
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn all_segments_use_key_version(&self, key_version: u32) -> bool {
        self.coordinator.all_segments_use_key_version(key_version)
    }

    /// Retire (delete) every sealed old-generation WAL segment, keeping only
    /// segments stamped `keep_key_version` and the active segment (Issue #3617).
    /// See [`FlushCoordinator::retire_old_generation_segments`].
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn retire_old_generation_segments(&self, keep_key_version: u32) -> Result<usize> {
        self.coordinator
            .retire_old_generation_segments(keep_key_version)
    }

    /// Force-roll the active WAL segment for a full-MEK key rotation (Issue
    /// #3617): drain and durably flush every in-flight ring-buffer entry into
    /// the CURRENT (old-generation) segment, then atomically seal it, run
    /// `advance` (which flips the WAL keyring to the new generation), and open a
    /// fresh segment stamped with — and encrypting under — the new generation.
    ///
    /// # Quiesce / correctness
    ///
    /// Appends land in the lock-free ring buffer BEFORE any flush. This method
    /// first drains and flushes them (with fsync) into the old segment, so no
    /// acknowledged entry is lost across the roll. The subsequent seal + keyring
    /// flip + reopen all run under the coordinator `writer` mutex, which every
    /// flush also holds while it reads the write cipher and writes the segment
    /// header — so the (segment header generation, frame-encrypting generation)
    /// pair is always consistent and a batch can never straddle two generations.
    /// Any append racing the roll is simply picked up by whichever flush runs
    /// next and written, consistently, to whatever segment is current at that
    /// flush. `advance` runs while holding only the `writer` mutex, so it must
    /// not acquire any lock ordered after `wal` (no cold flush inside it).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn seal_active_segment_for_rotation<F: FnOnce()>(&self, advance: F) -> Result<()> {
        // 1. Drain + flush all pending entries into the current (old) segment,
        //    with fsync, so nothing acknowledged is left only in the ring buffer.
        let entries = self.wal.drain_all();
        if !entries.is_empty() {
            self.coordinator.flush(entries, true)?;
        }
        // 2. Seal old + flip keyring + open new, atomically under the writer mutex.
        self.coordinator.seal_active_segment_and_reopen(advance)
    }

    /// Read WAL entries from disk, starting from the specified LSN.
    ///
    /// This reads all segment files in the WAL directory and returns entries
    /// with LSN >= start_lsn. Used for recovery.
    ///
    /// Honors the configured crash-torn-tail recovery policy (Issue #3433): by
    /// default an undecodable trailing entry in the final segment stops replay
    /// there (keeping the intact prefix); with `tolerate_torn_tail = false` any
    /// parse failure hard-errors.
    pub fn read_from(&self, start_lsn: LSN) -> Result<Vec<super::WalEntry>> {
        // Thread the configured WAL cipher (if any) into the reader: encrypted
        // segments cannot be decoded without it, so an encryption-at-rest
        // database replaying its WAL tail after a crash must decrypt here.
        // Passing `None` (no encryption) preserves plaintext behavior exactly.
        // Snapshot the shared presence cell for the duration of the read. Holding
        // the guard keeps the loaded keyring alive while the reader borrows it.
        let keyring_guard = self.wal_keyring.load();
        crate::storage::wal::segment_reader::read_entries_from_dir_with_keyring(
            self.wal_dir(),
            start_lsn,
            keyring_guard.as_ref().map(|k| k.as_ref()),
            self.tolerate_torn_tail,
        )
    }

    /// Shutdown the WAL system gracefully.
    ///
    /// This signals the background thread to stop, waits for it to finish,
    /// and performs a final flush of all pending entries.
    pub fn shutdown(&mut self) {
        // Gracefully shutdown the WAL.
        // This stops accepting new writes, waits for active batches to complete,
        // and then closes the ring buffers.
        self.wal.shutdown_graceful();

        // Signal shutdown
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Wake up flush thread so it can see the shutdown signal
        self.flush_notifier.notify();

        // Wait for flush thread to finish
        if let Some(handle) = self.flush_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ConcurrentWalSystem {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GLOBAL_INTERNER;
    use crate::core::id::NodeId;
    use crate::core::property::PropertyMap;
    use crate::core::temporal::time;
    use tempfile::tempdir;

    fn create_test_operation(id: u64) -> WalOperation {
        WalOperation::CreateNode {
            node_id: NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("Node{}", id)).unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        }
    }

    // ── Issue #3617: WAL DEK force-roll (dual-generation rotation) ────────

    fn wal_test_cipher(seed: u8) -> Arc<dyn crate::encryption::cipher::Cipher> {
        use zeroize::Zeroizing;
        let key = Zeroizing::new([seed; 32]);
        Arc::new(crate::encryption::Aes256GcmCipher::new(&key))
    }

    fn encrypted_sync_config(
        dir: &std::path::Path,
        cipher: Arc<dyn crate::encryption::cipher::Cipher>,
    ) -> ConcurrentWalSystemConfig {
        let mut config = ConcurrentWalSystemConfig::new(dir);
        config.durability_mode = DurabilityMode::Synchronous;
        config.wal_cipher = Some(cipher);
        config
    }

    #[test]
    fn force_roll_writes_new_generation_and_recovers_both() {
        let dir = tempdir().unwrap();
        let old_dek = wal_test_cipher(7);
        let new_dek = wal_test_cipher(9);
        let wal = ConcurrentWalSystem::new(encrypted_sync_config(dir.path(), Arc::clone(&old_dek)))
            .unwrap();

        // Write a couple entries under the OLD generation (v16, key_version 1).
        wal.append(create_test_operation(1)).unwrap();
        wal.append(create_test_operation(2)).unwrap();

        // Force-roll: seal the old segment and start a new one under a NEW DEK
        // (key_version 2). The keyring flip runs inside the atomic hand-off.
        let ring = wal.wal_keyring().unwrap().clone();
        let advance_cipher = Arc::clone(&new_dek);
        wal.seal_active_segment_for_rotation(move || {
            ring.add_generation(2, advance_cipher);
        })
        .unwrap();

        // Subsequent appends land in the fresh, new-generation segment.
        wal.append(create_test_operation(3)).unwrap();
        wal.append(create_test_operation(4)).unwrap();
        wal.flush().unwrap();

        // Recovery via the (now two-generation) keyring recovers every entry —
        // old segments under the old DEK, new under the new.
        let entries = wal.read_from(LSN::initial()).unwrap();
        let ids: Vec<u64> = entries
            .iter()
            .filter_map(|e| match &e.operation {
                WalOperation::CreateNode { node_id, .. } => Some(node_id.as_u64()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            vec![1, 2, 3, 4],
            "all entries across both generations recover"
        );
    }

    /// Hammer appends from many threads WHILE a force-roll happens: every
    /// acknowledged entry must recover (none lost) and decrypt (none unreadable),
    /// proving the seal is atomic w.r.t. concurrent appenders (Issue #3617).
    #[test]
    fn append_hammer_across_force_roll_loses_nothing() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicU64, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let old_dek = wal_test_cipher(7);
        let new_dek = wal_test_cipher(9);
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::GroupCommit {
            max_batch_size: 16,
            max_delay_ms: 2,
        };
        config.wal_cipher = Some(Arc::clone(&old_dek));
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        let threads = 4u64;
        let per_thread = 200u64;
        let next_id = Arc::new(AtomicU64::new(1));
        let barrier = Arc::new(Barrier::new(threads as usize + 1));

        let mut handles = Vec::new();
        for _ in 0..threads {
            let wal = Arc::clone(&wal);
            let next_id = Arc::clone(&next_id);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..per_thread {
                    let id = next_id.fetch_add(1, AtOrd::Relaxed);
                    let (_lsn, handle) = wal
                        .wal
                        .append_with_handle(create_test_operation(id))
                        .unwrap();
                    // Ensure the entry is flushed so nothing is stuck unflushed.
                    let _ = wal.commit();
                    let _ = handle.wait();
                }
            }));
        }

        // Release the appenders, then force-roll partway through the storm.
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let ring = wal.wal_keyring().unwrap().clone();
        let advance_cipher = Arc::clone(&new_dek);
        wal.seal_active_segment_for_rotation(move || {
            ring.add_generation(2, advance_cipher);
        })
        .unwrap();

        for h in handles {
            h.join().unwrap();
        }
        wal.flush().unwrap();

        let total = threads * per_thread;
        let entries = wal.read_from(LSN::initial()).unwrap();
        let mut ids: Vec<u64> = entries
            .iter()
            .filter_map(|e| match &e.operation {
                WalOperation::CreateNode { node_id, .. } => Some(node_id.as_u64()),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len() as u64,
            total,
            "every acknowledged entry across the roll must recover (none lost, none unreadable)"
        );
        assert_eq!(*ids.first().unwrap(), 1);
        assert_eq!(*ids.last().unwrap(), total);
    }

    // ── Issue #3616 PR2: runtime WAL keyring install (plaintext → encrypted) ──

    // NOTE: `seed` is a CIPHER SEED (distinguishes one test cipher's key bytes
    // from another), NOT a key_version. `WalKeyring::single` always stamps
    // INITIAL_WAL_KEY_VERSION (== 1) as the on-disk key_version regardless of the
    // seed, which is why T3 asserts the post-install cohort is "key_version 1".
    fn wal_keyring(seed: u8) -> crate::encryption::wal_encryption::WalKeyring {
        crate::encryption::wal_encryption::WalKeyring::single(wal_test_cipher(seed))
    }

    fn recovered_ids(entries: &[crate::storage::wal::WalEntry]) -> Vec<u64> {
        entries
            .iter()
            .filter_map(|e| match &e.operation {
                WalOperation::CreateNode { node_id, .. } => Some(node_id.as_u64()),
                _ => None,
            })
            .collect()
    }

    /// T1 — None→Some transition: a fresh plaintext WAL reports `is_encrypted()
    /// == false`; after `install_wal_keyring` it reports `true`; a record
    /// appended after the install reads back correctly, and pre-install records
    /// still recover (mixed plaintext/encrypted directory).
    #[test]
    fn install_keyring_none_to_some_transition() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        assert!(
            !wal.is_encrypted(),
            "a fresh plaintext WAL must not be encrypted"
        );
        wal.append(create_test_operation(1)).unwrap();
        wal.append(create_test_operation(2)).unwrap();

        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        assert!(
            wal.is_encrypted(),
            "installing a keyring must flip the WAL to encrypted"
        );

        // A record appended after the install lands in the fresh encrypted
        // segment and must read back correctly.
        wal.append(create_test_operation(3)).unwrap();
        wal.flush().unwrap();

        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            vec![1, 2, 3],
            "pre-install plaintext and post-install encrypted records both recover in order"
        );
    }

    /// T2 — concurrent appenders during install lose nothing. N appender threads
    /// hammer while one thread installs a keyring mid-storm (mirrors
    /// `append_hammer_across_force_roll_loses_nothing`). Every acknowledged
    /// append must recover, none lost, none unreadable, no hang.
    #[test]
    fn install_keyring_during_concurrent_appends_loses_nothing() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicU64, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::GroupCommit {
            max_batch_size: 16,
            max_delay_ms: 2,
        };
        // Starts PLAINTEXT — the install flips it to encrypted mid-storm.
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        let threads = 4u64;
        let per_thread = 200u64;
        let next_id = Arc::new(AtomicU64::new(1));
        let barrier = Arc::new(Barrier::new(threads as usize + 1));

        let mut handles = Vec::new();
        for _ in 0..threads {
            let wal = Arc::clone(&wal);
            let next_id = Arc::clone(&next_id);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..per_thread {
                    let id = next_id.fetch_add(1, AtOrd::Relaxed);
                    let (_lsn, handle) = wal
                        .wal
                        .append_with_handle(create_test_operation(id))
                        .unwrap();
                    let _ = wal.commit();
                    let _ = handle.wait();
                }
            }));
        }

        // Release the appenders, then install a keyring partway through.
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(3));
        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        assert!(wal.is_encrypted());

        for h in handles {
            h.join().unwrap();
        }
        wal.flush().unwrap();

        let total = threads * per_thread;
        // In-process recovery via the live system's shared keyring cell (same
        // in-process limitation as `append_hammer_across_force_roll_loses_nothing`
        // — a full process-restart reopen is out of scope for this seam test).
        let entries = wal.read_from(LSN::initial()).unwrap();
        let mut ids = recovered_ids(&entries);
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len() as u64,
            total,
            "every acknowledged entry across the install must recover (none lost, none unreadable)"
        );
        assert_eq!(*ids.first().unwrap(), 1);
        assert_eq!(*ids.last().unwrap(), total);
    }

    /// T3 — install-then-roll ordering / mixed-format recovery. Records written
    /// before install live in plaintext (v13) segments; records after install
    /// live in encrypted (v16) segment(s). A full recovery reads BOTH cohorts
    /// back correctly in order, and the on-disk directory really is mixed-format.
    #[test]
    fn install_keyring_mixed_format_recovery() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // Pre-install cohort → plaintext segment(s).
        for id in 1..=5 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();
        assert!(
            !wal.all_segments_use_key_version(1),
            "pre-install segments must be plaintext (no key_version stamp)"
        );

        wal.install_wal_keyring(wal_keyring(9)).unwrap();

        // Post-install cohort → encrypted v16 segment(s).
        for id in 6..=10 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        // The directory now holds BOTH plaintext and encrypted segments, so no
        // single key_version covers every segment.
        assert!(
            !wal.all_segments_use_key_version(1),
            "a mixed plaintext+encrypted directory is not wholly one key_version"
        );

        // POSITIVE proof the install actually produced an encrypted (v16) segment
        // on disk — not merely flipped the in-memory flag while still writing
        // plaintext. `!all_segments_use_key_version(1)` above is satisfied by the
        // surviving pre-install PLAINTEXT segments alone, so it cannot prove the
        // "mixed-format" claim; the max stamped key_version across the directory
        // being `Some(1)` requires at least one keyversioned v16 segment to exist.
        assert_eq!(
            crate::storage::wal::segment_reader::max_key_version_in_dir(dir.path()),
            Some(1),
            "the post-install cohort must be written as an encrypted v16 segment \
             stamped key_version 1 (INITIAL_WAL_KEY_VERSION), proving the mixed format"
        );

        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            (1..=10).collect::<Vec<_>>(),
            "both plaintext and encrypted cohorts recover in order"
        );
    }

    /// Encryption-at-rest negative control (FIX B, Issue #3616 PR2 review).
    /// The five install tests before this one are all satisfied by the surviving
    /// pre-install PLAINTEXT segments — none POSITIVELY assert that a post-install
    /// segment is actually v16-encrypted on disk, so an install that flipped the
    /// in-memory flag but kept writing plaintext (a future shared-cell desync)
    /// would pass them all. This test closes that blind spot two ways:
    ///
    /// 1. POSITIVE: at least one on-disk segment header is
    ///    `WAL_VERSION_ENCRYPTED_KEYVERSIONED` (v16), proven via the reader's own
    ///    header-only scan (`max_key_version_in_dir` returns `Some(1)`).
    /// 2. STRONGEST: re-reading the WAL directory with NO keyring must NOT recover
    ///    the post-install cohort as plaintext (it is genuinely undecryptable),
    ///    while the pre-install cohort still reads back as plaintext v13.
    #[test]
    fn install_keyring_post_install_segment_is_encrypted_at_rest() {
        use crate::storage::wal::segment_reader::{
            max_key_version_in_dir, read_entries_from_dir_with_keyring,
        };

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // Pre-install cohort → plaintext v13 segment(s).
        for id in 1..=4 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();
        assert_eq!(
            max_key_version_in_dir(dir.path()),
            None,
            "before install, no segment carries a stamped key_version (all plaintext)"
        );

        wal.install_wal_keyring(wal_keyring(7)).unwrap();

        // Post-install cohort → encrypted v16 segment(s).
        for id in 5..=8 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        // (1) POSITIVE v16-at-rest proof: a keyversioned segment now exists on
        // disk, stamped INITIAL_WAL_KEY_VERSION (1). A desync that kept writing
        // plaintext would leave this `None`.
        assert_eq!(
            max_key_version_in_dir(dir.path()),
            Some(1),
            "the post-install cohort must be an encrypted v16 segment on disk"
        );

        // Full recovery WITH the keyring reads every cohort back in order.
        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            (1..=8).collect::<Vec<_>>(),
            "with the keyring, both plaintext and encrypted cohorts recover in order"
        );

        // (2) STRONGEST proof: re-read the SAME directory with NO keyring. The
        // post-install (v16) cohort must NOT be recoverable-as-plaintext — either
        // the read errors (undecryptable) or it returns only the plaintext prefix.
        // The pre-install (v13) cohort is plaintext and reads transparently.
        match read_entries_from_dir_with_keyring(dir.path(), LSN::initial(), None, true) {
            Ok(no_key_entries) => {
                let ids = recovered_ids(&no_key_entries);
                for id in 1..=4 {
                    assert!(
                        ids.contains(&id),
                        "pre-install plaintext record {id} must still read without a keyring"
                    );
                }
                for id in 5..=8 {
                    assert!(
                        !ids.contains(&id),
                        "post-install record {id} must NOT be recoverable as plaintext \
                         without the keyring (it is encrypted at rest)"
                    );
                }
            }
            Err(_) => {
                // Undecryptable without the keyring is itself proof of
                // encryption-at-rest for the post-install cohort.
            }
        }
    }

    /// T4 — double install rejected. Installing a second keyring when one is
    /// already present returns a structured error (not a silent replace) and
    /// loses no data.
    #[test]
    fn install_keyring_twice_is_rejected() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        wal.append(create_test_operation(1)).unwrap();
        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        wal.append(create_test_operation(2)).unwrap();

        let err = wal
            .install_wal_keyring(wal_keyring(9))
            .expect_err("a second install must be rejected, not silently applied");
        let msg = err.to_string();
        assert!(
            msg.contains("keyring") || msg.contains("already"),
            "rejection error should explain a keyring is already present, got: {msg}"
        );

        // Still encrypted and no data lost.
        assert!(wal.is_encrypted());
        wal.append(create_test_operation(3)).unwrap();
        wal.flush().unwrap();
        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            vec![1, 2, 3],
            "a rejected double-install must not lose or corrupt any records"
        );
    }

    /// T5 — no torn reads on the presence cell. Concurrent loaders read the cell
    /// while one thread stores into it (via install); a load must always yield
    /// either the old `None` or a fully-valid `Some`, never a partial/invalid
    /// keyring.
    #[test]
    fn install_keyring_no_torn_reads_on_cell() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicBool, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        let loaders = 6usize;
        let stop = Arc::new(AtomicBool::new(false));
        let barrier = Arc::new(Barrier::new(loaders + 1));

        let mut handles = Vec::new();
        for _ in 0..loaders {
            let wal = Arc::clone(&wal);
            let stop = Arc::clone(&stop);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                // Per-thread record of which cell states this loader observed, so
                // after join we can prove the None→Some store is actually seen
                // across threads (not merely that a load never tore). The bare
                // `k.current().is_some()` check alone can never fail, so it cannot
                // catch a store that is never observed.
                let mut saw_none = false;
                let mut saw_some = false;
                barrier.wait();
                while !stop.load(AtOrd::Relaxed) {
                    match wal.wal_keyring() {
                        None => saw_none = true,
                        Some(k) => {
                            saw_some = true;
                            // Any observed keyring must be fully valid (never
                            // torn): a Some always has a resolvable current gen.
                            assert!(
                                k.current().is_some(),
                                "a loaded keyring must be fully-constructed (current generation present)"
                            );
                        }
                    }
                }
                (saw_none, saw_some)
            }));
        }

        barrier.wait();
        // Let the loaders spin on the pre-install `None` first, so that state is
        // observed before the store flips it.
        std::thread::sleep(std::time::Duration::from_millis(5));
        // Hammer the store against the concurrent loads.
        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        // Keep the loaders running long enough to observe the post-install `Some`.
        std::thread::sleep(std::time::Duration::from_millis(5));
        stop.store(true, AtOrd::Relaxed);

        let mut any_saw_none = false;
        let mut any_saw_some = false;
        for h in handles {
            let (saw_none, saw_some) = h.join().unwrap();
            any_saw_none |= saw_none;
            any_saw_some |= saw_some;
        }
        assert!(
            any_saw_none,
            "loaders must have observed the pre-install None state"
        );
        assert!(
            any_saw_some,
            "loaders must have observed the post-install Some state \
             (proving the store is seen across threads)"
        );
        assert!(
            wal.wal_keyring().is_some(),
            "install must be visible after store"
        );
    }

    /// FIX A (Issue #3616 PR2 review) — concurrent double-install TOCTOU. Two
    /// threads race `install_wal_keyring` on the same system. Without the install
    /// lock both could observe `None`, both pass the presence check, and both run
    /// seal→store→reopen — the second silently replacing the first keyring and
    /// producing an undecryptable segment. With the lock, EXACTLY one returns
    /// `Ok` and one returns `Err`, no data is lost, and the survivor keyring
    /// decrypts the post-install cohort (no undecryptable segment).
    #[test]
    fn install_keyring_concurrent_double_install_rejected() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicUsize, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        // A few acknowledged appends before the install (plaintext v13).
        for id in 1..=3 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        let ok_count = Arc::new(AtomicUsize::new(0));
        let err_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let mut handles = Vec::new();
        for seed in [7u8, 9u8] {
            let wal = Arc::clone(&wal);
            let ok_count = Arc::clone(&ok_count);
            let err_count = Arc::clone(&err_count);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                match wal.install_wal_keyring(wal_keyring(seed)) {
                    Ok(()) => ok_count.fetch_add(1, AtOrd::Relaxed),
                    Err(_) => err_count.fetch_add(1, AtOrd::Relaxed),
                };
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Exactly one installer wins; the other is rejected (never a silent
        // second seal→store→reopen).
        assert_eq!(
            ok_count.load(AtOrd::Relaxed),
            1,
            "exactly one concurrent install must succeed"
        );
        assert_eq!(
            err_count.load(AtOrd::Relaxed),
            1,
            "exactly one concurrent install must be rejected"
        );
        assert!(wal.is_encrypted(), "the survivor keyring must be installed");

        // Post-install cohort is written under the survivor keyring (encrypted
        // v16 at rest).
        for id in 4..=6 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();
        assert_eq!(
            crate::storage::wal::segment_reader::max_key_version_in_dir(dir.path()),
            Some(1),
            "the post-install cohort must be an encrypted v16 segment (survivor keyring), \
             not an undecryptable segment from a double seal→store→reopen"
        );

        // Every acknowledged append recovers in order, and the survivor keyring
        // decrypts the post-install cohort.
        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            (1..=6).collect::<Vec<_>>(),
            "no acknowledged append is lost and the survivor keyring decrypts the \
             post-install cohort"
        );
    }

    // ── Issue #3616 PR4: runtime WAL keyring UNINSTALL (encrypted → plaintext) ──
    //
    // The exact inverse of the PR2 install seam: seal the encrypted (v16) active
    // segment, store `None` into the shared presence cell, and reopen a fresh
    // PLAINTEXT (v13) segment — the structural seam the encrypted → plaintext
    // DISABLE engine drives. Because dropping the keyring removes read-decrypt
    // capability, the pre-uninstall encrypted segments become undecryptable
    // through the live cell (`read_from` now snapshots `None`); the disable
    // DRIVER retires them after capturing a plaintext snapshot (a later slice).
    // These seam tests model that retire by deleting the pre-uninstall segment
    // files, then prove the post-uninstall cohort is plaintext at rest.

    /// D1 — Some→None transition: an encrypted WAL reports `is_encrypted() ==
    /// true`; after `uninstall_wal_keyring` it reports `false`; records appended
    /// after the uninstall land in a PLAINTEXT (v13) segment readable with NO
    /// keyring, and a full round-trip with the retired keyring still recovers
    /// every cohort (encrypted pre-uninstall + plaintext post-uninstall) in order.
    #[test]
    fn uninstall_keyring_some_to_none_transition() {
        use crate::storage::wal::segment_reader::read_entries_from_dir_with_keyring;
        use std::collections::HashSet;

        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // Reach the ENCRYPTED state the disable engine strips (None → Some).
        wal.install_wal_keyring(wal_keyring(7)).unwrap();
        assert!(
            wal.is_encrypted(),
            "the WAL must be encrypted before uninstall"
        );

        // Pre-uninstall cohort → encrypted v16 segment(s).
        for id in 1..=4 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        // Snapshot the encrypted segment files (the driver retires these after
        // capturing a plaintext snapshot); retain a keyring clone to model the
        // driver's decrypt capability over them until then.
        let pre_uninstall_stems: HashSet<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".log").map(str::to_string)
            })
            .collect();
        let retired = wal
            .wal_keyring()
            .expect("an encrypted WAL must expose its keyring");

        // Reverse seam.
        wal.uninstall_wal_keyring().unwrap();
        assert!(
            !wal.is_encrypted(),
            "uninstalling the keyring must flip the WAL to plaintext"
        );

        // Post-uninstall cohort → plaintext v13 segment(s).
        for id in 5..=8 {
            wal.append(create_test_operation(id)).unwrap();
        }
        wal.flush().unwrap();

        // (a) ROUND-TRIP: with the retired keyring the encrypted pre-uninstall
        // cohort and the plaintext post-uninstall cohort both recover in order
        // (plaintext segments parse regardless of a present keyring).
        let all =
            read_entries_from_dir_with_keyring(dir.path(), LSN::initial(), Some(&retired), true)
                .unwrap();
        assert_eq!(
            recovered_ids(&all),
            (1..=8).collect::<Vec<_>>(),
            "with the retired keyring both cohorts recover in order across the uninstall"
        );

        // (b) PLAINTEXT-AT-REST: model the driver's retire of the encrypted
        // segments, then a read with NO keyring recovers the post-uninstall
        // cohort as plaintext — positive proof the reopened segment is v13, not
        // an in-memory-flag flip that kept writing encrypted.
        for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name
                .strip_suffix(".log")
                .or_else(|| name.strip_suffix(".log.meta"));
            if let Some(stem) = stem
                && pre_uninstall_stems.contains(stem)
            {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        let plaintext_only =
            read_entries_from_dir_with_keyring(dir.path(), LSN::initial(), None, true).unwrap();
        assert_eq!(
            recovered_ids(&plaintext_only),
            (5..=8).collect::<Vec<_>>(),
            "after retiring the encrypted segments, the post-uninstall cohort reads back \
             as plaintext with NO keyring (the reopened segment is v13 at rest)"
        );
    }

    /// D2 — uninstall on a plaintext WAL is rejected. Uninstalling when NO keyring
    /// is present returns a structured error (not a silent no-op or a spurious
    /// roll) and loses no data — the mirror of the double-install rejection.
    #[test]
    fn uninstall_keyring_when_plaintext_is_rejected() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path());
        config.durability_mode = DurabilityMode::Synchronous;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        wal.append(create_test_operation(1)).unwrap();
        wal.append(create_test_operation(2)).unwrap();

        let err = wal
            .uninstall_wal_keyring()
            .expect_err("uninstalling a plaintext WAL must be rejected, not silently applied");
        let msg = err.to_string();
        assert!(
            msg.contains("keyring") || msg.contains("plaintext") || msg.contains("not"),
            "rejection error should explain no keyring is installed, got: {msg}"
        );

        // Still plaintext and no data lost.
        assert!(!wal.is_encrypted());
        wal.append(create_test_operation(3)).unwrap();
        wal.flush().unwrap();
        let entries = wal.read_from(LSN::initial()).unwrap();
        assert_eq!(
            recovered_ids(&entries),
            vec![1, 2, 3],
            "a rejected uninstall must not lose or corrupt any records"
        );
    }

    #[test]
    fn test_concurrent_wal_system_creation() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        assert_eq!(wal.total_appends(), 0);
        assert_eq!(wal.current_lsn(), LSN(1));
    }

    #[test]
    fn test_append_sync_mode() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let lsn = wal.append(create_test_operation(1)).unwrap();
        assert_eq!(lsn, LSN(1));
        assert_eq!(wal.total_appends(), 1);
    }

    #[test]
    fn test_append_sync_mode_handles_more_than_stripe_capacity() {
        let dir = tempdir().unwrap();
        let mut config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        // Keep capacity intentionally tiny to regression-test the benchmark footgun:
        // mode-aware `append()` must continue making progress even when the buffered
        // async path would hit backpressure quickly.
        config.stripe_capacity = 8;
        let wal = ConcurrentWalSystem::new(config).unwrap();

        for i in 1..=64 {
            let lsn = wal.append(create_test_operation(i)).unwrap();
            assert_eq!(lsn, LSN(i));
        }

        assert_eq!(wal.total_appends(), 64);
        assert_eq!(wal.total_flushed(), 64);
    }

    #[test]
    fn test_append_async_mode() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_flush_interval_ms(10_000) // Explicitly set config interval to avoid racing with default 10ms
            .with_durability_mode(DurabilityMode::Async {
                flush_interval_ms: 10_000,
            });
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append several entries
        for i in 1..=10 {
            let lsn = wal.append(create_test_operation(i)).unwrap();
            assert_eq!(lsn, LSN(i));
        }

        assert_eq!(wal.total_appends(), 10);

        // Explicit flush - ensure all entries are durable.
        // Note: The background flush thread may have already flushed some/all
        // entries, so we check total_flushed() rather than the return stats.
        // This makes the test deterministic regardless of timing.
        wal.flush().unwrap();

        // Wait for flush to complete (handle race with background thread)
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(5);
        while wal.total_flushed() < 10 {
            // LCOV_EXCL_START
            if start.elapsed() > timeout {
                break;
            }
            // LCOV_EXCL_STOP
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(wal.total_flushed(), 10, "All 10 entries should be flushed");

        wal.shutdown();
        assert_eq!(wal.total_flushed(), 10, "All 10 entries should be flushed");
    }

    #[test]
    fn test_concurrent_appends() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Async {
                flush_interval_ms: 100,
            })
            .with_num_stripes(4);
        let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

        let num_threads = 4;
        let ops_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let wal = Arc::clone(&wal);
                thread::spawn(move || {
                    for i in 0..ops_per_thread {
                        let id = (t * ops_per_thread + i + 1) as u64;
                        wal.append_async(create_test_operation(id)).unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(wal.total_appends(), (num_threads * ops_per_thread) as u64);
    }

    #[test]
    fn test_flush_persists_entries() {
        let dir = tempdir().unwrap();
        // Use Synchronous mode to avoid background flush thread interference
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append entries (append_async in Sync mode still buffers)
        for i in 1..=5 {
            wal.append_async(create_test_operation(i)).unwrap();
        }

        // Force flush - since no background thread, all 5 entries should be flushed here
        let stats = wal.flush().unwrap();
        assert_eq!(stats.entries_flushed, 5);

        // Verify flushed count
        assert_eq!(wal.total_flushed(), 5);

        wal.shutdown();
    }

    #[test]
    fn test_group_commit_mode() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::GroupCommit {
                max_batch_size: 10,
                max_delay_ms: 10,
            })
            .with_flush_interval_ms(5);
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append entries
        for i in 1..=5 {
            wal.append(create_test_operation(i)).unwrap();
        }

        // Wait for background flush with polling (more resilient than single sleep)
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(5); // Increased timeout for CI
        let mut flushed = false;
        while start.elapsed() < timeout {
            if wal.total_flushed() >= 1 {
                flushed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        // Should have been flushed by background thread
        assert!(
            flushed,
            "Expected at least 1 entry to be flushed within {}ms, but got {} flushed",
            timeout.as_millis(),
            wal.total_flushed()
        );

        wal.shutdown();
    }

    #[test]
    fn test_shutdown_flushes_remaining() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
            DurabilityMode::Async {
                flush_interval_ms: 100,
            },
        );
        let mut wal = ConcurrentWalSystem::new(config).unwrap();

        // Append entries without explicit flush
        for i in 1..=5 {
            wal.append_async(create_test_operation(i)).unwrap();
        }

        // Shutdown should flush remaining
        wal.shutdown();

        // All entries should be flushed
        assert_eq!(wal.total_flushed(), 5);
    }

    // ============================================================
    // Batch Append Tests (Issue #219)
    // ============================================================

    #[test]
    fn test_append_batch_async() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
            DurabilityMode::Async {
                flush_interval_ms: 10_000,
            },
        );
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let ops = vec![
            create_test_operation(1),
            create_test_operation(2),
            create_test_operation(3),
        ];

        let lsns = wal.append_batch(ops).unwrap();

        assert_eq!(lsns.len(), 3);
        assert_eq!(lsns[0], LSN(1));
        assert_eq!(lsns[1], LSN(2));
        assert_eq!(lsns[2], LSN(3));
        assert_eq!(wal.total_appends(), 3);
    }

    #[test]
    fn test_append_batch_sync() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let ops = vec![create_test_operation(1), create_test_operation(2)];

        let lsns = wal.append_batch(ops).unwrap();

        assert_eq!(lsns.len(), 2);
        assert_eq!(lsns[0], LSN(1));
        assert_eq!(lsns[1], LSN(2));
        assert_eq!(wal.total_appends(), 2);
    }

    #[test]
    fn test_append_batch_empty() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path());
        let wal = ConcurrentWalSystem::new(config).unwrap();

        let lsns = wal.append_batch(vec![]).unwrap();

        assert_eq!(lsns.len(), 0);
        assert_eq!(wal.total_appends(), 0);
    }

    #[test]
    fn test_append_batch_large() {
        let dir = tempdir().unwrap();
        let config = ConcurrentWalSystemConfig::new(dir.path()).with_durability_mode(
            DurabilityMode::Async {
                flush_interval_ms: 10_000,
            },
        );
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // Create 100 operations
        let ops: Vec<_> = (1..=100).map(create_test_operation).collect();

        let lsns = wal.append_batch(ops).unwrap();

        assert_eq!(lsns.len(), 100);
        assert_eq!(lsns[0], LSN(1));
        assert_eq!(lsns[99], LSN(100));
        assert_eq!(wal.total_appends(), 100);
    }

    #[test]
    fn test_append_sync_persistence_guarantee() {
        // This test verifies that append_sync actually waits for the flush.
        // While we can't easily deterministic race condition, we can verify basic
        // persistence guarantee: immediately after append_sync returns, total_flushed
        // must be incremented.

        let dir = tempdir().unwrap();
        // Use Synchronous mode
        let config = ConcurrentWalSystemConfig::new(dir.path())
            .with_durability_mode(DurabilityMode::Synchronous);
        let wal = ConcurrentWalSystem::new(config).unwrap();

        // 1. Initial state
        assert_eq!(wal.total_flushed(), 0);

        // 2. Perform append_sync
        let lsn = wal.append_sync(create_test_operation(1)).unwrap();

        // 3. Immediately assert flushed count
        // If append_sync didn't wait, and flush was async/delayed, this might fail.
        // But since it's sync, it MUST be 1.
        assert_eq!(
            wal.total_flushed(),
            1,
            "Should be flushed immediately after return"
        );
        assert_eq!(lsn, LSN(1));

        // 4. Batch append sync
        let ops = vec![create_test_operation(2), create_test_operation(3)];
        let lsns = wal.append_batch(ops).unwrap();

        // 5. Assert flushed count increased by 2
        assert_eq!(
            wal.total_flushed(),
            3,
            "Batch should be flushed immediately"
        );
        assert_eq!(lsns.len(), 2);
    }
}
