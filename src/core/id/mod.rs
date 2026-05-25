//! Strongly-typed ID types for graph elements.
//!
//! This module provides distinct types for different kinds of identifiers to prevent
//! mix-ups at compile time. For example, you cannot accidentally pass a `NodeId` where
//! an `EdgeId` is expected.

use crate::core::error::StorageError;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum valid ID value. Values above this are reserved.
///
/// This prevents potential DoS attacks where malicious code creates IDs with
/// extreme values (like u64::MAX) that could cause issues in:
/// - Arithmetic operations (addition/subtraction with IDs)
/// - Array indexing or allocation attempts
/// - Serialization buffer sizing
///
/// The reserved range of 1000 values provides a safety margin without meaningfully
/// restricting the ID space (you can still have ~18 quintillion valid IDs).
pub const MAX_VALID_ID: u64 = u64::MAX - 1000;

/// Unique identifier for a node in the graph.
///
/// # The Spark
/// Graph databases run on connections, and every connection needs an anchor.
/// `NodeId` acts as this unique anchor. We strongly type this instead of using a
/// raw `u64` so that you cannot accidentally pass an edge ID to a function
/// expecting a node ID.
///
/// # Examples
/// ```
/// use aletheiadb::core::NodeId;
/// let node_id = NodeId::new(42).unwrap();
/// assert_eq!(node_id.as_u64(), 42);
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, bytemuck::Pod, bytemuck::Zeroable,
)]
#[repr(transparent)]
pub struct NodeId(u64);

impl NodeId {
    /// Create a new NodeId from a u64 value with validation.
    ///
    /// # The Spark
    /// Graph databases run on connections, and every connection needs an anchor.
    /// `NodeId` acts as this unique anchor. We strongly type this instead of using a
    /// raw `u64` so that you cannot accidentally pass an edge ID to a function
    /// expecting a node ID.
    ///
    /// # The Details
    /// Creating a valid `NodeId` requires validation. The inner `u64` must not exceed
    /// [`MAX_VALID_ID`]. This reserved space prevents potential DoS attacks during
    /// vector resizing or memory allocation.
    ///
    /// # Errors
    /// Returns [`StorageError::InvalidId`] if the provided `id` exceeds [`MAX_VALID_ID`].
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::core::id::NodeId;
    ///
    /// // Valid ID
    /// let id = NodeId::new(42).unwrap();
    /// assert_eq!(id.as_u64(), 42);
    /// ```
    #[inline]
    pub fn new(id: u64) -> Result<Self, StorageError> {
        if id > MAX_VALID_ID {
            return Err(StorageError::InvalidId {
                id,
                id_type: "node",
            });
        }
        Ok(NodeId(id))
    }

    /// Create a new NodeId without validation (for internal use only).
    ///
    /// # Internal Use Only
    /// This function bypasses validation. Only use when you're certain the ID is valid,
    /// such as when loading from trusted storage or in performance-critical paths where
    /// validation has already occurred.
    #[inline]
    pub(crate) const fn new_unchecked(id: u64) -> Self {
        NodeId(id)
    }

    /// Get the inner u64 value.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Node({})", self.0)
    }
}

/// Unique identifier for an edge in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct EdgeId(u64);

impl EdgeId {
    /// Create a new EdgeId from a u64 value with validation.
    ///
    /// # The Spark
    /// Edges are the relationships that give a graph its meaning.
    /// `EdgeId` provides a unique identifier for these relationships. We strongly type
    /// this instead of using a raw `u64` to prevent accidentally mixing up node and
    /// edge identifiers, which would cause silent corruption or mapping failures.
    ///
    /// # The Details
    /// Creating a valid `EdgeId` requires validation. The inner `u64` must not exceed
    /// [`MAX_VALID_ID`]. This reserved space prevents potential DoS attacks during
    /// vector resizing or memory allocation.
    ///
    /// # Errors
    /// Returns [`StorageError::InvalidId`] if the provided `id` exceeds [`MAX_VALID_ID`].
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::core::id::EdgeId;
    ///
    /// // Valid ID
    /// let id = EdgeId::new(42).unwrap();
    /// assert_eq!(id.as_u64(), 42);
    /// ```
    #[inline]
    pub fn new(id: u64) -> Result<Self, StorageError> {
        if id > MAX_VALID_ID {
            return Err(StorageError::InvalidId {
                id,
                id_type: "edge",
            });
        }
        Ok(EdgeId(id))
    }

