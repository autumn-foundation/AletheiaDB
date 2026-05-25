#[cfg(test)]
use super::*;

// --- extracted mod sentry_tests { ---
#[allow(unused_imports)]
use super::*;

#[test]
fn test_id_generator_reset_to() {
    let generator = IdGenerator::new();
    // Generate a few IDs
    assert_eq!(generator.next(), Ok(0));
    assert_eq!(generator.next(), Ok(1));

    // Reset to a higher value (simulating recovery)
    generator.reset_to(100);
    assert_eq!(generator.current(), 100);

    // Next ID should be 100
    assert_eq!(generator.next(), Ok(100));
    assert_eq!(generator.next(), Ok(101));
}

#[test]
fn test_id_generator_ensure_at_least() {
    let generator = IdGenerator::with_start(50);

    // Ensure at least 40 (less than current 50) - should do nothing
    generator.ensure_at_least(40);
    assert_eq!(generator.current(), 50);

    // Ensure at least 50 (equal to current) - should do nothing
    generator.ensure_at_least(50);
    assert_eq!(generator.current(), 50);

    // Ensure at least 60 (greater than current) - should update
    generator.ensure_at_least(60);
    assert_eq!(generator.current(), 60);
    assert_eq!(generator.next(), Ok(60));
}

#[test]
fn test_id_generator_ensure_at_least_concurrent() {
    use std::sync::Arc;
    use std::thread;

    let generator = Arc::new(IdGenerator::new());
    let num_threads = 10;

    // Each thread tries to ensure at least its thread_id * 100
    // The final value should be the maximum of all inputs
    let handles: Vec<_> = (0..num_threads)
        .map(|i| {
            let generator_clone = Arc::clone(&generator);
            thread::spawn(move || {
                generator_clone.ensure_at_least((i as u64) * 100);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // The max input was (9 * 100) = 900
    // ensure_at_least(X) sets current to max(current, X).
    // So the final value must be >= max(inputs) = 900.

    assert!(generator.current() >= 900);

    // It should be exactly 900 unless we called next() somewhere (we didn't).
    assert_eq!(generator.current(), 900);
}

#[test]
fn test_tx_id_generator_basics() {
    let tx_gen = TxIdGenerator::new();

    // Initial state: starts at 1
    // current() returns counter - 1. So initially 1 - 1 = 0.
    // This means "last generated ID was 0" (reserved).
    assert_eq!(tx_gen.current(), TxId(0));

    // First generation
    let id1 = tx_gen.next();
    assert_eq!(id1, TxId(1));
    assert_eq!(tx_gen.current(), TxId(1));

    // Second generation
    let id2 = tx_gen.next();
    assert_eq!(id2, TxId(2));
    assert_eq!(tx_gen.current(), TxId(2));

    // Verify strict ordering
    assert!(id2 > id1);
}

#[test]
fn test_tx_id_generator_default() {
    let tx_gen: TxIdGenerator = Default::default();
    assert_eq!(tx_gen.current(), TxId(0));
}

#[test]
fn test_tx_id_display() {
    let tx_id = TxId::new(12345);
    assert_eq!(format!("{}", tx_id), "TxId(12345)");
    assert_ne!(format!("{}", tx_id), "TxId(0)");
    assert_ne!(format!("{}", tx_id), "");
}

#[test]
fn test_entity_id_conversion_negative() {
    let node_id = NodeId::new(1).unwrap();
    let entity_node: EntityId = node_id.into();

    // Should be Node, not Edge
    assert!(entity_node.is_node());
    assert!(!entity_node.is_edge());
    assert_eq!(entity_node.as_node(), Some(node_id));
    assert_eq!(entity_node.as_edge(), None);

    let edge_id = EdgeId::new(2).unwrap();
    let entity_edge: EntityId = edge_id.into();

    // Should be Edge, not Node
    assert!(!entity_edge.is_node());
    assert!(entity_edge.is_edge());
    assert_eq!(entity_edge.as_node(), None);
    assert_eq!(entity_edge.as_edge(), Some(edge_id));
}

// --- extracted mod tests { ---
#[allow(unused_imports)]
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
    assert_eq!(entity_id.as_edge(), None);
}

#[test]
fn test_entity_id_from_edge() {
    let edge_id = EdgeId::new(2).unwrap();
    let entity_id: EntityId = edge_id.into();
    assert!(!entity_id.is_node());
    assert!(entity_id.is_edge());
    assert_eq!(entity_id.as_edge(), Some(edge_id));
    assert_eq!(entity_id.as_node(), None);
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

// --- extracted mod proptests { ---
#[allow(unused_imports)]
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

    /// Property: ID ordering is transitive: if a < b and b < c then a < c
    #[test]
    fn prop_id_ordering_is_transitive(
        a in valid_id_strategy(),
        b in valid_id_strategy(),
        c in valid_id_strategy(),
    ) {
        let node_a = NodeId::new(a).unwrap();
        let node_b = NodeId::new(b).unwrap();
        let node_c = NodeId::new(c).unwrap();

        if node_a < node_b && node_b < node_c {
            prop_assert!(node_a < node_c,
                "Ordering transitivity violated: {:?} < {:?} < {:?} but a >= c",
                node_a, node_b, node_c);
        }

        // Also verify for EdgeId
        let edge_a = EdgeId::new(a).unwrap();
        let edge_b = EdgeId::new(b).unwrap();
        let edge_c = EdgeId::new(c).unwrap();

        if edge_a < edge_b && edge_b < edge_c {
            prop_assert!(edge_a < edge_c,
                "EdgeId ordering transitivity violated");
        }
    }

    /// Property: IDs from a generator never exceed MAX_VALID_ID
    #[test]
    fn prop_generator_ids_within_max_valid_id(start in 0u64..MAX_VALID_ID-50, count in 1usize..50) {
        let generator = IdGenerator::with_start(start);

        for _ in 0..count {
            match generator.next() {
                Ok(id) => {
                    prop_assert!(id <= MAX_VALID_ID,
                        "Generator produced ID {} exceeding MAX_VALID_ID {}", id, MAX_VALID_ID);
                }
                Err(_) => break,
            }
        }
    }

    /// Property: TxIdGenerator produces strictly increasing transaction IDs
    #[test]
    fn prop_tx_id_generator_monotonic(_dummy in 0..50usize) {
        let tx_gen = TxIdGenerator::new();
        let mut prev = tx_gen.next();

        for _ in 0..10 {
            let curr = tx_gen.next();
            prop_assert!(curr > prev,
                "TxId should be strictly increasing: {:?} vs {:?}", prev, curr);
            prev = curr;
        }
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

// --- extracted mod warden_repro { ---
#[allow(unused_imports)]
use super::*;

#[test]
#[should_panic(expected = "Transaction ID overflow")]
fn test_tx_id_overflow_panic() {
    let generator = TxIdGenerator::new();
    // Force counter to max to simulate exhaustion
    generator.set_counter(u64::MAX);
    // This should panic to prevent wrapping to 0
    let _ = generator.next();
}

// --- extracted mod sentinel_id_generator_tests { ---
#[allow(unused_imports)]
use super::*;

#[test]
fn test_id_generator_current_approximate_exhaustive() {
    let generator = IdGenerator::with_start(42);

    let current = generator.current_approximate();
    assert_eq!(current, 42);

    generator.next().unwrap();
    let current2 = generator.current_approximate();
    assert_eq!(current2, 43);
}

#[test]
fn test_id_generator_ensure_at_least_exhaustive() {
    let generator = IdGenerator::with_start(42);

    // This fails if `>` was replaced with `==` (since 50 != 42, it wouldn't update)
    generator.ensure_at_least(50);
    assert_eq!(generator.current(), 50);

    // This fails if `>` was replaced with `<` (since 40 < 50 is true, it would update)
    generator.ensure_at_least(40);
    assert_eq!(generator.current(), 50);
}

#[test]
fn test_tx_id_generator_next_exhaustive() {
    let generator = TxIdGenerator::new(); // Starts at 1

    // Kill "replace TxIdGenerator::next -> TxId with Default::default()"
    let first = generator.next();
    assert_eq!(first, TxId::new(1));

    // Kill "replace + with -" or "*"
    let second = generator.next();
    assert_eq!(second, TxId::new(2));

    // Kill "replace == with !=" for u64::MAX check
    // If it were `!=`, it would panic immediately because 1 != u64::MAX

    // Ensure returning default current doesn't pass
    let generator_current = generator.current();
    assert_eq!(generator_current, TxId::new(2));
}

#[test]
fn test_id_generator_next_boundaries() {
    // Kill "> with == / < / >="
    // We set generator right to MAX_VALID_ID
    let generator = IdGenerator::with_start(MAX_VALID_ID);

    // This will fetch_add MAX_VALID_ID and return MAX_VALID_ID, incrementing to MAX_VALID_ID+1
    let first = generator.next();
    assert_eq!(first, Ok(MAX_VALID_ID));

    // The next attempt will return the incremented MAX_VALID_ID+1 and fail the limit check
    let second = generator.next();
    assert!(second.is_err());

    if let Err(crate::core::error::StorageError::InvalidId { id, .. }) = second {
        assert_eq!(id, MAX_VALID_ID + 1);
    } else {
        panic!("Expected InvalidId error");
    }
}

#[test]
fn test_max_valid_id_math() {
    // Kill `replace - with +` or `/` in `pub const MAX_VALID_ID: u64 = u64::MAX - 1000;`
    let id_plus = u64::MAX.wrapping_add(1000);
    let id_div = u64::MAX / 1000;
    assert_ne!(MAX_VALID_ID, id_plus);
    assert_ne!(MAX_VALID_ID, id_div);
    assert_eq!(MAX_VALID_ID, u64::MAX - 1000);
}

#[test]
fn test_id_generator_reset_to_exhaustive() {
    let generator = IdGenerator::new();
    generator.reset_to(42);
    assert_eq!(generator.current(), 42);

    // This fails if reset_to returned early / was empty body
    let id = generator.next().unwrap();
    assert_eq!(id, 42);
}

#[test]
fn test_id_generator_default() {
    let generator: IdGenerator = Default::default();
    assert_eq!(generator.current(), 0);
}

#[test]
fn test_node_id_new_unchecked_exhaustive() {
    let unchecked = NodeId::new_unchecked(42);
    assert_eq!(unchecked.as_u64(), 42);
    assert_ne!(unchecked.as_u64(), 0);
}

#[test]
fn test_edge_id_new_unchecked_exhaustive() {
    let unchecked = EdgeId::new_unchecked(42);
    assert_eq!(unchecked.as_u64(), 42);
    assert_ne!(unchecked.as_u64(), 0);
}

#[test]
fn test_version_id_new_unchecked_exhaustive() {
    let unchecked = VersionId::new_unchecked(42);
    assert_eq!(unchecked.as_u64(), 42);
    assert_ne!(unchecked.as_u64(), 0);
}

// --- extracted mod tests_conversions { ---
#[allow(unused_imports)]
use super::*;
use std::str::FromStr;

#[test]
fn test_node_id_conversions() {
    let n = NodeId::try_from(42).unwrap();
    assert_eq!(n, NodeId::new(42).unwrap());

    let err = NodeId::try_from(u64::MAX).unwrap_err();
    assert!(matches!(err, StorageError::InvalidId { .. }));

    let n2 = NodeId::from_str("42").unwrap();
    assert_eq!(n2, n);

    let err = NodeId::from_str("invalid").unwrap_err();
    assert!(matches!(err, StorageError::InvalidId { .. }));

    let err2 = NodeId::from_str("-1").unwrap_err();
    assert!(matches!(err2, StorageError::InvalidId { .. }));

    let err3 = NodeId::from_str(&u64::MAX.to_string()).unwrap_err();
    assert!(matches!(err3, StorageError::InvalidId { .. }));
}

#[test]
fn test_edge_id_conversions() {
    let e = EdgeId::try_from(42).unwrap();
    assert_eq!(e, EdgeId::new(42).unwrap());

    let err = EdgeId::try_from(u64::MAX).unwrap_err();
    assert!(matches!(err, StorageError::InvalidId { .. }));

    let e2 = EdgeId::from_str("42").unwrap();
    assert_eq!(e2, e);

    let err = EdgeId::from_str("invalid").unwrap_err();
    assert!(matches!(err, StorageError::InvalidId { .. }));

    let err2 = EdgeId::from_str(&u64::MAX.to_string()).unwrap_err();
    assert!(matches!(err2, StorageError::InvalidId { .. }));
}

#[test]
fn test_version_id_conversions() {
    let v = VersionId::try_from(42).unwrap();
    assert_eq!(v, VersionId::new(42).unwrap());

    let err = VersionId::try_from(u64::MAX).unwrap_err();
    assert!(matches!(err, StorageError::InvalidId { .. }));

    let v2 = VersionId::from_str("42").unwrap();
    assert_eq!(v2, v);

    let err = VersionId::from_str("invalid").unwrap_err();
    assert!(matches!(err, StorageError::InvalidId { .. }));

    let err2 = VersionId::from_str(&u64::MAX.to_string()).unwrap_err();
    assert!(matches!(err2, StorageError::InvalidId { .. }));
}

#[test]
fn test_tx_id_conversions() {
    let t = TxId::try_from(42).unwrap();
    assert_eq!(t, TxId::new(42));

    let t2 = TxId::from_str("42").unwrap();
    assert_eq!(t2, t);

    let err = TxId::from_str("invalid").unwrap_err();
    assert!(matches!(err, StorageError::InvalidId { .. }));

    let err2 = TxId::from_str("-1").unwrap_err();
    assert!(matches!(err2, StorageError::InvalidId { .. }));
}

#[test]
fn test_id_conversions_exhaustive() {
    let n_try = NodeId::try_from(42).unwrap();
    assert_eq!(n_try.as_u64(), 42);

    let n_str = NodeId::from_str("42").unwrap();
    assert_eq!(n_str.as_u64(), 42);

    let e_try = EdgeId::try_from(42).unwrap();
    assert_eq!(e_try.as_u64(), 42);

    let e_str = EdgeId::from_str("42").unwrap();
    assert_eq!(e_str.as_u64(), 42);

    let v_try = VersionId::try_from(42).unwrap();
    assert_eq!(v_try.as_u64(), 42);

    let v_str = VersionId::from_str("42").unwrap();
    assert_eq!(v_str.as_u64(), 42);

    let t_try = TxId::try_from(42).unwrap();
    assert_eq!(t_try.as_u64(), 42);

    let t_str = TxId::from_str("42").unwrap();
    assert_eq!(t_str.as_u64(), 42);
}
