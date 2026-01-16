//! Tests for Hybrid Logical Clock (HLC) implementation.
//!
//! These tests verify HLC behavior for distributed temporal consistency:
//! - Monotonic ordering with clock skew
//! - Causality preservation
//! - Serialization/deserialization

use gallifreydb::core::hlc::HybridTimestamp;
use proptest::prelude::*;

#[test]
fn test_hybrid_timestamp_creation() {
    // Test creating a HybridTimestamp with wallclock and logical components
    let wallclock = 1_000_000_000_000i64; // 1 second in microseconds
    let logical = 0u32;

    let hlc = HybridTimestamp::new(wallclock, logical);

    assert_eq!(hlc.wallclock(), wallclock);
    assert_eq!(hlc.logical(), logical);
}

#[test]
fn test_send_when_wallclock_advances() {
    // When wallclock advances, logical counter should reset to 0
    let prev = HybridTimestamp::new(1000, 5); // Previous timestamp with logical=5
    let new_wallclock = 2000; // Wallclock has advanced

    let next = prev.send(new_wallclock);

    // Wallclock should advance
    assert_eq!(next.wallclock(), 2000);
    // Logical should reset to 0
    assert_eq!(next.logical(), 0);
}

#[test]
fn test_send_when_wallclock_same() {
    // When wallclock doesn't advance, logical counter should increment
    let prev = HybridTimestamp::new(1000, 5);
    let same_wallclock = 1000; // Wallclock hasn't advanced

    let next = prev.send(same_wallclock);

    // Wallclock should stay the same
    assert_eq!(next.wallclock(), 1000);
    // Logical should increment
    assert_eq!(next.logical(), 6);
}

#[test]
fn test_send_when_wallclock_regresses() {
    // When wallclock regresses (clock skew), use previous wallclock and increment logical
    let prev = HybridTimestamp::new(2000, 3);
    let regressed_wallclock = 1500; // Wallclock went backwards

    let next = prev.send(regressed_wallclock);

    // Should keep previous wallclock (monotonicity)
    assert_eq!(next.wallclock(), 2000);
    // Logical should increment
    assert_eq!(next.logical(), 4);
}

#[test]
fn test_ordering_by_wallclock() {
    // HLCs should order by wallclock first
    let earlier = HybridTimestamp::new(1000, 10);
    let later = HybridTimestamp::new(2000, 0);

    assert!(earlier < later);
    assert!(later > earlier);
    assert_ne!(earlier, later);
}

#[test]
fn test_ordering_by_logical_when_wallclock_same() {
    // When wallclock is same, order by logical counter
    let first = HybridTimestamp::new(1000, 5);
    let second = HybridTimestamp::new(1000, 6);

    assert!(first < second);
    assert!(second > first);
    assert_ne!(first, second);
}

#[test]
fn test_ordering_equality() {
    // HLCs with same wallclock and logical are equal
    let hlc1 = HybridTimestamp::new(1000, 5);
    let hlc2 = HybridTimestamp::new(1000, 5);

    assert_eq!(hlc1, hlc2);
    assert!(hlc1 >= hlc2);
    assert!(hlc1 <= hlc2);
}

#[test]
fn test_serialization_roundtrip() {
    // Serialize and deserialize should preserve exact values
    let original = HybridTimestamp::new(1_234_567_890_123_456i64, 42);

    let bytes = original.serialize();
    assert_eq!(bytes.len(), 12); // 8 bytes wallclock + 4 bytes logical

    let (deserialized, consumed) = HybridTimestamp::deserialize(&bytes).unwrap();
    assert_eq!(consumed, 12);
    assert_eq!(deserialized, original);
    assert_eq!(deserialized.wallclock(), 1_234_567_890_123_456i64);
    assert_eq!(deserialized.logical(), 42);
}

#[test]
fn test_serialization_into_buffer() {
    // serialize_into should append to existing buffer
    let hlc = HybridTimestamp::new(1000, 5);
    let mut buffer = vec![0xFF; 4]; // Pre-filled buffer

    hlc.serialize_into(&mut buffer);

    assert_eq!(buffer.len(), 16); // 4 pre-filled + 12 serialized
    assert_eq!(buffer[0], 0xFF); // Pre-filled data preserved

    // Deserialize from offset
    let (deserialized, consumed) = HybridTimestamp::deserialize(&buffer[4..]).unwrap();
    assert_eq!(consumed, 12);
    assert_eq!(deserialized, hlc);
}

