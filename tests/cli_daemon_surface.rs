//! `aletheia daemon` operator surface — Issue #2905.
//!
//! The daemon is the one local database owner, so its CLI has to answer three
//! operator questions without guesswork:
//!
//! 1. *Where is it?* — `daemon status` reports the base URL and the `/mcp`
//!    endpoint, and `--json` emits a machine-readable block (including a
//!    ready-to-paste MCP client configuration) so a user never has to
//!    reconstruct the URL by hand.
//! 2. *Which surface does it serve?* — `start` records the surface it launched
//!    (`unified`, which serves `/mcp`, or the `legacy` HTTP-only server).
//! 3. *Is it really running?* — liveness must not silently report "not running"
//!    on platforms without `/proc`.
//!
//! These tests never spawn a database: `daemon status` only reads its pid file.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

struct CliRun {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str], data_dir: Option<&Path>) -> CliRun {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aletheia"));
    cmd.args(args);
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

/// Write a pid file describing a daemon that is definitely *not* running.
///
/// The pid comes from a process we spawned and reaped, rather than a "surely
/// nobody has this" constant: `pid_max` is 4194304 on many distros, so a
/// hardcoded 999999 can be a live process and turn these into confusing
/// intermittent failures.
fn write_stale_pid_file(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("daemon.pid");
    std::fs::write(
        &path,
        format!(
            "{}\n/nonexistent/aletheia-daemon\n127.0.0.1\n18080\nunified\n",
            reaped_pid()
        ),
    )
    .expect("write pid file");
    path
}

/// A pid that is guaranteed dead: spawn a trivial process and reap it.
fn reaped_pid() -> u32 {
    let mut child = spawn_long_lived_process();
    let pid = child.id();
    let _ = child.kill();
    let _ = child.wait();
    pid
}

/// `daemon status` must tell the operator the daemon's URL and its MCP
/// endpoint — the two values every MCP client configuration needs.
#[test]
fn daemon_status_reports_base_url_and_mcp_endpoint() {
    let dir = TempDir::new().expect("tempdir");
    let pid_file = write_stale_pid_file(dir.path());

    let out = run(
        &["daemon", "status", "--pid-file", pid_file.to_str().unwrap()],
        None,
    );

    assert_eq!(out.code, 0, "status exits 0: {}{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains("http://127.0.0.1:18080"),
        "status prints the base URL: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("http://127.0.0.1:18080/mcp"),
        "status prints the MCP endpoint: {}",
        out.stdout
    );
}

/// `daemon status --json` is the machine-readable form: it carries the daemon
/// URL, the MCP endpoint, the surface, and a paste-ready MCP client config.
#[test]
fn daemon_status_json_emits_a_paste_ready_mcp_client_config() {
    let dir = TempDir::new().expect("tempdir");
    let pid_file = write_stale_pid_file(dir.path());

    let out = run(
        &[
            "daemon",
            "status",
            "--json",
            "--pid-file",
            pid_file.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out.code, 0, "status --json exits 0: {}", out.stderr);

    let parsed: serde_json::Value = serde_json::from_str(out.stdout.trim())
        .unwrap_or_else(|e| panic!("status --json is not JSON ({e}): {}", out.stdout));

    assert_eq!(parsed["running"], serde_json::json!(false));
    assert_eq!(parsed["url"], "http://127.0.0.1:18080");
    assert_eq!(parsed["mcp_endpoint"], "http://127.0.0.1:18080/mcp");
    assert_eq!(parsed["surface"], "unified");

    // The client config block names the proxy binary and the daemon-URL env var
    // an MCP client must be configured with.
    let client = &parsed["mcp_client_config"]["mcpServers"]["aletheiadb"];
    assert!(
        client["command"]
            .as_str()
            .is_some_and(|c| c.contains("aletheia-mcp")),
        "config names the aletheia-mcp proxy: {parsed}"
    );
    assert_eq!(
        client["env"]["ALETHEIADB_DAEMON_URL"], "http://127.0.0.1:18080",
        "config points the proxy at this daemon: {parsed}"
    );
}

/// A pid file written by an older release (pid + exe only) must still load —
/// status degrades to "unknown URL" rather than failing.
#[test]
fn daemon_status_accepts_a_legacy_two_line_pid_file() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("daemon.pid");
    std::fs::write(
        &path,
        format!("{}\n/nonexistent/aletheia-server\n", reaped_pid()),
    )
    .expect("write pid file");

    let out = run(
        &["daemon", "status", "--pid-file", path.to_str().unwrap()],
        None,
    );
    assert_eq!(
        out.code, 0,
        "a legacy pid file is readable: {}{}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout.contains("not running"),
        "status still reports liveness: {}",
        out.stdout
    );
}

/// `daemon start --surface` only accepts the two documented surfaces; an
/// unknown value is a clear error, not a silent default.
#[test]
fn daemon_start_rejects_an_unknown_surface() {
    let dir = TempDir::new().expect("tempdir");
    let pid_file = dir.path().join("daemon.pid");

    let out = run(
        &[
            "daemon",
            "start",
            "--surface",
            "quantum",
            "--pid-file",
            pid_file.to_str().unwrap(),
        ],
        Some(dir.path()),
    );

    assert_eq!(out.code, 1, "an unknown surface fails: {}", out.stdout);
    assert!(
        out.stderr.contains("surface"),
        "the error names the offending flag: {}",
        out.stderr
    );
    assert!(
        !pid_file.exists(),
        "a rejected start writes no pid file (no half-started daemon)"
    );
}

/// The whole point of the daemon is that **one** process owns the files. A CLI
/// command that opened the same data directory while a daemon holds it would be
/// a second writer — exactly the configuration the daemon exists to prevent.
#[test]
fn cli_refuses_to_open_a_data_dir_a_live_daemon_owns() {
    let dir = TempDir::new().expect("tempdir");
    // This test process is the lock owner: it is unambiguously alive, needs no
    // spawned stand-in, and cannot be confused with the process under test --
    // `run` launches the CLI as a *separate* process, so there is no self-pid
    // shortcut for it to take.
    std::fs::write(dir.path().join("daemon.lock"), live_pid().to_string()).expect("write lock");

    let out = run(&["node", "create", "Person"], Some(dir.path()));

    assert_eq!(
        out.code, 1,
        "the CLI must refuse, not become a second writer: {}",
        out.stdout
    );
    assert!(
        out.stderr.contains("daemon"),
        "the error names the daemon that owns the directory: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("aletheia daemon stop"),
        "the error says how to proceed: {}",
        out.stderr
    );
}

/// A lock left behind by a crashed daemon must not brick the CLI: liveness is
/// checked, not mere file existence.
#[test]
fn cli_ignores_a_stale_daemon_lock() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("daemon.lock"), reaped_pid().to_string()).expect("write lock");

    let out = run(&["node", "create", "Person"], Some(dir.path()));

    assert_eq!(
        out.code, 0,
        "a lock owned by a dead pid is ignored: {}{}",
        out.stdout, out.stderr
    );
}

