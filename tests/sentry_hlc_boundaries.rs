use aletheiadb::core::hlc::{
    ClockSkewDirection, ClockSkewViolation, HybridTimestamp, MAX_BACKWARD_DRIFT_US,
    MAX_FORWARD_JUMP_US, evaluate_clock_skew,
};
use aletheiadb::core::temporal::MAX_VALID_TIMESTAMP;
use aletheiadb::utils::error::{StorageError, TemporalError};

#[test]
fn audit_hlc_max_valid_timestamp_boundary() {
    // Audit: Verify MAX_VALID_TIMESTAMP is strictly enforced.
    // Boundary: MAX_VALID_TIMESTAMP is valid.
    assert!(HybridTimestamp::new(MAX_VALID_TIMESTAMP, 0).is_ok());

    // Boundary: MAX_VALID_TIMESTAMP + 1 is invalid.
    match HybridTimestamp::new(MAX_VALID_TIMESTAMP + 1, 0) {
        Err(TemporalError::InvalidTimestamp { timestamp, .. }) => {
            assert_eq!(timestamp.wallclock(), MAX_VALID_TIMESTAMP + 1);
        }
        _ => panic!("Expected InvalidTimestamp error"),
    }
}

#[test]
fn audit_hlc_send_overflow_boundary() {
    // Audit: Verify logical overflow behavior exactly at u32::MAX.
    // If wallclock doesn't advance, send() should fail.
    let ts = HybridTimestamp::new(1000, u32::MAX).unwrap();
    match ts.send(1000) {
        Err(TemporalError::LogicalCounterOverflow {
            wallclock,
            current_logical,
        }) => {
            assert_eq!(wallclock, 1000);
            assert_eq!(current_logical, u32::MAX);
        }
        _ => panic!("Expected LogicalCounterOverflow"),
    }

    // If wallclock advances, send() should succeed (reset logical).
    let next = ts.send(1001).unwrap();
    assert_eq!(next.logical(), 0);
}

#[test]
fn audit_hlc_receive_collision_strict() {
    // Audit: Verify collision resolution logic with strict values.
    // Local(1000, 10), Msg(1000, 20), Physical(1000).
    // Result MUST be (1000, 21).
    let local = HybridTimestamp::new(1000, 10).unwrap();
    let msg = HybridTimestamp::new(1000, 20).unwrap();
    let next = local.receive(msg, 1000).unwrap();

    assert_eq!(next.wallclock(), 1000);
    assert_eq!(next.logical(), 21, "Must pick max(10, 20) + 1");
}

#[test]
fn audit_hlc_clock_skew_boundaries() {
    // Audit: Verify skew detection exactly at boundaries.
    let frontier = 1_000_000;

    // 1. Backward drift
    // Boundary: -MAX_BACKWARD_DRIFT_US (Allowed)
    let drift_ok = -MAX_BACKWARD_DRIFT_US;
    let current = frontier + drift_ok;
    let res = evaluate_clock_skew(current, frontier, None, false);
    assert!(res.is_ok(), "Drift {} should be allowed", drift_ok);

    // Boundary: -MAX_BACKWARD_DRIFT_US - 1 (Violation)
    let drift_bad = -MAX_BACKWARD_DRIFT_US - 1;
    let current = frontier + drift_bad;
    let res = evaluate_clock_skew(current, frontier, None, false);
    match res {
        Err(ClockSkewViolation {
            direction: ClockSkewDirection::Backward,
            drift_us,
            ..
        }) => {
            assert_eq!(drift_us, drift_bad);
        }
        _ => panic!("Expected Backward violation"),
    }

    // 2. Forward jump
    // Boundary: MAX_FORWARD_JUMP_US (Allowed)
    let jump_ok = MAX_FORWARD_JUMP_US;
    let current = frontier + jump_ok;
    let res = evaluate_clock_skew(current, frontier, Some(MAX_FORWARD_JUMP_US), false);
    assert!(res.is_ok(), "Jump {} should be allowed", jump_ok);

    // Boundary: MAX_FORWARD_JUMP_US + 1 (Violation)
    let jump_bad = MAX_FORWARD_JUMP_US + 1;
    let current = frontier + jump_bad;
    let res = evaluate_clock_skew(current, frontier, Some(MAX_FORWARD_JUMP_US), false);
    match res {
        Err(ClockSkewViolation {
            direction: ClockSkewDirection::Forward,
            drift_us,
            ..
        }) => {
            assert_eq!(drift_us, jump_bad);
        }
        _ => panic!("Expected Forward violation"),
    }
}

#[test]
fn audit_hlc_deserialize_sentinel() {
    // Audit: Verify i64::MAX is allowed as sentinel in deserialization.
    // Serialization format: [i64 wallclock][u32 logical].
    let mut buf = Vec::new();
    buf.extend_from_slice(&i64::MAX.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());

    let (ts, _) = HybridTimestamp::deserialize(&buf).expect("i64::MAX sentinel should be allowed");
    assert_eq!(ts.wallclock(), i64::MAX);
}

#[test]
fn audit_hlc_deserialize_invalid_timestamp() {
    // Audit: Verify MAX_VALID_TIMESTAMP + 1 is rejected in deserialization.
    let invalid = MAX_VALID_TIMESTAMP + 1;
    let mut buf = Vec::new();
    buf.extend_from_slice(&invalid.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());

    let res = HybridTimestamp::deserialize(&buf);
    assert!(
        matches!(res, Err(StorageError::CorruptedData(_))),
        "Should reject invalid wallclock"
    );
}
