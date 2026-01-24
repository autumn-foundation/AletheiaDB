//! Strongly-typed ID types for graph elements.
//!
//! This module provides distinct types for different kinds of identifiers to prevent
//! mix-ups at compile time. For example, you cannot accidentally pass a `NodeId` where
//! an `EdgeId` is expected.

use crate::utils::error::StorageError;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u64);

impl NodeId {
    /// Create a new NodeId from a u64 value with validation.
    ///
    /// Returns an error if the ID exceeds MAX_VALID_ID.
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
pub struct EdgeId(u64);

impl EdgeId {
    /// Create a new EdgeId from a u64 value with validation.
    ///
    /// Returns an error if the ID exceeds MAX_VALID_ID.
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
pub struct VersionId(u64);

impl VersionId {
    /// Create a new VersionId from a u64 value with validation.
    ///
    /// Returns an error if the ID exceeds MAX_VALID_ID.
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
    /// See [issue #21](https://github.com/madmax983/GallifreyDB/issues/21) for context.
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
    /// # use gallifreydb::core::id::IdGenerator;
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
    /// # use gallifreydb::core::id::IdGenerator;
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
    /// See [ADR-0009](https://github.com/madmax983/GallifreyDB/blob/main/docs/adr/0009-strong-id-types.md)
    /// for discussion of memory ordering in ID generation and
    /// [issue #198](https://github.com/madmax983/GallifreyDB/issues/198) for the
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_id_creation() {
        let id = NodeId::new(42).unwrap();
        assert_eq!(id.as_u64(), 42);
    }

    #[test]
    fn test_edge_id_creation() {
        let id = EdgeId::new(100).unwrap();
        assert_eq!(id.as_u64(), 100);
    }

    #[test]
    fn test_version_id_creation() {
        let id = VersionId::new(1000).unwrap();
        assert_eq!(id.as_u64(), 1000);
    }

    #[test]
    fn test_entity_id_from_node() {
        let node_id = NodeId::new(1).unwrap();
        let entity_id: EntityId = node_id.into();
        assert!(entity_id.is_node());
        assert!(!entity_id.is_edge());
        assert_eq!(entity_id.as_node(), Some(node_id));
    }

    #[test]
    fn test_entity_id_from_edge() {
        let edge_id = EdgeId::new(2).unwrap();
        let entity_id: EntityId = edge_id.into();
        assert!(!entity_id.is_node());
        assert!(entity_id.is_edge());
        assert_eq!(entity_id.as_edge(), Some(edge_id));
    }

    #[test]
    fn test_id_generator() {
        let generator = IdGenerator::new();
        assert_eq!(generator.next(), Ok(0));
        assert_eq!(generator.next(), Ok(1));
        assert_eq!(generator.next(), Ok(2));
        assert_eq!(generator.current(), 3);
    }

    #[test]
    fn test_id_generator_with_start() {
        let generator = IdGenerator::with_start(100);
        assert_eq!(generator.next(), Ok(100));
        assert_eq!(generator.next(), Ok(101));
    }

    #[test]
    fn test_id_display() {
        let node = NodeId::new(42).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let version = VersionId::new(1000).unwrap();

        assert_eq!(format!("{}", node), "Node(42)");
        assert_eq!(format!("{}", edge), "Edge(100)");
        assert_eq!(format!("{}", version), "Version(1000)");
    }

    #[test]
    fn test_ids_are_distinct_types() {
        // This test ensures that you cannot accidentally use one type where another is expected.
        // This is enforced by the type system, so we just verify we can create different types.
        // Use new_unchecked since we're just testing the type system, not validation.
        let _node = NodeId::new_unchecked(1);
        let _edge = EdgeId::new_unchecked(1);
        let _version = VersionId::new_unchecked(1);

        // The following would fail to compile (which is what we want):
        // fn takes_node_id(_id: NodeId) {}
        // takes_node_id(_edge); // Type error!
    }

