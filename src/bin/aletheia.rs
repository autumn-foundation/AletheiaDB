//! AletheiaDB command line interface.
//!
//! This binary provides:
//! - Direct local graph operations (inspired by MCP tool semantics)
//! - Daemon management for the HTTP server process

use std::env;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aletheiadb::{
    AletheiaDB, Edge, EdgeId, GLOBAL_INTERNER, Node, NodeId, PropertyMap, PropertyMapBuilder,
    PropertyValue,
};

const DEFAULT_PID_FILE: &str = ".aletheia/daemon.pid";
const DEFAULT_LOG_FILE: &str = ".aletheia/daemon.log";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 1963;
const SERVER_BIN_NAME: &str = "aletheia-server";

#[derive(Debug, Clone)]
struct DaemonMetadata {
    pid: u32,
    server_exe: PathBuf,
}

/// The main entry point for the AletheiaDB CLI.
///
/// This simply wraps `run()` and handles exiting with the correct status code
/// if an error occurs.
fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

/// Parses CLI arguments and routes to the appropriate subcommand handler.
///
/// # Returns
/// - `Ok(())` if the command succeeded.
/// - `Err(String)` with an error message if something failed or syntax was wrong.
fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("node") => handle_node(args.collect()),
        Some("edge") => handle_edge(args.collect()),
        Some("traverse") => handle_traverse(args.collect()),
        Some("daemon") => handle_daemon(args.collect()),
        Some("help") | Some("--help") | Some("-h") | None => {
            print_usage();
            Ok(())
        }
        Some(cmd) => Err(format!("unknown command '{cmd}'")),
    }
}

/// Prints the CLI usage instructions to standard output.
fn print_usage() {
    println!(
        "AletheiaDB CLI\n\n\
Usage:\n\
  aletheia node create <label> [--properties '{{\"k\":\"v\"}}']\n\
  aletheia node get <node_id>\n\
  aletheia edge create <source_id> <target_id> <label> [--properties '{{\"k\":\"v\"}}']\n\
  aletheia edge get <edge_id>\n\
  aletheia traverse <start_node_id> <edge_label> [--direction outgoing|incoming|both]\n\
  aletheia daemon start [--pid-file PATH] [--log-file PATH] [--host HOST] [--port PORT]\n\
  aletheia daemon stop [--pid-file PATH]\n\
  aletheia daemon status [--pid-file PATH]\n\
\nCommands map to core MCP-style graph operations while using local storage.\n"
    );
}

/// Opens the AletheiaDB database, honouring environment-driven config:
/// `ALETHEIADB_CONFIG` (TOML path) takes precedence over `ALETHEIADB_DATA_DIR`
/// (canonical durable layout). With neither set the database is ephemeral.
///
/// Converts underlying database errors into a clean string for CLI output.
fn open_db() -> Result<AletheiaDB, String> {
    AletheiaDB::open_from_env().map_err(|e| format!("failed to initialize database: {e}"))
}

/// Handles all subcommands under `aletheia node`.
///
/// Routes to either `create` or `get` based on the first argument in `args`.
fn handle_node(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("create") => {
            if args.len() < 2 {
                return Err("usage: aletheia node create <label> [--properties JSON]".to_string());
            }
            let label = &args[1];
            let properties = parse_optional_properties(&args[2..])?;
            let db = open_db()?;
            let node_id = db
                .create_node(label, properties)
                .map_err(|e| format!("create_node failed: {e}"))?;
            print_json_pretty(&serde_json::json!({ "node_id": node_id.as_u64() }))
        }
        Some("get") => {
            if args.len() != 2 {
                return Err("usage: aletheia node get <node_id>".to_string());
            }
            let node_id = parse_node_id(&args[1])?;
            let db = open_db()?;
            let node = db
                .get_node(node_id)
                .map_err(|e| format!("get_node failed: {e}"))?;
            print_json_pretty(&node_to_json(&node))
        }
        Some(sub) => Err(format!("unknown node subcommand '{sub}'")),
        None => Err("usage: aletheia node <create|get> ...".to_string()),
    }
}

