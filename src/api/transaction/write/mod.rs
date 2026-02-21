//! Write transactions with ACID guarantees.
//!
//! Write transactions provide full ACID properties:
//! - **Atomicity**: All-or-nothing commit via write buffering
//! - **Consistency**: Referential integrity validation before commit
//! - **Isolation**: Snapshot Isolation with write-write conflict detection
//! - **Durability**: WAL with fsync guarantees
//!
//! Write transactions buffer all changes in memory until commit.
//! On commit, changes are validated and applied atomically.

use super::{
    ReadOps, TransactionSnapshot, TxId, TxMetadata, TxState, TxVisibilityManager, WriteBuffer,
    WriteOps,
};
use crate::core::error::{Result, StorageError, TransactionError};
use crate::core::graph::{Edge, Node};
use crate::core::hlc::{
    SendWithSelfHealError, evaluate_clock_skew, is_clock_skew_self_heal_enabled,
    send_with_overflow_self_heal,
};
use crate::core::id::{EdgeId, IdGenerator, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyMapBuilder};
use crate::core::temporal::{Timestamp, time};
use crate::core::version::VersionMetadata;
use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::DurabilityMode;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use parking_lot::RwLock;
use std::sync::{Arc, Mutex};
use std::time::Instant;

mod apply;
mod conflict;
mod validation;
mod wal;
#[cfg(test)]
mod repro_dangling_edge;

#[cfg(test)]
pub(crate) const MAX_BACKWARD_DRIFT_US: i64 = crate::core::hlc::MAX_BACKWARD_DRIFT_US;
pub(crate) const MAX_FORWARD_JUMP_US: i64 = crate::core::hlc::MAX_FORWARD_JUMP_US;

/// Maximum offset for valid_from timestamps in the future.
///
/// Users can backdate facts (valid_from < transaction_time) for historical corrections,
/// but we limit how far into the future they can set valid_from to prevent abuse and
/// maintain query semantics.
pub(crate) const MAX_VALID_TIME_FUTURE_OFFSET_US: i64 = 365 * 24 * 60 * 60 * 1_000_000; // 1 year

/// Write transaction with full ACID guarantees.
///
/// Write transactions buffer all operations in memory and apply them
/// atomically on commit. This ensures consistency and enables rollback.
///
/// # Example
///
/// ```rust,no_run
/// # use aletheiadb::{AletheiaDB, properties, api::transaction::WriteOps};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let db = AletheiaDB::new()?;
/// # let other = aletheiadb::core::NodeId::new(1)?;
/// # let props = properties! { "name" => "Alice" };
/// # let edge_props = properties! { "since" => 2024 };
/// let mut tx = db.write_transaction()?;
/// let node_id = tx.create_node("Person", props)?;
/// tx.create_edge(node_id, other, "KNOWS", edge_props)?;
/// tx.commit()?;  // or tx.rollback()
/// # Ok(())
/// # }
/// ```
pub struct WriteTransaction {
    pub(crate) tx_id: TxId,
    pub(crate) start_timestamp: Timestamp,
    pub(crate) state: TxState,

    // Snapshot for Snapshot Isolation
    pub(crate) snapshot: TransactionSnapshot,

    // Write buffer for uncommitted changes
    pub(crate) buffer: WriteBuffer,

    // Shared references to storage (Arc for zero-copy sharing)
    pub(crate) current: Arc<CurrentStorage>,
    pub(crate) historical: Arc<RwLock<HistoricalStorage>>,
    pub(crate) temporal_indexes: Arc<TemporalIndexes>,
    pub(crate) wal: Arc<ConcurrentWalSystem>,
    pub(crate) current_timestamp: Arc<Mutex<Timestamp>>,
    pub(crate) commit_clock_observed_at: Arc<Mutex<Instant>>,
    pub(crate) visibility_manager: Arc<TxVisibilityManager>,

