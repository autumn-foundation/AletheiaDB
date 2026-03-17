#[test]
fn test_max_backward_drift_us_exact() {
    // Assert exactly 5 minutes in microseconds
    assert_eq!(aletheiadb::core::hlc::MAX_BACKWARD_DRIFT_US, 300_000_000);
}
#[test]
fn test_max_forward_jump_us_exact() {
    // Assert exactly 1 hour in microseconds
    assert_eq!(aletheiadb::core::hlc::MAX_FORWARD_JUMP_US, 3_600_000_000);
}
#[test]
fn test_send_max_valid_timestamp_boundary() {
    let current = aletheiadb::core::hlc::HybridTimestamp::new(aletheiadb::core::temporal::MAX_VALID_TIMESTAMP, 0).unwrap();
    // Valid update to the exact boundary
    let next = current.send(aletheiadb::core::temporal::MAX_VALID_TIMESTAMP).unwrap();
    assert_eq!(next.wallclock(), aletheiadb::core::temporal::MAX_VALID_TIMESTAMP);
    assert_eq!(next.logical(), 1);

    // Invalid update beyond boundary
    let err = current.send(aletheiadb::core::temporal::MAX_VALID_TIMESTAMP + 1).unwrap_err();
    assert!(matches!(err, aletheiadb::core::error::TemporalError::InvalidTimestamp { .. }));
}
#[test]
fn test_receive_max_valid_timestamp_boundary() {
    let local = aletheiadb::core::hlc::HybridTimestamp::new(aletheiadb::core::temporal::MAX_VALID_TIMESTAMP, 0).unwrap();
    let msg = aletheiadb::core::hlc::HybridTimestamp::new(aletheiadb::core::temporal::MAX_VALID_TIMESTAMP, 10).unwrap();

    // Valid receive exactly at boundary
    let next = local.receive(msg, aletheiadb::core::temporal::MAX_VALID_TIMESTAMP).unwrap();
    assert_eq!(next.wallclock(), aletheiadb::core::temporal::MAX_VALID_TIMESTAMP);
    assert_eq!(next.logical(), 11);

    // Invalid receive where physical clock > MAX_VALID_TIMESTAMP
    let err = local.receive(msg, aletheiadb::core::temporal::MAX_VALID_TIMESTAMP + 1).unwrap_err();
    assert!(matches!(err, aletheiadb::core::error::TemporalError::InvalidTimestamp { .. }));
}
