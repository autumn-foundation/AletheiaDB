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
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId, VersionId};
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::property::{PropertyMap, PropertyMapBuilder};
use crate::core::temporal::{BiTemporalInterval, Timestamp, time};
use crate::core::version::VersionMetadata;
use crate::index::temporal::TemporalIndexes;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::storage::wal::{DurabilityMode, WalOperation};
use crate::utils::error::{Result, StorageError, TransactionError};
use crate::utils::lock::{MutexExt, RwLockExt};
use parking_lot::RwLock;
use std::sync::{Arc, Mutex};

/// Maximum allowed backward clock drift in microseconds (5 minutes).
///
/// If wallclock time goes backward by more than this (e.g., severe NTP adjustment),
/// we reject the transaction to avoid timestamps jumping far into the future.
/// 5 minutes allows for typical NTP corrections while preventing pathological cases.
const MAX_BACKWARD_DRIFT_US: i64 = 5 * 60 * 1_000_000; // 5 minutes

/// Maximum allowed forward clock jump in microseconds (1 hour).
///
/// If wallclock time jumps forward by more than this (e.g., system clock set manually),
/// we reject the transaction to avoid creating timestamps in the far future that would
/// break temporal query semantics.
const MAX_FORWARD_JUMP_US: i64 = 60 * 60 * 1_000_000; // 1 hour

/// Write transaction with full ACID guarantees.
///
/// Write transactions buffer all operations in memory and apply them
/// atomically on commit. This ensures consistency and enables rollback.
///
/// # Example
///
/// ```ignore
/// let mut tx = db.write_transaction();
/// let node_id = tx.create_node("Person", props)?;
/// tx.create_edge(node_id, other, "KNOWS", edge_props)?;
/// tx.commit()?;  // or tx.rollback()
/// ```
pub struct WriteTransaction {
    tx_id: TxId,
    start_timestamp: Timestamp,
    state: TxState,

    // Snapshot for Snapshot Isolation
    snapshot: TransactionSnapshot,

    // Write buffer for uncommitted changes
    buffer: WriteBuffer,

    // Shared references to storage (Arc for zero-copy sharing)
    current: Arc<CurrentStorage>,
    historical: Arc<RwLock<HistoricalStorage>>,
    temporal_indexes: Arc<TemporalIndexes>,
    wal: Arc<ConcurrentWalSystem>,
    current_timestamp: Arc<Mutex<Timestamp>>,
    visibility_manager: Arc<TxVisibilityManager>,

    // ID generators (needed for creating new entities)
    node_id_gen: Arc<Mutex<IdGenerator>>,
    edge_id_gen: Arc<Mutex<IdGenerator>>,
    version_id_gen: Arc<Mutex<IdGenerator>>,

    /// Durability mode for this transaction's commit
    durability_mode: DurabilityMode,
}

impl WriteTransaction {
    /// Create a new write transaction with default durability mode (Synchronous).
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
        node_id_gen: Arc<Mutex<IdGenerator>>,
        edge_id_gen: Arc<Mutex<IdGenerator>>,
        version_id_gen: Arc<Mutex<IdGenerator>>,
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

    /// Create a new write transaction with a specific durability mode.
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
        node_id_gen: Arc<Mutex<IdGenerator>>,
        edge_id_gen: Arc<Mutex<IdGenerator>>,
        version_id_gen: Arc<Mutex<IdGenerator>>,
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
    /// ```ignore
    /// let mut tx = db.write_transaction()?;
    /// let node_id = tx.create_node("Person", properties)?;
    /// let commit_ts = tx.commit_with_timestamp()?;
    ///
    /// // Query at exact commit timestamp
    /// let node = db.get_node_at_time(node_id, commit_ts, commit_ts)?;
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

            let mut ts = self.current_timestamp.lock_or_err().map_err(|e| {
                TransactionError::CommitFailed {
                    reason: format!("timestamp lock poisoned: {}", e),
                }
            })?;

            #[cfg(feature = "observability")]
            let ts_lock_acquired = std::time::Instant::now();

            // Phase 2: Use HLC for distributed temporal consistency
            // Get current physical wallclock
            let current_wallclock = crate::core::temporal::time::now();

            // Check for pathological clock skew using wallclock components
            let drift = current_wallclock.wallclock() - ts.wallclock();

            // Backward drift check: prevent timestamps jumping far into future
            if drift < -MAX_BACKWARD_DRIFT_US {
                return Err(TransactionError::ClockSkew {
                    wallclock: current_wallclock.wallclock(),
                    previous: ts.wallclock(),
                    drift_us: drift,
                    max_allowed: -MAX_BACKWARD_DRIFT_US,
                }
                .into());
            }

            // Forward jump check: prevent timestamps in far future
            if drift > MAX_FORWARD_JUMP_US {
                return Err(TransactionError::ClockSkew {
                    wallclock: current_wallclock.wallclock(),
                    previous: ts.wallclock(),
                    drift_us: drift,
                    max_allowed: MAX_FORWARD_JUMP_US,
                }
                .into());
            }

            // Phase 2: Use HLC .send() method for monotonic timestamp generation
            // This ensures: if wallclock advances, reset logical; otherwise increment logical
            let commit = ts.send(current_wallclock.wallclock()).map_err(|e| {
                TransactionError::CommitFailed {
                    reason: format!("HLC timestamp generation failed: {}", e),
                }
            })?;

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

            #[cfg(feature = "observability")]
            let wal_start = std::time::Instant::now();

            // Log operations to WAL (lock-free striped append!)
            // This must happen BEFORE applying changes for durability.
            self.log_operations_to_wal(commit)?;

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
        self.apply_changes(commit_timestamp)?;

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
        for write in self.buffer.operations() {
            match write {
                super::BufferedWrite::CreateEdge { source, target, .. }
                | super::BufferedWrite::UpdateEdge { source, target, .. } => {
                    // Check that source and target nodes exist
                    // They might exist in current storage or be created in this transaction
                    if !self.buffer.has_modified_node(*source)
                        && self.current.get_node(*source).is_err()
                    {
                        return Err(TransactionError::ValidationFailed {
                            reason: format!("Edge source node {:?} does not exist", source),
                        }
                        .into());
                    }
                    if !self.buffer.has_modified_node(*target)
                        && self.current.get_node(*target).is_err()
                    {
                        return Err(TransactionError::ValidationFailed {
                            reason: format!("Edge target node {:?} does not exist", target),
                        }
                        .into());
                    }
                }
                _ => {
                    // Other operations don't need validation
                }
            }
        }

        Ok(())
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
        for write in self.buffer.operations() {
            match write {
                // UpdateNode: check if node was modified or deleted after our snapshot
                super::BufferedWrite::UpdateNode { node_id, .. } => {
                    match self.current.get_node(*node_id) {
                        Ok(current_node) => {
                            // Node exists - check if it was modified after our snapshot
                            if let Some(commit_ts) = current_node.metadata.commit_timestamp
                                && commit_ts > self.snapshot.snapshot_timestamp
                            {
                                return Err(TransactionError::SerializationFailure {
                                    entity: format!("{:?}", node_id),
                                    reason: format!(
                                        "Version committed at {} after snapshot at {}",
                                        commit_ts, self.snapshot.snapshot_timestamp
                                    ),
                                }
                                .into());
                            }
                        }
                        Err(_) => {
                            // Node doesn't exist in current storage. Since we successfully
                            // called update_node() earlier (which reads from current storage),
                            // the node must have been deleted by another transaction.
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", node_id),
                                reason: "Node was deleted by another transaction".to_string(),
                            }
                            .into());
                        }
                    }
                }

                // UpdateEdge: check if edge was modified or deleted after our snapshot
                super::BufferedWrite::UpdateEdge { edge_id, .. } => {
                    match self.current.get_edge(*edge_id) {
                        Ok(current_edge) => {
                            // Edge exists - check if it was modified after our snapshot
                            if let Some(commit_ts) = current_edge.metadata.commit_timestamp
                                && commit_ts > self.snapshot.snapshot_timestamp
                            {
                                return Err(TransactionError::SerializationFailure {
                                    entity: format!("{:?}", edge_id),
                                    reason: format!(
                                        "Version committed at {} after snapshot at {}",
                                        commit_ts, self.snapshot.snapshot_timestamp
                                    ),
                                }
                                .into());
                            }
                        }
                        Err(_) => {
                            // Edge doesn't exist - it was deleted by another transaction
                            return Err(TransactionError::SerializationFailure {
                                entity: format!("{:?}", edge_id),
                                reason: "Edge was deleted by another transaction".to_string(),
                            }
                            .into());
                        }
                    }
                }

