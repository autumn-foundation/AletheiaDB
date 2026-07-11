//! End-to-end tests for the `aletheia` CLI binary (`src/bin/aletheia.rs`).
//!
//! Issue #3480 — Layer 2 mutation-kill coverage.
//!
//! Each test invokes the real built binary via `env!("CARGO_BIN_EXE_aletheia")`
//! in its own process, so no `#[serial]` coordination is needed. Every test
//! that opens a durable database gets its own `TempDir` for
//! `ALETHEIADB_DATA_DIR`, keeping the graph state isolated between tests.
//!
//! Assertions target stdout CONTENT, stderr text, and EXIT CODES so that
//! return-value-stub and condition-flip mutants in the CLI routing, argument
//! guards, traverse direction filter, and backup-report serialization are
//! killed.

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

/// Parse the last non-empty stdout line as JSON (backup/restore print a single
/// JSON line; create/get pretty-print a JSON object — both parse whole-stdout
/// fine except backup which is one compact line).
fn stdout_json(out: &str) -> serde_json::Value {
    serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("stdout not JSON ({e}): {out:?}"))
}

// ============================================================================
// help / usage / unknown command
// ============================================================================

#[test]
fn help_lists_subcommands_and_exits_zero() {
    // kills: routing stubs for the help arm; usage-content field drops.
    let r = run(&["help"], None);
    assert_eq!(r.code, 0, "help must exit 0; stderr={:?}", r.stderr);
    for sub in ["node", "edge", "traverse", "backup", "restore"] {
        assert!(
            r.stdout.contains(sub),
            "usage must mention `{sub}`; stdout={:?}",
            r.stdout
        );
    }
}

#[test]
fn no_args_prints_usage_and_exits_zero() {
    // kills: collapsing the `None` arm into an error path.
    let r = run(&[], None);
    assert_eq!(r.code, 0, "no-args must exit 0; stderr={:?}", r.stderr);
    assert!(
        r.stdout.contains("Usage:"),
        "expected usage banner; stdout={:?}",
        r.stdout
    );
}

#[test]
fn unknown_command_errors_with_exit_one() {
    // kills: the `Some(cmd) => Err(...)` fallthrough being stubbed to Ok.
    let r = run(&["frobnicate"], None);
    assert_eq!(r.code, 1, "unknown command must exit 1");
    assert!(r.stderr.contains("error:"), "stderr={:?}", r.stderr);
    assert!(
        r.stderr.contains("unknown command"),
        "stderr={:?}",
        r.stderr
    );
    assert!(r.stderr.contains("frobnicate"), "stderr={:?}", r.stderr);
}

// ============================================================================
// node create / get + argument guards
// ============================================================================

#[test]
fn node_create_then_get_roundtrips() {
    // kills: create/get routing stubs and node_id/field drops.
    let dir = TempDir::new().unwrap();
    let dp = Some(dir.path());

    let create = run(
        &[
            "node",
            "create",
            "Person",
            "--properties",
            r#"{"name":"Alice"}"#,
        ],
        dp,
    );
    assert_eq!(
        create.code, 0,
        "create must exit 0; stderr={:?}",
        create.stderr
    );
    let cj = stdout_json(&create.stdout);
    let node_id = cj
        .get("node_id")
        .and_then(|v| v.as_u64())
        .expect("node_id must be numeric");

    let get = run(&["node", "get", &node_id.to_string()], dp);
    assert_eq!(get.code, 0, "get must exit 0; stderr={:?}", get.stderr);
    let gj = stdout_json(&get.stdout);
    assert_eq!(gj.get("id").and_then(|v| v.as_u64()), Some(node_id));
    assert_eq!(gj.get("label"), Some(&serde_json::json!("Person")));
    assert_eq!(
        gj.get("properties").and_then(|p| p.get("name")),
        Some(&serde_json::json!("Alice"))
    );
}

#[test]
fn node_create_missing_label_exits_one_with_usage() {
    // kills: `args.len() < 2` guard flip on node create.
    let dir = TempDir::new().unwrap();
    let r = run(&["node", "create"], Some(dir.path()));
    assert_eq!(r.code, 1, "missing label must exit 1");
    assert!(r.stderr.contains("usage:"), "stderr={:?}", r.stderr);
}

#[test]
fn node_get_wrong_arity_exits_one() {
    // kills: `args.len() != 2` guard flip on node get.
    let dir = TempDir::new().unwrap();
    let r = run(&["node", "get"], Some(dir.path()));
    assert_eq!(r.code, 1, "missing id must exit 1");
    assert!(r.stderr.contains("usage:"), "stderr={:?}", r.stderr);
}

// ============================================================================
// edge create / get + argument guards
// ============================================================================

