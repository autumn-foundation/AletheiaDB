//! AletheiaDB command line interface.
//!
//! This binary provides:
//! - Direct local graph operations (inspired by MCP tool semantics)
//! - Daemon management for the HTTP server process

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aletheiadb::{
    AletheiaDB, Edge, EdgeId, GLOBAL_INTERNER, Node, NodeId, PropertyMap, PropertyMapBuilder,
    PropertyValue,
};

const DEFAULT_PID_FILE: &str = ".aletheia/daemon.pid";
const DEFAULT_LOG_FILE: &str = ".aletheia/daemon.log";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

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

fn open_db() -> Result<AletheiaDB, String> {
    AletheiaDB::new().map_err(|e| format!("failed to initialize database: {e}"))
}

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
            println!("{{\"node_id\":{}}}", node_id.as_u64());
            Ok(())
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
            print_json_pretty(&node_to_json(&node));
            Ok(())
        }
        Some(sub) => Err(format!("unknown node subcommand '{sub}'")),
        None => Err("usage: aletheia node <create|get> ...".to_string()),
    }
}

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
            println!("{{\"edge_id\":{}}}", edge_id.as_u64());
            Ok(())
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
            print_json_pretty(&edge_to_json(&edge));
            Ok(())
        }
        Some(sub) => Err(format!("unknown edge subcommand '{sub}'")),
        None => Err("usage: aletheia edge <create|get> ...".to_string()),
    }
}

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
        for edge_id in db.get_incoming_edges(start) {
            let edge = db
                .get_edge(edge_id)
                .map_err(|e| format!("failed to load edge {}: {e}", edge_id.as_u64()))?;
            let edge_label = resolve_label(edge.label);
            if edge_label == label.as_str() {
                reached.push(serde_json::json!({
                    "edge_id": edge_id.as_u64(),
                    "direction": "incoming",
                    "node_id": edge.source.as_u64(),
                }));
            }
        }
    }

    print_json_pretty(&serde_json::json!({
        "start_node_id": start.as_u64(),
        "edge_label": label,
        "direction": direction,
        "results": reached,
    }));
    Ok(())
}

fn handle_daemon(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("start") => daemon_start(&args[1..]),
        Some("stop") => daemon_stop(&args[1..]),
        Some("status") => daemon_status(&args[1..]),
        Some(sub) => Err(format!("unknown daemon subcommand '{sub}'")),
        None => Err("usage: aletheia daemon <start|stop|status> ...".to_string()),
    }
}

fn daemon_start(args: &[String]) -> Result<(), String> {
    let pid_file = arg_value(args, "--pid-file").unwrap_or_else(|| DEFAULT_PID_FILE.to_string());
    let log_file = arg_value(args, "--log-file").unwrap_or_else(|| DEFAULT_LOG_FILE.to_string());
    let host = arg_value(args, "--host").unwrap_or_else(|| DEFAULT_HOST.to_string());
    let port = arg_value(args, "--port")
        .map(|s| {
            s.parse::<u16>()
                .map_err(|e| format!("invalid port '{s}': {e}"))
        })
        .transpose()?
        .unwrap_or(DEFAULT_PORT);

    if let Some(pid) = read_pid(Path::new(&pid_file))?
        && is_process_running(pid)
    {
        return Err(format!("daemon already running with pid {pid}"));
    }

    ensure_parent_dir(&pid_file)?;
    ensure_parent_dir(&log_file)?;

    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .map_err(|e| format!("failed to open log file '{log_file}': {e}"))?;

    let mut cmd = Command::new("cargo");
    cmd.args([
        "run",
        "--bin",
        "aletheia-server",
        "--features",
        "http-server",
    ])
    .env("ALETHEIADB_HOST", &host)
    .env("ALETHEIADB_PORT", port.to_string())
    .stdin(Stdio::null())
    .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
    .stderr(Stdio::from(log));

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch daemon process: {e}"))?;

    fs::write(&pid_file, format!("{}\n", child.id()))
        .map_err(|e| format!("failed to write pid file '{pid_file}': {e}"))?;

    println!(
        "daemon started (pid={}, host={}, port={}, log={})",
        child.id(),
        host,
        port,
        log_file
    );
    Ok(())
}