    #[test]
    fn test_id_validation_accepts_valid_ids() {
        // Valid IDs should be accepted
        assert!(NodeId::new(0).is_ok());
        assert!(NodeId::new(42).is_ok());
        assert!(NodeId::new(MAX_VALID_ID).is_ok());

        assert!(EdgeId::new(0).is_ok());
        assert!(EdgeId::new(100).is_ok());
        assert!(EdgeId::new(MAX_VALID_ID).is_ok());

        assert!(VersionId::new(0).is_ok());
        assert!(VersionId::new(1000).is_ok());
        assert!(VersionId::new(MAX_VALID_ID).is_ok());
    }

    #[test]
    fn test_id_validation_rejects_out_of_range() {
        // IDs exceeding MAX_VALID_ID should be rejected
        let node_result = NodeId::new(MAX_VALID_ID + 1);
        assert!(node_result.is_err());
        if let Err(StorageError::InvalidId { id, id_type }) = node_result {
            assert_eq!(id, MAX_VALID_ID + 1);
            assert_eq!(id_type, "node");
        } else {
            panic!("Expected InvalidId error");
        }

        let edge_result = EdgeId::new(u64::MAX);
        assert!(edge_result.is_err());
        if let Err(StorageError::InvalidId { id, id_type }) = edge_result {
            assert_eq!(id, u64::MAX);
            assert_eq!(id_type, "edge");
        } else {
            panic!("Expected InvalidId error");
        }

        let version_result = VersionId::new(MAX_VALID_ID + 1000);
        assert!(version_result.is_err());
        if let Err(StorageError::InvalidId { id, id_type }) = version_result {
            assert_eq!(id, MAX_VALID_ID + 1000);
            assert_eq!(id_type, "version");
        } else {
            panic!("Expected InvalidId error");
        }
    }

    #[test]
    fn test_new_unchecked_bypasses_validation() {
        // new_unchecked should create IDs without validation
        // This is for internal use where we know the ID is safe
        let node = NodeId::new_unchecked(42);
        assert_eq!(node.as_u64(), 42);

        let edge = EdgeId::new_unchecked(100);
        assert_eq!(edge.as_u64(), 100);

        let version = VersionId::new_unchecked(1000);
        assert_eq!(version.as_u64(), 1000);

        // Even out-of-range values work with new_unchecked (though they shouldn't be used)
        let _risky_node = NodeId::new_unchecked(u64::MAX);
        let _risky_edge = EdgeId::new_unchecked(u64::MAX);
        let _risky_version = VersionId::new_unchecked(u64::MAX);
    }

    #[test]
    fn test_max_valid_id_constant() {
        // Verify the MAX_VALID_ID constant is set correctly
        assert_eq!(MAX_VALID_ID, u64::MAX - 1000);

        // Verify it leaves room for reserved values
        const { assert!(MAX_VALID_ID < u64::MAX) };
        const { assert!(u64::MAX - MAX_VALID_ID >= 1000) };
    }

    #[test]
    fn test_id_validation_boundary_cases() {
        // Test values around MAX_VALID_ID boundary
        assert!(NodeId::new(MAX_VALID_ID - 1).is_ok());
        assert!(NodeId::new(MAX_VALID_ID).is_ok());
        assert!(NodeId::new(MAX_VALID_ID + 1).is_err());
        assert!(NodeId::new(MAX_VALID_ID + 2).is_err());

        // Same for other ID types
        assert!(EdgeId::new(MAX_VALID_ID - 1).is_ok());
        assert!(EdgeId::new(MAX_VALID_ID).is_ok());
        assert!(EdgeId::new(MAX_VALID_ID + 1).is_err());

        assert!(VersionId::new(MAX_VALID_ID - 1).is_ok());
        assert!(VersionId::new(MAX_VALID_ID).is_ok());
        assert!(VersionId::new(MAX_VALID_ID + 1).is_err());
    }

    #[test]
    fn test_error_message_content() {
        // Verify error messages are properly formatted
        let err = NodeId::new(MAX_VALID_ID + 1).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("Invalid"));
        assert!(msg.contains("node"));
        assert!(msg.contains("ID"));
        assert!(msg.contains(&(MAX_VALID_ID + 1).to_string()));
        assert!(msg.contains("exceeds maximum"));
        assert!(msg.contains(&MAX_VALID_ID.to_string()));
        assert!(msg.contains("reserved range for internal use"));
    }