#[test]
fn edge_create_then_get_roundtrips() {
    // kills: edge routing stubs and source/target/id/label field drops.
    let dir = TempDir::new().unwrap();
    let dp = Some(dir.path());

    let a = run(
        &[
            "node",
            "create",
            "Person",
            "--properties",
            r#"{"name":"A"}"#,
        ],
        dp,
    );
    assert_eq!(a.code, 0, "stderr={:?}", a.stderr);
    let a_id = stdout_json(&a.stdout)
        .get("node_id")
        .and_then(|v| v.as_u64())
        .unwrap();
    let b = run(
        &[
            "node",
            "create",
            "Person",
            "--properties",
            r#"{"name":"B"}"#,
        ],
        dp,
    );
    assert_eq!(b.code, 0, "stderr={:?}", b.stderr);
    let b_id = stdout_json(&b.stdout)
        .get("node_id")
        .and_then(|v| v.as_u64())
        .unwrap();

    let e = run(
        &[
            "edge",
            "create",
            &a_id.to_string(),
            &b_id.to_string(),
            "KNOWS",
        ],
        dp,
    );
    assert_eq!(e.code, 0, "edge create must exit 0; stderr={:?}", e.stderr);
    let edge_id = stdout_json(&e.stdout)
        .get("edge_id")
        .and_then(|v| v.as_u64())
        .unwrap();

    let g = run(&["edge", "get", &edge_id.to_string()], dp);
    assert_eq!(g.code, 0, "edge get must exit 0; stderr={:?}", g.stderr);
    let gj = stdout_json(&g.stdout);
    assert_eq!(gj.get("id").and_then(|v| v.as_u64()), Some(edge_id));
    assert_eq!(gj.get("label"), Some(&serde_json::json!("KNOWS")));
    assert_eq!(gj.get("source").and_then(|v| v.as_u64()), Some(a_id));
    assert_eq!(gj.get("target").and_then(|v| v.as_u64()), Some(b_id));
}

#[test]
fn edge_create_short_args_exits_one() {
    // kills: `args.len() < 4` guard flip on edge create.
    let dir = TempDir::new().unwrap();
    let r = run(&["edge", "create", "0", "1"], Some(dir.path()));
    assert_eq!(r.code, 1, "short edge create must exit 1");
    assert!(r.stderr.contains("usage:"), "stderr={:?}", r.stderr);
}

#[test]
fn edge_get_wrong_arity_exits_one() {
    // kills: `args.len() != 2` guard flip on edge get.
    let dir = TempDir::new().unwrap();
    let r = run(&["edge", "get"], Some(dir.path()));
    assert_eq!(r.code, 1, "missing edge id must exit 1");
    assert!(r.stderr.contains("usage:"), "stderr={:?}", r.stderr);
}

// ============================================================================
// traverse — direction filter (kills `direction == "outgoing" || "both"` flips)
// ============================================================================

/// Build a two-node graph `a -KNOWS-> b` and return `(dir, a_id, b_id)`.
fn build_edge_graph(dir: &Path) -> (u64, u64) {
    let dp = Some(dir);
    let a = run(
        &[
            "node",
            "create",
            "Person",
            "--properties",
            r#"{"name":"A"}"#,
        ],
        dp,
    );
    assert_eq!(a.code, 0, "stderr={:?}", a.stderr);
    let a_id = stdout_json(&a.stdout)
        .get("node_id")
        .and_then(|v| v.as_u64())
        .unwrap();
    let b = run(
        &[
            "node",
            "create",
            "Person",
            "--properties",
            r#"{"name":"B"}"#,
        ],
        dp,
    );
    assert_eq!(b.code, 0, "stderr={:?}", b.stderr);
    let b_id = stdout_json(&b.stdout)
        .get("node_id")
        .and_then(|v| v.as_u64())
        .unwrap();
    let e = run(
        &[
            "edge",
            "create",
            &a_id.to_string(),
            &b_id.to_string(),
            "KNOWS",
        ],
        dp,
    );
    assert_eq!(e.code, 0, "edge create; stderr={:?}", e.stderr);
    (a_id, b_id)
}

#[test]
fn traverse_outgoing_follows_source_edges() {
    // kills: flipping/removing the `direction == "outgoing" || "both"` branch.
    let dir = TempDir::new().unwrap();
    let (a_id, b_id) = build_edge_graph(dir.path());

    let r = run(&["traverse", &a_id.to_string(), "KNOWS"], Some(dir.path()));
    assert_eq!(r.code, 0, "stderr={:?}", r.stderr);
    let j = stdout_json(&r.stdout);
    assert_eq!(j.get("direction"), Some(&serde_json::json!("outgoing")));
    let results = j.get("results").and_then(|v| v.as_array()).unwrap();
    assert_eq!(
        results.len(),
        1,
        "outgoing must reach exactly b; got {results:?}"
    );
    assert_eq!(
        results[0].get("direction"),
        Some(&serde_json::json!("outgoing"))
    );
    assert_eq!(
        results[0].get("node_id").and_then(|v| v.as_u64()),
        Some(b_id)
    );
}

