//! Observability contract demo.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example observability_demo --features observability
//! cargo run --example observability_demo --features metrics-rs
//! ```

use aletheiadb::Error;
use aletheiadb::{AletheiaDB, PropertyMapBuilder, WriteOps};

#[cfg(feature = "observability")]
use aletheiadb::observability;

#[cfg(feature = "metrics-rs")]
use aletheiadb::observability::metrics_rs_adapter::MetricsRsRecorder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "observability")]
    {
        let recorder = telemetry_recorder();
        observability::install(
            observability::TelemetryConfig::builder()
                .service_name("aletheiadb-demo")
                .metrics(recorder)
                .build(),
        );

        let span = observability::query_execute_span("observability_demo", "example");
        let _entered = span.enter();
        println!("Observability contract installed");
    }

    #[cfg(not(feature = "observability"))]
    println!("Running without observability. Enable the `observability` feature for spans.");

    let db = AletheiaDB::new()?;

    let alice_id = db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30)
            .build(),
    )?;

    let bob_id = db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("age", 25)
            .build(),
    )?;

    let edge_id = db.create_edge(
        alice_id,
        bob_id,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2020).build(),
    )?;

    println!("Created Alice={alice_id:?}, Bob={bob_id:?}, edge={edge_id:?}");

    for i in 0..5 {
        db.write(|tx| {
            let node_id = tx.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", format!("Doc {i}"))
                    .insert("version", i)
                    .build(),
            )?;
            println!("Transaction {i}: created {node_id:?}");
            Ok::<_, Error>(())
        })?;
    }

    #[cfg(feature = "observability")]
    {
        let metrics = observability::metrics();
        println!("Transaction commits: {}", metrics.transaction_commits_total);
        println!("Storage errors: {}", metrics.error_storage_total);
        println!("Critical errors: {}", metrics.has_critical_errors());
    }

    Ok(())
}

#[cfg(all(feature = "observability", feature = "metrics-rs"))]
fn telemetry_recorder() -> std::sync::Arc<dyn observability::MetricsRecorder> {
    std::sync::Arc::new(MetricsRsRecorder)
}

#[cfg(all(feature = "observability", not(feature = "metrics-rs")))]
fn telemetry_recorder() -> std::sync::Arc<dyn observability::MetricsRecorder> {
    std::sync::Arc::new(observability::NoOpMetrics)
}