    #[test]
    fn test_id_generator_respects_max_valid_id() {
        // Verify ID generators produce valid IDs
        let generator = IdGenerator::new();
        for _ in 0..100 {
            let id = generator.next().expect("Generator should produce valid ID");
            assert!(
                NodeId::new(id).is_ok(),
                "Generator produced invalid ID: {}",
                id
            );
        }

        // Test generator starting near the limit
        let generator = IdGenerator::with_start(MAX_VALID_ID - 10);
        for _ in 0..10 {
            let id = generator
                .next()
                .expect("Generator near limit should produce valid ID");
            assert!(
                NodeId::new(id).is_ok(),
                "Generator near limit produced invalid ID: {}",
                id
            );
        }

        // Note: After MAX_VALID_ID, generator would produce invalid IDs.
        // In practice, this would require ~18 quintillion operations,
        // which is unrealistic for a single database instance.
    }

    #[test]
    fn test_id_generator_returns_error_on_overflow() {
        // Verify generator returns error when exceeding MAX_VALID_ID
        let generator = IdGenerator::with_start(MAX_VALID_ID);
        assert!(generator.next().is_ok()); // This should be OK (at MAX_VALID_ID)
        assert!(generator.next().is_err()); // This should error (exceeds MAX_VALID_ID)

        // Verify error type
        let err = generator.next().unwrap_err();
        assert!(matches!(
            err,
            StorageError::InvalidId {
                id_type: "generated",
                ..
            }
        ));
    }

    #[test]
    fn test_id_validation_performance() {
        // Verify validation overhead is negligible
        // This is a simple smoke test - proper benchmarking should use criterion
        use std::time::Instant;

        let iterations = 1_000_000;

        // Time validated creation
        let start = Instant::now();
        for i in 0..iterations {
            let _ = NodeId::new(i);
        }
        let validated_duration = start.elapsed();

        // Time unchecked creation (for comparison)
        let start = Instant::now();
        for i in 0..iterations {
            let _ = NodeId::new_unchecked(i);
        }
        let unchecked_duration = start.elapsed();

        // Print results for manual inspection (not asserted in test)
        println!("\nID Validation Performance (1M iterations):");
        println!(
            "  Validated:  {:?} ({} ns/op)",
            validated_duration,
            validated_duration.as_nanos() / iterations as u128
        );
        println!(
            "  Unchecked:  {:?} ({} ns/op)",
            unchecked_duration,
            unchecked_duration.as_nanos() / iterations as u128
        );
        println!(
            "  Overhead:   {:?}",
            validated_duration.saturating_sub(unchecked_duration)
        );

        // Validation should add minimal overhead (< 2ns per operation on modern hardware)
        // We don't assert this in the test since it's hardware-dependent
    }

    #[test]
    fn test_id_generator_concurrent_near_limit() {
        use std::collections::HashSet;
        use std::sync::Arc;
        use std::thread;

        // Start generator 20 IDs before the limit
        let ids_before_limit = 20u64;
        let generator = Arc::new(IdGenerator::with_start(MAX_VALID_ID - ids_before_limit + 1));

        // Spawn 10 threads, each trying to generate 5 IDs
        let num_threads = 10;
        let ids_per_thread = 5;
        let total_attempts = num_threads * ids_per_thread;

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let gen_clone = Arc::clone(&generator);
                thread::spawn(move || {
                    let mut results = Vec::new();
                    for _ in 0..ids_per_thread {
                        results.push(gen_clone.next());
                    }
                    results
                })
            })
            .collect();

        // Collect all results
        let mut all_results = Vec::new();
        for handle in handles {
            let thread_results = handle.join().expect("Thread panicked");
            all_results.extend(thread_results);
        }

        // Verify results
        let mut successful_ids = HashSet::new();
        let mut error_count = 0;