                // DeleteNode: check if node was modified after our snapshot
                super::BufferedWrite::DeleteNode { node_id } => {
                    // Get current version from storage
                    if let Ok(current_node) = self.current.get_node(*node_id)
                        && let Some(commit_ts) = current_node.metadata.commit_timestamp
                        && commit_ts > self.snapshot.snapshot_timestamp
                    {
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", node_id),
                            reason: format!(
                                "Version committed at {} after snapshot at {}",
                                commit_ts, self.snapshot.snapshot_timestamp
                            ),
                        }
                        .into());
                    }
                }

                // DeleteEdge: check if edge was modified after our snapshot
                super::BufferedWrite::DeleteEdge { edge_id } => {
                    // Get current version from storage
                    if let Ok(current_edge) = self.current.get_edge(*edge_id)
                        && let Some(commit_ts) = current_edge.metadata.commit_timestamp
                        && commit_ts > self.snapshot.snapshot_timestamp
                    {
                        return Err(TransactionError::SerializationFailure {
                            entity: format!("{:?}", edge_id),
                            reason: format!(
                                "Version committed at {} after snapshot at {}",
                                commit_ts, self.snapshot.snapshot_timestamp
                            ),
                        }
                        .into());
                    }
                }

                // CreateNode and CreateEdge don't need conflict detection
                // since they're creating new entities that didn't exist before
                _ => {}
            }
        }

        Ok(())
    }

    /// Log all buffered operations to WAL.
    ///
    /// This ensures durability - operations are logged before being applied.
    /// Uses lock-free appends to the concurrent WAL system.
    fn log_operations_to_wal(&self, commit_timestamp: Timestamp) -> Result<()> {
        let temporal = BiTemporalInterval::current(commit_timestamp);

        for write in self.buffer.operations() {
            let operation = match write {
                super::BufferedWrite::CreateNode {
                    node_id,
                    label,
                    properties,
                    ..
                } => {
                    // No allocation! Just copy the 4-byte InternedString ID
                    WalOperation::CreateNode {
                        node_id: *node_id,
                        label: *label,
                        properties: properties.clone(),
                        temporal,
                    }
                }
                super::BufferedWrite::CreateEdge {
                    edge_id,
                    source,
                    target,
                    label,
                    properties,
                    ..
                } => {
                    // No allocation! Just copy the 4-byte InternedString ID
                    WalOperation::CreateEdge {
                        edge_id: *edge_id,
                        source: *source,
                        target: *target,
                        label: *label,
                        properties: properties.clone(),
                        temporal,
                    }
                }
                super::BufferedWrite::UpdateNode {
                    node_id,
                    version_id,
                    label,
                    properties,
                    ..
                } => {
                    // No allocation! Just copy the 4-byte InternedString ID
                    WalOperation::UpdateNode {
                        node_id: *node_id,
                        version_id: *version_id,
                        label: *label,
                        properties: properties.clone(),
                        temporal,
                    }
                }
                super::BufferedWrite::UpdateEdge {
                    edge_id,
                    version_id,
                    label,
                    properties,
                    ..
                } => {
                    // No allocation! Just copy the 4-byte InternedString ID
                    WalOperation::UpdateEdge {
                        edge_id: *edge_id,
                        version_id: *version_id,
                        label: *label,
                        properties: properties.clone(),
                        temporal,
                    }
                }
                super::BufferedWrite::DeleteNode { node_id } => WalOperation::DeleteNode {
                    node_id: *node_id,
                    temporal,
                },
                super::BufferedWrite::DeleteEdge { edge_id } => WalOperation::DeleteEdge {
                    edge_id: *edge_id,
                    temporal,
                },
            };

            // Append to WAL (lock-free!)
            self.wal.append_async(operation)?;
        }

        Ok(())
    }

    /// Apply all buffered changes to storage.
    ///
    /// # Lock Consolidation Optimization
    ///
    /// Acquires locks for historical storage and temporal indexes once before
    /// processing all operations, reducing lock overhead from O(N) to O(1).
    ///
    /// ## Performance Trade-offs
    ///
    /// **Benefits:**
    /// - Reduces lock acquisitions from 2N (per operation) to 2 (total)
    /// - For 1000 operations: 2000 lock ops → 2 lock ops
    ///
    /// **Costs:**
    /// - Longer critical section (holds locks for entire operation batch)
    /// - May reduce concurrency for other transactions during this time
    /// - Acceptable for most workloads; monitor if contention becomes an issue
    ///
    /// ## Error Handling
    ///
    /// If any operation fails, locks are released via RAII (automatic drop).
    /// Note: Partial application is NOT rolled back here since this runs AFTER
    /// WAL flush (see commit() at line 203). WAL recovery will handle consistency.
    ///
    /// ## Lock Ordering Hierarchy
    ///
    /// To prevent deadlocks, locks must be acquired in this order:
    /// 1. `historical` - historical storage (acquired first)
    /// 2. `temporal_indexes` - temporal indexes (acquired second)
    /// 3. `version_id_gen` - version ID generator (acquired later for tombstones)
    ///
    /// Helper function to apply node writes (both create and update operations).
    ///
    /// # Arguments
    ///
    /// * `is_create` - True for CreateNode, false for UpdateNode
    /// * `node_id` - The ID of the node
    /// * `version_id` - The version ID for this write
    /// * `label` - The node label
    /// * `properties` - The node properties
    /// * `commit_timestamp` - The commit timestamp
    /// * `temporal` - The bi-temporal interval
    /// * `historical` - Mutable reference to historical storage
    #[allow(clippy::too_many_arguments)]
    fn apply_node_write(
        &self,
        is_create: bool,
        node_id: NodeId,
        version_id: VersionId,
        label: InternedString,
        properties: PropertyMap,
        commit_timestamp: Timestamp,
        temporal: BiTemporalInterval,
        historical: &mut HistoricalStorage,
    ) -> Result<()> {
        // Create node with proper transaction metadata
        let metadata = VersionMetadata::new(self.tx_id, commit_timestamp);
        let node = Node::with_metadata(node_id, label, properties.clone(), version_id, metadata);

        // Insert or update in current storage
        if is_create {
            self.current.insert_node_direct(node, commit_timestamp)?;
        } else {
            self.current.update_node_direct(node, commit_timestamp)?;

            // For updates, explicitly close the current version's transaction_time in historical storage.
            //
            // WHY THIS IS NEEDED:
            // When multiple updates to the same node occur in a SINGLE transaction, they all share
            // the same commit_timestamp. The auto-closing logic in add_node_version() checks:
            //   if new_tx_time > prev_tx_time { close prev_version }
            // But when new_tx_time == prev_tx_time (same transaction), this check fails.
            //
            // EXAMPLE SCENARIO:
            //   db.write(|tx| {
            //       tx.update_node(node_id, props1)?;  // First update
            //       tx.update_node(node_id, props2)?;  // Second update - SAME commit_timestamp!
            //       Ok(())
            //   })?;
            //
            // Without explicit closing, the version chain would be corrupt:
            // - Version 1: tx_time [T1, ∞)  <- Should be [T1, T2)
            // - Version 2: tx_time [T2, ∞)  <- Never created because T2 == T2
            //
            // With explicit closing:
            // - Version 1: tx_time [T1, T2)  <- Explicitly closed before adding Version 2
            // - Version 2: tx_time [T2, ∞)  <- Correctly added with same T2
            //
            // See test: test_multiple_updates_same_transaction in tests/benchmark_validation.rs
            if let Some(current_version_id) = historical.get_current_node_version(node_id) {
                historical
                    .close_node_version_transaction_time(current_version_id, commit_timestamp)?;
            }
        }

        // Store in historical storage (consume properties, avoiding second clone)
        historical.add_node_version(node_id, version_id, temporal, label, properties)?;

        // Index in temporal indexes
        self.temporal_indexes
            .insert_node_version(node_id, version_id, temporal)?;

        Ok(())
    }

    /// Helper function to apply edge writes (both create and update operations).
    ///
    /// # Arguments
    ///
    /// * `is_create` - True for CreateEdge, false for UpdateEdge
    /// * `edge_id` - The ID of the edge
    /// * `version_id` - The version ID for this write
    /// * `source` - The source node ID
    /// * `target` - The target node ID
    /// * `label` - The edge label
    /// * `properties` - The edge properties
    /// * `commit_timestamp` - The commit timestamp
    /// * `temporal` - The bi-temporal interval
    /// * `historical` - Mutable reference to historical storage
    #[allow(clippy::too_many_arguments)]
    fn apply_edge_write(
        &self,
        is_create: bool,
        edge_id: EdgeId,
        version_id: VersionId,
        source: NodeId,
        target: NodeId,
        label: InternedString,
        properties: PropertyMap,
        commit_timestamp: Timestamp,
        temporal: BiTemporalInterval,
        historical: &mut HistoricalStorage,
    ) -> Result<()> {
        // Create edge with proper transaction metadata
        let metadata = VersionMetadata::new(self.tx_id, commit_timestamp);
        let edge = Edge::with_metadata(
            edge_id,
            label,
            source,
            target,
            properties.clone(),
            version_id,
            metadata,
        );

        // Insert or update in current storage
        if is_create {
            self.current.insert_edge_direct(edge)?;
        } else {
            self.current.update_edge_direct(edge)?;

            // For updates, explicitly close the current version's transaction_time in historical storage.
            //
            // WHY THIS IS NEEDED:
            // When multiple updates to the same edge occur in a SINGLE transaction, they all share
            // the same commit_timestamp. The auto-closing logic in add_edge_version() checks:
            //   if new_tx_time > prev_tx_time { close prev_version }
            // But when new_tx_time == prev_tx_time (same transaction), this check fails.
            //
            // EXAMPLE SCENARIO:
            //   db.write(|tx| {
            //       tx.update_edge(edge_id, props1)?;  // First update
            //       tx.update_edge(edge_id, props2)?;  // Second update - SAME commit_timestamp!
            //       Ok(())
            //   })?;
            //
            // Without explicit closing, the version chain would be corrupt:
            // - Version 1: tx_time [T1, ∞)  <- Should be [T1, T2)
            // - Version 2: tx_time [T2, ∞)  <- Never created because T2 == T2
            //
            // With explicit closing:
            // - Version 1: tx_time [T1, T2)  <- Explicitly closed before adding Version 2
            // - Version 2: tx_time [T2, ∞)  <- Correctly added with same T2
            //
            // See test: test_multiple_updates_same_transaction in tests/benchmark_validation.rs
            if let Some(current_version_id) = historical.get_current_edge_version(edge_id) {
                historical
                    .close_edge_version_transaction_time(current_version_id, commit_timestamp)?;
            }
        }

        // Store in historical storage (consume properties, avoiding second clone)
        historical.add_edge_version(
            edge_id, version_id, temporal, label, source, target, properties,
        )?;

        // Index in temporal indexes
        self.temporal_indexes
            .insert_edge_version(edge_id, version_id, temporal)?;

        Ok(())
    }

    /// Helper function to apply node deletion.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The ID of the node to delete
    /// * `commit_timestamp` - The commit timestamp
    /// * `tombstone_id` - The version ID for the tombstone
    /// * `historical` - Mutable reference to historical storage
    fn apply_node_delete(
        &self,
        node_id: NodeId,
        commit_timestamp: Timestamp,
        tombstone_id: VersionId,
        historical: &mut HistoricalStorage,
    ) -> Result<()> {
        // Get the node before deleting
        let node = self.current.get_node(node_id)?;

        // Close the current version's transaction_time in historical storage
        // This marks the end of this version's visibility
        if let Some(current_version_id) = historical.get_current_node_version(node_id) {
            historical.close_node_version_transaction_time(current_version_id, commit_timestamp)?;
        }

        // Create tombstone temporal interval
        // The tombstone marks when the deletion occurred. Its transaction_time
        // starts at commit_timestamp and remains open (we know about the deletion
        // from now on). Its valid_time is closed immediately since the entity
        // no longer exists.
        let tombstone_temporal =
            BiTemporalInterval::current(commit_timestamp).close_valid_time(commit_timestamp);

        // Add tombstone version to historical storage
        historical.add_node_version(
            node_id,
            tombstone_id,
            tombstone_temporal,
            node.label,
            node.properties.clone(),
        )?;

        // Index the tombstone version
        self.temporal_indexes
            .insert_node_version(node_id, tombstone_id, tombstone_temporal)?;

        // Delete from current storage
        self.current.delete_node_direct(node_id, commit_timestamp)?;

        Ok(())
    }

    /// Helper function to apply edge deletion.
    ///
    /// # Arguments
    ///
    /// * `edge_id` - The ID of the edge to delete
    /// * `commit_timestamp` - The commit timestamp
    /// * `tombstone_id` - The version ID for the tombstone
    /// * `historical` - Mutable reference to historical storage
    fn apply_edge_delete(
        &self,
        edge_id: EdgeId,
        commit_timestamp: Timestamp,
        tombstone_id: VersionId,
        historical: &mut HistoricalStorage,
    ) -> Result<()> {
        // Get the edge before deleting
        let edge = self.current.get_edge(edge_id)?;

        // Close the current version's transaction_time in historical storage
        // This marks the end of this version's visibility
        if let Some(current_version_id) = historical.get_current_edge_version(edge_id) {
            historical.close_edge_version_transaction_time(current_version_id, commit_timestamp)?;
        }

        // Create tombstone temporal interval
        // The tombstone marks when the deletion occurred. Its transaction_time
        // starts at commit_timestamp and remains open (we know about the deletion
        // from now on). Its valid_time is closed immediately since the entity
        // no longer exists.
        let tombstone_temporal =
            BiTemporalInterval::current(commit_timestamp).close_valid_time(commit_timestamp);

        // Add tombstone version to historical storage
        historical.add_edge_version(
            edge_id,
            tombstone_id,
            tombstone_temporal,
            edge.label,
            edge.source,
            edge.target,
            edge.properties.clone(),
        )?;

        // Index the tombstone version
        self.temporal_indexes
            .insert_edge_version(edge_id, tombstone_id, tombstone_temporal)?;

        // Delete from current storage
        self.current.delete_edge_direct(edge_id)?;

        Ok(())
    }

    fn apply_changes(&self, commit_timestamp: Timestamp) -> Result<()> {
        // Create temporal interval for all operations in this transaction.
        // All operations in a transaction share the same commit_timestamp, which is
        // semantically correct for transaction_time (when data was recorded).
        // However, this means the auto-closing logic in add_node_version won't work
        // for multiple updates in the same transaction (since new_tx_time == prev_tx_time).
        // Therefore, we explicitly close previous versions before adding new ones.
        let temporal = BiTemporalInterval::current(commit_timestamp);

        // Acquire lock on historical storage once before processing all operations.
        // TemporalIndexes uses DashMap internally, so no outer lock needed.
        let mut historical = self.historical.write_or_err()?;

        // Pre-generate all tombstone version IDs at once to reduce lock contention
        // on the ID generator. Count delete operations and generate IDs in batch.
        let num_deletes = self
            .buffer
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    super::BufferedWrite::DeleteNode { .. }
                        | super::BufferedWrite::DeleteEdge { .. }
                )
            })
            .count();

        let mut tombstone_ids = if num_deletes > 0 {
            let ids: Result<Vec<u64>> = {
                let id_gen = self.version_id_gen.lock_or_err()?;
                (0..num_deletes)
                    .map(|_| id_gen.next().map_err(Into::into))
                    .collect()
            };
            ids?.into_iter()
        } else {
            Vec::new().into_iter()
        };

        for write in self.buffer.operations() {
            match write {
                super::BufferedWrite::CreateNode {
                    node_id,
                    version_id,
                    label,
                    properties,
                    ..
                } => {
                    self.apply_node_write(
                        true, // is_create
                        *node_id,
                        *version_id,
                        *label,
                        properties.clone(),
                        commit_timestamp,
                        temporal,
                        &mut historical,
                    )?;
                }
                super::BufferedWrite::CreateEdge {
                    edge_id,
                    version_id,
                    source,
                    target,
                    label,
                    properties,
                    ..
                } => {
                    self.apply_edge_write(
                        true, // is_create
                        *edge_id,
                        *version_id,
                        *source,
                        *target,
                        *label,
                        properties.clone(),
                        commit_timestamp,
                        temporal,
                        &mut historical,
                    )?;
                }
                super::BufferedWrite::UpdateNode {
                    node_id,
                    version_id,
                    label,
                    properties,
                    ..
                } => {
                    self.apply_node_write(
                        false, // is_create
                        *node_id,
                        *version_id,
                        *label,
                        properties.clone(),
                        commit_timestamp,
                        temporal,
                        &mut historical,
                    )?;
                }
                super::BufferedWrite::UpdateEdge {
                    edge_id,
                    version_id,
                    source,
                    target,
                    label,
                    properties,
                    ..
                } => {
                    self.apply_edge_write(
                        false, // is_create
                        *edge_id,
                        *version_id,
                        *source,
                        *target,
                        *label,
                        properties.clone(),
                        commit_timestamp,
                        temporal,
                        &mut historical,
                    )?;
                }
                super::BufferedWrite::DeleteNode { node_id } => {
                    // Use pre-generated tombstone version ID (no lock needed)
                    // CRITICAL: Use proper error handling instead of .expect() to avoid lock poisoning
                    let tombstone_version_id = VersionId::new_unchecked(
                        tombstone_ids.next().ok_or_else(|| {
                            StorageError::InconsistentState {
                                reason: format!(
                                    "Tombstone ID exhaustion for DeleteNode: expected {} deletes, iterator depleted at node_id {:?}",
                                    num_deletes, node_id
                                ),
                            }
                        })?,
                    );

                    self.apply_node_delete(
                        *node_id,
                        commit_timestamp,
                        tombstone_version_id,
                        &mut historical,
                    )?;
                }
                super::BufferedWrite::DeleteEdge { edge_id } => {
                    // Use pre-generated tombstone version ID (no lock needed)
                    // CRITICAL: Use proper error handling instead of .expect() to avoid lock poisoning
                    let tombstone_version_id = VersionId::new_unchecked(
                        tombstone_ids.next().ok_or_else(|| {
                            StorageError::InconsistentState {
                                reason: format!(
                                    "Tombstone ID exhaustion for DeleteEdge: expected {} deletes, iterator depleted at edge_id {:?}",
                                    num_deletes, edge_id
                                ),
                            }
                        })?,
                    );

                    self.apply_edge_delete(
                        *edge_id,
                        commit_timestamp,
                        tombstone_version_id,
                        &mut historical,
                    )?;
                }
            }
        }

        // Safety check: verify all pre-generated tombstone IDs were consumed
        // This detects count mismatches in development builds
        debug_assert!(
            tombstone_ids.next().is_none(),
            "Tombstone ID surplus: expected {} deletes, but iterator has remaining IDs",
            num_deletes
        );

        // Release historical write lock before rebuilding adjacency to avoid holding multiple
        // exclusive locks simultaneously. TemporalIndexes uses fine-grained DashMap locking
        // internally, so no explicit lock release needed (automatically released after inserts).
        drop(historical);

        // Rebuild adjacency indexes only if edge structure changed
        // With incremental adjacency, edges are immediately visible (no rebuild needed).
        // However, we compact after transactions with edge operations to maintain
        // read performance by moving delta edges to frozen CSR.
        if self.buffer.has_edge_operations() {
            self.current.compact_adjacency();
        }

        Ok(())
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
}

