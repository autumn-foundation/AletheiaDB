//! End-to-end tests for the `aletheia completions <shell>` subcommand
//! (Issue #3619 — shell completions).
//!
//! Each test invokes the real built binary via `env!("CARGO_BIN_EXE_aletheia")`
//! in its own process. Completion generation must NOT touch the data directory,
//! but every invocation is still given its own `TempDir` for
//! `ALETHEIADB_DATA_DIR` (and `ALETHEIADB_CONFIG` is removed) to keep the
//! harness identical to `tests/cli_aletheia.rs` and isolated from ambient state.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// Result of running the CLI binary once.
struct CliRun {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the CLI binary with `args`. When `data_dir` is `Some`, it is exported as
/// `ALETHEIADB_DATA_DIR`; when `None`, that variable is explicitly removed from
/// the child's environment so the process sees no durable data directory.
fn run(args: &[&str], data_dir: Option<&Path>) -> CliRun {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aletheia"));
    cmd.args(args);
    // Never let an ambient config path interfere with these tests.
    cmd.env_remove("ALETHEIADB_CONFIG");
    match data_dir {
        Some(dir) => {
            cmd.env("ALETHEIADB_DATA_DIR", dir);
        }
        None => {
            cmd.env_remove("ALETHEIADB_DATA_DIR");
        }
    }
    let output = cmd.output().expect("failed to spawn aletheia binary");
    CliRun {
        code: output.status.code().expect("process terminated by signal"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn completions_generate_for_every_supported_shell() {
    // Each shell asserts a shell-SPECIFIC hallmark (case-sensitive, as
    // clap_complete emits it) rather than a generic `aletheia` substring, so a
    // regression that emits the wrong dialect for a shell is caught.
    let hallmarks = [
        ("bash", "complete"),
        ("zsh", "#compdef"),
        ("fish", "complete -c aletheia"),
        ("powershell", "Register-ArgumentCompleter"),
        ("elvish", "edit:completion"),
    ];
    for (shell, hallmark) in hallmarks {
        let dir = TempDir::new().expect("tempdir");
        let r = run(&["completions", shell], Some(dir.path()));
        assert_eq!(
            r.code, 0,
            "completions {shell} must exit 0; stderr={:?}",
            r.stderr
        );
        assert!(
            !r.stdout.trim().is_empty(),
            "completions {shell} must produce a non-empty script; stdout={:?}",
            r.stdout
        );
        assert!(
            r.stdout.contains("aletheia"),
            "completions {shell} script must reference the binary name; stdout len={}",
            r.stdout.len()
        );
        assert!(
            r.stdout.contains(hallmark),
            "completions {shell} script must contain shell-specific hallmark `{hallmark}`; \
             stdout len={}",
            r.stdout.len()
        );
    }
}

/// The completions command is pure output: it must never create the data
/// directory or write anything into it.
#[test]
fn completions_do_not_touch_data_dir() {
    let dir = TempDir::new().expect("tempdir");
    let r = run(&["completions", "bash"], Some(dir.path()));
    assert_eq!(
        r.code, 0,
        "completions bash must exit 0; stderr={:?}",
        r.stderr
    );
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "completions must not create data-dir contents"
    );
}

#[test]
fn completions_bash_contains_bash_hallmark() {
    let dir = TempDir::new().expect("tempdir");
    let r = run(&["completions", "bash"], Some(dir.path()));
    assert_eq!(
        r.code, 0,
        "completions bash must exit 0; stderr={:?}",
        r.stderr
    );
    assert!(
        r.stdout.contains("complete") || r.stdout.contains("_aletheia"),
        "bash completion must contain a bash-completion hallmark; stdout={:?}",
        r.stdout
    );
}

#[test]
fn completions_missing_shell_arg_errors_and_lists_shells() {
    let dir = TempDir::new().expect("tempdir");
    let r = run(&["completions"], Some(dir.path()));
    assert_ne!(r.code, 0, "completions with no shell must exit non-zero");
    let msg = format!("{}{}", r.stdout, r.stderr).to_lowercase();
    for shell in ["bash", "zsh", "fish"] {
        assert!(
            msg.contains(shell),
            "error must mention supported shell `{shell}`; got={msg:?}"
        );
    }
}

#[test]
fn completions_unknown_shell_errors() {
    let dir = TempDir::new().expect("tempdir");
    let r = run(&["completions", "bogusshell"], Some(dir.path()));
    assert_ne!(
        r.code, 0,
        "unknown shell must exit non-zero; stdout={:?}",
        r.stdout
    );
    let msg = format!("{}{}", r.stdout, r.stderr);
    assert!(
        msg.contains("bogusshell"),
        "error must name the bad value `bogusshell`; got={msg:?}"
    );
    assert!(
        msg.contains("bash"),
        "error must list a supported shell (bash); got={msg:?}"
    );
}

#[test]
fn help_documents_completions_command() {
    let r = run(&["help"], None);
    assert_eq!(r.code, 0, "help must exit 0; stderr={:?}", r.stderr);
    assert!(
        r.stdout.contains("completions"),
        "usage must document the completions command; stdout={:?}",
        r.stdout
    );
}