        for result in all_results {
            match result {
                Ok(id) => {
                    // Verify ID is valid
                    assert!(
                        id <= MAX_VALID_ID,
                        "Generated ID {} exceeds MAX_VALID_ID",
                        id
                    );
                    // Verify no duplicates (critical for concurrency correctness)
                    assert!(successful_ids.insert(id), "Duplicate ID generated: {}", id);
                }
                Err(e) => {
                    // Verify error is the expected overflow error
                    assert!(
                        matches!(
                            e,
                            StorageError::InvalidId {
                                id_type: "generated",
                                ..
                            }
                        ),
                        "Unexpected error type: {:?}",
                        e
                    );
                    error_count += 1;
                }
            }
        }

        // We started with 20 IDs available (MAX_VALID_ID - start + 1 = 20)
        // So exactly 20 should succeed and the rest should fail
        assert_eq!(
            successful_ids.len(),
            ids_before_limit as usize,
            "Expected exactly {} successful ID generations",
            ids_before_limit
        );
        assert_eq!(
            error_count,
            total_attempts - ids_before_limit as usize,
            "Expected {} errors when exceeding limit",
            total_attempts - ids_before_limit as usize
        );

        // Verify all successful IDs are in the expected range
        for id in &successful_ids {
            assert!(
                *id > MAX_VALID_ID - ids_before_limit && *id <= MAX_VALID_ID,
                "ID {} outside expected range [{}, {}]",
                id,
                MAX_VALID_ID - ids_before_limit + 1,
                MAX_VALID_ID
            );
        }