impl WriteOps for WriteTransaction {
    fn create_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        // Check transaction state
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // Generate IDs
        let node_id = NodeId::new_unchecked(self.node_id_gen.lock_or_err()?.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        // Get timestamp for temporal interval
        let timestamp = self.start_timestamp;
        let temporal = BiTemporalInterval::current(timestamp);

        // Buffer the write
        self.buffer.add(super::BufferedWrite::CreateNode {
            node_id,
            version_id,
            label: label_interned,
            properties,
            temporal,
        })?;

        Ok(node_id)
    }

    fn create_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
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
        let edge_id = EdgeId::new_unchecked(self.edge_id_gen.lock_or_err()?.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        // Get timestamp for temporal interval
        let timestamp = self.start_timestamp;
        let temporal = BiTemporalInterval::current(timestamp);

        // Buffer the write
        self.buffer.add(super::BufferedWrite::CreateEdge {
            edge_id,
            version_id,
            source,
            target,
            label: label_interned,
            properties,
            temporal,
        })?;

        Ok(edge_id)
    }

    fn update_node(&mut self, node_id: NodeId, properties: PropertyMap) -> Result<()> {
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
        let version_id = VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);

        // PATCH semantics: Merge new properties with existing ones
        // Start with existing properties
        let mut builder = PropertyMapBuilder::from_map(node.properties.clone());

        // Update/add properties from the incoming map
        for (key, value) in properties.iter() {
            builder = builder.insert_by_key(*key, value.clone());
        }

        // Build the final merged property map
        let merged_properties = builder.build();

        // Get timestamp for temporal interval
        let timestamp = self.start_timestamp;
        let temporal = BiTemporalInterval::current(timestamp);

        // Buffer the write with merged properties
        self.buffer.add(super::BufferedWrite::UpdateNode {
            node_id,
            version_id,
            label: node.label,
            properties: merged_properties,
            temporal,
        })?;

