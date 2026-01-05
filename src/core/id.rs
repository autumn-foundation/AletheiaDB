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
    #[inline]
    pub fn next(&self) -> Result<u64, StorageError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if id > MAX_VALID_ID {
            return Err(StorageError::InvalidId {
                id,
                id_type: "generated",
            });
        }
        Ok(id)
    }

    /// Get the current value without incrementing.
    #[inline]
    pub fn current(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed)
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
