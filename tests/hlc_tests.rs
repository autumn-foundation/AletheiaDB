//! Tests for Hybrid Logical Clock (HLC) implementation.
//!
//! These tests verify HLC behavior for distributed temporal consistency:
//! - Monotonic ordering with clock skew
//! - Causality preservation
//! - Serialization/deserialization

use aletheiadb::core::hlc::{HybridTimestamp, MAX_BACKWARD_DRIFT_US, evaluate_clock_skew};
use aletheiadb::core::temporal::MAX_VALID_TIMESTAMP;
use aletheiadb::core::error::{StorageError, TemporalError};
use proptest::prelude::*;

#[test]
fn test_evaluate_clock_skew_exact_boundaries() {
    let current = 1_000_000;

    // Test Backward Drift Boundary
    // Drift = current - frontier
    // We want drift = -MAX_BACKWARD_DRIFT_US
    // So 1_000_000 - frontier = -MAX_BACKWARD_DRIFT_US
    // frontier = 1_000_000 + MAX_BACKWARD_DRIFT_US
    let frontier_exact = current + MAX_BACKWARD_DRIFT_US;

    // Should be OK (exact boundary allowed)
    let result = evaluate_clock_skew(current, frontier_exact, None, false);
    assert!(
        result.is_ok(),
        "Exact backward drift limit should be allowed, got {:?}",
        result
    );

    // Test strictly greater backward drift (violation)
    let frontier_violation = current + MAX_BACKWARD_DRIFT_US + 1;
    let result = evaluate_clock_skew(current, frontier_violation, None, false);
    assert!(
        result.is_err(),
        "Exceeding backward drift limit should fail"
    );

    // Test Forward Jump Boundary
    let max_forward = 100_000;
    // Drift = current - frontier
    // We want drift = max_forward
    // current - frontier = max_forward
    // frontier = current - max_forward
    let frontier_forward_exact = current - max_forward;

    let result = evaluate_clock_skew(current, frontier_forward_exact, Some(max_forward), false);
    assert!(
        result.is_ok(),
        "Exact forward jump limit should be allowed, got {:?}",
        result
    );

    // Test strictly greater forward drift (violation)
    let frontier_forward_violation = current - max_forward - 1;
    let result = evaluate_clock_skew(
        current,
        frontier_forward_violation,
        Some(max_forward),
        false,
    );
    assert!(result.is_err(), "Exceeding forward jump limit should fail");
}

#[test]
fn test_new_validates_wallclock() {
    // Valid wallclock should succeed
    let valid = HybridTimestamp::new(1_000_000, 0);
    assert!(valid.is_ok());

    // MAX_VALID_TIMESTAMP should succeed
    let at_max = HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0);
    assert!(at_max.is_ok());

    // Wallclock exceeding MAX_VALID_TIMESTAMP should fail
    let invalid = HybridTimestamp::new(MAX_VALID_TIMESTAMP + 1, 0);
    assert!(matches!(
        invalid,
        Err(TemporalError::InvalidTimestamp { .. })
    ));

    // i64::MAX should fail
    let max_i64 = HybridTimestamp::new(i64::MAX, 0);
    assert!(matches!(
        max_i64,
        Err(TemporalError::InvalidTimestamp { .. })
    ));
}

#[test]
fn test_send_when_wallclock_advances() {
    // When wallclock advances, logical counter should reset to 0
    let prev = HybridTimestamp::new(1000, 5).unwrap(); // Previous timestamp with logical=5
    let new_wallclock = 2000; // Wallclock has advanced

    let next = prev.send(new_wallclock).unwrap();

    // Wallclock should advance
    assert_eq!(next.wallclock(), 2000);
    // Logical should reset to 0
    assert_eq!(next.logical(), 0);
}

#[test]
fn test_send_when_wallclock_same() {
    // When wallclock doesn't advance, logical counter should increment
    let prev = HybridTimestamp::new(1000, 5).unwrap();
    let same_wallclock = 1000; // Wallclock hasn't advanced

    let next = prev.send(same_wallclock).unwrap();

    // Wallclock should stay the same
    assert_eq!(next.wallclock(), 1000);
    // Logical should increment
    assert_eq!(next.logical(), 6);
}

#[test]
fn test_send_when_wallclock_regresses() {
    // When wallclock regresses (clock skew), use previous wallclock and increment logical
    let prev = HybridTimestamp::new(2000, 3).unwrap();
    let regressed_wallclock = 1500; // Wallclock went backwards

    let next = prev.send(regressed_wallclock).unwrap();

    // Should keep previous wallclock (monotonicity)
    assert_eq!(next.wallclock(), 2000);
    // Logical should increment
    assert_eq!(next.logical(), 4);
}