    /// Create a new EdgeId without validation (for internal use only).
    ///
    /// # Internal Use Only
    /// This function bypasses validation. Only use when you're certain the ID is valid,
    /// such as when loading from trusted storage or in performance-critical paths where
    /// validation has already occurred.
    #[inline]
    pub(crate) const fn new_unchecked(id: u64) -> Self {
        EdgeId(id)
    }

    /// Get the inner u64 value.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Edge({})", self.0)
    }
}

/// Unique identifier for a version of a node or edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VersionId(u64);

impl VersionId {
    /// Create a new VersionId from a u64 value with validation.
    ///
    /// # The Spark
    /// AletheiaDB allows traversing time, not just data.
    /// `VersionId` provides an anchor in this temporal dimension. It's a strongly
    /// typed identifier so that historical snapshots cannot be confused with spatial
    /// entities like nodes or edges.
    ///
    /// # The Details
    /// Creating a valid `VersionId` requires validation. The inner `u64` must not exceed
    /// [`MAX_VALID_ID`]. This reserved space prevents potential DoS attacks during
    /// vector resizing or memory allocation.
    ///
    /// # Errors
    /// Returns [`StorageError::InvalidId`] if the provided `id` exceeds [`MAX_VALID_ID`].
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::core::id::VersionId;
    ///
    /// // Valid ID
    /// let id = VersionId::new(42).unwrap();
    /// assert_eq!(id.as_u64(), 42);
    /// ```
    #[inline]
    pub fn new(id: u64) -> Result<Self, StorageError> {
        if id > MAX_VALID_ID {
            return Err(StorageError::InvalidId {
                id,
                id_type: "version",
            });
        }
        Ok(VersionId(id))
    }

    /// Create a new VersionId without validation (for internal use only).
    ///
    /// # Internal Use Only
    /// This function bypasses validation. Only use when you're certain the ID is valid,
    /// such as when loading from trusted storage or in performance-critical paths where
    /// validation has already occurred.
    #[inline]
    pub(crate) const fn new_unchecked(id: u64) -> Self {
        VersionId(id)
    }

    /// Get the inner u64 value.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for VersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Version({})", self.0)
    }
}

/// Represents either a node or an edge identifier.
///
/// Useful for operations that work with both nodes and edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityId {
    /// Node entity variant
    Node(NodeId),
    /// Edge entity variant
    Edge(EdgeId),
}

impl EntityId {
    /// Returns true if this is a node ID.
    #[inline]
    pub const fn is_node(&self) -> bool {
        matches!(self, EntityId::Node(_))
    }

    /// Returns true if this is an edge ID.
    #[inline]
    pub const fn is_edge(&self) -> bool {
        matches!(self, EntityId::Edge(_))
    }

    /// Returns the inner NodeId if this is a node, None otherwise.
    #[inline]
    pub const fn as_node(&self) -> Option<NodeId> {
        match self {
            EntityId::Node(id) => Some(*id),
            EntityId::Edge(_) => None,
        }
    }

    /// Returns the inner EdgeId if this is an edge, None otherwise.
    #[inline]
    pub const fn as_edge(&self) -> Option<EdgeId> {
        match self {
            EntityId::Node(_) => None,
            EntityId::Edge(id) => Some(*id),
        }
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntityId::Node(id) => write!(f, "{}", id),
            EntityId::Edge(id) => write!(f, "{}", id),
        }
    }
}

impl From<NodeId> for EntityId {
    fn from(id: NodeId) -> Self {
        EntityId::Node(id)
    }
}

impl From<EdgeId> for EntityId {
    fn from(id: EdgeId) -> Self {
        EntityId::Edge(id)
    }
}

