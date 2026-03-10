use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::{BiTemporalInterval, TimeRange};

#[test]
fn test_bitemporal_is_currently_valid_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let bi_open = BiTemporalInterval::new(TimeRange::from(ts100), TimeRange::from(ts100));
    let bi_closed = BiTemporalInterval::new(TimeRange::at(ts100), TimeRange::at(ts100));

    assert!(
        bi_open.is_currently_valid(),
        "is_currently_valid should not be false when open"
    );
    assert!(
        !bi_closed.is_currently_valid(),
        "is_currently_valid should not be true when closed"
    );
}

#[test]
fn test_bitemporal_is_currently_recorded_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let bi_open = BiTemporalInterval::new(TimeRange::from(ts100), TimeRange::from(ts100));
    let bi_closed = BiTemporalInterval::new(TimeRange::at(ts100), TimeRange::at(ts100));

    assert!(
        bi_open.is_currently_recorded(),
        "is_currently_recorded should not be false when open"
    );
    assert!(
        !bi_closed.is_currently_recorded(),
        "is_currently_recorded should not be true when closed"
    );
}

#[test]
fn test_bitemporal_is_current_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let bi_open = BiTemporalInterval::new(TimeRange::from(ts100), TimeRange::from(ts100));
    let bi_closed = BiTemporalInterval::new(TimeRange::at(ts100), TimeRange::at(ts100));

    assert!(
        bi_open.is_current(),
        "is_current should not be false when open"
    );
    assert!(
        !bi_closed.is_current(),
        "is_current should not be true when closed"
    );
}

#[test]
fn test_bitemporal_is_valid_at_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let bi_open = BiTemporalInterval::new(TimeRange::from(ts100), TimeRange::from(ts100));
    let bi_closed = BiTemporalInterval::new(TimeRange::at(ts100), TimeRange::at(ts100));

    assert!(
        bi_open.is_valid_at(ts100),
        "is_valid_at should not be false within bounds"
    );
    assert!(
        !bi_closed.is_valid_at(ts200),
        "is_valid_at should not be true outside bounds"
    );
}

#[test]
fn test_bitemporal_is_recorded_at_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let bi_open = BiTemporalInterval::new(TimeRange::from(ts100), TimeRange::from(ts100));
    let bi_closed = BiTemporalInterval::new(TimeRange::at(ts100), TimeRange::at(ts100));

    assert!(
        bi_open.is_recorded_at(ts100),
        "is_recorded_at should not be false within bounds"
    );
    assert!(
        !bi_closed.is_recorded_at(ts200),
        "is_recorded_at should not be true outside bounds"
    );
}

#[test]
fn test_bitemporal_is_visible_at_defaults() {
    let ts100 = HybridTimestamp::new(100, 0).unwrap();
    let ts200 = HybridTimestamp::new(200, 0).unwrap();
    let bi_open = BiTemporalInterval::new(TimeRange::from(ts100), TimeRange::from(ts100));
    let bi_closed = BiTemporalInterval::new(TimeRange::at(ts100), TimeRange::at(ts100));

    assert!(
        bi_open.is_visible_at(ts100, ts100),
        "is_visible_at should not be false within bounds"
    );
    assert!(
        !bi_closed.is_visible_at(ts200, ts200),
        "is_visible_at should not be true outside bounds"
    );
}