#[test]
fn test_serialization_into_buffer() {
    // serialize_into should append to existing buffer
    let hlc = HybridTimestamp::new(1000, 5).unwrap();
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
    assert!(
        matches!(result, Err(StorageError::CorruptedData(_))),
        "Expected CorruptedData error"
    );
}

#[test]
fn test_deserialize_validates_wallclock() {
    // Deserialization should reject invalid wallclock values
    // Use MAX_VALID_TIMESTAMP + 1, not i64::MAX (which is allowed as TIMESTAMP_MAX sentinel)
    let invalid_wallclock = MAX_VALID_TIMESTAMP + 1;
    let logical = 42u32;

    let mut buffer = Vec::new();
    buffer.extend_from_slice(&invalid_wallclock.to_le_bytes());
    buffer.extend_from_slice(&logical.to_le_bytes());

    let result = HybridTimestamp::deserialize(&buffer);
    assert!(
        matches!(result, Err(StorageError::CorruptedData(_))),
        "deserialize should reject wallclock > MAX_VALID_TIMESTAMP (except i64::MAX sentinel)"
    );

    // Valid wallclock should succeed
    let valid_wallclock = MAX_VALID_TIMESTAMP;
    let mut valid_buffer = Vec::new();
    valid_buffer.extend_from_slice(&valid_wallclock.to_le_bytes());
    valid_buffer.extend_from_slice(&logical.to_le_bytes());

    let result = HybridTimestamp::deserialize(&valid_buffer);
    assert!(
        result.is_ok(),
        "deserialize should accept wallclock <= MAX_VALID_TIMESTAMP"
    );

    // i64::MAX should be allowed as TIMESTAMP_MAX sentinel value
    let sentinel_wallclock = i64::MAX;
    let mut sentinel_buffer = Vec::new();
    sentinel_buffer.extend_from_slice(&sentinel_wallclock.to_le_bytes());
    sentinel_buffer.extend_from_slice(&logical.to_le_bytes());

    let result = HybridTimestamp::deserialize(&sentinel_buffer);
    assert!(
        result.is_ok(),
        "deserialize should accept i64::MAX as TIMESTAMP_MAX sentinel"
    );
}

#[test]
fn test_send_logical_overflow() {
    // When logical counter is at u32::MAX, incrementing should return an error
    let prev = HybridTimestamp::new(1000, u32::MAX).unwrap();

    // This should return Err because logical counter would overflow
    let result = prev.send(1000);
    assert!(
        matches!(result, Err(TemporalError::LogicalCounterOverflow { .. })),
        "send should return LogicalCounterOverflow error"
    );
}

#[test]
fn test_receive_when_message_ahead() {
    // When receiving a message with timestamp ahead of local time
    let local = HybridTimestamp::new(1000, 5).unwrap();
    let msg = HybridTimestamp::new(2000, 10).unwrap();
    let new_wallclock = 1500; // Local wallclock between local and msg

    let next = local.receive(msg, new_wallclock).unwrap();

    // Should use max(local.wallclock, msg.wallclock, new_wallclock) = 2000
    assert_eq!(next.wallclock(), 2000);
    // Logical should be msg.logical + 1
    assert_eq!(next.logical(), 11);
}

#[test]
fn test_receive_when_wallclock_advances() {
    // When receiving a message but local wallclock has advanced past it
    let local = HybridTimestamp::new(1000, 5).unwrap();
    let msg = HybridTimestamp::new(900, 3).unwrap();
    let new_wallclock = 2000; // Wallclock advances

    let next = local.receive(msg, new_wallclock).unwrap();

    // Should use max wallclock = 2000
    assert_eq!(next.wallclock(), 2000);
    // Logical should reset to 0
    assert_eq!(next.logical(), 0);
}