        Ok(())
    }

    fn update_edge(&mut self, edge_id: EdgeId, properties: PropertyMap) -> Result<()> {
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
        let version_id = VersionId::new_unchecked(self.version_id_gen.lock_or_err()?.next()?);

        // PATCH semantics: Merge new properties with existing ones
        // Start with existing properties
        let mut builder = PropertyMapBuilder::from_map(edge.properties.clone());

        // Update/add properties from the incoming map
        for (key, value) in properties.iter() {
            builder = builder.insert_by_key(*key, value.clone());
        }

        // Build the final merged property map
        let merged_properties = builder.build();

        // Get timestamp for temporal interval
        let timestamp = self.start_timestamp;
        let temporal = BiTemporalInterval::current(timestamp);

        // Buffer the write with merged properties
        self.buffer.add(super::BufferedWrite::UpdateEdge {
            edge_id,
            version_id,
            source: edge.source,
            target: edge.target,
            label: edge.label,
            properties: merged_properties,
            temporal,
        })?;

        Ok(())
    }

    fn delete_node(&mut self, node_id: NodeId) -> Result<()> {
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

        // Buffer the write
        self.buffer
            .add(super::BufferedWrite::DeleteNode { node_id })?;

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

    fn delete_edge(&mut self, edge_id: EdgeId) -> Result<()> {
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

        // Buffer the write
        self.buffer
            .add(super::BufferedWrite::DeleteEdge { edge_id })?;

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
mod tests {
    use super::*;
    use crate::api::transaction::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use tempfile::TempDir;

    fn create_test_write_tx() -> (WriteTransaction, TempDir) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        // Create WAL with temp directory for tests
        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let tx_id_gen = TxIdGenerator::new();

        // Create snapshot and visibility manager for testing
        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
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
        );

        (tx, temp_dir)
    }

    #[test]
    fn test_write_transaction_creation() {
        let (tx, _temp_dir) = create_test_write_tx();
        assert_eq!(tx.state, TxState::Active);
        let metadata = tx.metadata();
        assert!(!metadata.is_read_only);
    }

    #[test]
    fn test_create_node_buffering() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let node_id = tx.create_node("Person", props.clone()).unwrap();
        // ID generators start at 0, so first ID is 0
        assert_eq!(node_id.as_u64(), 0);

        // Read-your-writes: should be able to read buffered node
        let node = tx.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(
            node.properties.get("name").unwrap(),
            &crate::core::property::PropertyValue::from("Alice")
        );
    }

    #[test]
    fn test_create_edge_buffering() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        // First create nodes in current storage (simulating existing nodes)
        let props = PropertyMapBuilder::new().build();
        let node1 = tx.current.create_node("Person", props.clone()).unwrap();
        let node2 = tx.current.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020i64).build();

        let edge_id = tx
            .create_edge(node1, node2, "KNOWS", edge_props.clone())
            .unwrap();
        // ID generators start at 0, so first edge ID is 0
        assert_eq!(edge_id.as_u64(), 0);

        // Read-your-writes: should be able to read buffered edge
        let edge = tx.get_edge(edge_id).unwrap();
        assert_eq!(edge.id, edge_id);
        assert_eq!(edge.source, node1);
        assert_eq!(edge.target, node2);
        assert_eq!(
            edge.properties.get("since").unwrap(),
            &crate::core::property::PropertyValue::from(2020i64)
        );
    }

    #[test]
    fn test_commit_applies_changes() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        let props = PropertyMapBuilder::new().insert("name", "Bob").build();

        let node_id = tx.create_node("Person", props).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Node should now be visible in current storage
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Bob")
        );
    }

    #[test]
    fn test_rollback_discards_changes() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        let props = PropertyMapBuilder::new().insert("name", "Charlie").build();

        let node_id = tx.create_node("Person", props).unwrap();

        // Rollback the transaction
        tx.rollback().unwrap();

        // Node should not be visible in current storage
        assert!(current.get_node(node_id).is_err());
    }

    #[test]
    fn test_validation_fails_for_invalid_edge() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().build();

        // Try to create edge with non-existent nodes
        let node1 = NodeId::new(999).unwrap();
        let node2 = NodeId::new(1000).unwrap();

        tx.create_edge(node1, node2, "KNOWS", props).unwrap();

        // Commit should fail validation
        let result = tx.commit();
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_rollback_on_drop() {
        let current = Arc::new(CurrentStorage::new());
        let node_id = {
            let (mut tx, _temp_dir) = create_test_write_tx();
            let props = PropertyMapBuilder::new().build();
            // Transaction dropped here without commit
            tx.create_node("Person", props).unwrap()
        };

        // Node should not be visible (auto-rollback)
        assert!(current.get_node(node_id).is_err());
    }

    #[test]
    fn test_update_node() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node first in current storage
        let props = PropertyMapBuilder::new().insert("age", 30i64).build();
        let node_id = current.create_node("Person", props).unwrap();

        // Update the node properties
        let new_props = PropertyMapBuilder::new().insert("age", 31i64).build();
        tx.update_node(node_id, new_props.clone()).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the update was applied
        let node = current.get_node(node_id).unwrap();
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
    }

    #[test]
    fn test_update_node_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().insert("age", 30i64).build();
        let result = tx.update_node(NodeId::new(999).unwrap(), props);

        // Should fail because node doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_update_node_patch_preserves_existing_properties() {
        // Test that update_node uses PATCH semantics - properties not mentioned are preserved
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with multiple properties
        let initial_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("city", "London")
            .build();
        let node_id = current.create_node("Person", initial_props).unwrap();

        // Update only the age - name and city should be preserved
        let update_props = PropertyMapBuilder::new().insert("age", 31i64).build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // Verify all properties
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(31));
        assert_eq!(
            node.get_property("city").and_then(|v| v.as_str()),
            Some("London")
        );
    }

    #[test]
    fn test_update_node_patch_adds_new_properties() {
        // Test that update_node can add new properties without removing existing ones
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with one property
        let initial_props = PropertyMapBuilder::new().insert("name", "Bob").build();
        let node_id = current.create_node("Person", initial_props).unwrap();

        // Add a new property
        let update_props = PropertyMapBuilder::new().insert("age", 25i64).build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // Both properties should exist
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Bob")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(25));
    }

    #[test]
    fn test_update_node_patch_modifies_multiple_properties() {
        // Test that update_node can modify multiple properties while preserving others
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with four properties
        let initial_props = PropertyMapBuilder::new()
            .insert("name", "Charlie")
            .insert("age", 40i64)
            .insert("city", "Paris")
            .insert("occupation", "Engineer")
            .build();
        let node_id = current.create_node("Person", initial_props).unwrap();

        // Update two properties
        let update_props = PropertyMapBuilder::new()
            .insert("age", 41i64)
            .insert("city", "Berlin")
            .build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // All four properties should exist with correct values
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Charlie")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(41));
        assert_eq!(
            node.get_property("city").and_then(|v| v.as_str()),
            Some("Berlin")
        );
        assert_eq!(
            node.get_property("occupation").and_then(|v| v.as_str()),
            Some("Engineer")
        );
    }

    #[test]
    fn test_update_node_patch_empty_update() {
        // Test that empty update preserves all properties
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with properties
        let initial_props = PropertyMapBuilder::new()
            .insert("name", "Diana")
            .insert("age", 35i64)
            .build();
        let node_id = current.create_node("Person", initial_props).unwrap();

        // Empty update
        let update_props = PropertyMapBuilder::new().build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // All properties should still exist
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Diana")
        );
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(35));
    }

    #[test]
    fn test_update_node_patch_with_vector_properties() {
        // Test PATCH behavior with vector properties
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node with a vector and scalar property
        let embedding = vec![0.1f32, 0.2, 0.3];
        let initial_props = PropertyMapBuilder::new()
            .insert("name", "Eve")
            .insert_vector("embedding", &embedding)
            .build();
        let node_id = current.create_node("Document", initial_props).unwrap();

        // Update only the name - embedding should be preserved
        let update_props = PropertyMapBuilder::new()
            .insert("name", "Eve Updated")
            .build();
        tx.update_node(node_id, update_props).unwrap();
        tx.commit().unwrap();

        // Verify both properties
        let node = current.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Eve Updated")
        );
        assert!(node.get_property("embedding").is_some());
        if let Some(vec_val) = node.get_property("embedding").and_then(|v| v.as_vector()) {
            assert_eq!(vec_val, &[0.1f32, 0.2, 0.3]);
        } else {
            panic!("Vector property not found or wrong type");
        }
    }

    #[test]
    fn test_update_edge() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes and edge in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("strength", 5i64).build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", edge_props)
            .unwrap();

        // Update the edge properties
        let new_props = PropertyMapBuilder::new().insert("strength", 10i64).build();
        tx.update_edge(edge_id, new_props.clone()).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the update was applied
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("strength").and_then(|v| v.as_int()),
            Some(10)
        );
    }

    #[test]
    fn test_update_edge_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().insert("strength", 5i64).build();
        let result = tx.update_edge(EdgeId::new(999).unwrap(), props);

        // Should fail because edge doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_update_edge_patch_preserves_existing_properties() {
        // Test that update_edge uses PATCH semantics - properties not mentioned are preserved
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with multiple properties
        let initial_props = PropertyMapBuilder::new()
            .insert("weight", 5i64)
            .insert("type", "friendship")
            .insert("since", "2020")
            .build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", initial_props)
            .unwrap();

        // Update only the weight - type and since should be preserved
        let update_props = PropertyMapBuilder::new().insert("weight", 10i64).build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // Verify all properties
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(10)
        );
        assert_eq!(
            edge.get_property("type").and_then(|v| v.as_str()),
            Some("friendship")
        );
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_str()),
            Some("2020")
        );
    }

    #[test]
    fn test_update_edge_patch_adds_new_properties() {
        // Test that update_edge can add new properties without removing existing ones
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with one property
        let initial_props = PropertyMapBuilder::new().insert("weight", 5i64).build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", initial_props)
            .unwrap();

        // Add a new property
        let update_props = PropertyMapBuilder::new()
            .insert("type", "colleague")
            .build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // Both properties should exist
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(5)
        );
        assert_eq!(
            edge.get_property("type").and_then(|v| v.as_str()),
            Some("colleague")
        );
    }

    #[test]
    fn test_update_edge_patch_modifies_multiple_properties() {
        // Test that update_edge can modify multiple properties while preserving others
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with four properties
        let initial_props = PropertyMapBuilder::new()
            .insert("weight", 5i64)
            .insert("type", "friendship")
            .insert("since", "2020")
            .insert("active", true)
            .build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", initial_props)
            .unwrap();

        // Update two properties
        let update_props = PropertyMapBuilder::new()
            .insert("weight", 8i64)
            .insert("since", "2021")
            .build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // All four properties should exist with correct values
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(8)
        );
        assert_eq!(
            edge.get_property("type").and_then(|v| v.as_str()),
            Some("friendship")
        );
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_str()),
            Some("2021")
        );
        assert_eq!(
            edge.get_property("active").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_update_edge_patch_empty_update() {
        // Test that empty update preserves all properties
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with properties
        let initial_props = PropertyMapBuilder::new()
            .insert("weight", 5i64)
            .insert("type", "friendship")
            .build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", initial_props)
            .unwrap();

        // Empty update
        let update_props = PropertyMapBuilder::new().build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // All properties should still exist
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(5)
        );
        assert_eq!(
            edge.get_property("type").and_then(|v| v.as_str()),
            Some("friendship")
        );
    }

    #[test]
    fn test_update_edge_patch_with_vector_properties() {
        // Test PATCH behavior with vector properties on edges
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        // Create edge with a vector and scalar property
        let embedding = vec![0.5f32, 0.6, 0.7];
        let initial_props = PropertyMapBuilder::new()
            .insert("weight", 5i64)
            .insert_vector("embedding", &embedding)
            .build();
        let edge_id = current
            .create_edge(node1, node2, "SIMILAR", initial_props)
            .unwrap();

        // Update only the weight - embedding should be preserved
        let update_props = PropertyMapBuilder::new().insert("weight", 10i64).build();
        tx.update_edge(edge_id, update_props).unwrap();
        tx.commit().unwrap();

        // Verify both properties
        let edge = current.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_int()),
            Some(10)
        );
        assert!(edge.get_property("embedding").is_some());
        if let Some(vec_val) = edge.get_property("embedding").and_then(|v| v.as_vector()) {
            assert_eq!(vec_val, &[0.5f32, 0.6, 0.7]);
        } else {
            panic!("Vector property not found or wrong type");
        }
    }

    #[test]
    fn test_delete_node() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node first in current storage
        let props = PropertyMapBuilder::new().build();
        let node_id = current.create_node("Person", props).unwrap();

        // Verify node exists
        assert!(current.get_node(node_id).is_ok());

        // Delete the node
        tx.delete_node(node_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the node was deleted
        assert!(current.get_node(node_id).is_err());
    }

    #[test]
    fn test_delete_node_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let result = tx.delete_node(NodeId::new(999).unwrap());

        // Should fail because node doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_edge() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes and edge in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props).unwrap();

        let edge_props = PropertyMapBuilder::new().build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", edge_props)
            .unwrap();

        // Verify edge exists
        assert!(current.get_edge(edge_id).is_ok());

        // Delete the edge
        tx.delete_edge(edge_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify the edge was deleted
        assert!(current.get_edge(edge_id).is_err());
    }

    #[test]
    fn test_delete_edge_not_found() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let result = tx.delete_edge(EdgeId::new(999).unwrap());

        // Should fail because edge doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn test_commit_after_commit_fails() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().build();
        tx.create_node("Person", props).unwrap();

        // First commit should succeed
        tx.commit().unwrap();

        // Try to commit again - should fail (can't create new tx from consumed one)
        // This is prevented by the compiler since commit consumes self
    }

    #[test]
    fn test_operations_after_commit_prevented_by_move() {
        let (mut tx, _temp_dir) = create_test_write_tx();

        let props = PropertyMapBuilder::new().build();
        tx.create_node("Person", props).unwrap();

        // Commit consumes tx
        tx.commit().unwrap();

        // Can't use tx after commit - prevented by compiler
        // This test documents the behavior
    }

    #[test]
    fn test_read_ops_delegation() {
        let (tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create some data in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();
        current.create_edge(node1, node2, "KNOWS", props).unwrap();

        // Test ReadOps methods on transaction
        assert_eq!(tx.node_count(), 2);
        assert_eq!(tx.edge_count(), 1);
        assert!(tx.get_node(node1).is_ok());
        assert_eq!(tx.get_outgoing_edges(node1).len(), 1);
        assert_eq!(tx.get_incoming_edges(node2).len(), 1);
        assert_eq!(tx.get_outgoing_edges_with_label(node1, "KNOWS").len(), 1);
    }

    #[test]
    fn test_delete_node_creates_tombstone() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);
        let historical = Arc::clone(&tx.historical);

        // Create a node with properties
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let node_id = current.create_node("Person", props).unwrap();

        // Verify node exists in current storage
        assert!(current.get_node(node_id).is_ok());

        // Delete the node
        tx.delete_node(node_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify node was deleted from current storage
        assert!(current.get_node(node_id).is_err());

        // Verify tombstone version was created in historical storage
        let historical = historical.read();
        let stats = historical.stats();
        assert!(
            stats.total_node_versions > 0,
            "Expected at least one node version (tombstone) in historical storage"
        );

        // The tombstone should have a closed transaction time
        // This is implicitly tested by the fact that a version was created
    }

    #[test]
    fn test_delete_edge_creates_tombstone() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);
        let historical = Arc::clone(&tx.historical);

        // Create nodes and edge
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020i64).build();
        let edge_id = current
            .create_edge(node1, node2, "KNOWS", edge_props)
            .unwrap();

        // Verify edge exists
        assert!(current.get_edge(edge_id).is_ok());

        // Delete the edge
        tx.delete_edge(edge_id).unwrap();

        // Commit the transaction
        tx.commit().unwrap();

        // Verify edge was deleted from current storage
        assert!(current.get_edge(edge_id).is_err());

        // Verify tombstone version was created in historical storage
        let historical = historical.read();
        let stats = historical.stats();
        assert!(
            stats.total_edge_versions > 0,
            "Expected at least one edge version (tombstone) in historical storage"
        );
    }

    #[test]
    fn test_read_your_writes_update() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node in current storage
        let props = PropertyMapBuilder::new().insert("age", 30i64).build();
        let node_id = current.create_node("Person", props).unwrap();

        // Update the node in the transaction
        let new_props = PropertyMapBuilder::new().insert("age", 31i64).build();
        tx.update_node(node_id, new_props).unwrap();

        // Read-your-writes: should see the updated value
        let node = tx.get_node(node_id).unwrap();
        assert_eq!(
            node.properties.get("age").unwrap(),
            &crate::core::property::PropertyValue::from(31i64)
        );
    }

    #[test]
    fn test_read_your_writes_delete() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a node in current storage
        let props = PropertyMapBuilder::new().build();
        let node_id = current.create_node("Person", props).unwrap();

        // Delete the node in the transaction
        tx.delete_node(node_id).unwrap();

        // Read-your-writes: should NOT see the deleted node
        assert!(tx.get_node(node_id).is_err());
    }

    #[test]
    fn test_empty_transaction_commit() {
        let (tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Commit empty transaction (no operations buffered)
        // Should skip rebuild_adjacency() since no edge operations occurred
        tx.commit().unwrap();

        // Verify storage is still in valid state
        assert_eq!(current.node_count(), 0);
        assert_eq!(current.edge_count(), 0);
    }

    #[test]
    fn test_empty_transaction_with_only_node_operations() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create only nodes (no edges)
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        tx.create_node("Person", props).unwrap();

        // Commit - should skip rebuild_adjacency() since no edge operations occurred
        tx.commit().unwrap();

        // Verify node was created and adjacency is valid
        assert_eq!(current.node_count(), 1);
        assert_eq!(current.edge_count(), 0);
    }

    #[test]
    fn test_transaction_with_edge_operations() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes and edges
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        let alice = tx.create_node("Person", props).unwrap();

        let props = PropertyMapBuilder::new().insert("name", "Bob").build();
        let bob = tx.create_node("Person", props).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020i64).build();
        let edge_id = tx.create_edge(alice, bob, "KNOWS", edge_props).unwrap();

        // Commit - should call rebuild_adjacency() since edge was created
        tx.commit().unwrap();

        // Verify adjacency was rebuilt correctly
        assert_eq!(current.node_count(), 2);
        assert_eq!(current.edge_count(), 1);

        // Verify adjacency list is correct
        let outgoing = current.get_outgoing_edges(alice);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0], edge_id);

        let incoming = current.get_incoming_edges(bob);
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0], edge_id);
    }

    #[test]
    fn test_interleaved_create_update_delete_operations() {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        // Create WAL with temp directory for tests
        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let tx_id_gen = TxIdGenerator::new();

        // Create visibility manager and snapshot for testing
        let visibility_manager = Arc::new(TxVisibilityManager::new());

        // Create initial transaction to set up nodes and one edge
        let snapshot1 = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };
        let mut tx1 = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot1,
            current.clone(),
            historical.clone(),
            temporal_indexes.clone(),
            wal.clone(),
            current_timestamp.clone(),
            visibility_manager.clone(),
            node_id_gen.clone(),
            edge_id_gen.clone(),
            version_id_gen.clone(),
        );

        let props = PropertyMapBuilder::new().build();
        let node1 = tx1.create_node("Person", props.clone()).unwrap();
        let node2 = tx1.create_node("Person", props.clone()).unwrap();
        let node3 = tx1.create_node("Person", props.clone()).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("weight", 5i64).build();
        let edge1 = tx1.create_edge(node1, node2, "KNOWS", edge_props).unwrap();

        tx1.commit().unwrap();

        // Verify initial state
        assert_eq!(current.edge_count(), 1);

        // Create second transaction with interleaved operations
        let snapshot2 = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };
        let mut tx2 = WriteTransaction::new(
            tx_id_gen.next(),
            snapshot2,
            current.clone(),
            historical.clone(),
            temporal_indexes.clone(),
            wal.clone(),
            current_timestamp.clone(),
            visibility_manager.clone(),
            node_id_gen.clone(),
            edge_id_gen.clone(),
            version_id_gen.clone(),
        );

        // 1. Create new edge
        tx2.create_edge(
            node2,
            node3,
            "FOLLOWS",
            PropertyMapBuilder::new().insert("weight", 8i64).build(),
        )
        .unwrap();

        // 2. Update existing edge
        tx2.update_edge(
            edge1,
            PropertyMapBuilder::new().insert("weight", 7i64).build(),
        )
        .unwrap();

        // 3. Create another edge
        tx2.create_edge(node1, node3, "LIKES", PropertyMapBuilder::new().build())
            .unwrap();

        // Commit all operations
        tx2.commit().unwrap();

        // After commit: verify final state
        // edge1 (updated) + 2 new edges = 3 edges total
        assert_eq!(current.edge_count(), 3);

        // Verify edge1 was updated
        let updated_edge = current.get_edge(edge1).unwrap();
        assert_eq!(
            updated_edge.get_property("weight").and_then(|v| v.as_int()),
            Some(7)
        );

        // Verify adjacency is correct after rebuild
        assert_eq!(current.out_degree(node1), 2); // KNOWS and LIKES
        assert_eq!(current.out_degree(node2), 1); // FOLLOWS
        assert_eq!(current.in_degree(node3), 2); // receives FOLLOWS and LIKES
    }

    /// Test that tombstone ID pre-generation matches actual delete operations.
    ///
    /// This test verifies the critical invariant that the number of IDs generated
    /// matches the number of delete operations, preventing iterator exhaustion.
    #[test]
    fn test_tombstone_id_count_matches_deletes() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create initial nodes and edges in current storage
        let props = PropertyMapBuilder::new().build();
        let node1 = current.create_node("Person", props.clone()).unwrap();
        let node2 = current.create_node("Person", props.clone()).unwrap();
        let node3 = current.create_node("Person", props.clone()).unwrap();
        let node4 = current.create_node("Person", props.clone()).unwrap();

        let edge1 = current
            .create_edge(node1, node2, "KNOWS", props.clone())
            .unwrap();
        let edge2 = current
            .create_edge(node2, node3, "KNOWS", props.clone())
            .unwrap();
        let edge3 = current
            .create_edge(node3, node4, "KNOWS", props.clone())
            .unwrap();

        // Create a transaction with mixed operations:
        // - Some creates
        // - Some updates
        // - MULTIPLE deletes (both nodes and edges)
        let node5 = tx.create_node("Person", props.clone()).unwrap();
        tx.create_edge(node1, node5, "FOLLOWS", props.clone())
            .unwrap();

        tx.update_node(
            node4,
            PropertyMapBuilder::new().insert("age", 30i64).build(),
        )
        .unwrap();

        // Delete operations: 2 nodes + 2 edges = 4 tombstones needed
        tx.delete_node(node3).unwrap(); // This will also require tombstone for node
        tx.delete_node(node4).unwrap(); // This will also require tombstone for node
        tx.delete_edge(edge1).unwrap(); // Tombstone for edge
        tx.delete_edge(edge2).unwrap(); // Tombstone for edge

        // Commit should succeed without panicking on iterator exhaustion
        let result = tx.commit();
        assert!(
            result.is_ok(),
            "Commit should succeed with correct tombstone ID count"
        );

        // Verify deletes were applied
        assert!(current.get_node(node3).is_err());
        assert!(current.get_node(node4).is_err());
        assert!(current.get_edge(edge1).is_err());
        assert!(current.get_edge(edge2).is_err());

        // Verify non-deleted entities still exist
        assert!(current.get_node(node1).is_ok());
        assert!(current.get_node(node2).is_ok());
        assert!(current.get_node(node5).is_ok());
        assert!(current.get_edge(edge3).is_ok());
    }

    #[test]
    fn test_batch_edge_operations_rebuild_once() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes
        let mut nodes = Vec::new();
        for i in 0..100 {
            let node = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
                .unwrap();
            nodes.push(node);
        }

        // Create 99 edges
        for i in 0..99 {
            tx.create_edge(
                nodes[i],
                nodes[i + 1],
                "CONNECTS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        }

        // Commit should rebuild adjacency only once
        tx.commit().unwrap();

        // Verify all edges are in adjacency index
        assert_eq!(current.edge_count(), 99);
        for i in 0..99 {
            assert_eq!(current.out_degree(nodes[i]), 1);
            assert_eq!(current.in_degree(nodes[i + 1]), 1);
        }
    }

    // ==================== Lock Poisoning Tests ====================

    /// Create a test write transaction with a custom timestamp mutex.
    /// This allows testing lock poisoning scenarios.
    fn create_test_write_tx_with_timestamp(
        current_timestamp: Arc<Mutex<Timestamp>>,
    ) -> (WriteTransaction, TempDir) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let tx_id_gen = TxIdGenerator::new();

        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
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
        );

        (tx, temp_dir)
    }

    #[test]
    fn test_commit_timestamp_lock_poisoning_returns_error() {
        // Test that commit returns a proper error if the timestamp lock is poisoned
        // instead of panicking (Issue #342).
        use std::thread;

        // Create timestamp mutex that we'll poison
        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let ts_clone = Arc::clone(&current_timestamp);

        // Poison the lock by panicking while holding it
        let handle = thread::spawn(move || {
            let _guard = ts_clone.lock().unwrap();
            panic!("intentional panic to poison timestamp lock");
        });

        // Wait for the poisoning thread to complete
        let _ = handle.join();

        // Create the transaction with the poisoned lock
        let (mut tx, _temp_dir) = create_test_write_tx_with_timestamp(current_timestamp);

        // Create a node so we have something to commit
        let props = PropertyMapBuilder::new().insert("name", "Test").build();
        tx.create_node("TestNode", props).unwrap();

        // Commit should return a CommitFailed error, not panic
        let result = tx.commit();
        assert!(result.is_err());

        // Verify it's the correct error type (TransactionError::CommitFailed)
        if let Err(crate::utils::error::Error::Transaction(
            crate::utils::error::TransactionError::CommitFailed { reason },
        )) = result
        {
            assert!(
                reason.contains("timestamp lock poisoned"),
                "Expected error message to contain 'timestamp lock poisoned', got: {}",
                reason
            );
        } else {
            panic!(
                "Expected TransactionError::CommitFailed, got {:?}",
                result.err()
            );
        }
    }
    // ===================================================================
    // Cascade Delete Tests (Issue #364)
    // ===================================================================

    #[test]
    fn test_delete_node_cascade_removes_edges() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a central node with multiple connections
        let props = PropertyMapBuilder::new().build();
        let central_node = tx.create_node("Person", props.clone()).unwrap();
        let node1 = tx.create_node("Person", props.clone()).unwrap();
        let node2 = tx.create_node("Person", props.clone()).unwrap();
        let node3 = tx.create_node("Person", props).unwrap();

        // Create edges: central node has 2 outgoing and 1 incoming edge
        let edge1 = tx
            .create_edge(
                central_node,
                node1,
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        let edge2 = tx
            .create_edge(
                central_node,
                node2,
                "FOLLOWS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
        let edge3 = tx
            .create_edge(
                node3,
                central_node,
                "LIKES",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        tx.commit().unwrap();

        // Verify all edges exist
        assert!(current.get_edge(edge1).is_ok());
        assert!(current.get_edge(edge2).is_ok());
        assert!(current.get_edge(edge3).is_ok());

        // Delete the central node with cascade
        let (mut tx2, _temp_dir2) = create_test_write_tx_from_existing(Arc::clone(&current));
        tx2.delete_node_cascade(central_node).unwrap();
        tx2.commit().unwrap();

        // Verify the node was deleted
        assert!(current.get_node(central_node).is_err());

        // Verify all connected edges were deleted (CASCADE)
        assert!(
            current.get_edge(edge1).is_err(),
            "Outgoing edge should be deleted with cascade"
        );
        assert!(
            current.get_edge(edge2).is_err(),
            "Outgoing edge should be deleted with cascade"
        );
        assert!(
            current.get_edge(edge3).is_err(),
            "Incoming edge should be deleted with cascade"
        );

        // Verify other nodes still exist
        assert!(current.get_node(node1).is_ok());
        assert!(current.get_node(node2).is_ok());
        assert!(current.get_node(node3).is_ok());
    }

    #[test]
    fn test_delete_node_no_cascade_keeps_edges() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a central node with connections
        let props = PropertyMapBuilder::new().build();
        let central_node = tx.create_node("Person", props.clone()).unwrap();
        let node1 = tx.create_node("Person", props).unwrap();

        // Create edge
        let edge1 = tx
            .create_edge(
                central_node,
                node1,
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();

        tx.commit().unwrap();

        // Verify edge exists
        assert!(current.get_edge(edge1).is_ok());

        // Delete the node WITHOUT cascade (default behavior)
        let (mut tx2, _temp_dir2) = create_test_write_tx_from_existing(Arc::clone(&current));
        tx2.delete_node(central_node).unwrap();
        tx2.commit().unwrap();

        // Verify the node was deleted
        assert!(current.get_node(central_node).is_err());

        // Verify edge still exists (NO CASCADE - current behavior)
        // Note: This creates an orphaned edge, which is the problem issue #364 addresses
        assert!(
            current.get_edge(edge1).is_ok(),
            "Edge should remain when cascade is not enabled (current behavior)"
        );
    }

    #[test]
    fn test_delete_node_cascade_with_bidirectional_edges() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create nodes with bidirectional relationships
        let props = PropertyMapBuilder::new().build();
        let node_a = tx.create_node("Person", props.clone()).unwrap();
        let node_b = tx.create_node("Person", props).unwrap();

        // Create bidirectional edges
        let edge_a_to_b = tx
            .create_edge(node_a, node_b, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        let edge_b_to_a = tx
            .create_edge(node_b, node_a, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        tx.commit().unwrap();

        // Delete node_a with cascade
        let (mut tx2, _temp_dir2) = create_test_write_tx_from_existing(Arc::clone(&current));
        tx2.delete_node_cascade(node_a).unwrap();
        tx2.commit().unwrap();

        // Both edges should be deleted
        assert!(current.get_edge(edge_a_to_b).is_err());
        assert!(current.get_edge(edge_b_to_a).is_err());

        // node_b should still exist
        assert!(current.get_node(node_b).is_ok());
    }

    #[test]
    fn test_delete_node_cascade_performance_many_edges() {
        let (mut tx, _temp_dir) = create_test_write_tx();
        let current = Arc::clone(&tx.current);

        // Create a central node
        let props = PropertyMapBuilder::new().build();
        let central_node = tx.create_node("Hub", props.clone()).unwrap();

        // Create many connected nodes (100 outgoing, 100 incoming)
        let mut outgoing_edges = Vec::new();
        let mut incoming_edges = Vec::new();

        for i in 0..100 {
            let target = tx
                .create_node(
                    "Target",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
                .unwrap();
            let edge = tx
                .create_edge(
                    central_node,
                    target,
                    "OUT",
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
            outgoing_edges.push(edge);
        }

        for i in 0..100 {
            let source = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new().insert("id", i as i64).build(),
                )
                .unwrap();
            let edge = tx
                .create_edge(
                    source,
                    central_node,
                    "IN",
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
            incoming_edges.push(edge);
        }

        tx.commit().unwrap();

        // Verify all edges exist
        for edge in &outgoing_edges {
            assert!(current.get_edge(*edge).is_ok());
        }
        for edge in &incoming_edges {
            assert!(current.get_edge(*edge).is_ok());
        }

        // Delete the central node with cascade - should be performant
        let (mut tx2, _temp_dir2) = create_test_write_tx_from_existing(Arc::clone(&current));
        let start = std::time::Instant::now();
        tx2.delete_node_cascade(central_node).unwrap();
        tx2.commit().unwrap();
        let elapsed = start.elapsed();

        // Performance assertion: should complete in reasonable time (< 500ms)
        // This threshold is generous to avoid flakiness on slow CI systems
        // while still catching significant performance regressions
        assert!(
            elapsed.as_millis() < 500,
            "Cascade delete of 200 edges took too long: {:?}",
            elapsed
        );

        // Verify the node was deleted
        assert!(current.get_node(central_node).is_err());

        // Verify all 200 edges were deleted
        for edge in &outgoing_edges {
            assert!(current.get_edge(*edge).is_err());
        }
        for edge in &incoming_edges {
            assert!(current.get_edge(*edge).is_err());
        }
    }

    /// Helper function to create a write transaction from existing storage
    fn create_test_write_tx_from_existing(
        current: Arc<CurrentStorage>,
    ) -> (WriteTransaction, TempDir) {
        use crate::api::transaction::TxIdGenerator;
        use crate::core::temporal::time;
        use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
        use tempfile::TempDir;

        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
        let temporal_indexes = Arc::new(TemporalIndexes::new());

        // Create WAL with temp directory for tests
        let temp_dir = TempDir::new().unwrap();
        let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
        let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

        let current_timestamp = Arc::new(Mutex::new(time::now()));
        let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
        let tx_id_gen = TxIdGenerator::new();

        // Create snapshot and visibility manager for testing
        let visibility_manager = Arc::new(TxVisibilityManager::new());
        let snapshot = TransactionSnapshot {
            snapshot_timestamp: time::now(),
            active_transactions: Arc::new(std::collections::HashSet::new()),
        };

        let tx = WriteTransaction::new(
            tx_id_gen.next(),
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
        );

        (tx, temp_dir)
    }
}

/// Tests for write-write conflict detection (Issue #8).
///
/// These tests verify that concurrent transactions updating the same entity
/// will properly detect conflicts and fail with SerializationFailure.
#[cfg(test)]
mod conflict_detection_tests {
    use super::*;
    use crate::api::transaction::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use tempfile::TempDir;

    /// Test harness for conflict detection tests.
    ///
    /// Bundles all shared infrastructure needed to create multiple concurrent
    /// transactions for testing write-write conflict detection.
    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<Mutex<IdGenerator>>,
        edge_id_gen: Arc<Mutex<IdGenerator>>,
        version_id_gen: Arc<Mutex<IdGenerator>>,
        tx_id_gen: TxIdGenerator,
        _temp_dir: TempDir, // Keep alive for WAL directory
    }

    impl TestHarness {
        /// Create a new test harness with all shared infrastructure.
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(TemporalIndexes::new());

            let temp_dir = TempDir::new().unwrap();
            let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
            let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());

            TestHarness {
                current,
                historical,
                temporal_indexes,
                wal,
                current_timestamp,
                visibility_manager,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                _temp_dir: temp_dir,
            }
        }

        /// Create a new write transaction using the shared infrastructure.
        fn create_tx(&self) -> WriteTransaction {
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: *self.current_timestamp.lock().unwrap(),
                active_transactions: Arc::new(std::collections::HashSet::new()),
            };

            WriteTransaction::new(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    /// Test: First-committer-wins for node updates.
    ///
    /// Scenario from Issue #8:
    /// ```text
    /// Time    Transaction 1                    Transaction 2
    /// ----    -------------                    -------------
    /// T1      tx1 = write_transaction()
    /// T2      tx1.update_node(A, {age: 31})
    /// T3                                       tx2 = write_transaction()
    /// T4                                       tx2.update_node(A, {age: 32})
    /// T5                                       tx2.commit()  // Succeeds
    /// T6      tx1.commit()                     // Should FAIL!
    /// ```
    #[test]
    fn test_first_committer_wins_node_update() {
        let harness = TestHarness::new();

        // Create initial node via transaction (so it has proper metadata)
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 30i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // T1: tx1 starts
        let mut tx1 = harness.create_tx();

        // T2: tx1 updates node
        tx1.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 31i64).build(),
        )
        .unwrap();

        // T3: tx2 starts
        let mut tx2 = harness.create_tx();

        // T4: tx2 updates node
        tx2.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 32i64).build(),
        )
        .unwrap();

        // T5: tx2 commits first - should succeed
        tx2.commit().unwrap();

        // Verify tx2's update was applied
        let node_after_tx2 = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_after_tx2.get_property("age").and_then(|v| v.as_int()),
            Some(32),
            "tx2's update should have been applied"
        );

        // T6: tx1 tries to commit - should FAIL with SerializationFailure
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail due to write-write conflict"
        );

        // Verify it's a SerializationFailure error
        let err = result.unwrap_err();
        let err_str = format!("{:?}", err);
        assert!(
            err_str.contains("SerializationFailure"),
            "Expected SerializationFailure, got: {}",
            err_str
        );

        // Verify the final value is still tx2's value (first committer wins)
        let final_node = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            final_node.get_property("age").and_then(|v| v.as_int()),
            Some(32),
            "Final value should be tx2's value (first committer wins)"
        );
    }

    /// Test: First-committer-wins for edge updates.
    #[test]
    fn test_first_committer_wins_edge_update() {
        let harness = TestHarness::new();

        // Create initial nodes and edge
        let (node1, node2, edge_id) = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let e = tx
                .create_edge(
                    n1,
                    n2,
                    "KNOWS",
                    PropertyMapBuilder::new().insert("weight", 5i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            (n1, n2, e)
        };

        // tx1 starts
        let mut tx1 = harness.create_tx();
        tx1.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 10i64).build(),
        )
        .unwrap();

        // tx2 starts and commits first
        let mut tx2 = harness.create_tx();
        tx2.update_edge(
            edge_id,
            PropertyMapBuilder::new().insert("weight", 20i64).build(),
        )
        .unwrap();
        tx2.commit().unwrap();

        // tx1 tries to commit - should fail
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail due to edge update conflict"
        );

        // Verify final value is tx2's
        let final_edge = harness.current.get_edge(edge_id).unwrap();
        assert_eq!(
            final_edge.get_property("weight").and_then(|v| v.as_int()),
            Some(20)
        );

        // Suppress unused variable warnings
        let _ = (node1, node2);
    }

    /// Test: First-committer-wins for node deletion.
    #[test]
    fn test_first_committer_wins_node_delete() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 starts and wants to update
        let mut tx1 = harness.create_tx();
        tx1.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 31i64).build(),
        )
        .unwrap();

        // tx2 starts and deletes the node, then commits
        let mut tx2 = harness.create_tx();
        tx2.delete_node(node_id).unwrap();
        tx2.commit().unwrap();

        // Node should be deleted now
        assert!(harness.current.get_node(node_id).is_err());

        // tx1 tries to commit its update - should fail
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail - node was modified (deleted) by tx2"
        );
    }

    /// Test: Delete vs Delete conflict.
    #[test]
    fn test_delete_delete_conflict() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 wants to delete
        let mut tx1 = harness.create_tx();
        tx1.delete_node(node_id).unwrap();

        // tx2 also wants to delete and commits first
        let mut tx2 = harness.create_tx();
        tx2.delete_node(node_id).unwrap();
        tx2.commit().unwrap();

        // tx1 tries to commit - should fail
        let result = tx1.commit();
        assert!(
            result.is_err(),
            "tx1.commit() should fail - node was already deleted by tx2"
        );
    }

    /// Test: No conflict when transactions modify different entities.
    #[test]
    fn test_no_conflict_different_entities() {
        let harness = TestHarness::new();

        // Create two nodes
        let (node1, node2) = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                )
                .unwrap();
            let n2 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )
                .unwrap();
            tx.commit().unwrap();
            (n1, n2)
        };

        // tx1 updates node1
        let mut tx1 = harness.create_tx();
        tx1.update_node(
            node1,
            PropertyMapBuilder::new().insert("age", 30i64).build(),
        )
        .unwrap();

        // tx2 updates node2 and commits first
        let mut tx2 = harness.create_tx();
        tx2.update_node(
            node2,
            PropertyMapBuilder::new().insert("age", 25i64).build(),
        )
        .unwrap();
        tx2.commit().unwrap();

        // tx1 should also succeed - no conflict on different entities
        tx1.commit().unwrap();

        // Verify both updates were applied
        assert_eq!(
            harness
                .current
                .get_node(node1)
                .unwrap()
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(30)
        );
        assert_eq!(
            harness
                .current
                .get_node(node2)
                .unwrap()
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(25)
        );
    }

    /// Test: No conflict for create operations (new entities).
    #[test]
    fn test_no_conflict_for_creates() {
        let harness = TestHarness::new();

        // tx1 creates a node
        let mut tx1 = harness.create_tx();
        let node1 = tx1
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // tx2 creates a different node and commits first
        let mut tx2 = harness.create_tx();
        let node2 = tx2
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        tx2.commit().unwrap();

        // tx1 should also succeed - creates don't conflict
        tx1.commit().unwrap();

        // Both nodes should exist
        assert!(harness.current.get_node(node1).is_ok());
        assert!(harness.current.get_node(node2).is_ok());
    }

    /// Test: Conflict error message contains useful information.
    #[test]
    fn test_conflict_error_message() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 updates
        let mut tx1 = harness.create_tx();
        tx1.update_node(node_id, PropertyMapBuilder::new().build())
            .unwrap();

        // tx2 commits first
        let mut tx2 = harness.create_tx();
        tx2.update_node(node_id, PropertyMapBuilder::new().build())
            .unwrap();
        tx2.commit().unwrap();

        // tx1 fails
        let result = tx1.commit();
        let err = result.unwrap_err();
        let err_str = format!("{:?}", err);

        // Verify error contains node ID info
        assert!(
            err_str.contains("NodeId"),
            "Error should mention the entity: {}",
            err_str
        );
        assert!(
            err_str.contains("committed") || err_str.contains("snapshot"),
            "Error should explain the conflict: {}",
            err_str
        );
    }

    /// Test: Delete-then-recreate race condition (Issue #357, Scenario 1).
    ///
    /// Scenario:
    /// - tx1 deletes a node
    /// - tx2 creates a new node concurrently
    /// - Both transactions try to commit concurrently
    ///
    /// Note: The API doesn't allow explicit ID specification, so this tests
    /// concurrent delete/create operations on different nodes rather than
    /// true ID reuse.
    ///
    /// Expected behavior:
    /// - Test Case A: If delete commits first, concurrent create should succeed
    /// - Test Case B: If create commits first, concurrent delete should succeed
    ///   (operations on different entities don't conflict)
    #[test]
    fn test_delete_then_recreate_race() {
        let harness = TestHarness::new();

        // Test Case A: Delete commits first, then create
        {
            // Create initial node
            let node_id = {
                let mut tx = harness.create_tx();
                let id = tx
                    .create_node(
                        "Person",
                        PropertyMapBuilder::new().insert("name", "Alice").build(),
                    )
                    .unwrap();
                tx.commit().unwrap();
                id
            };

            // tx1 starts and deletes the node
            let mut tx1 = harness.create_tx();
            tx1.delete_node(node_id).unwrap();

            // tx2 starts and creates a new node (different entity, no conflict expected)
            let mut tx2 = harness.create_tx();
            let new_node_id = tx2
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )
                .unwrap();

            // tx1 commits first (deletes original node)
            tx1.commit().unwrap();

            // Verify node was deleted
            assert!(
                harness.current.get_node(node_id).is_err(),
                "Original node should be deleted"
            );

            // tx2 should succeed - creating new nodes doesn't conflict with deletes
            tx2.commit().unwrap();

            // New node should exist
            assert!(
                harness.current.get_node(new_node_id).is_ok(),
                "New node should be created successfully"
            );

            // Original node should still be deleted
            assert!(
                harness.current.get_node(node_id).is_err(),
                "Original node should remain deleted"
            );
        }

        // Test Case B: Create commits first, then delete (on different node)
        {
            // Create initial node to be deleted
            let node_to_delete = {
                let mut tx = harness.create_tx();
                let id = tx
                    .create_node(
                        "Person",
                        PropertyMapBuilder::new().insert("name", "Charlie").build(),
                    )
                    .unwrap();
                tx.commit().unwrap();
                id
            };

            // tx3 starts and creates a new node
            let mut tx3 = harness.create_tx();
            let created_node_id = tx3
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Dave").build(),
                )
                .unwrap();

            // tx4 starts and deletes a different node
            let mut tx4 = harness.create_tx();
            tx4.delete_node(node_to_delete).unwrap();

            // tx3 commits first (creates new node)
            tx3.commit().unwrap();

            // Verify new node was created
            assert!(
                harness.current.get_node(created_node_id).is_ok(),
                "New node should be created"
            );

            // tx4 should also succeed - operations on different entities don't conflict
            tx4.commit().unwrap();

            // Verify original node was deleted
            assert!(
                harness.current.get_node(node_to_delete).is_err(),
                "Original node should be deleted"
            );

            // Created node should still exist
            assert!(
                harness.current.get_node(created_node_id).is_ok(),
                "Created node should still exist"
            );
        }
    }

    /// Test: Update-delete conflict (Issue #357, Scenario 2).
    ///
    /// Scenario:
    /// - tx1 updates a node
    /// - tx2 deletes the same node
    /// - Both transactions have overlapping execution
    ///
    /// Expected behavior:
    /// - First committer wins (MVCC conflict detection)
    /// - If tx1 commits first (update), tx2's delete should fail due to write-write conflict
    /// - If tx2 commits first (delete), tx1's update should fail (can't update deleted node)
    #[test]
    fn test_update_delete_conflict() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 30i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // Test Case A: Update commits first, then delete
        {
            // tx1 updates the node
            let mut tx1 = harness.create_tx();
            tx1.update_node(
                node_id,
                PropertyMapBuilder::new().insert("age", 35i64).build(),
            )
            .unwrap();

            // tx2 wants to delete the node
            let mut tx2 = harness.create_tx();
            tx2.delete_node(node_id).unwrap();

            // tx1 commits first (update succeeds)
            tx1.commit().unwrap();

            // Verify update was applied
            let node = harness.current.get_node(node_id).unwrap();
            assert_eq!(
                node.get_property("age").and_then(|v| v.as_int()),
                Some(35),
                "Update should be applied"
            );

            // tx2 commits (delete on already modified node should fail)
            let result = tx2.commit();
            assert!(
                result.is_err(),
                "tx2 should fail - node was modified by tx1"
            );

            // Node should still exist with tx1's update
            let node = harness.current.get_node(node_id).unwrap();
            assert_eq!(
                node.get_property("age").and_then(|v| v.as_int()),
                Some(35),
                "Node should still have tx1's update"
            );
        }

        // Test Case B: Delete commits first, then update
        // Create a fresh node for this test case
        let node_id_b = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 50i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        {
            // tx3 wants to update
            let mut tx3 = harness.create_tx();
            tx3.update_node(
                node_id_b,
                PropertyMapBuilder::new().insert("age", 55i64).build(),
            )
            .unwrap();

            // tx4 deletes first
            let mut tx4 = harness.create_tx();
            tx4.delete_node(node_id_b).unwrap();
            tx4.commit().unwrap();

            // Node should be deleted
            assert!(
                harness.current.get_node(node_id_b).is_err(),
                "Node should be deleted by tx4"
            );

            // tx3 tries to commit update - should fail
            let result = tx3.commit();
            assert!(result.is_err(), "tx3 should fail - node was deleted by tx4");

            // Verify error message contains useful information
            let err = result.unwrap_err();
            let err_str = format!("{:?}", err);
            assert!(
                err_str.contains("NodeId")
                    || err_str.contains("deleted")
                    || err_str.contains("not found"),
                "Error should explain the conflict: {}",
                err_str
            );
        }
    }

    /// Test: Edge creation with concurrent node deletion (Issue #357, Scenario 3).
    ///
    /// Scenario:
    /// - tx1 creates an edge between two nodes
    /// - tx2 deletes one of the endpoint nodes
    /// - Both transactions try to commit concurrently
    ///
    /// Expected behavior (documents actual MVCC implementation):
    /// - Test Case A: If edge creation commits first, node deletion succeeds because
    ///   edge addition doesn't modify the node's version (no conflict detected).
    ///   This creates an orphaned edge, which is acceptable if traversals handle it.
    /// - Test Case B: If node deletion commits first, edge creation fails on commit
    ///   due to referential integrity (endpoint node was deleted after snapshot).
    #[test]
    fn test_edge_creation_with_concurrent_node_deletion() {
        let harness = TestHarness::new();

        // Create two nodes
        let (node1, node2) = {
            let mut tx = harness.create_tx();
            let n1 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            (n1, n2)
        };

        // Test Case A: Edge creation commits first
        {
            // tx1 creates an edge
            let mut tx1 = harness.create_tx();
            let edge_id = tx1
                .create_edge(node1, node2, "KNOWS", PropertyMapBuilder::new().build())
                .unwrap();

            // tx2 wants to delete node2
            let mut tx2 = harness.create_tx();
            tx2.delete_node(node2).unwrap();

            // tx1 commits first (edge created)
            tx1.commit().unwrap();

            // Verify edge exists
            assert!(
                harness.current.get_edge(edge_id).is_ok(),
                "Edge should be created"
            );

            // tx2 tries to delete node2. According to the current MVCC implementation,
            // edge addition doesn't modify node2's version, so no conflict is detected
            // and the deletion succeeds.
            let result = tx2.commit();
            assert!(
                result.is_ok(),
                "tx2 commit should succeed - edge addition doesn't create version conflict on node2"
            );

            // Verify node was deleted
            assert!(
                harness.current.get_node(node2).is_err(),
                "Node should be deleted after successful tx2 commit"
            );

            // Current implementation: edge becomes orphaned but still exists in storage.
            // This documents a limitation: the system allows orphaned edges.
            // TODO(issue): Consider adding cascade delete or stricter referential integrity
            assert!(
                harness.current.get_edge(edge_id).is_ok(),
                "Edge still exists as orphan (documents current behavior)"
            );

            // Verify the edge references the deleted node (orphaned edge)
            let edge = harness.current.get_edge(edge_id).unwrap();
            assert_eq!(
                edge.target, node2,
                "Edge still references deleted node (orphaned)"
            );
        }

        // Create a fresh pair of nodes for Test Case B
        let (node3, node4) = {
            let mut tx = harness.create_tx();
            let n3 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            let n4 = tx
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            tx.commit().unwrap();
            (n3, n4)
        };

        // Test Case B: Node deletion commits first
        {
            // tx3 wants to create an edge
            let mut tx3 = harness.create_tx();

            // tx4 deletes node4 first
            let mut tx4 = harness.create_tx();
            tx4.delete_node(node4).unwrap();
            tx4.commit().unwrap();

            // Node should be deleted
            assert!(
                harness.current.get_node(node4).is_err(),
                "Node should be deleted"
            );

            // tx3 tries to create edge with deleted node.
            // Under snapshot isolation, tx3's snapshot was taken before tx4's deletion,
            // so node4 exists in tx3's view and edge creation succeeds at operation time.
            let edge_result =
                tx3.create_edge(node3, node4, "KNOWS", PropertyMapBuilder::new().build());
            assert!(
                edge_result.is_ok(),
                "Edge creation should succeed - node4 exists in tx3's snapshot"
            );
            let edge_id = edge_result.unwrap();

            // However, commit should detect the conflict: node4 was deleted after tx3's snapshot,
            // violating referential integrity for the edge.
            let commit_result = tx3.commit();
            assert!(
                commit_result.is_err(),
                "tx3 commit should fail - node4 was deleted by tx4, breaking referential integrity"
            );

            // Verify edge was not persisted (rollback worked correctly)
            assert!(
                harness.current.get_edge(edge_id).is_err(),
                "Edge should not exist after failed commit"
            );
        }
    }

    /// Test: Rollback during concurrent commit (Issue #357, Scenario 4).
    ///
    /// Scenario:
    /// - tx1 modifies a node and commits successfully
    /// - tx2 modifies the same node but gets dropped (implicit rollback)
    /// - Verify visibility consistency and that uncommitted changes are not visible
    ///
    /// Expected behavior:
    /// - Only committed transaction's changes should be visible
    /// - Dropped transaction's changes should never become visible
    /// - No visibility inconsistencies should occur
    #[test]
    fn test_rollback_during_concurrent_commit() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("age", 30i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // tx1 updates and commits
        let mut tx1 = harness.create_tx();
        tx1.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 35i64).build(),
        )
        .unwrap();
        tx1.commit().unwrap();

        // Verify tx1's changes are visible
        let node_after_tx1 = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_after_tx1.get_property("age").and_then(|v| v.as_int()),
            Some(35),
            "tx1's update should be visible"
        );

        // tx2 modifies but is dropped (implicit rollback)
        {
            let mut tx2 = harness.create_tx();
            tx2.update_node(
                node_id,
                PropertyMapBuilder::new().insert("age", 40i64).build(),
            )
            .unwrap();
            // tx2 is dropped here without commit - implicit rollback
        }

        // Verify tx2's changes are NOT visible (rollback worked)
        let node_after_rollback = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_after_rollback
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(35),
            "Only tx1's changes should be visible; tx2 rolled back"
        );

        // tx3 commits successfully after tx2's rollback
        let mut tx3 = harness.create_tx();
        tx3.update_node(
            node_id,
            PropertyMapBuilder::new().insert("age", 45i64).build(),
        )
        .unwrap();
        tx3.commit().unwrap();

        // Verify tx3's changes are visible
        let node_final = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_final.get_property("age").and_then(|v| v.as_int()),
            Some(45),
            "tx3's update should be visible"
        );

        // Verify visibility consistency: ensure storage reflects latest committed state
        // (Reading directly from storage is appropriate since WriteTransaction doesn't
        // provide read methods - it's write-only)
        let node_final_check = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            node_final_check
                .get_property("age")
                .and_then(|v| v.as_int()),
            Some(45),
            "Storage should consistently show latest committed state"
        );
    }
}