fn daemon_stop(args: &[String]) -> Result<(), String> {
    let pid_file = arg_value(args, "--pid-file").unwrap_or_else(|| DEFAULT_PID_FILE.to_string());
    let pid = read_pid(Path::new(&pid_file))?
        .ok_or_else(|| format!("no pid file found at '{pid_file}' (daemon not running?)"))?;

    let status = Command::new("kill")
        .arg(pid.to_string())
        .status()
        .map_err(|e| format!("failed to invoke kill: {e}"))?;

    if !status.success() {
        return Err(format!("failed to stop daemon process {pid}"));
    }

    fs::remove_file(&pid_file)
        .map_err(|e| format!("failed to remove pid file '{pid_file}': {e}"))?;

    println!("daemon stopped (pid={pid})");
    Ok(())
}

fn daemon_status(args: &[String]) -> Result<(), String> {
    let pid_file = arg_value(args, "--pid-file").unwrap_or_else(|| DEFAULT_PID_FILE.to_string());
    match read_pid(Path::new(&pid_file))? {
        Some(pid) if is_process_running(pid) => {
            println!("daemon is running (pid={pid})");
            Ok(())
        }
        Some(pid) => {
            println!("daemon is not running (stale pid file with pid={pid})");
            Ok(())
        }
        None => {
            println!("daemon is not running (no pid file)");
            Ok(())
        }
    }
}

fn read_pid(path: &Path) -> Result<Option<u32>, String> {
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed reading pid file '{}': {e}", path.display()))?;

    let pid = content
        .trim()
        .parse::<u32>()
        .map_err(|e| format!("invalid pid in '{}': {e}", path.display()))?;
    Ok(Some(pid))
}

fn is_process_running(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ensure_parent_dir(path: &str) -> Result<(), String> {
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory '{}': {e}", parent.display()))?;
    }
    Ok(())
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(token) = iter.next() {
        if token == flag {
            return iter.next().cloned();
        }
    }
    None
}

fn parse_optional_properties(args: &[String]) -> Result<PropertyMap, String> {
    match arg_value(args, "--properties") {
        Some(json) => json_to_property_map(&json),
        None => Ok(PropertyMap::new()),
    }
}

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

fn parse_node_id(raw: &str) -> Result<NodeId, String> {
    let id = raw
        .parse::<u64>()
        .map_err(|e| format!("invalid node id '{raw}': {e}"))?;
    NodeId::new(id).map_err(|e| format!("invalid node id: {e}"))
}

fn parse_edge_id(raw: &str) -> Result<EdgeId, String> {
    let id = raw
        .parse::<u64>()
        .map_err(|e| format!("invalid edge id '{raw}': {e}"))?;
    EdgeId::new(id).map_err(|e| format!("invalid edge id: {e}"))
}

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

fn node_to_json(node: &Node) -> serde_json::Value {
    serde_json::json!({
        "id": node.id.as_u64(),
        "label": resolve_label(node.label),
        "properties": property_map_to_json(&node.properties),
    })
}

fn edge_to_json(edge: &Edge) -> serde_json::Value {
    serde_json::json!({
        "id": edge.id.as_u64(),
        "label": resolve_label(edge.label),
        "source": edge.source.as_u64(),
        "target": edge.target.as_u64(),
        "properties": property_map_to_json(&edge.properties),
    })
}

fn resolve_label(label: aletheiadb::InternedString) -> String {
    GLOBAL_INTERNER
        .resolve_with(label, |s| s.to_string())
        .unwrap_or_else(|| "<unknown-label>".to_string())
}

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

fn print_json_pretty(value: &serde_json::Value) {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => {
            let _ = std::io::stdout().write_all(rendered.as_bytes());
            let _ = std::io::stdout().write_all(b"\n");
        }
        Err(_) => println!("{value}"),
    }
}