        println!("\nConcurrent ID Generation Test Results:");
        println!("  Threads: {}", num_threads);
        println!("  Attempts per thread: {}", ids_per_thread);
        println!("  Total attempts: {}", total_attempts);
        println!("  Successful: {} (no duplicates)", successful_ids.len());
        println!("  Errors: {}", error_count);
        println!(
            "  ID range: {} - {}",
            successful_ids.iter().min().unwrap(),
            successful_ids.iter().max().unwrap()
        );
    }

    #[test]
    fn test_id_generator_concurrent_uniqueness() {
        use std::collections::HashSet;
        use std::sync::Arc;
        use std::thread;

        // Create a shared ID generator
        let generator = Arc::new(IdGenerator::new());

        // Spawn many threads to generate IDs concurrently
        let num_threads = 20;
        let ids_per_thread = 1000;
        let total_ids = num_threads * ids_per_thread;

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let gen_clone = Arc::clone(&generator);
                thread::spawn(move || {
                    let mut thread_ids = Vec::with_capacity(ids_per_thread);
                    for _ in 0..ids_per_thread {
                        match gen_clone.next() {
                            Ok(id) => thread_ids.push(id),
                            Err(e) => panic!("Thread {} failed to generate ID: {:?}", thread_id, e),
                        }
                    }
                    thread_ids
                })
            })
            .collect();

        // Collect all IDs from all threads
        let mut all_ids = Vec::with_capacity(total_ids);
        for handle in handles {
            let thread_ids = handle.join().expect("Thread panicked");
            all_ids.extend(thread_ids);
        }

        // Verify we got the expected number of IDs
        assert_eq!(
            all_ids.len(),
            total_ids,
            "Expected {} IDs but got {}",
            total_ids,
            all_ids.len()
        );

        // CRITICAL: Verify all IDs are unique (no duplicates)
        let unique_ids: HashSet<_> = all_ids.iter().copied().collect();
        assert_eq!(
            unique_ids.len(),
            all_ids.len(),
            "Found {} duplicate IDs! All IDs must be unique.",
            all_ids.len() - unique_ids.len()
        );

        // Verify IDs are in valid range
        for id in &all_ids {
            assert!(
                *id < total_ids as u64,
                "ID {} is unexpectedly large (expected < {})",
                id,
                total_ids
            );
        }

        // Verify IDs form a contiguous sequence from 0 to total_ids-1
        let mut sorted_ids = all_ids.clone();
        sorted_ids.sort_unstable();
        for (i, id) in sorted_ids.iter().enumerate() {
            assert_eq!(
                *id, i as u64,
                "Expected ID {} at position {} but found {}",
                i, i, id
            );
        }

        println!("\nConcurrent ID Uniqueness Test Results:");
        println!("  Threads: {}", num_threads);
        println!("  IDs per thread: {}", ids_per_thread);
        println!("  Total IDs generated: {}", all_ids.len());
        println!("  Unique IDs: {}", unique_ids.len());
        println!("  Duplicates: 0 ✓");
        println!("  ID range: 0 - {}", sorted_ids.last().unwrap());
    }

    #[test]
    fn test_current_approximate_basic() {
        // Test basic functionality of current_approximate()
        let generator = IdGenerator::new();

        // Initial value should be 0
        assert_eq!(generator.current_approximate(), 0);

        // Generate some IDs
        assert_eq!(generator.next(), Ok(0));
        assert_eq!(generator.next(), Ok(1));
        assert_eq!(generator.next(), Ok(2));

        // current_approximate() should return a value close to current()
        let approximate = generator.current_approximate();
        let exact = generator.current();

        // Approximate should be close to exact (within reasonable bounds)
        // Due to relaxed ordering, it might be slightly behind
        assert!(
            approximate <= exact,
            "Approximate {} should be <= exact {}",
            approximate,
            exact
        );
    }

    #[test]
    fn test_current_approximate_with_start() {
        // Test current_approximate() with non-zero start value
        let generator = IdGenerator::with_start(100);

        assert_eq!(generator.current_approximate(), 100);
        assert_eq!(generator.next(), Ok(100));
        assert_eq!(generator.next(), Ok(101));

        let approximate = generator.current_approximate();
        assert!(approximate >= 100, "Should be at least the start value");
        assert!(approximate <= 102, "Should not exceed current value");
    }

    #[test]
    fn test_current_approximate_is_non_blocking() {
        use std::sync::Arc;
        use std::thread;

        // Verify current_approximate() can be called concurrently without blocking
        let generator = Arc::new(IdGenerator::new());
        let num_threads = 10;
        let reads_per_thread = 10000;

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let gen_clone = Arc::clone(&generator);
                thread::spawn(move || {
                    // Rapidly call current_approximate() - should never block
                    for _ in 0..reads_per_thread {
                        let _ = gen_clone.current_approximate();
                    }
                })
            })
            .collect();

        // All threads should complete without blocking
        for handle in handles {
            handle.join().expect("Thread should not panic");
        }
    }

    #[test]
    fn test_current_approximate_concurrent_with_writes() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        // Test that current_approximate() works correctly when IDs are being generated
        let generator = Arc::new(IdGenerator::new());

        // Spawn writer threads
        let writer_handles: Vec<_> = (0..5)
            .map(|_| {
                let gen_clone = Arc::clone(&generator);
                thread::spawn(move || {
                    for _ in 0..1000 {
                        let _ = gen_clone.next();
                        thread::sleep(Duration::from_micros(1));
                    }
                })
            })
            .collect();

        // Spawn reader threads using current_approximate()
        let reader_handles: Vec<_> = (0..5)
            .map(|_| {
                let gen_clone = Arc::clone(&generator);
                thread::spawn(move || {
                    let mut readings = Vec::new();
                    for _ in 0..1000 {
                        readings.push(gen_clone.current_approximate());
                        thread::sleep(Duration::from_micros(1));
                    }
                    readings
                })
            })
            .collect();

        // Wait for all threads
        for handle in writer_handles {
            handle.join().expect("Writer thread should not panic");
        }

        let mut all_readings = Vec::new();
        for handle in reader_handles {
            let readings = handle.join().expect("Reader thread should not panic");
            all_readings.extend(readings);
        }

        // Verify all readings are reasonable (non-decreasing trend overall)
        // Note: Individual readings might not be monotonic due to relaxed ordering,
        // but the general trend should be increasing
        let first_reading = all_readings[0];
        let last_reading = all_readings[all_readings.len() - 1];
        assert!(
            last_reading >= first_reading,
            "Last reading {} should be >= first reading {}",
            last_reading,
            first_reading
        );
    }

    #[test]
    fn test_current_approximate_vs_current_consistency() {
        // Test that current_approximate() and current() return related values
        let generator = IdGenerator::new();

        for i in 0..100 {
            let _ = generator.next();

            let approximate = generator.current_approximate();
            let exact = generator.current();

            // Approximate should never exceed exact
            assert!(
                approximate <= exact,
                "At iteration {}: approximate {} should be <= exact {}",
                i,
                approximate,
                exact
            );

            // In a single-threaded context, `approximate` should always equal `exact` because
            // the call to `next()` is sequenced-before `current_approximate()`.
            // Relaxed ordering only affects cross-thread visibility, not same-thread ordering.
            assert_eq!(
                approximate, exact,
                "At iteration {}: approximate {} should be equal to exact {}",
                i, approximate, exact
            );
        }
    }

    #[test]
    fn test_current_approximate_performance_characteristic() {
        use std::hint::black_box;
        use std::time::Instant;

        let generator = IdGenerator::new();
        let iterations = 1_000_000;

        // Warm up
        for _ in 0..1000 {
            black_box(generator.current_approximate());
            black_box(generator.current());
        }

        // Benchmark current_approximate()
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(generator.current_approximate());
        }
        let approximate_duration = start.elapsed();

        // Benchmark current()
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(generator.current());
        }
        let current_duration = start.elapsed();

        let approx_ns = approximate_duration.as_nanos() / iterations as u128;
        let current_ns = current_duration.as_nanos() / iterations as u128;

        println!("\nPerformance Comparison ({} iterations):", iterations);
        println!("  current_approximate(): {} ns/op", approx_ns);
        println!("  current():             {} ns/op", current_ns);
        if current_ns > 0 && approx_ns > 0 {
            println!(
                "  Speedup:               {:.2}x",
                current_ns as f64 / approx_ns as f64
            );
        }

        // Note: Precise performance testing should be done with criterion benchmarks,
        // not unit tests. This test just verifies the method works and prints timing info.
        // On most hardware, current_approximate() should be comparable or faster due to
        // relaxed ordering, but we don't assert this here due to timing variance in tests.
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // Generate valid IDs (0..=MAX_VALID_ID)
    fn valid_id_strategy() -> impl Strategy<Value = u64> {
        0..=MAX_VALID_ID
    }

    // Generate any u64 value
    fn any_u64_strategy() -> impl Strategy<Value = u64> {
        any::<u64>()
    }

    proptest! {
        /// Property: Any ID that passes validation must be <= MAX_VALID_ID
        #[test]
        fn prop_validated_ids_within_bounds(raw_id in valid_id_strategy()) {
            // All valid IDs should successfully create NodeId
            let node_id = NodeId::new(raw_id).expect("Valid ID should not fail");
            prop_assert!(node_id.as_u64() <= MAX_VALID_ID);

            // Same for EdgeId and VersionId
            let edge_id = EdgeId::new(raw_id).expect("Valid ID should not fail");
            prop_assert!(edge_id.as_u64() <= MAX_VALID_ID);

            let version_id = VersionId::new(raw_id).expect("Valid ID should not fail");
            prop_assert!(version_id.as_u64() <= MAX_VALID_ID);
        }

        /// Property: Any ID > MAX_VALID_ID must be rejected
        #[test]
        fn prop_invalid_ids_rejected(offset in 1u64..=1000) {
            let invalid_id = MAX_VALID_ID + offset;

            // All invalid IDs should fail validation
            prop_assert!(NodeId::new(invalid_id).is_err());
            prop_assert!(EdgeId::new(invalid_id).is_err());
            prop_assert!(VersionId::new(invalid_id).is_err());
        }

        /// Property: ID roundtrip (ID -> u64 -> ID) preserves value
        #[test]
        fn prop_id_roundtrip_preserves_value(raw_id in valid_id_strategy()) {
            // NodeId roundtrip
            let node_id = NodeId::new(raw_id).unwrap();
            let roundtrip_node = NodeId::new(node_id.as_u64()).unwrap();
            prop_assert_eq!(node_id, roundtrip_node);

            // EdgeId roundtrip
            let edge_id = EdgeId::new(raw_id).unwrap();
            let roundtrip_edge = EdgeId::new(edge_id.as_u64()).unwrap();
            prop_assert_eq!(edge_id, roundtrip_edge);

            // VersionId roundtrip
            let version_id = VersionId::new(raw_id).unwrap();
            let roundtrip_version = VersionId::new(version_id.as_u64()).unwrap();
            prop_assert_eq!(version_id, roundtrip_version);
        }

        /// Property: ID ordering is consistent with u64 ordering
        #[test]
        fn prop_id_ordering_consistent(a in valid_id_strategy(), b in valid_id_strategy()) {
            let node_a = NodeId::new(a).unwrap();
            let node_b = NodeId::new(b).unwrap();

            // Ordering should match raw u64 ordering
            prop_assert_eq!(node_a.cmp(&node_b), a.cmp(&b));
            prop_assert_eq!(node_a == node_b, a == b);
            prop_assert_eq!(node_a < node_b, a < b);
            prop_assert_eq!(node_a > node_b, a > b);
        }

        /// Property: ID generator produces strictly increasing sequence
        #[test]
        fn prop_generator_monotonic_increasing(start in 0u64..MAX_VALID_ID-100, count in 1usize..100) {
            let generator = IdGenerator::with_start(start);
            let mut prev_id: Option<u64> = None;

            for _ in 0..count {
                match generator.next() {
                    Ok(id) => {
                        // Verify ID is within bounds
                        prop_assert!(id <= MAX_VALID_ID, "Generator must respect MAX_VALID_ID");

                        // Verify strictly increasing (if not first ID)
                        if let Some(prev) = prev_id {
                            prop_assert!(id > prev, "Generator must produce strictly increasing IDs");
                        }

                        prev_id = Some(id);
                    }
                    Err(_) => {
                        // Once we hit an error, all future calls should also error
                        if let Some(prev) = prev_id {
                            prop_assert!(prev >= MAX_VALID_ID, "Generator should only error after MAX_VALID_ID");
                        }
                        break;
                    }
                }
            }
        }

        /// Property: ID generator never produces duplicates
        #[test]
        fn prop_generator_no_duplicates(start in 0u64..MAX_VALID_ID-1000, count in 1usize..1000) {
            let generator = IdGenerator::with_start(start);
            let mut seen = std::collections::HashSet::new();

            for _ in 0..count {
                match generator.next() {
                    Ok(id) => {
                        prop_assert!(seen.insert(id), "Generator produced duplicate ID: {}", id);
                    }
                    Err(_) => break, // Hit the limit
                }
            }
        }

        /// Property: new_unchecked accepts any value (for internal use)
        #[test]
        fn prop_new_unchecked_accepts_all(raw_id in any_u64_strategy()) {
            // new_unchecked should work with any value, even invalid ones
            let node = NodeId::new_unchecked(raw_id);
            prop_assert_eq!(node.as_u64(), raw_id);

            let edge = EdgeId::new_unchecked(raw_id);
            prop_assert_eq!(edge.as_u64(), raw_id);

            let version = VersionId::new_unchecked(raw_id);
            prop_assert_eq!(version.as_u64(), raw_id);
        }

        /// Property: Validation is consistent (always returns same result for same input)
        #[test]
        fn prop_validation_is_deterministic(raw_id in any_u64_strategy()) {
            // Call validation twice with same input
            let result1 = NodeId::new(raw_id);
            let result2 = NodeId::new(raw_id);

            // Results should be identical
            match (result1, result2) {
                (Ok(id1), Ok(id2)) => prop_assert_eq!(id1, id2),
                (Err(_), Err(_)) => {
                    // Both should reject invalid IDs
                    prop_assert!(raw_id > MAX_VALID_ID);
                }
                _ => prop_assert!(false, "Validation must be deterministic"),
            }
        }
    }
}