#[test]
fn test_deserialize_truncated_buffer() {
    // Deserialization should fail on truncated buffer
    let short_buffer = vec![0u8; 11]; // Need 12 bytes

    let result = HybridTimestamp::deserialize(&short_buffer);
    assert!(result.is_err());
}

// Property-based tests for HLC invariants

proptest! {
    #[test]
    fn prop_monotonicity_on_send(
        wallclock in 0i64..1_000_000_000_000i64,
        logical in 0u32..1000u32,
        new_wallclock in 0i64..1_000_000_000_000i64,
    ) {
        // HLC send operation must produce monotonically increasing timestamps
        let hlc = HybridTimestamp::new(wallclock, logical);
        let next = hlc.send(new_wallclock);

        // Next timestamp must be >= current timestamp
        prop_assert!(next >= hlc, "send must produce monotonically increasing timestamps");
    }

    #[test]
    fn prop_send_sequence_is_monotonic(
        initial_wallclock in 0i64..1_000_000_000_000i64,
        initial_logical in 0u32..100u32,
        wallclocks in prop::collection::vec(0i64..1_000_000_000_000i64, 1..20),
    ) {
        // A sequence of send operations must produce strictly increasing HLCs
        let mut current = HybridTimestamp::new(initial_wallclock, initial_logical);

        for wallclock in wallclocks {
            let next = current.send(wallclock);
            prop_assert!(
                next > current,
                "each send must produce a strictly greater timestamp: current={:?}, next={:?}",
                current,
                next
            );
            current = next;
        }
    }

    #[test]
    fn prop_serialization_preserves_values(
        wallclock in i64::MIN..i64::MAX,
        logical in 0u32..u32::MAX,
    ) {
        // Serialization must be lossless
        let original = HybridTimestamp::new(wallclock, logical);
        let bytes = original.serialize();
        let (deserialized, consumed) = HybridTimestamp::deserialize(&bytes).unwrap();

        prop_assert_eq!(consumed, 12);
        prop_assert_eq!(deserialized, original);
        prop_assert_eq!(deserialized.wallclock(), wallclock);
        prop_assert_eq!(deserialized.logical(), logical);
    }

    #[test]
    fn prop_ordering_transitivity(
        w1 in 0i64..1_000_000_000_000i64,
        l1 in 0u32..1000u32,
        w2 in 0i64..1_000_000_000_000i64,
        l2 in 0u32..1000u32,
        w3 in 0i64..1_000_000_000_000i64,
        l3 in 0u32..1000u32,
    ) {
        // Ordering must be transitive: if a < b and b < c, then a < c
        let a = HybridTimestamp::new(w1, l1);
        let b = HybridTimestamp::new(w2, l2);
        let c = HybridTimestamp::new(w3, l3);

        if a < b && b < c {
            prop_assert!(a < c, "ordering must be transitive");
        }
    }

    #[test]
    fn prop_wallclock_advance_resets_logical(
        wallclock in 0i64..1_000_000_000_000i64,
        logical in 1u32..1000u32,  // Start with non-zero logical
        advance_by in 1i64..1_000_000i64,  // Ensure wallclock advances
    ) {
        // When wallclock advances, logical counter must reset to 0
        let hlc = HybridTimestamp::new(wallclock, logical);
        let new_wallclock = wallclock.saturating_add(advance_by);

        // Only test if wallclock actually advances
        prop_assume!(new_wallclock > wallclock);

        let next = hlc.send(new_wallclock);

        prop_assert_eq!(next.wallclock(), new_wallclock);
        prop_assert_eq!(next.logical(), 0, "logical must reset to 0 when wallclock advances");
    }

    #[test]
    fn prop_wallclock_same_increments_logical(
        wallclock in 0i64..1_000_000_000_000i64,
        logical in 0u32..1000u32,
    ) {
        // When wallclock doesn't advance, logical counter must increment
        let hlc = HybridTimestamp::new(wallclock, logical);
        let next = hlc.send(wallclock);  // Same wallclock

        prop_assert_eq!(next.wallclock(), wallclock);
        prop_assert_eq!(next.logical(), logical + 1, "logical must increment when wallclock stays same");
    }
}