#[test]
fn traverse_incoming_excludes_outgoing_edges() {
    // kills: the incoming branch being merged with / replaced by the outgoing one.
    let dir = TempDir::new().unwrap();
    let (a_id, b_id) = build_edge_graph(dir.path());

    // Source node has NO incoming KNOWS edge -> empty results.
    let from_a = run(
        &[
            "traverse",
            &a_id.to_string(),
            "KNOWS",
            "--direction",
            "incoming",
        ],
        Some(dir.path()),
    );
    assert_eq!(from_a.code, 0, "stderr={:?}", from_a.stderr);
    let ja = stdout_json(&from_a.stdout);
    assert_eq!(ja.get("direction"), Some(&serde_json::json!("incoming")));
    assert_eq!(
        ja.get("results").and_then(|v| v.as_array()).unwrap().len(),
        0,
        "source node must have no incoming edges"
    );

    // Target node DOES have an incoming KNOWS edge back to a.
    let from_b = run(
        &[
            "traverse",
            &b_id.to_string(),
            "KNOWS",
            "--direction",
            "incoming",
        ],
        Some(dir.path()),
    );
    assert_eq!(from_b.code, 0, "stderr={:?}", from_b.stderr);
    let jb = stdout_json(&from_b.stdout);
    let results = jb.get("results").and_then(|v| v.as_array()).unwrap();
    assert_eq!(
        results.len(),
        1,
        "target must have one incoming edge; got {results:?}"
    );
    assert_eq!(
        results[0].get("direction"),
        Some(&serde_json::json!("incoming"))
    );
    assert_eq!(
        results[0].get("node_id").and_then(|v| v.as_u64()),
        Some(a_id)
    );
}

#[test]
fn traverse_both_includes_outgoing_edge() {
    // kills: the `|| direction == "both"` disjunct being dropped from either branch.
    let dir = TempDir::new().unwrap();
    let (a_id, b_id) = build_edge_graph(dir.path());

    let r = run(
        &[
            "traverse",
            &a_id.to_string(),
            "KNOWS",
            "--direction",
            "both",
        ],
        Some(dir.path()),
    );
    assert_eq!(r.code, 0, "stderr={:?}", r.stderr);
    let j = stdout_json(&r.stdout);
    assert_eq!(j.get("direction"), Some(&serde_json::json!("both")));
    let results = j.get("results").and_then(|v| v.as_array()).unwrap();
    assert_eq!(
        results.len(),
        1,
        "both from source must include the outgoing edge"
    );
    assert_eq!(
        results[0].get("direction"),
        Some(&serde_json::json!("outgoing"))
    );
    assert_eq!(
        results[0].get("node_id").and_then(|v| v.as_u64()),
        Some(b_id)
    );
}

#[test]
fn traverse_short_args_exits_one() {
    // kills: `args.len() < 2` guard flip on traverse.
    let dir = TempDir::new().unwrap();
    let r = run(&["traverse", "0"], Some(dir.path()));
    assert_eq!(r.code, 1, "short traverse must exit 1");
    assert!(r.stderr.contains("usage:"), "stderr={:?}", r.stderr);
}

// ============================================================================
// backup — assert exact report field VALUES (kills stub-to-0 per field)
// ============================================================================

#[test]
fn backup_reports_exact_counts_for_known_graph() {
    // kills: stub-to-0 / stub-to-Default on each backup-report field.
    let dir = TempDir::new().unwrap();
    let dp = Some(dir.path());

    // Create exactly 2 nodes + 1 edge.
    let (_a, _b) = build_edge_graph(dir.path());

    let backup_path = dir.path().join("out.albk");
    let r = run(&["backup", backup_path.to_str().unwrap()], dp);
    assert_eq!(r.code, 0, "backup must exit 0; stderr={:?}", r.stderr);
    assert!(backup_path.exists(), "backup file must be written");

    let j = stdout_json(&r.stdout);
    assert_eq!(j.get("ok"), Some(&serde_json::json!(true)));
    assert_eq!(
        j.get("current_node_count").and_then(|v| v.as_u64()),
        Some(2),
        "expected 2 nodes; report={j:?}"
    );
    assert_eq!(
        j.get("current_edge_count").and_then(|v| v.as_u64()),
        Some(1),
        "expected 1 edge; report={j:?}"
    );
    assert_eq!(
        j.get("node_versions").and_then(|v| v.as_u64()),
        Some(2),
        "expected 2 node versions; report={j:?}"
    );
    assert_eq!(
        j.get("edge_versions").and_then(|v| v.as_u64()),
        Some(1),
        "expected 1 edge version; report={j:?}"
    );
    // bytes_written and source_lsn are non-zero for a real backup; stub-to-0
    // mutants would report 0 here.
    assert!(
        j.get("bytes_written").and_then(|v| v.as_u64()).unwrap() > 0,
        "bytes_written must be > 0; report={j:?}"
    );
    assert!(
        j.get("source_lsn").and_then(|v| v.as_u64()).unwrap() > 0,
        "source_lsn must be > 0; report={j:?}"
    );
}