    // ID generators (needed for creating new entities)
    // IdGenerator uses AtomicU64 internally, so no external Mutex is needed.
    pub(crate) node_id_gen: Arc<IdGenerator>,
    pub(crate) edge_id_gen: Arc<IdGenerator>,
    pub(crate) version_id_gen: Arc<IdGenerator>,

    /// Durability mode for this transaction's commit
    pub(crate) durability_mode: DurabilityMode,
}

impl WriteTransaction {
    /// Create a new write transaction with default durability mode (Synchronous).
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        tx_id: TxId,
        snapshot: TransactionSnapshot,
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
    ) -> Self {
        Self::new_with_durability(
            tx_id,
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
            DurabilityMode::Synchronous,
        )
    }

    /// Create a new write transaction with explicit commit clock observation state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_clock_observed_at(
        tx_id: TxId,
        snapshot: TransactionSnapshot,
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        commit_clock_observed_at: Arc<Mutex<Instant>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
    ) -> Self {
        Self::new_with_durability_and_clock_observed_at(
            tx_id,
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            commit_clock_observed_at,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
            DurabilityMode::Synchronous,
        )
    }

    /// Create a new write transaction with a specific durability mode.
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_durability(
        tx_id: TxId,
        snapshot: TransactionSnapshot,
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
        durability_mode: DurabilityMode,
    ) -> Self {
        Self::new_with_durability_and_clock_observed_at(
            tx_id,
            snapshot,
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            Arc::new(Mutex::new(Instant::now())),
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
            durability_mode,
        )
    }

    /// Create a new write transaction with specific durability and clock observation state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_durability_and_clock_observed_at(
        tx_id: TxId,
        snapshot: TransactionSnapshot,
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        commit_clock_observed_at: Arc<Mutex<Instant>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<IdGenerator>,
        edge_id_gen: Arc<IdGenerator>,
        version_id_gen: Arc<IdGenerator>,
        durability_mode: DurabilityMode,
    ) -> Self {
        WriteTransaction {
            tx_id,
            start_timestamp: time::now(),
            state: TxState::Active,
            snapshot,
            buffer: WriteBuffer::new(),
            current,
            historical,
            temporal_indexes,
            wal,
            current_timestamp,
            commit_clock_observed_at,
            visibility_manager,
            node_id_gen,
            edge_id_gen,
            version_id_gen,
            durability_mode,
        }
    }

    /// Get transaction metadata.
    pub fn metadata(&self) -> TxMetadata {
        TxMetadata {
            tx_id: self.tx_id,
            start_timestamp: self.start_timestamp,
            commit_timestamp: None,
            state: self.state,
            is_read_only: false,
        }
    }

    /// Get transaction ID.
    pub fn tx_id(&self) -> TxId {
        self.tx_id
    }

    fn lock_adaptive_forward_jump_limit(
        &self,
        observed_at: Instant,
    ) -> Result<(std::sync::MutexGuard<'_, Instant>, i64)> {
        let previous_observed_at =
            self.commit_clock_observed_at
                .lock()
                .map_err(|_| TransactionError::LockPoisoned {
                    resource: "commit_clock_observed_at".to_string(),
                })?;
        let elapsed = observed_at.duration_since(*previous_observed_at);
        let elapsed_us = i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX);
        Ok((
            previous_observed_at,
            MAX_FORWARD_JUMP_US.saturating_add(elapsed_us),
        ))
    }

    /// Commit the transaction.
    ///
    /// This validates all buffered writes and applies them atomically
    /// to the storage. If validation fails or any operation fails,
    /// the transaction is rolled back.
    ///
    /// The durability behavior depends on the transaction's durability mode:
    /// - **Synchronous**: Waits for fsync (maximum durability, default)
    /// - **Async**: Returns after flush to OS cache (background thread syncs)
    /// - **GroupCommit**: Waits for batch fsync (ACID + high throughput)
    /// - **AsyncBatched**: Returns after flush to OS cache, batched fsync in background (<100µs latency)
    pub fn commit(self) -> Result<()> {
        self.commit_with_timestamp().map(|_| ())
    }

    /// Commit the transaction and return the commit timestamp.
    ///
    /// This is useful for benchmarks and tests that need to query the database
    /// at the exact commit timestamp to verify temporal semantics.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::{AletheiaDB, properties, api::transaction::WriteOps};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let properties = properties! { "name" => "Alice" };
    /// let mut tx = db.write_transaction()?;
    /// let node_id = tx.create_node("Person", properties)?;
    /// let commit_ts = tx.commit_with_timestamp()?;
    ///
    /// // Query at exact commit timestamp
    /// let node = db.get_node_at_time(node_id, commit_ts, commit_ts)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Durability Modes
    ///
    /// - **Synchronous**: WAL fsynced to disk before returning (ACID, ~1.5ms)
    /// - **Async**: Returns after flush to OS cache (background thread syncs)
    /// - **GroupCommit**: Waits for batch fsync (ACID + high throughput)
    /// - **AsyncBatched**: Returns after flush to OS cache, batched fsync in background (<100µs latency)
    pub fn commit_with_timestamp(mut self) -> Result<Timestamp> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!(
            "transaction_commit",
            tx_id = %self.tx_id
        )
        .entered();

        #[cfg(feature = "observability")]
        let commit_start = std::time::Instant::now();

        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Transition to preparing state
        self.state = TxState::Preparing;

        // Validate all buffered writes
        self.validate()?;

        // Detect write-write conflicts (Snapshot Isolation)
        self.detect_conflicts()?;

        // Acquire commit timestamp and perform mode-aware WAL flush.
        //
        // CRITICAL: We must hold the timestamp lock until WAL logging is complete
        // to prevent a race condition where transactions commit out-of-order.
        //
        // Timestamp management for Bi-Temporal Database:
        //
        // For temporal queries to work correctly, we MUST use wallclock timestamps
        // for transaction_time, not a logical clock. This allows querying historical
        // state at specific points in time (e.g., "what was the state at 2PM yesterday?").
        //
        // Monotonicity: Wallclock time is monotonically increasing (assuming NTP is working),
        // which satisfies the ordering requirements for Snapshot Isolation:
        // - commit_ts > snapshot_ts for transactions that started before this commit
        // - future snapshots will have timestamp >= this commit
        //
        // DURABILITY MODES (handled by ConcurrentWalSystem):
        // - Synchronous: Appends drain and flush immediately with fsync
        // - Async: Appends go to ring buffers, background thread syncs
        // - GroupCommit: Appends go to ring buffers, wait for epoch completion
        let commit_timestamp = {
            #[cfg(feature = "observability")]
            let ts_lock_start = std::time::Instant::now();

            let mut ts =
                self.current_timestamp
                    .lock()
                    .map_err(|_| TransactionError::LockPoisoned {
                        resource: "current_timestamp".to_string(),
                    })?;

            #[cfg(feature = "observability")]
            let ts_lock_acquired = std::time::Instant::now();

            // Phase 2: Use HLC for distributed temporal consistency
            // Get current physical wallclock
            let current_wallclock = crate::core::temporal::time::now();
            let observed_at = Instant::now();
            let (mut previous_observed_at, adaptive_forward_limit_us) =
                self.lock_adaptive_forward_jump_limit(observed_at)?;

            let self_heal_clock_skew = is_clock_skew_self_heal_enabled();
            let skew_decision = evaluate_clock_skew(
                current_wallclock.wallclock(),
                ts.wallclock(),
                Some(adaptive_forward_limit_us),
                self_heal_clock_skew,
            )
            .map_err(|violation| TransactionError::ClockSkew {
                wallclock: current_wallclock.wallclock(),
                previous: ts.wallclock(),
                drift_us: violation.drift_us,
                max_allowed: violation.max_allowed,
            })?;

            if self_heal_clock_skew && let Some(_direction) = skew_decision.healed_direction {
                #[cfg(feature = "observability")]
                tracing::warn!(
                    wallclock_ts = %current_wallclock,
                    prev_ts = %ts,
                    drift_us = skew_decision.drift_us,
                    reason = _direction.as_str(),
                    "Self-healing clock skew by clamping to local HLC frontier"
                );
            }

            // Phase 2: Use HLC .send() method for monotonic timestamp generation
            // This ensures: if wallclock advances, reset logical; otherwise increment logical
            let commit = send_with_overflow_self_heal(
                &ts,
                skew_decision.effective_wallclock,
                self_heal_clock_skew,
                |error| match error {
                    SendWithSelfHealError::InitialSend(error) => TransactionError::CommitFailed {
                        reason: format!("HLC timestamp generation failed: {}", error),
                    },
                    SendWithSelfHealError::FallbackWallclockOverflow {
                        wallclock,
                        current_logical,
                    } => TransactionError::CommitFailed {
                        reason: format!(
                            "HLC logical counter overflow at wallclock={}: {}",
                            wallclock, current_logical
                        ),
                    },
                    SendWithSelfHealError::FallbackSend(fallback_error) => {
                        TransactionError::CommitFailed {
                            reason: format!(
                                "HLC timestamp generation failed while self-healing: {}",
                                fallback_error
                            ),
                        }
                    }
                },
            )?;

            // Observability: Warn about clock skew issues
            #[cfg(feature = "observability")]
            {
                // Clock went backwards: wallclock < previous wallclock
                if current_wallclock.wallclock() < ts.wallclock() {
                    tracing::warn!(
                        wallclock_ts = %current_wallclock,
                        prev_ts = %ts,
                        skew_us = ts.wallclock() - current_wallclock.wallclock(),
                        logical_counter = commit.logical(),
                        "Clock skew detected: wallclock went backwards (NTP adjustment?)"
                    );
                } else if commit.wallclock() > ts.wallclock() + 60_000_000 {
                    // Large forward jump (>60 seconds)
                    tracing::warn!(
                        wallclock_ts = %current_wallclock,
                        prev_ts = %ts,
                        jump_us = commit.wallclock() - ts.wallclock(),
                        "Large clock jump detected: timestamps will be lumpy"
                    );
                }
            }

            // Update current_timestamp for next transaction's snapshot
            *ts = commit;
            // Persist observation only after we successfully advanced the frontier.
            *previous_observed_at = observed_at;
            drop(previous_observed_at);

            #[cfg(feature = "observability")]
            let wal_start = std::time::Instant::now();

            // Log operations to WAL (lock-free striped append!)
            // This must happen BEFORE applying changes for durability.
            wal::log_operations_to_wal(&self, commit)?;

            #[cfg(feature = "observability")]
            let wal_logged = std::time::Instant::now();

            // Commit with configured durability mode
            // For Sync: drains and flushes immediately
            // For Async: returns immediately
            // For GroupCommit: registers and returns epoch
            let wait_epoch = self.wal.commit()?;

            #[cfg(feature = "observability")]
            let wal_commit_completed = std::time::Instant::now();

            // For GroupCommit mode, wait for the epoch to be flushed.
            // AsyncBatched mode returns an epoch but does NOT wait.
            if let Some(epoch) = wait_epoch
                && let Some(gc) = self.wal.group_commit_coordinator()
                && self.durability_mode.waits_for_durability()
            {
                gc.wait_for_flush(epoch)?;
            }

            #[cfg(feature = "observability")]
            {
                // Record detailed breakdown for Honeycomb
                let ts_lock_wait_us =
                    ts_lock_acquired.duration_since(ts_lock_start).as_micros() as u64;
                let wal_log_us = wal_logged.duration_since(wal_start).as_micros() as u64;
                let wal_commit_us =
                    wal_commit_completed.duration_since(wal_logged).as_micros() as u64;
                let total_us = wal_commit_completed
                    .duration_since(ts_lock_start)
                    .as_micros() as u64;

                // Calculate total commit duration for Honeycomb queries
                let total_commit_us = commit_start.elapsed().as_micros() as u64;
                let operations_count = self.buffer.operations().len();

                tracing::info!(
                    ts_lock_wait_us,
                    wal_log_us,
                    wal_commit_us,
                    total_us,
                    total_commit_us,
                    operations_count,
                    commit_ts = %commit,
                    durability_mode = ?self.durability_mode,
                    "Transaction commit breakdown (concurrent WAL)"
                );
            }

            commit
        };

        // Apply all changes atomically
        apply::apply_changes(&self, commit_timestamp)?;

        // Notify temporal vector index of transaction completion (for snapshot creation)
        // Only call this if the transaction modified vector properties to avoid unnecessary overhead
        if self.buffer.has_vector_operations() {
            self.current.on_temporal_vector_transaction()?;
        }

        // Register commit with visibility manager
        self.visibility_manager
            .register_commit(self.tx_id, commit_timestamp);

        // Mark as committed
        self.state = TxState::Committed;

        #[cfg(feature = "observability")]
        {
            let total_commit_us = commit_start.elapsed().as_micros() as u64;
            tracing::debug!(
                total_commit_us,
                tx_id = %self.tx_id,
                "Transaction committed"
            );
        }

        Ok(commit_timestamp)
    }

    /// Rollback the transaction.
    ///
    /// Discards all buffered writes. This is automatically called
    /// if the transaction is dropped without committing.
    pub fn rollback(mut self) -> Result<()> {
        if self.state == TxState::Committed {
            return Err(TransactionError::AlreadyCommitted {
                tx_id: self.tx_id.as_u64(),
            }
            .into());
        }

        // Clear the write buffer
        self.buffer.clear();

        // Register abort with visibility manager
        self.visibility_manager.register_abort(self.tx_id);

        self.state = TxState::Aborted;

        Ok(())
    }

    /// Check if this transaction has any node writes (create, update, delete).
    pub(crate) fn has_node_writes(&self) -> bool {
        self.buffer.operations().iter().any(|op| {
            matches!(
                op,
                super::BufferedWrite::CreateNode { .. }
                    | super::BufferedWrite::UpdateNode { .. }
                    | super::BufferedWrite::DeleteNode { .. }
            )
        })
    }

    /// Check if this transaction has any edge writes (create, update, delete).
    pub(crate) fn has_edge_writes(&self) -> bool {
        self.buffer.operations().iter().any(|op| {
            matches!(
                op,
                super::BufferedWrite::CreateEdge { .. }
                    | super::BufferedWrite::UpdateEdge { .. }
                    | super::BufferedWrite::DeleteEdge { .. }
            )
        })
    }

    /// Check if this transaction has any vector property writes.
    pub(crate) fn has_vector_writes(&self) -> bool {
        self.buffer.has_vector_operations()
    }

    /// Validate all buffered writes.
    ///
    /// Checks:
    /// - Referential integrity (edges reference valid nodes)
    /// - No constraint violations
    fn validate(&self) -> Result<()> {
        validation::validate(self)
    }

    /// Detect write-write conflicts for Snapshot Isolation.
    ///
    /// Checks if any entity modified by this transaction has been committed
    /// by another transaction after our snapshot was taken. This implements
    /// the First-Committer-Wins rule of Snapshot Isolation.
    ///
    /// # Errors
    ///
    /// Returns `SerializationFailure` if a write-write conflict is detected.
    fn detect_conflicts(&self) -> Result<()> {
        conflict::detect_conflicts(self)
    }
}