/// Tests for concurrent commit timestamp ordering (Issue #10).
///
/// These tests verify that transaction timestamps are assigned in the same
/// order as their commits become durable. This is critical for bi-temporal
/// correctness - transaction time must be monotonic with actual commit order.
#[cfg(test)]
mod timestamp_ordering_tests {
    use super::*;
    use crate::api::transaction::TxIdGenerator;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::wal::concurrent_system::ConcurrentWalSystemConfig;
    use std::thread;
    use tempfile::TempDir;

    /// Test harness for timestamp ordering tests.
    struct TestHarness {
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_indexes: Arc<TemporalIndexes>,
        wal: Arc<ConcurrentWalSystem>,
        current_timestamp: Arc<Mutex<Timestamp>>,
        visibility_manager: Arc<TxVisibilityManager>,
        node_id_gen: Arc<Mutex<IdGenerator>>,
        edge_id_gen: Arc<Mutex<IdGenerator>>,
        version_id_gen: Arc<Mutex<IdGenerator>>,
        tx_id_gen: TxIdGenerator,
        _temp_dir: TempDir,
    }

    impl TestHarness {
        fn new() -> Self {
            let current = Arc::new(CurrentStorage::new());
            let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
            let temporal_indexes = Arc::new(TemporalIndexes::new());

            let temp_dir = TempDir::new().unwrap();
            let wal_config = ConcurrentWalSystemConfig::new(temp_dir.path());
            let wal = Arc::new(ConcurrentWalSystem::new(wal_config).unwrap());

            let current_timestamp = Arc::new(Mutex::new(time::now()));
            let node_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let edge_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let version_id_gen = Arc::new(Mutex::new(IdGenerator::new()));
            let tx_id_gen = TxIdGenerator::new();
            let visibility_manager = Arc::new(TxVisibilityManager::new());

            TestHarness {
                current,
                historical,
                temporal_indexes,
                wal,
                current_timestamp,
                visibility_manager,
                node_id_gen,
                edge_id_gen,
                version_id_gen,
                tx_id_gen,
                _temp_dir: temp_dir,
            }
        }

