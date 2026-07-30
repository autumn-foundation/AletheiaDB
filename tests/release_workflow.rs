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

/// Both prior releases (v0.1.0, v0.1.1) failed at `cargo publish` with
/// "a value is required for '--token <TOKEN>'" — the secret expanded to an
/// empty argument. The token must therefore travel via the environment, and
/// the empty case must be named explicitly so it is never re-diagnosed from a
/// clap error that says nothing about secrets.
#[test]
fn release_workflow_diagnoses_a_missing_crates_io_token() {
    let workflow = fs::read_to_string(".github/workflows/release.yml")
        .expect("release workflow should be readable");

    assert!(
        !workflow.contains("--token ${{ secrets.CARGO_REGISTRY_TOKEN }}"),
        "an unset secret expands to an empty --token argument, which fails \
         with a clap usage error that never mentions the missing secret; \
         pass CARGO_REGISTRY_TOKEN through the environment instead"
    );
    assert!(
        workflow.contains("CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}"),
        "the publish step should receive the token via the environment"
    );
    assert!(
        workflow.contains("Check for the crates.io token"),
        "the publish job should name the empty-secret case explicitly"
    );
}

/// 0.1.0 and 0.1.1 were both published by hand shortly BEFORE their tags were
/// pushed, so crates.io publishing is not on the release critical path. A
/// missing or expired token must therefore fail only the publish job — it must
/// not gate the GitHub release and binary assets, which do work today.
#[test]
fn missing_token_does_not_gate_the_github_release() {
    let workflow = fs::read_to_string(".github/workflows/release.yml")
        .expect("release workflow should be readable");

    let preflight = workflow
        .find("  preflight:")
        .expect("release workflow should define a preflight job");
    let publish = workflow
        .find("  publish-crate:")
        .expect("release workflow should define a publish-crate job");

    // The token check must live in publish-crate, not in the gating preflight.
    let token_check = workflow
        .find("Check for the crates.io token")
        .expect("the token check should exist");
    assert!(
        token_check > publish,
        "the crates.io token check belongs in publish-crate; in preflight it \
         would abort the whole pipeline — including the GitHub release and \
         binary assets — on every tag whenever the secret is absent"
    );

    let preflight_block = &workflow[preflight..publish];
    assert!(
        !preflight_block.contains("CARGO_REGISTRY_TOKEN"),
        "preflight must not reference CARGO_REGISTRY_TOKEN"
    );
}

/// The preflight checks that remain must precede the public release.
#[test]
fn release_workflow_preflight_gates_the_public_release() {
    let workflow = fs::read_to_string(".github/workflows/release.yml")
        .expect("release workflow should be readable");

    let preflight = workflow
        .find("  preflight:")
        .expect("release workflow should define a preflight job");
    let create_release = workflow
        .find("  create-release:")
        .expect("release workflow should define a create-release job");
    assert!(
        preflight < create_release,
        "preflight must be declared before create-release so the version and \
         changelog checks gate the public release"
    );
    assert!(
        workflow.contains("needs: preflight"),
        "at least one job should depend on preflight, or it gates nothing"
    );
}

/// A tag whose version disagrees with `Cargo.toml` would publish the wrong
/// version to crates.io, which cannot be undone.
#[test]
fn release_workflow_verifies_tag_matches_crate_version() {
    let workflow = fs::read_to_string(".github/workflows/release.yml")
        .expect("release workflow should be readable");

    assert!(
        workflow.contains("Verify the tag matches the crate version"),
        "the release workflow must refuse a tag that disagrees with Cargo.toml"
    );
    assert!(
        workflow.contains("cargo publish --locked"),
        "release publishes should use the committed Cargo.lock"
    );
}

/// The version in `Cargo.toml` must have a matching, dated `CHANGELOG.md`
/// section — otherwise a tag cuts a release with no release notes.
#[test]
fn changelog_documents_the_current_crate_version() {
    let manifest = fs::read_to_string("Cargo.toml").expect("Cargo.toml should be readable");
    let version = manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = "))
        .map(|v| v.trim().trim_matches('"').to_string())
        .expect("Cargo.toml should declare a package version");

    let changelog = fs::read_to_string("CHANGELOG.md").expect("CHANGELOG.md should be readable");
    let heading = format!("## [{version}] - ");
    assert!(
        changelog.contains(&heading),
        "CHANGELOG.md is missing a dated '## [{version}] - <date>' section; \
         release notes must land before the release is tagged"
    );

    // The released section must precede older ones, and `[Unreleased]` must not
    // be carrying entries that belong in the release being cut.
    let unreleased = changelog
        .find("## [Unreleased]")
        .expect("CHANGELOG.md should keep an [Unreleased] section");
    let released = changelog
        .find(&heading)
        .expect("release heading was just asserted to exist");
    assert!(
        unreleased < released,
        "[Unreleased] should sit above the most recent released section"
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
