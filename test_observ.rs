// ⚠️ REQUIRES FEATURE: observability
// [dependencies]
// aletheiadb = { version = "0.1", features = ["observability"] }

use aletheiadb::observability;

fn main() {
    // Initialize observability (call once at startup)
    let config = observability::Config::from_env();
    observability::init(config);

    let db = aletheiadb::AletheiaDB::new().unwrap();

    // Metrics automatically collected
    // Check for critical errors
    let metrics = observability::metrics();
    if metrics.has_critical_errors() {
        panic!("Data corruption detected!");
    }
}
