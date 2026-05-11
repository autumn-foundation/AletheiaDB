use std::fs;
use std::path::Path;

#[test]
fn release_workflow_archives_real_cli_binary() {
    let workflow = fs::read_to_string(".github/workflows/release.yml")
        .expect("release workflow should be readable");

    assert!(
        workflow.contains("release/aletheia.exe"),
        "Windows release artifact should archive the real aletheia CLI"
    );
    assert!(
        workflow.contains("release aletheia"),
        "Unix release artifact should archive the real aletheia CLI"
    );
    assert!(
        !workflow.contains("release/aletheiadb.exe"),
        "release workflow must not archive the placeholder default binary"
    );
}

#[test]
fn release_workflow_does_not_mask_publish_failure() {
    let workflow = fs::read_to_string(".github/workflows/release.yml")
        .expect("release workflow should be readable");

    assert!(
        !workflow.contains("continue-on-error: true"),
        "crates.io publish failures must fail the release workflow"
    );
}

#[test]
fn package_does_not_ship_placeholder_default_binary() {
    let main_path = Path::new("src/main.rs");
    if main_path.exists() {
        let main_rs = fs::read_to_string(main_path).expect("src/main.rs should be readable");
        assert!(
            !main_rs.contains("Hello, world!"),
            "default package binary must not be the Cargo hello-world template"
        );
    }
}