/// A pid that is certainly alive: this test process.
fn live_pid() -> u32 {
    std::process::id()
}

/// Spawn a process that stays alive long enough to probe for liveness.
///
/// Only `daemon_liveness_uses_a_portable_pid_probe` needs this -- it is the one
/// assertion that requires a pid which is alive *and can then be reaped*. Every
/// other test here uses `live_pid()`.
///
/// Windows uses `ping`, not `timeout`: `timeout` refuses to run without a
/// console ("ERROR: Input redirection is not supported") and exits at once, so
/// under a test harness it yields an already-dead pid. Kept in step with the
/// same helper in `src/daemon_lock.rs`, which spawns for the same reason.
fn spawn_long_lived_process() -> std::process::Child {
    let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
        ("ping", vec!["-n", "31", "127.0.0.1"])
    } else {
        ("sleep", vec!["30"])
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn `{program}` as a live process: {e}"));

    // Confirm it is running before asserting anything about liveness, using
    // `try_wait` rather than the probe under test, so a dead fixture reports
    // itself instead of looking like a broken probe.
    std::thread::sleep(std::time::Duration::from_millis(100));
    match child.try_wait() {
        Ok(None) => child,
        Ok(Some(status)) => panic!(
            "the `{program}` test helper exited immediately with {status}; it cannot \
             stand in for a live process on this platform"
        ),
        Err(e) => panic!("cannot determine whether the `{program}` helper is running: {e}"),
    }
}

/// Liveness must not depend on `/proc`, which exists only on Linux.
///
/// A `/proc`-only implementation reports "not running" for every live daemon on
/// macOS and Windows, which makes `daemon stop` refuse forever. The pid probe is
/// shared with the daemon's own lock, so exercising it here proves the portable
/// path works on whatever platform this test runs on — unlike a source grep,
/// which stays green after the code it guards stops compiling.
///
/// (The Windows `tasklist` output parsing has its own unit tests in
/// `aletheiadb::daemon_lock`, runnable on any host.)
#[test]
fn daemon_liveness_uses_a_portable_pid_probe() {
    let mut child = spawn_long_lived_process();
    let pid = child.id();
    assert!(
        aletheiadb::daemon_lock::pid_is_alive(pid),
        "a running process must be reported alive on this platform"
    );
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        !aletheiadb::daemon_lock::pid_is_alive(pid),
        "a reaped process must be reported dead"
    );
}

/// Embedded `aletheia-mcp` owns storage, so it must honor the daemon's claim
/// too. Without this the likeliest daemon misconfiguration — a client left
/// without `ALETHEIADB_DAEMON_URL` while `ALETHEIADB_DATA_DIR` still points at
/// the daemon's directory — silently produces a second writer.
#[test]
fn embedded_mcp_server_refuses_a_data_dir_a_live_daemon_owns() {
    let dir = TempDir::new().expect("tempdir");
    // As above: this process is the live owner; `aletheia-mcp` is spawned separately.
    std::fs::write(dir.path().join("daemon.lock"), live_pid().to_string()).expect("write lock");

    let output = Command::new(env!("CARGO_BIN_EXE_aletheia-mcp"))
        .env_remove("ALETHEIADB_CONFIG")
        .env_remove("ALETHEIADB_DAEMON_URL")
        .env("ALETHEIADB_DATA_DIR", dir.path())
        .env("ALETHEIADB_AUTH_MODE", "anonymous")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn aletheia-mcp");

    assert!(
        !output.status.success(),
        "embedded aletheia-mcp must refuse a daemon-owned directory"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("owned by a running AletheiaDB daemon"),
        "it refuses for the right reason: {stderr}"
    );
    assert!(
        stderr.contains("ALETHEIADB_DAEMON_URL"),
        "and points at the fix — daemon-client mode: {stderr}"
    );
}