#[test]
fn backup_missing_arg_exits_one() {
    // kills: the `args.first().ok_or_else(...)` usage guard on backup.
    let dir = TempDir::new().unwrap();
    let r = run(&["backup"], Some(dir.path()));
    assert_eq!(r.code, 1, "missing backup path must exit 1");
    assert!(r.stderr.contains("usage:"), "stderr={:?}", r.stderr);
}

// ============================================================================
// restore — missing env, non-empty target, and successful materialization
// ============================================================================

/// Produce a valid `.albk` artifact (2 nodes + 1 edge) at `path`.
fn make_backup_artifact(artifact: &Path) {
    let src = TempDir::new().unwrap();
    let _ = build_edge_graph(src.path());
    let r = run(&["backup", artifact.to_str().unwrap()], Some(src.path()));
    assert_eq!(
        r.code, 0,
        "backup for artifact must succeed; stderr={:?}",
        r.stderr
    );
    assert!(artifact.exists(), "artifact must be written");
}

#[test]
fn restore_without_data_dir_env_exits_one() {
    // kills: dropping the ALETHEIADB_DATA_DIR requirement on restore.
    let art_dir = TempDir::new().unwrap();
    let artifact = art_dir.path().join("a.albk");
    make_backup_artifact(&artifact);

    let r = run(&["restore", artifact.to_str().unwrap()], None);
    assert_eq!(r.code, 1, "restore without data dir must exit 1");
    assert!(
        r.stderr.contains("ALETHEIADB_DATA_DIR"),
        "stderr must mention the missing data dir; stderr={:?}",
        r.stderr
    );
}

#[test]
fn restore_missing_arg_exits_one() {
    // kills: the `args.first().ok_or_else(...)` usage guard on restore.
    let r = run(&["restore"], None);
    assert_eq!(r.code, 1, "missing restore path must exit 1");
    assert!(r.stderr.contains("usage:"), "stderr={:?}", r.stderr);
}

#[test]
fn restore_into_fresh_dir_succeeds_and_materializes() {
    // kills: restore success path / ok:true field being stubbed away.
    let art_dir = TempDir::new().unwrap();
    let artifact = art_dir.path().join("a.albk");
    make_backup_artifact(&artifact);

    let target = TempDir::new().unwrap();
    let r = run(
        &["restore", artifact.to_str().unwrap()],
        Some(target.path()),
    );
    assert_eq!(
        r.code, 0,
        "restore into fresh dir must exit 0; stderr={:?}",
        r.stderr
    );
    let j = stdout_json(&r.stdout);
    assert_eq!(j.get("ok"), Some(&serde_json::json!(true)));
    assert_eq!(
        j.get("data_dir").and_then(|v| v.as_str()),
        Some(target.path().to_str().unwrap())
    );
    // Restore must have materialized index files on disk.
    assert!(
        target.path().join("indexes").join("manifest.idx").exists(),
        "restore must materialize a manifest.idx under the target"
    );
}

#[test]
fn restore_into_non_empty_dir_errors() {
    // kills: dropping the check_target_empty (TargetNotEmpty) precondition.
    let art_dir = TempDir::new().unwrap();
    let artifact = art_dir.path().join("a.albk");
    make_backup_artifact(&artifact);

    let target = TempDir::new().unwrap();
    // First restore populates the target (creates indexes/manifest.idx).
    let first = run(
        &["restore", artifact.to_str().unwrap()],
        Some(target.path()),
    );
    assert_eq!(
        first.code, 0,
        "first restore must succeed; stderr={:?}",
        first.stderr
    );

    // Second restore into the now-occupied directory must be refused.
    let second = run(
        &["restore", artifact.to_str().unwrap()],
        Some(target.path()),
    );
    assert_eq!(second.code, 1, "restore into non-empty dir must exit 1");
    assert!(
        second.stderr.contains("not empty"),
        "stderr must report the non-empty target; stderr={:?}",
        second.stderr
    );
}