/// Atomic ID generator for creating unique IDs.
///
/// This is thread-safe and can be used concurrently without external synchronization.
/// A thread-safe generator for strictly increasing element IDs (nodes/edges).
///
/// # The Spark
/// When creating new nodes or edges, we need a way to assign them unique identifiers
/// safely across multiple threads. `IdGenerator` handles this via atomic counters.
///
/// # Examples
/// ```
/// use aletheiadb::core::IdGenerator;
/// let generator = IdGenerator::new();
/// let first_id = generator.next().unwrap();
/// let second_id = generator.next().unwrap();
/// assert_eq!(first_id, 0);
/// assert_eq!(second_id, 1);
/// ```
pub struct IdGenerator {
    next_id: AtomicU64,
}

impl IdGenerator {
    /// Create a new ID generator starting from 0.
    pub const fn new() -> Self {
        IdGenerator {
            next_id: AtomicU64::new(0),
        }
    }

    /// Create a new ID generator starting from a specific value.
    pub const fn with_start(start: u64) -> Self {
        IdGenerator {
            next_id: AtomicU64::new(start),
        }
    }

    /// Generate the next unique ID.
    ///
    /// This method is thread-safe and lock-free.
    ///
    /// Returns an error if the generator would exceed `MAX_VALID_ID`. In practice, this requires
    /// ~18 quintillion operations and is unrealistic for a single database instance.
    ///
    /// # Memory Ordering
    ///
    /// Uses `Ordering::SeqCst` (sequentially consistent) to ensure:
    /// - **Cross-thread visibility**: All threads observe ID operations in a globally consistent order
    /// - **Uniqueness guarantee**: No two threads can receive the same ID value
    /// - **Monotonicity**: IDs are strictly increasing across all threads
    ///
    /// While `Ordering::AcqRel` could provide atomicity, `SeqCst` offers the strongest correctness
    /// guarantees for ID generation. The ~5-10% performance overhead is acceptable because:
    /// 1. ID generation is infrequent compared to ID lookups (not a hot path)
    /// 2. Correctness is prioritized over micro-optimizations in ID allocation
    /// 3. The cost is per-ID, not per-operation on the graph
    ///
    /// See [issue #21](https://github.com/madmax983/AletheiaDB/issues/21) for context.
    #[inline]
    pub fn next(&self) -> Result<u64, StorageError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        if id > MAX_VALID_ID {
            return Err(StorageError::InvalidId {
                id,
                id_type: "generated",
            });
        }
        Ok(id)
    }

    /// Get the current value without incrementing.
    ///
    /// # Memory Ordering
    ///
    /// Uses `Ordering::SeqCst` to maintain consistency with `next()`, ensuring all threads
    /// observe the same global order of ID operations. This provides a consistent snapshot
    /// of the next ID that will be allocated.
    #[inline]
    pub fn current(&self) -> u64 {
        self.next_id.load(Ordering::SeqCst)
    }

    /// Get an approximate current value without incrementing, optimized for non-critical use cases.
    ///
    /// This method provides a **relaxed** view of the current ID counter, trading strict
    /// consistency for ~10x better performance (~1ns vs ~10ns per call). The returned value
    /// may be slightly stale due to relaxed memory ordering.
    ///
    /// # Memory Ordering
    ///
    /// Uses `Ordering::Relaxed` which provides:
    /// - **No cross-thread synchronization**: Different threads may observe updates in different orders
    /// - **No happens-before guarantees**: The value may not reflect recent writes from other threads
    /// - **Atomicity only**: Reads are atomic but may see stale values
    ///
    /// This is **significantly faster** than `current()` because it avoids the cross-core
    /// synchronization overhead of sequential consistency. On modern hardware:
    /// - `current_approximate()`: ~1ns (relaxed load)
    /// - `current()`: ~5-10ns (SeqCst load with full memory barrier)
    ///
    /// # When to Use
    ///
    /// **✓ Safe and appropriate for:**
    /// - **Metrics collection**: Counting operations, tracking rates (`operations_per_second`)
    /// - **Progress indicators**: Displaying approximate progress (`"Processed ~1.2M items..."`)
    /// - **Debugging/logging**: Non-critical diagnostics (`debug!("Current ID: ~{}", id)`)
    /// - **Approximate counts**: Where exact accuracy isn't required (`"~500 items remaining"`)
    /// - **Performance monitoring**: Low-overhead instrumentation
    ///
    /// **✗ DO NOT use for:**
    /// - **Snapshot isolation decisions**: Use `current()` for MVCC/transaction visibility
    /// - **Transaction visibility**: Determining what data a transaction can see
    /// - **Correctness-critical paths**: Where stale values could cause incorrect behavior
    /// - **Consistency guarantees**: Where you need a globally consistent view across threads
    /// - **Synchronization**: Coordinating between threads (use proper synchronization primitives)
    ///
    /// # Example: Metrics Collection
    ///
    /// ```rust
    /// # use aletheiadb::core::id::IdGenerator;
    /// let generator = IdGenerator::new();
    ///
    /// // In a metrics reporting loop (runs every second)
    /// fn report_metrics(generator: &IdGenerator) {
    ///     // Using approximate is fine here - we don't need exact precision
    ///     let approx_count = generator.current_approximate();
    ///     println!("Approximate ID count: ~{}", approx_count);
    ///     // Metrics don't need perfect accuracy, and this is 10x faster
    /// }
    /// ```
    ///
    /// # Example: When NOT to Use
    ///
    /// ```rust
    /// # use aletheiadb::core::id::IdGenerator;
    /// let generator = IdGenerator::new();
    ///
    /// // ❌ WRONG: Using approximate for snapshot isolation
    /// // let snapshot_id = generator.current_approximate(); // DON'T DO THIS
    ///
    /// // ✓ CORRECT: Use current() for snapshot isolation
    /// let snapshot_id = generator.current();
    /// // Snapshot isolation requires a consistent view across all threads
    /// ```
    ///
    /// # Performance Characteristics
    ///
    /// Benchmark results (1M operations):
    /// - `current_approximate()`: ~1ns/op (relaxed load)
    /// - `current()`: ~5-10ns/op (SeqCst load)
    /// - **Speedup**: ~5-10x faster
    ///
    /// The performance advantage comes from avoiding CPU cache coherence overhead.
    /// Relaxed loads can be satisfied from the CPU's local cache without waiting for
    /// cache line synchronization across cores.
    ///
    /// # Cross-Reference
    ///
    /// See [ADR-0009](https://github.com/madmax983/AletheiaDB/blob/main/docs/adr/0009-strong-id-types.md)
    /// for discussion of memory ordering in ID generation and
    /// [issue #198](https://github.com/madmax983/AletheiaDB/issues/198) for the
    /// motivation behind this method.
    #[inline]
    pub fn current_approximate(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed)
    }

    /// Reset the generator to a specific value.
    ///
    /// This is used during recovery to initialize the ID generator from the maximum ID
    /// found in the WAL, ensuring continued ID generation without conflicts.
    ///
    /// # Arguments
    ///
    /// * `value` - The next ID to generate (typically max_id + 1)
    ///
    /// # Memory Ordering
    ///
    /// Uses `Ordering::SeqCst` to ensure all threads observe the reset consistently.
    /// This is critical during recovery when re-initializing generators.
    #[inline]
    pub(crate) fn reset_to(&self, value: u64) {
        self.next_id.store(value, Ordering::SeqCst);
    }

    /// Ensure the generator's next value is at least the specified minimum.
    ///
    /// This is used during recovery when we need to account for IDs from multiple
    /// sources (e.g., current storage and historical storage) without overwriting
    /// a higher value that was already set.
    ///
    /// # Thread Safety
    ///
    /// This method uses compare-and-swap (CAS) in a loop to atomically ensure
    /// the value is at least `min_value`, avoiding check-then-act race conditions
    /// that could occur with separate load/store operations.
    ///
    /// # Arguments
    ///
    /// * `min_value` - The minimum next ID to generate
    ///
    /// # Memory Ordering
    ///
    /// Uses `Ordering::SeqCst` for compare_exchange to ensure all threads observe
    /// a globally consistent order of operations.
    #[inline]
    pub(crate) fn ensure_at_least(&self, min_value: u64) {
        let mut current = self.next_id.load(Ordering::SeqCst);
        while min_value > current {
            match self.next_id.compare_exchange(
                current,
                min_value,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,                  // Successfully updated
                Err(actual) => current = actual, // Retry with actual current value
            }
        }
    }
}

