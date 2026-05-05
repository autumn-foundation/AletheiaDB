//! Issue #155 fuzzing infrastructure checks.
//!
//! These tests keep the cargo-fuzz setup from silently rotting. The fuzz targets
//! themselves exercise runtime behavior; this test verifies the repository
//! contract that makes those targets discoverable and runnable.

use std::fs;
use std::path::Path;

const FUZZ_TARGETS: [&str; 5] = [
    "wal_entry_parsing",
    "wal_replay",
    "temporal_reconstruction",
    "property_serialization",
    "timestamp_arithmetic",
];

#[test]
fn cargo_fuzz_manifest_defines_initial_issue_155_targets() {
    let manifest_path = Path::new("fuzz/Cargo.toml");
    let manifest = fs::read_to_string(manifest_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest_path.display()));

    assert!(
        manifest.contains("cargo-fuzz = true"),
        "fuzz manifest must be marked for cargo-fuzz"
    );
    assert!(
        manifest.contains("libfuzzer-sys"),
        "fuzz manifest must depend on libfuzzer-sys"
    );
    assert!(
        manifest.contains("arbitrary"),
        "structured fuzz targets must use the arbitrary crate"
    );

    for target in FUZZ_TARGETS {
        assert!(
            manifest.contains(&format!("name = \"{target}\"")),
            "missing fuzz target {target} in fuzz/Cargo.toml"
        );

        let target_path = Path::new("fuzz/fuzz_targets").join(format!("{target}.rs"));
        assert!(
            target_path.exists(),
            "missing fuzz target source {}",
            target_path.display()
        );
    }
}

#[test]
fn crate_exposes_fuzz_only_support_module_without_lint_noise() {
    let lib_rs = fs::read_to_string("src/lib.rs").expect("read src/lib.rs");
    assert!(
        lib_rs.contains("pub mod fuzzing;"),
        "src/lib.rs must expose the fuzzing support module behind cfg"
    );

    let cargo_toml = fs::read_to_string("Cargo.toml").expect("read Cargo.toml");
    assert!(
        cargo_toml.contains("cfg(fuzzing)"),
        "Cargo.toml must whitelist cfg(fuzzing) for -D warnings"
    );
}

#[test]
fn testing_docs_describe_local_and_ci_fuzzing_workflow() {
    let testing = fs::read_to_string("TESTING.md").expect("read TESTING.md");

    for required in [
        "cargo install cargo-fuzz",
        "cargo fuzz run wal_entry_parsing",
        "cargo fuzz run wal_replay",
        "cargo fuzz run temporal_reconstruction",
        "cargo fuzz run property_serialization",
        "cargo fuzz run timestamp_arithmetic",
    ] {
        assert!(
            testing.contains(required),
            "TESTING.md must document `{required}`"
        );
    }

    let workflow_path = Path::new(".github/workflows/fuzz.yml");
    assert!(
        workflow_path.exists(),
        "nightly fuzzing workflow is missing at {}",
        workflow_path.display()
    );
}