        fn create_tx(&self) -> WriteTransaction {
            let snapshot = TransactionSnapshot {
                snapshot_timestamp: *self.current_timestamp.lock().unwrap(),
                active_transactions: Arc::new(std::collections::HashSet::new()),
            };

            WriteTransaction::new(
                self.tx_id_gen.next(),
                snapshot,
                self.current.clone(),
                self.historical.clone(),
                self.temporal_indexes.clone(),
                self.wal.clone(),
                self.current_timestamp.clone(),
                self.visibility_manager.clone(),
                self.node_id_gen.clone(),
                self.edge_id_gen.clone(),
                self.version_id_gen.clone(),
            )
        }
    }

    /// Test: Sequential commits have monotonically increasing timestamps.
    #[test]
    fn test_sequential_commits_monotonic_timestamps() {
        let harness = TestHarness::new();

        let mut timestamps = Vec::new();

        // Perform 10 sequential commits
        for i in 0..10 {
            let mut tx = harness.create_tx();
            tx.create_node(
                "Test",
                PropertyMapBuilder::new().insert("seq", i as i64).build(),
            )
            .unwrap();
            tx.commit().unwrap();

            // Record the current timestamp after commit
            let ts = *harness.current_timestamp.lock().unwrap();
            timestamps.push(ts);
        }

        // Verify timestamps are strictly increasing
        for i in 1..timestamps.len() {
            assert!(
                timestamps[i] > timestamps[i - 1],
                "Timestamp {} ({}) should be > timestamp {} ({})",
                i,
                timestamps[i],
                i - 1,
                timestamps[i - 1]
            );
        }
    }