impl ReadOps for WriteTransaction {
    fn get_node(&self, id: NodeId) -> Result<Node> {
        // Read-your-writes: check write buffer first
        if let Some(buffered) = self.buffer.get_node_write(id) {
            match buffered {
                super::BufferedWrite::CreateNode {
                    node_id,
                    label,
                    properties,
                    version_id,
                    ..
                } => {
                    // Return the buffered node
                    return Ok(Node::with_metadata(
                        *node_id,
                        *label,
                        properties.clone(),
                        *version_id,
                        VersionMetadata {
                            created_by_tx: self.tx_id,
                            commit_timestamp: None, // Not yet committed
                        },
                    ));
                }
                super::BufferedWrite::UpdateNode {
                    node_id,
                    label,
                    properties,
                    version_id,
                    ..
                } => {
                    // Return the updated node
                    return Ok(Node::with_metadata(
                        *node_id,
                        *label,
                        properties.clone(),
                        *version_id,
                        VersionMetadata {
                            created_by_tx: self.tx_id,
                            commit_timestamp: None,
                        },
                    ));
                }
                super::BufferedWrite::DeleteNode { .. } => {
                    // Node has been deleted in this transaction
                    return Err(StorageError::NodeNotFound(id).into());
                }
                _ => {} // Not a node operation
            }
        }

        // Fall back to snapshot-isolated read from storage
        let node = self.current.get_node(id)?;

        // Check if this version is visible in our snapshot
        if !self
            .visibility_manager
            .is_visible(&self.snapshot, node.metadata.created_by_tx)
        {
            // Version not visible - return NodeNotFound
            return Err(StorageError::NodeNotFound(id).into());
        }

        Ok(node)
    }

    fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        // Read-your-writes: check write buffer first
        if let Some(buffered) = self.buffer.get_edge_write(id) {
            match buffered {
                super::BufferedWrite::CreateEdge {
                    edge_id,
                    source,
                    target,
                    label,
                    properties,
                    version_id,
                    ..
                } => {
                    // Return the buffered edge
                    return Ok(Edge::with_metadata(
                        *edge_id,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                        *version_id,
                        VersionMetadata {
                            created_by_tx: self.tx_id,
                            commit_timestamp: None,
                        },
                    ));
                }
                super::BufferedWrite::UpdateEdge {
                    edge_id,
                    source,
                    target,
                    label,
                    properties,
                    version_id,
                    ..
                } => {
                    // Return the updated edge
                    return Ok(Edge::with_metadata(
                        *edge_id,
                        *label,
                        *source,
                        *target,
                        properties.clone(),
                        *version_id,
                        VersionMetadata {
                            created_by_tx: self.tx_id,
                            commit_timestamp: None,
                        },
                    ));
                }
                super::BufferedWrite::DeleteEdge { .. } => {
                    // Edge has been deleted in this transaction
                    return Err(StorageError::EdgeNotFound(id).into());
                }
                _ => {} // Not an edge operation
            }
        }

        // Fall back to snapshot-isolated read from storage
        let edge = self.current.get_edge(id)?;

