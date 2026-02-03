use gallifreydb::core::history::{EntityHistory, VersionInfo, VersionDiff};
use gallifreydb::core::id::VersionId;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::temporal::{BiTemporalInterval, time};

fn main() {
    println!("🎨 Mosaic Visual Verification Script\n");

    // Create some timestamps
    let t1 = time::now();
    // Simulate a later time for v2
    // We can't easily manipulate HybridTimestamp directly without internal APIs,
    // but distinct calls to now() might return same time if too fast.
    // However, for display it doesn't matter if they are identical.
    let t2 = time::now();

    // Create a VersionInfo (v1)
    let v1 = VersionInfo {
        version_number: 1,
        version_id: VersionId::new(1).unwrap(),
        temporal: BiTemporalInterval::current(t1),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .insert("role", "user")
            .build(),
        label: "Person".to_string(),
    };

    // Create a VersionInfo (v2)
    let v2 = VersionInfo {
        version_number: 2,
        version_id: VersionId::new(2).unwrap(),
        temporal: BiTemporalInterval::current(t2),
        properties: PropertyMapBuilder::new()
            .insert("name", "Alice") // Unchanged
            .insert("age", 31i64)    // Modified
            .insert("active", false) // Modified (bool)
            // "role" removed
            .insert("department", "Engineering") // Added
            .build(),
        label: "Person".to_string(),
    };

    // Print VersionInfo
    println!("--- VersionInfo Display (v1) ---");
    println!("{}\n", v1);

    // Create EntityHistory
    let history = EntityHistory {
        versions: vec![v1.clone(), v2.clone()],
    };

    // Print EntityHistory
    println!("--- EntityHistory Display ---");
    println!("{}\n", history);

    // Create VersionDiff
    let diff = VersionDiff::compute(
        &v1.properties,
        &v2.properties,
        v1.version_id,
        v2.version_id,
    );

    // Print VersionDiff
    println!("--- VersionDiff Display ---");
    println!("{}\n", diff);
}