impl Default for IdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Transaction ID - globally unique identifier for transactions
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
/// Unique identifier for a transaction in the database.
///
/// # The Spark
/// To track bi-temporal state correctly, AletheiaDB needs to know not only *when*
/// a change occurred, but *who* made it. The `TxId` provides this context, linking
/// specific versions of graph elements back to the logical transaction that created them.
///
/// # Examples
/// ```
/// use aletheiadb::TxId;
/// let tx_id = TxId::new(100);
/// assert_eq!(tx_id.as_u64(), 100);
/// ```
pub struct TxId(u64);

impl TxId {
    /// Create a new transaction ID
    pub fn new(id: u64) -> Self {
        TxId(id)
    }

    /// Get the inner ID value
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for TxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TxId({})", self.0)
    }
}

/// Global transaction ID generator
///
/// Generates monotonically increasing transaction IDs using atomic operations.
/// A thread-safe generator for strictly increasing transaction IDs.
///
/// # The Spark
/// Multiple concurrent writers need unique transaction IDs. The `TxIdGenerator` uses
/// atomic operations to ensure that every transaction receives a unique, monotonically
/// increasing ID without requiring locks.
///
/// # Examples
/// ```
/// use aletheiadb::core::id::TxIdGenerator;
/// let generator = TxIdGenerator::new();
/// let first_tx = generator.next();
/// let second_tx = generator.next();
/// assert_eq!(first_tx.as_u64(), 1);
/// assert_eq!(second_tx.as_u64(), 2);
/// ```
pub struct TxIdGenerator {
    counter: AtomicU64,
}

