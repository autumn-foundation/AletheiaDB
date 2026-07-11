//! Integration test for the `aletheia demo` CLI subcommand (Issue #3380 AC3).
//!
//! Invokes the built `aletheia` binary via `CARGO_BIN_EXE_aletheia` and asserts
//! that `aletheia demo` boots a seeded, ephemeral bi-temporal graph and prints
//! the guided showcase (current-state, AS OF point-in-time, traversal, and
//! history) — exiting 0 with no data dir, no server, and no network.
//!
//! This locks the one-command experience so it can never silently drift from
//! `examples/demo.rs` / `tests/quickstart_demo.rs`.

use std::process::Command;

/// Runs `aletheia demo` and returns (success, stdout).
fn run_demo() -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_aletheia"))
        .arg("demo")
        // Intentionally provide no ALETHEIADB_DATA_DIR / config: the demo must
        // be self-contained and ephemeral.
        .output()
        .expect("failed to spawn `aletheia demo`");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("`aletheia demo` exited non-zero.\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }
    (output.status.success(), stdout)
}

#[test]
fn demo_exits_zero_and_prints_all_showcase_sections() {
    let (success, stdout) = run_demo();
    assert!(success, "`aletheia demo` should exit 0");

    // Each of the three required showcase queries must be present, plus the
    // history section.
    let expected_markers = ["Current-state lookup", "AS OF", "Traversal", "History"];
    for marker in expected_markers {
        assert!(
            stdout.contains(marker),
            "demo output missing showcase marker {marker:?}.\nfull output:\n{stdout}"
        );
    }
}

#[test]
fn demo_output_names_the_seeded_cast() {
    let (_success, stdout) = run_demo();

    // The seeded narrative must be visible in the output.
    for key in ["Alice", "Company", "KNOWS"] {
        assert!(
            stdout.contains(key),
            "demo output missing seeded key {key:?}.\nfull output:\n{stdout}"
        );
    }
}

#[test]
fn demo_time_travel_answer_differs_from_current_state() {
    let (_success, stdout) = run_demo();

    // The AS OF differentiator: Alice is a "CTO" now but was an "Engineer" on
    // the founding day. Assert on the exact founding-day line — a marker UNIQUE
    // to the point-in-time (AS OF) reconstruction — so the test fails if AS OF
    // silently regresses to returning the CURRENT (CTO) state. (A bare
    // "Engineer" substring is printed anyway by Bob/Carol/the filler engineers
    // in the traversal section, so it cannot guard this claim.)
    assert!(
        stdout.contains("→ On founding day, Alice was: Engineer"),
        "expected the AS OF founding-day line to report 'Engineer' (the \
         point-in-time reconstruction must not return current state).\n\
         full output:\n{stdout}"
    );
    // And the current-state line must still show the latest title.
    assert!(
        stdout.contains("→ Alice is currently: CTO"),
        "expected the current-state line to report 'CTO'.\nfull output:\n{stdout}"
    );
}