        // Check if this version is visible in our snapshot
        if !self
            .visibility_manager
            .is_visible(&self.snapshot, edge.metadata.created_by_tx)
        {
            // Version not visible - return EdgeNotFound
            return Err(StorageError::EdgeNotFound(id).into());
        }

        Ok(edge)
    }

    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_outgoing_edges(node_id)
    }

    fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_incoming_edges(node_id)
    }

    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_outgoing_edges_with_label(node_id, label)
    }

    fn node_count(&self) -> usize {
        self.current.node_count()
    }

    fn edge_count(&self) -> usize {
        self.current.edge_count()
    }

    fn find_nodes_by_property(
        &self,
        label: &str,
        property_key: &str,
        property_value: &crate::core::property::PropertyValue,
    ) -> Vec<NodeId> {
        use crate::core::interning::GLOBAL_INTERNER;

        // 1. Get committed matches, excluding nodes modified in the buffer
        let mut results: Vec<NodeId> = self
            .current
            .find_nodes_by_property(label, property_key, property_value)
            .into_iter()
            .filter(|node_id| !self.buffer.has_modified_node(*node_id))
            .filter(|node_id| {
                self.current
                    .get_node(*node_id)
                    .map(|node| {
                        self.visibility_manager
                            .is_visible(&self.snapshot, node.metadata.created_by_tx)
                    })
                    .unwrap_or(false)
            })
            .collect();

        // 2. Scan buffered writes for matching CreateNode/UpdateNode
        let label_id = GLOBAL_INTERNER.get_id(label);
        let key_id = GLOBAL_INTERNER.get_id(property_key);

        if let (Some(label_id), Some(key_id)) = (label_id, key_id) {
            for op in self.buffer.operations() {
                match op {
                    super::BufferedWrite::CreateNode {
                        node_id,
                        label: node_label,
                        properties,
                        ..
                    }
                    | super::BufferedWrite::UpdateNode {
                        node_id,
                        label: node_label,
                        properties,
                        ..
                    } => {
                        if *node_label == label_id
                            && let Some(val) = properties.get_by_interned_key(&key_id)
                            && val == property_value
                        {
                            results.push(*node_id);
                        }
                    }
                    // DeleteNode is already excluded by has_modified_node filter
                    _ => {}
                }
            }
        }

        results
    }
}

