//! A second global init must fail cleanly and never corrupt the first
//! provider/propagator (Issue #3376, LOW4). Runs in its own test binary because
//! it installs a real process-global subscriber.
//!
//! Run with: `cargo test --test otel_double_init --features otel`

#![cfg(feature = "otel")]

use aletheiadb::observability::otel::{
    self, InMemorySpanExporter, OtelConfig, OtelError, SamplerConfig,
};
use serial_test::serial;

fn config() -> OtelConfig {
    OtelConfig {
        enabled: true,
        sampler: SamplerConfig::AlwaysOn,
        ..OtelConfig::default()
    }
}

#[test]
#[serial]
fn second_init_reports_already_set_and_preserves_first_provider() {
    let (guard1, exp1): (_, InMemorySpanExporter) =
        otel::init_in_memory_global(&config()).expect("first init installs the global subscriber");
    assert!(otel::is_active(), "first init must activate tracing");

    // A second init loses the subscriber race. With the LOW4 fix it attempts the
    // subscriber install FIRST, so it returns `SubscriberAlreadySet` without
    // swapping out the first provider/propagator — and it does not panic.
    // (`OtelGuard` is not `Debug`, so reduce to a bool before asserting.)
    let already_set = matches!(
        otel::init_in_memory_global(&config()),
        Err(OtelError::SubscriberAlreadySet(_))
    );
    assert!(
        already_set,
        "second init should report SubscriberAlreadySet without panicking"
    );
    assert!(
        otel::is_active(),
        "the first provider must remain active after a failed re-init"
    );

    // The first provider still records and exports.
    {
        let span = tracing::info_span!("aletheiadb.http.request");
        let _g = span.enter();
    }
    guard1.force_flush();
    assert!(
        exp1.spans()
            .iter()
            .any(|s| s.name == "aletheiadb.http.request"),
        "the first provider must still export after a failed re-init"
    );

    drop(guard1);
}
