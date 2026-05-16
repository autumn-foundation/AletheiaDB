use std::path::Path;
use std::process::Command;

fn run_cli(data_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aletheia"))
        .args(args)
        .env_remove("ALETHEIADB_CONFIG")
        .env("ALETHEIADB_DATA_DIR", data_dir)
        .output()
        .expect("failed to run aletheia CLI")
}

fn assert_success(output: &std::process::Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_restores_persisted_interner_before_property_parsing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data_dir = temp.path();

    let first = run_cli(
        data_dir,
        &[
            "node",
            "create",
            "Decision",
            "--properties",
            r#"{"choice":"standalone","summary":"Tree-sitter codegraph starts outside AletheiaDB"}"#,
        ],
    );
    assert_success(&first, "first node create");
    let first_node: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("first create stdout should be JSON");
    assert_eq!(first_node["node_id"], 0);

    let second = run_cli(
        data_dir,
        &[
            "node",
            "create",
            "Project",
            "--properties",
            r#"{"name":"Aletheia Codegraph","summary":"Standalone Tree-sitter codebase graph memory project","status":"experimental","repo":"aletheia-codegraph"}"#,
        ],
    );
    assert_success(&second, "second node create");
    let second_node: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("second create stdout should be JSON");
    assert_eq!(second_node["node_id"], 1);

    let edge = run_cli(
        data_dir,
        &[
            "edge",
            "create",
            "0",
            "1",
            "ABOUT",
            "--properties",
            r#"{"context":"Decision concerns the project boundary","strength":1}"#,
        ],
    );
    assert_success(&edge, "edge create");

    let get_node = run_cli(data_dir, &["node", "get", "0"]);
    assert_success(&get_node, "node get");

    let stderr = String::from_utf8_lossy(&get_node.stderr);
    assert!(
        !stderr.contains("String interner mismatch")
            && !stderr.contains("data loss")
            && !stderr.contains("Failed to restore temporal versions"),
        "restore warnings indicate interner/index corruption:\n{stderr}"
    );

    let node: serde_json::Value =
        serde_json::from_slice(&get_node.stdout).expect("node get stdout should be JSON");
    assert_eq!(node["id"], 0);
    assert_eq!(node["label"], "Decision");
    assert_eq!(node["properties"]["choice"], "standalone");

    let get_edge = run_cli(data_dir, &["edge", "get", "0"]);
    assert_success(&get_edge, "edge get");
    let edge_json: serde_json::Value =
        serde_json::from_slice(&get_edge.stdout).expect("edge get stdout should be JSON");
    assert_eq!(edge_json["label"], "ABOUT");
    assert_eq!(edge_json["source"], 0);
    assert_eq!(edge_json["target"], 1);
}