impl WriteOps for WriteTransaction {
    fn create_node_with_valid_time(
        &mut self,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<NodeId> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Generate IDs
        let node_id = NodeId::new_unchecked(self.node_id_gen.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        // Get timestamp: use provided valid_from or default to transaction start time
        let timestamp = self.start_timestamp;
        let valid_from = valid_from.unwrap_or(timestamp);

        // Validate valid_from is not too far in future
        validation::validate_valid_from_future(valid_from)?;

        // Buffer the write
        self.buffer.add(super::BufferedWrite::CreateNode {
            node_id,
            version_id,
            label: label_interned,
            properties,
            valid_from,
        })?;

        Ok(node_id)
    }

    fn create_edge_with_valid_time(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<EdgeId> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Generate IDs
        let edge_id = EdgeId::new_unchecked(self.edge_id_gen.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        // Get timestamp: use provided valid_from or default to transaction start time
        let timestamp = self.start_timestamp;
        let valid_from = valid_from.unwrap_or(timestamp);

        // Buffer the write
        self.buffer.add(super::BufferedWrite::CreateEdge {
            edge_id,
            version_id,
            source,
            target,
            label: label_interned,
            properties,
            valid_from,
        })?;

        Ok(edge_id)
    }

    fn update_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Get current node to preserve label and existing properties
        let node = self.current.get_node(node_id)?;
        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);

        // PATCH semantics: Merge new properties with existing ones
        // Start with existing properties
        let mut builder = PropertyMapBuilder::from_map(node.properties.clone());

        // Update/add properties from the incoming map
        for (key, value) in properties.iter() {
            builder = builder.insert_by_key(*key, value.clone());
        }

        // Build the final merged property map
        let merged_properties = builder.build();

        // Get timestamp: use provided valid_from or default to transaction start time
        let timestamp = self.start_timestamp;
        let valid_from = valid_from.unwrap_or(timestamp);

        // Validate valid_from is not too far in future
        validation::validate_valid_from_future(valid_from)?;

        // Validate valid_from is not before entity creation
        let historical = self.historical.read();
        if let Some(current_version_id) = historical.get_current_node_version(node_id)
            && let Some(current_version) = historical.get_node_version(current_version_id)
        {
            let creation_time = current_version.temporal.valid_time().start();
            drop(historical); // Release lock before calling validation
            validation::validate_valid_from_not_before_creation(
                &format!("node:{}", node_id.as_u64()),
                creation_time,
                valid_from,
            )?;
        }

        // Buffer the write with merged properties
        self.buffer.add(super::BufferedWrite::UpdateNode {
            node_id,
            version_id,
            label: node.label,
            properties: merged_properties,
            valid_from,
        })?;

        Ok(())
    }

