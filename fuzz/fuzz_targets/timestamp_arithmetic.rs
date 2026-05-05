#![no_main]

use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::{BiTemporalInterval, TimeRange};
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
struct TimestampArithmeticCase {
    local: HybridTimestamp,
    message: HybridTimestamp,
    physical_delta: i32,
}

fuzz_target!(|case: TimestampArithmeticCase| {
    let physical = case
        .local
        .wallclock()
        .saturating_add(i64::from(case.physical_delta));

    if let Ok(sent) = case.local.send(physical) {
        assert!(sent >= case.local);
    }

    if let Ok(received) = case.local.receive(case.message, physical) {
        assert!(received >= case.local);
        assert!(received >= case.message);
    }

    let start = case.local.min(case.message);
    let end = case.local.max(case.message);

    if let Ok(range) = TimeRange::new(start, end) {
        if !range.is_empty() {
            assert!(range.contains(start));
            assert!(!range.contains(end));
        }

        let serialized = range.serialize();
        let (decoded, consumed) =
            TimeRange::deserialize(&serialized).expect("serialized TimeRange must deserialize");
        assert_eq!(consumed, serialized.len());
        assert_eq!(decoded, range);

        let interval = BiTemporalInterval::new(range, TimeRange::from(start));
        let serialized_interval = interval.serialize();
        let (decoded_interval, interval_consumed) =
            BiTemporalInterval::deserialize(&serialized_interval)
                .expect("serialized BiTemporalInterval must deserialize");
        assert_eq!(interval_consumed, serialized_interval.len());
        assert_eq!(decoded_interval, interval);
    }
});