/// Handles all subcommands under `aletheia edge`.
///
/// Routes to either `create` or `get` based on the first argument in `args`.
fn handle_edge(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("create") => {
            if args.len() < 4 {
                return Err(
                    "usage: aletheia edge create <source_id> <target_id> <label> [--properties JSON]"
                        .to_string(),
                );
            }
            let source = parse_node_id(&args[1])?;
            let target = parse_node_id(&args[2])?;
            let label = &args[3];
            let properties = parse_optional_properties(&args[4..])?;
            let db = open_db()?;
            let edge_id = db
                .create_edge(source, target, label, properties)
                .map_err(|e| format!("create_edge failed: {e}"))?;
            print_json_pretty(&serde_json::json!({ "edge_id": edge_id.as_u64() }))
        }
        Some("get") => {
            if args.len() != 2 {
                return Err("usage: aletheia edge get <edge_id>".to_string());
            }
            let edge_id = parse_edge_id(&args[1])?;
            let db = open_db()?;
            let edge = db
                .get_edge(edge_id)
                .map_err(|e| format!("get_edge failed: {e}"))?;
            print_json_pretty(&edge_to_json(&edge))
        }
        Some(sub) => Err(format!("unknown edge subcommand '{sub}'")),
        None => Err("usage: aletheia edge <create|get> ...".to_string()),
    }
}

/// Handles the `aletheia traverse` command.
///
/// Performs a single-hop graph traversal from a starting node, following edges
/// with a specific label in the specified direction. Outputs results as JSON.
fn handle_traverse(args: Vec<String>) -> Result<(), String> {
    if args.len() < 2 {
        return Err(
            "usage: aletheia traverse <start_node_id> <edge_label> [--direction outgoing|incoming|both]"
                .to_string(),
        );
    }

    let start = parse_node_id(&args[0])?;
    let label = &args[1];
    let direction = parse_direction(&args[2..])?;

    let db = open_db()?;
    let mut reached = Vec::new();

    if direction == "outgoing" || direction == "both" {
        for edge_id in db.get_outgoing_edges_with_label(start, label) {
            let target = db.get_edge_target(edge_id).map_err(|e| {
                format!(
                    "failed to resolve target for edge {}: {e}",
                    edge_id.as_u64()
                )
            })?;
            reached.push(serde_json::json!({
                "edge_id": edge_id.as_u64(),
                "direction": "outgoing",
                "node_id": target.as_u64(),
            }));
        }
    }

    if direction == "incoming" || direction == "both" {
        for edge_id in db.get_incoming_edges_with_label(start, label) {
            let source = db.get_edge_source(edge_id).map_err(|e| {
                format!(
                    "failed to resolve source for edge {}: {e}",
                    edge_id.as_u64()
                )
            })?;
            reached.push(serde_json::json!({
                "edge_id": edge_id.as_u64(),
                "direction": "incoming",
                "node_id": source.as_u64(),
            }));
        }
    }

    print_json_pretty(&serde_json::json!({
        "start_node_id": start.as_u64(),
        "edge_label": label,
        "direction": direction,
        "results": reached,
    }))
}

/// Handles all subcommands under `aletheia daemon`.
///
/// Routes to `start`, `stop`, or `status` to manage the background server process.
fn handle_daemon(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("start") => daemon_start(&args[1..]),
        Some("stop") => daemon_stop(&args[1..]),
        Some("status") => daemon_status(&args[1..]),
        Some(sub) => Err(format!("unknown daemon subcommand '{sub}'")),
        None => Err("usage: aletheia daemon <start|stop|status> ...".to_string()),
    }
}