    fn update_edge_with_valid_time(
        &mut self,
        edge_id: EdgeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Get current edge to preserve source, target, label and existing properties
        let edge = self.current.get_edge(edge_id)?;
        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);

        // PATCH semantics: Merge new properties with existing ones
        // Start with existing properties
        let mut builder = PropertyMapBuilder::from_map(edge.properties.clone());

        // Update/add properties from the incoming map
        for (key, value) in properties.iter() {
            builder = builder.insert_by_key(*key, value.clone());
        }

        // Build the final merged property map
        let merged_properties = builder.build();

        // Get timestamp: use provided valid_from or default to transaction start time
        let timestamp = self.start_timestamp;
        let valid_from = valid_from.unwrap_or(timestamp);

        // Buffer the write with merged properties
        self.buffer.add(super::BufferedWrite::UpdateEdge {
            edge_id,
            version_id,
            source: edge.source,
            target: edge.target,
            label: edge.label,
            properties: merged_properties,
            valid_from,
        })?;

        Ok(())
    }

    fn delete_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Verify node exists and check for vector properties
        let node = self.current.get_node(node_id)?;

        // If the node being deleted contains vector properties, mark the buffer
        // to ensure the temporal vector index is notified on commit
        if !self.buffer.has_vector_operations() && node.properties.contains_vector() {
            self.buffer.mark_has_vector_operations();
        }

        // Get timestamp: use provided valid_from or default to transaction start time
        let timestamp = self.start_timestamp;
        let valid_from = valid_from.unwrap_or(timestamp);

        // Validate valid_from is not too far in future
        validation::validate_valid_from_future(valid_from)?;

        // Validate valid_from is not before entity creation
        let historical = self.historical.read();
        if let Some(current_version_id) = historical.get_current_node_version(node_id)
            && let Some(current_version) = historical.get_node_version(current_version_id)
        {
            let creation_time = current_version.temporal.valid_time().start();
            drop(historical); // Release lock before calling validation
            validation::validate_valid_from_not_before_creation(
                &format!("node:{}", node_id.as_u64()),
                creation_time,
                valid_from,
            )?;
        }

        // Buffer the write
        self.buffer.add(super::BufferedWrite::DeleteNode {
            node_id,
            valid_from,
        })?;

        Ok(())
    }

    fn delete_node_cascade(&mut self, node_id: NodeId) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Verify node exists before attempting deletion
        let _node = self.current.get_node(node_id)?;

        // Collect all edges connected to this node (both outgoing and incoming)
        // We do this before any deletions to avoid borrowing issues
        //
        // LIMITATION: This uses ReadOps methods which currently don't support
        // read-your-writes semantics for edge traversal. This means edges created
        // in the same transaction (but not yet committed) won't be found and deleted.
        // This is consistent with the existing ReadOps behavior but may leave orphaned
        // edges in same-transaction scenarios. See issue for future improvement.
        let outgoing_edges = self.get_outgoing_edges(node_id);
        let incoming_edges = self.get_incoming_edges(node_id);

        // Delete all connected edges first to maintain referential integrity
        // This prevents orphaned edges that reference a deleted node
        // Performance: O(degree) where degree is the number of connected edges
        for edge_id in outgoing_edges.into_iter().chain(incoming_edges) {
            self.delete_edge(edge_id)?;
        }

        // Finally, delete the node itself
        // This is safe now because all edges referencing this node have been removed
        self.delete_node(node_id)?;

        Ok(())
    }

    fn delete_edge_with_valid_time(
        &mut self,
        edge_id: EdgeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Verify edge exists and check for vector properties
        let edge = self.current.get_edge(edge_id)?;

        // If the edge being deleted contains vector properties, mark the buffer
        // to ensure the temporal vector index is notified on commit
        if !self.buffer.has_vector_operations() && edge.properties.contains_vector() {
            self.buffer.mark_has_vector_operations();
        }

        // Get timestamp: use provided valid_from or default to transaction start time
        let timestamp = self.start_timestamp;
        let valid_from = valid_from.unwrap_or(timestamp);

        // Buffer the write
        self.buffer.add(super::BufferedWrite::DeleteEdge {
            edge_id,
            valid_from,
        })?;

        Ok(())
    }
}

impl Drop for WriteTransaction {
    fn drop(&mut self) {
        // Auto-rollback if not committed
        if self.state == TxState::Active {
            self.buffer.clear();
            // Register abort with visibility manager
            self.visibility_manager.register_abort(self.tx_id);
            self.state = TxState::Aborted;
        }
    }
}

#[cfg(test)]
mod tests;