impl TxIdGenerator {
    /// Create a new transaction ID generator starting from 1
    pub fn new() -> Self {
        TxIdGenerator {
            counter: AtomicU64::new(1),
        }
    }

    /// Generate the next transaction ID
    ///
    /// This operation is atomic and thread-safe.
    pub fn next(&self) -> TxId {
        let mut current = self.counter.load(Ordering::SeqCst);
        loop {
            if current == u64::MAX {
                panic!("Transaction ID overflow! Database requires restart/migration.");
            }
            match self.counter.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return TxId(current),
                Err(v) => current = v,
            }
        }
    }

    /// Get the current transaction ID (last generated)
    pub fn current(&self) -> TxId {
        TxId(self.counter.load(Ordering::SeqCst).saturating_sub(1))
    }

    /// Set the internal counter for testing overflow conditions.
    pub fn set_counter(&self, val: u64) {
        self.counter.store(val, Ordering::SeqCst);
    }
}

impl Default for TxIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

use std::str::FromStr;

impl TryFrom<u64> for NodeId {
    type Error = StorageError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        NodeId::new(value)
    }
}

impl FromStr for NodeId {
    type Err = StorageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value = s.parse::<u64>().map_err(|_| StorageError::InvalidId {
            id: u64::MAX,
            id_type: "NodeId",
        })?;
        NodeId::new(value)
    }
}

impl TryFrom<u64> for EdgeId {
    type Error = StorageError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        EdgeId::new(value)
    }
}

impl FromStr for EdgeId {
    type Err = StorageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value = s.parse::<u64>().map_err(|_| StorageError::InvalidId {
            id: u64::MAX,
            id_type: "EdgeId",
        })?;
        EdgeId::new(value)
    }
}

impl TryFrom<u64> for VersionId {
    type Error = StorageError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        VersionId::new(value)
    }
}

impl FromStr for VersionId {
    type Err = StorageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value = s.parse::<u64>().map_err(|_| StorageError::InvalidId {
            id: u64::MAX,
            id_type: "VersionId",
        })?;
        VersionId::new(value)
    }
}

impl TryFrom<u64> for TxId {
    type Error = StorageError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Ok(TxId::new(value))
    }
}

impl FromStr for TxId {
    type Err = StorageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value = s.parse::<u64>().map_err(|_| StorageError::InvalidId {
            id: u64::MAX,
            id_type: "TxId",
        })?;
        Ok(TxId::new(value))
    }
}

#[cfg(test)]
mod tests;