/// Starts the AletheiaDB background server daemon.
///
/// 1. Checks if it's already running.
/// 2. Spawns the `aletheia-server` process in the background.
/// 3. Writes the PID and executable path to `.aletheia/daemon.pid`.
fn daemon_start(args: &[String]) -> Result<(), String> {
    let pid_file = PathBuf::from(
        arg_value(args, "--pid-file").unwrap_or_else(|| DEFAULT_PID_FILE.to_string()),
    );
    let log_file = PathBuf::from(
        arg_value(args, "--log-file").unwrap_or_else(|| DEFAULT_LOG_FILE.to_string()),
    );
    let host = arg_value(args, "--host").unwrap_or_else(|| DEFAULT_HOST.to_string());
    let port = arg_value(args, "--port")
        .map(|s| {
            s.parse::<u16>()
                .map_err(|e| format!("invalid port '{s}': {e}"))
        })
        .transpose()?
        .unwrap_or(DEFAULT_PORT);

    if let Some(meta) = read_daemon_metadata(&pid_file)?
        && is_expected_daemon_running(&meta)
    {
        return Err(format!(
            "daemon already running with pid {} ({})",
            meta.pid,
            meta.server_exe.display()
        ));
    }

    ensure_parent_dir(&pid_file)?;
    ensure_parent_dir(&log_file)?;

    let server_exe = resolve_server_executable()?;

    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .map_err(|e| format!("failed to open log file '{}': {e}", log_file.display()))?;

    let mut cmd = Command::new(&server_exe);
    cmd.env("ALETHEIADB_HOST", &host)
        .env("ALETHEIADB_PORT", port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log));

    let child = cmd.spawn().map_err(|e| {
        format!(
            "failed to launch daemon process '{}': {e}",
            server_exe.display()
        )
    })?;

    let metadata = DaemonMetadata {
        pid: child.id(),
        server_exe,
    };

    write_daemon_metadata(&pid_file, &metadata)?;

    println!(
        "daemon started (pid={}, host={}, port={}, exe={}, log={})",
        metadata.pid,
        host,
        port,
        metadata.server_exe.display(),
        log_file.display()
    );
    Ok(())
}

/// Stops the running AletheiaDB background server daemon.
///
/// Reads the PID from the pid-file, verifies the process is still the expected
/// server executable, and sends a kill signal. Cleans up the pid-file afterwards.
fn daemon_stop(args: &[String]) -> Result<(), String> {
    let pid_file = PathBuf::from(
        arg_value(args, "--pid-file").unwrap_or_else(|| DEFAULT_PID_FILE.to_string()),
    );
    let meta = read_daemon_metadata(&pid_file)?.ok_or_else(|| {
        format!(
            "no pid file found at '{}' (daemon not running?)",
            pid_file.display()
        )
    })?;

    if !is_expected_daemon_running(&meta) {
        return Err(format!(
            "refusing to stop pid {}: process does not match expected daemon binary '{}'",
            meta.pid,
            meta.server_exe.display()
        ));
    }

    let status = Command::new("kill")
        .arg(meta.pid.to_string())
        .status()
        .map_err(|e| format!("failed to invoke kill: {e}"))?;

    if !status.success() {
        return Err(format!("failed to stop daemon process {}", meta.pid));
    }

    fs::remove_file(&pid_file)
        .map_err(|e| format!("failed to remove pid file '{}': {e}", pid_file.display()))?;

    println!("daemon stopped (pid={})", meta.pid);
    Ok(())
}

/// Checks and prints the status of the background server daemon.
///
/// Verifies whether the process ID in the pid-file is actively running and
/// matches the expected server executable.
fn daemon_status(args: &[String]) -> Result<(), String> {
    let pid_file = PathBuf::from(
        arg_value(args, "--pid-file").unwrap_or_else(|| DEFAULT_PID_FILE.to_string()),
    );
    match read_daemon_metadata(&pid_file)? {
        Some(meta) if is_expected_daemon_running(&meta) => {
            println!(
                "daemon is running (pid={}, exe={})",
                meta.pid,
                meta.server_exe.display()
            );
            Ok(())
        }
        Some(meta) => {
            println!(
                "daemon is not running or pid was reused (pid={}, expected_exe={})",
                meta.pid,
                meta.server_exe.display()
            );
            Ok(())
        }
        None => {
            println!("daemon is not running (no pid file)");
            Ok(())
        }
    }
}