#[test]
fn test_receive_when_all_same_wallclock() {
    // When local, msg, and new_wallclock are all the same
    let local = HybridTimestamp::new(1000, 5).unwrap();
    let msg = HybridTimestamp::new(1000, 3).unwrap();
    let new_wallclock = 1000;

    let next = local.receive(msg, new_wallclock).unwrap();

    // Wallclock stays 1000
    assert_eq!(next.wallclock(), 1000);
    // Logical should be max(local.logical, msg.logical) + 1 = max(5, 3) + 1 = 6
    assert_eq!(next.logical(), 6);
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
        let hlc = HybridTimestamp::new(wallclock, logical).unwrap();
        let next = hlc.send(new_wallclock).unwrap();

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
        let mut current = HybridTimestamp::new(initial_wallclock, initial_logical).unwrap();

        for wallclock in wallclocks {
            let next = current.send(wallclock).unwrap();
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
        wallclock in i64::MIN..=MAX_VALID_TIMESTAMP,
        logical in 0u32..u32::MAX,
    ) {
        // Serialization must be lossless
        let original = HybridTimestamp::new(wallclock, logical).unwrap();
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
        let a = HybridTimestamp::new(w1, l1).unwrap();
        let b = HybridTimestamp::new(w2, l2).unwrap();
        let c = HybridTimestamp::new(w3, l3).unwrap();

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
        let hlc = HybridTimestamp::new(wallclock, logical).unwrap();
        let new_wallclock = wallclock.saturating_add(advance_by);

        // Only test if wallclock actually advances
        prop_assume!(new_wallclock > wallclock);

        let next = hlc.send(new_wallclock).unwrap();

        prop_assert_eq!(next.wallclock(), new_wallclock);
        prop_assert_eq!(next.logical(), 0, "logical must reset to 0 when wallclock advances");
    }

    #[test]
    fn prop_wallclock_same_increments_logical(
        wallclock in 0i64..1_000_000_000_000i64,
        logical in 0u32..1000u32,
    ) {
        // When wallclock doesn't advance, logical counter must increment
        let hlc = HybridTimestamp::new(wallclock, logical).unwrap();
        let next = hlc.send(wallclock).unwrap();  // Same wallclock

        prop_assert_eq!(next.wallclock(), wallclock);
        prop_assert_eq!(next.logical(), logical + 1, "logical must increment when wallclock stays same");
    }

    #[test]
    fn prop_receive_monotonicity(
        local_wallclock in 0i64..1_000_000_000_000i64,
        local_logical in 0u32..1000u32,
        msg_wallclock in 0i64..1_000_000_000_000i64,
        msg_logical in 0u32..1000u32,
        physical_wallclock in 0i64..1_000_000_000_000i64,
    ) {
        // receive() must produce timestamps >= both local and message timestamps
        let local = HybridTimestamp::new(local_wallclock, local_logical).unwrap();
        let msg = HybridTimestamp::new(msg_wallclock, msg_logical).unwrap();

        let next = local.receive(msg, physical_wallclock).unwrap();

        // Result must be >= local timestamp
        prop_assert!(
            next >= local,
            "receive must produce timestamp >= local: local={:?}, next={:?}",
            local,
            next
        );

        // Result must be >= message timestamp
        prop_assert!(
            next >= msg,
            "receive must produce timestamp >= message: msg={:?}, next={:?}",
            msg,
            next
        );
    }

    #[test]
    fn prop_receive_causality_preservation(
        local_wallclock in 0i64..1_000_000_000_000i64,
        local_logical in 0u32..100u32,
        msg_wallclock in 0i64..1_000_000_000_000i64,
        msg_logical in 0u32..100u32,
        physical_wallclock in 0i64..1_000_000_000_000i64,
    ) {
        // If msg > local, then receive(msg) must produce timestamp > msg (causality)
        let local = HybridTimestamp::new(local_wallclock, local_logical).unwrap();
        let msg = HybridTimestamp::new(msg_wallclock, msg_logical).unwrap();

        let next = local.receive(msg, physical_wallclock).unwrap();

        if msg > local {
            prop_assert!(
                next > msg,
                "receive must preserve causality: if msg > local, then result > msg. msg={:?}, local={:?}, next={:?}",
                msg,
                local,
                next
            );
        }
    }

    #[test]
    fn prop_receive_sequence_monotonic(
        initial_wallclock in 0i64..1_000_000_000_000i64,
        initial_logical in 0u32..100u32,
        events in prop::collection::vec(
            (0i64..1_000_000_000_000i64, 0u32..100u32, 0i64..1_000_000_000_000i64),
            1..10
        ),
    ) {
        // A sequence of receive operations must produce monotonically increasing timestamps
        let mut current = HybridTimestamp::new(initial_wallclock, initial_logical).unwrap();

        for (msg_wc, msg_log, phys_wc) in events {
            let msg = HybridTimestamp::new(msg_wc, msg_log).unwrap();
            let next = current.receive(msg, phys_wc).unwrap();

            prop_assert!(
                next > current,
                "each receive must produce strictly greater timestamp: current={:?}, msg={:?}, next={:?}",
                current,
                msg,
                next
            );

            current = next;
        }
    }
}
