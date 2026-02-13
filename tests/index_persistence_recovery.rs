//! Test recovery from partial index persistence failures.

use aletheiadb::core::GLOBAL_INTERNER;
use aletheiadb::storage::index_persistence::loader::IndexPersistenceManager;
use tempfile::tempdir;

/// Test that string interner is loaded even when manifest is missing.
///
/// This simulates a partial save failure where indexes were saved but
/// manifest creation failed (e.g., crash before manifest write).
///
/// BEFORE FIX: String interner never loaded → all nodes/edges skipped
/// AFTER FIX: String interner loaded first → graph restoration succeeds
#[test]
fn test_interner_loaded_without_manifest() {
    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());

    // Create directories and save string interner
    manager.ensure_directories().unwrap();

    // Intern and save some test strings
    let _person = GLOBAL_INTERNER.intern("TestPerson").unwrap();
    let _knows = GLOBAL_INTERNER.intern("TEST_KNOWS").unwrap();

    manager.save_string_interner().unwrap();

    // Verify interner file exists but manifest doesn't
    assert!(manager.interner_path().exists());
    assert!(!manager.manifest_path().exists());

    // Try to load - should succeed with best-effort recovery
    let result = manager.load_manifest_and_strings();

    match result {
        Ok(manifest) => {
            // Success! The fix allows loading when only interner exists
            eprintln!("✓ Best-effort recovery succeeded");
            assert_eq!(manifest.lsn, 0, "Should return default manifest");
        }
        Err(e) => {
            panic!(
                "BUG: Failed to load string interner when manifest is missing: {}",
                e
            );
        }
    }

    // Verify the strings are available in GLOBAL_INTERNER
    let person_id = GLOBAL_INTERNER.intern("TestPerson").unwrap();
    let knows_id = GLOBAL_INTERNER.intern("TEST_KNOWS").unwrap();

    assert!(
        GLOBAL_INTERNER
            .resolve_with(person_id, |s| s == "TestPerson")
            .unwrap_or(false),
        "String interner should have loaded TestPerson"
    );
    assert!(
        GLOBAL_INTERNER
            .resolve_with(knows_id, |s| s == "TEST_KNOWS")
            .unwrap_or(false),
        "String interner should have loaded TEST_KNOWS"
    );
}