/// Resolves the absolute path to the `aletheia-server` executable.
///
/// Assumes the server binary is located in the same directory as this CLI binary.
fn resolve_server_executable() -> Result<PathBuf, String> {
    let mut exe_path =
        env::current_exe().map_err(|e| format!("failed to get current executable path: {e}"))?;

    exe_path.pop();
    let server_exe = exe_path.join(SERVER_BIN_NAME);

    if server_exe.is_file() {
        return Ok(server_exe);
    }

    #[cfg(windows)]
    {
        let server_exe_win = exe_path.join(format!("{}.exe", SERVER_BIN_NAME));
        if server_exe_win.is_file() {
            return Ok(server_exe_win);
        }
    }

    Err(format!(
        "could not find '{}' next to CLI binary at '{}'",
        SERVER_BIN_NAME,
        exe_path.display()
    ))
}

/// Checks if the process defined in `meta` is currently running and is the correct binary.
///
/// Uses `/proc/<pid>` on Unix systems to verify the process executable. This prevents
/// accidentally killing unrelated processes if a PID is reused by the OS after a crash.
fn is_expected_daemon_running(meta: &DaemonMetadata) -> bool {
    let proc_dir = PathBuf::from(format!("/proc/{}", meta.pid));
    if !proc_dir.exists() {
        return false;
    }

    let exe_path = proc_dir.join("exe");
    if let Ok(current_exe) = fs::read_link(&exe_path) {
        return current_exe == meta.server_exe;
    }

    let cmdline_path = proc_dir.join("cmdline");
    let cmdline = fs::read(cmdline_path).unwrap_or_default();
    let joined = String::from_utf8_lossy(&cmdline).replace('\0', " ");
    joined.contains(SERVER_BIN_NAME)
}

/// Writes the daemon's PID and executable path to the specified pid-file.
fn write_daemon_metadata(path: &Path, meta: &DaemonMetadata) -> Result<(), String> {
    let content = format!("{}\n{}\n", meta.pid, meta.server_exe.display());
    fs::write(path, content)
        .map_err(|e| format!("failed to write pid file '{}': {e}", path.display()))
}

/// Reads the daemon's PID and executable path from the specified pid-file.
fn read_daemon_metadata(path: &Path) -> Result<Option<DaemonMetadata>, String> {
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed reading pid file '{}': {e}", path.display()))?;

    let mut lines = content.lines();
    let pid_line = lines
        .next()
        .ok_or_else(|| format!("pid file '{}' missing pid line", path.display()))?;
    let exe_line = lines
        .next()
        .ok_or_else(|| format!("pid file '{}' missing executable line", path.display()))?;

    let pid = pid_line
        .parse::<u32>()
        .map_err(|e| format!("invalid pid in '{}': {e}", path.display()))?;

    Ok(Some(DaemonMetadata {
        pid,
        server_exe: PathBuf::from(exe_line),
    }))
}

/// Ensures the parent directory for a given file path exists, creating it if necessary.
fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory '{}': {e}", parent.display()))?;
    }
    Ok(())
}

/// Extracts the value of a specific command-line flag (e.g., `--port 1963`).
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(token) = iter.next() {
        if token == flag {
            return iter.next().cloned();
        }
    }
    None
}

/// Parses the `--properties` JSON argument if present, converting it to a `PropertyMap`.
fn parse_optional_properties(args: &[String]) -> Result<PropertyMap, String> {
    match arg_value(args, "--properties") {
        Some(json) => json_to_property_map(&json),
        None => Ok(PropertyMap::new()),
    }
}

/// Parses the `--direction` argument for traversals (defaults to "outgoing").
fn parse_direction(args: &[String]) -> Result<String, String> {
    let direction = arg_value(args, "--direction").unwrap_or_else(|| "outgoing".to_string());
    if matches!(direction.as_str(), "outgoing" | "incoming" | "both") {
        Ok(direction)
    } else {
        Err(format!(
            "invalid direction '{direction}', expected outgoing|incoming|both"
        ))
    }
}

/// Parses a string into a `NodeId`.
fn parse_node_id(raw: &str) -> Result<NodeId, String> {
    let id = raw
        .parse::<u64>()
        .map_err(|e| format!("invalid node id '{raw}': {e}"))?;
    NodeId::new(id).map_err(|e| format!("invalid node id: {e}"))
}

/// Parses a string into an `EdgeId`.
fn parse_edge_id(raw: &str) -> Result<EdgeId, String> {
    let id = raw
        .parse::<u64>()
        .map_err(|e| format!("invalid edge id '{raw}': {e}"))?;
    EdgeId::new(id).map_err(|e| format!("invalid edge id: {e}"))
}