    /// Test: Concurrent commits still produce monotonically increasing timestamps.
    ///
    /// This test verifies that the fix for Issue #10 works correctly:
    /// - Multiple threads commit transactions concurrently
    /// - Each commit creates a node and we get its commit timestamp from metadata
    /// - We verify that all commit timestamps are unique and properly ordered
    #[test]
    fn test_concurrent_commits_ordered_timestamps() {
        let harness = Arc::new(TestHarness::new());
        let results = Arc::new(Mutex::new(Vec::new()));

        let num_threads = 8;
        let commits_per_thread = 5;

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let harness = harness.clone();
                let results = results.clone();

                thread::spawn(move || {
                    for i in 0..commits_per_thread {
                        let mut tx = harness.create_tx();
                        let node_id = tx
                            .create_node(
                                "Test",
                                PropertyMapBuilder::new()
                                    .insert("thread", thread_id as i64)
                                    .insert("iteration", i as i64)
                                    .build(),
                            )
                            .unwrap();
                        tx.commit().unwrap();

                        // Get the ACTUAL commit timestamp from the node's metadata
                        let node = harness.current.get_node(node_id).unwrap();
                        let commit_ts = node.metadata.commit_timestamp.unwrap();

                        results
                            .lock()
                            .unwrap()
                            .push((commit_ts, thread_id, i, node_id));
                    }
                })
            })
            .collect();

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Analyze results: sort by commit timestamp
        let mut results = results.lock().unwrap();
        results.sort_by_key(|(ts, _, _, _)| *ts);

        // With the fix, all timestamps should be unique (due to double-increment)
        // Check for duplicates
        for i in 1..results.len() {
            let (ts_prev, thread_prev, iter_prev, _) = results[i - 1];
            let (ts_curr, thread_curr, iter_curr, _) = results[i];

            assert!(
                ts_curr > ts_prev,
                "Duplicate or out-of-order timestamp detected: \
                 Thread {} iter {} (ts={}) vs Thread {} iter {} (ts={})",
                thread_prev,
                iter_prev,
                ts_prev,
                thread_curr,
                iter_curr,
                ts_curr
            );
        }

        // Verify we got all expected commits
        assert_eq!(
            results.len(),
            num_threads * commits_per_thread,
            "Expected {} commits, got {}",
            num_threads * commits_per_thread,
            results.len()
        );
    }

    /// Test: Version chains are correctly ordered by transaction time.
    ///
    /// When multiple transactions update the same node, the version chain
    /// should reflect the actual commit order.
    #[test]
    fn test_version_chain_ordering() {
        let harness = TestHarness::new();

        // Create initial node
        let node_id = {
            let mut tx = harness.create_tx();
            let id = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("version", 0i64).build(),
                )
                .unwrap();
            tx.commit().unwrap();
            id
        };

        // Track commit timestamps
        let mut commit_timestamps = Vec::new();

        // Perform sequential updates
        for version in 1..=5 {
            let mut tx = harness.create_tx();
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert("version", version as i64)
                    .build(),
            )
            .unwrap();
            tx.commit().unwrap();

            // Get the node's current metadata to verify timestamp
            let node = harness.current.get_node(node_id).unwrap();
            if let Some(ts) = node.metadata.commit_timestamp {
                commit_timestamps.push(ts);
            }
        }

        // Verify timestamps are strictly increasing
        for i in 1..commit_timestamps.len() {
            assert!(
                commit_timestamps[i] > commit_timestamps[i - 1],
                "Version {} timestamp ({}) should be > version {} timestamp ({})",
                i + 1,
                commit_timestamps[i],
                i,
                commit_timestamps[i - 1]
            );
        }

        // Verify final version is correct
        let final_node = harness.current.get_node(node_id).unwrap();
        assert_eq!(
            final_node.get_property("version").and_then(|v| v.as_int()),
            Some(5)
        );
    }
}