/// Converts a raw JSON string into a structured `PropertyMap`.
fn json_to_property_map(raw: &str) -> Result<PropertyMap, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON properties payload: {e}"))?;

    let object = parsed
        .as_object()
        .ok_or_else(|| "properties JSON must be an object".to_string())?;

    let mut map = PropertyMapBuilder::new();
    for (key, value) in object {
        let converted = json_to_property_value(value)?;
        map = map.insert(key, converted);
    }
    Ok(map.build())
}

/// Converts a single JSON value into an internal `PropertyValue`.
fn json_to_property_value(value: &serde_json::Value) -> Result<PropertyValue, String> {
    match value {
        serde_json::Value::Null => Ok(PropertyValue::Null),
        serde_json::Value::Bool(v) => Ok(PropertyValue::Bool(*v)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(PropertyValue::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(PropertyValue::Float(f))
            } else {
                Err("unsupported numeric value".to_string())
            }
        }
        serde_json::Value::String(s) => Ok(PropertyValue::string(s)),
        serde_json::Value::Array(arr) => {
            let values = arr
                .iter()
                .map(json_to_property_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PropertyValue::array(values))
        }
        serde_json::Value::Object(_) => {
            Err("nested objects are not supported in properties".to_string())
        }
    }
}

/// Converts an internal `Node` object into a JSON representation for CLI output.
fn node_to_json(node: &Node) -> serde_json::Value {
    serde_json::json!({
        "id": node.id.as_u64(),
        "label": resolve_label(node.label),
        "properties": property_map_to_json(&node.properties),
    })
}

/// Converts an internal `Edge` object into a JSON representation for CLI output.
fn edge_to_json(edge: &Edge) -> serde_json::Value {
    serde_json::json!({
        "id": edge.id.as_u64(),
        "label": resolve_label(edge.label),
        "source": edge.source.as_u64(),
        "target": edge.target.as_u64(),
        "properties": property_map_to_json(&edge.properties),
    })
}

/// Resolves an `InternedString` back into its raw string representation.
fn resolve_label(label: aletheiadb::InternedString) -> String {
    GLOBAL_INTERNER
        .resolve_with(label, |s| s.to_string())
        .unwrap_or_else(|| "<unknown-label>".to_string())
}

/// Converts an internal `PropertyMap` into a JSON Object.
fn property_map_to_json(props: &PropertyMap) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in props.iter() {
        let key_string = GLOBAL_INTERNER
            .resolve_with(*key, |s| s.to_string())
            .unwrap_or_else(|| "<unknown-key>".to_string());
        map.insert(key_string, property_value_to_json(value));
    }
    serde_json::Value::Object(map)
}

/// Converts an internal `PropertyValue` into a standard JSON Value.
fn property_value_to_json(value: &PropertyValue) -> serde_json::Value {
    match value {
        PropertyValue::Null => serde_json::Value::Null,
        PropertyValue::Bool(v) => serde_json::Value::Bool(*v),
        PropertyValue::Int(v) => serde_json::json!(*v),
        PropertyValue::Float(v) => serde_json::json!(*v),
        PropertyValue::String(v) => serde_json::Value::String(v.to_string()),
        PropertyValue::Bytes(v) => serde_json::json!(v.to_vec()),
        PropertyValue::Array(values) => {
            serde_json::Value::Array(values.iter().map(property_value_to_json).collect())
        }
        PropertyValue::Vector(values) => {
            serde_json::Value::Array(values.iter().map(|f| serde_json::json!(*f)).collect())
        }
        PropertyValue::SparseVector(values) => serde_json::json!({
            "indices": values.indices(),
            "values": values.values(),
            "dimensions": values.dimension(),
        }),
    }
}

/// Pretty-prints a JSON value to standard output.
fn print_json_pretty(value: &serde_json::Value) -> Result<(), String> {
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|e| format!("failed to render JSON output: {e}"))?;

    let mut stdout = io::stdout().lock();
    match writeln!(stdout, "{rendered}") {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(format!("error writing JSON output: {e}")),
    }
}
