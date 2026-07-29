//! `aletheia daemon` as the single local database owner — Issue #2905.
//!
//! Drives the **real** `aletheia-daemon` binary and proves the properties the
//! issue's acceptance criteria and success metric ask for:
//!
//! * the daemon serves MCP over HTTP (`/mcp`), so an MCP-capable client needs no
//!   per-session database at all;
//! * **multiple independent MCP client sessions read and write the same state**
//!   through that one daemon (the success metric);
//! * the daemon remains the RBAC decision point (uniform 401 without a
//!   credential, 403 for a role that may not write);
//! * a second daemon refuses to take over a data directory that a live daemon
//!   already owns (no accidental multi-writer);
//! * the in-process stdio↔HTTP relay used by `aletheia-mcp`'s daemon-client mode
//!   works against the real daemon, and two relay sessions share one database.
//!
//! HTTP is spoken by hand over `TcpStream` so the assertions are about bytes on
//! the wire, not a client library's interpretation of them.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const ADMIN_KEY: &str = "daemon-admin-key-abcdefghijklmnop";

// ───────────────────────────── tiny HTTP client ─────────────────────────────

/// An HTTP response: status code plus body text.
struct HttpResponse {
    status: u16,
    body: String,
}

impl HttpResponse {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// Issue one HTTP request against `127.0.0.1:port` and read the whole response.
fn http_request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> std::io::Result<HttpResponse> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;

    let mut request = format!("{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    match body {
        Some(payload) => {
            request.push_str("content-type: application/json\r\n");
            request.push_str(&format!("content-length: {}\r\n", payload.len()));
            request.push_str("connection: close\r\n\r\n");
            request.push_str(payload);
        }
        None => {
            request.push_str("content-length: 0\r\nconnection: close\r\n\r\n");
        }
    }
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .map_or((text.as_str(), ""), |(h, b)| (h, b));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);

    // Honor chunked transfer encoding (autumn streams some responses).
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_string()
    };

    Ok(HttpResponse { status, body })
}

/// Decode an HTTP/1.1 chunked body.
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, remainder)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 || remainder.len() < size {
            break;
        }
        out.push_str(&remainder[..size]);
        rest = remainder[size..].trim_start_matches("\r\n");
    }
    out
}

/// POST one JSON-RPC frame to the daemon's `/mcp` endpoint.
fn mcp_call(port: u16, key: Option<&str>, frame: &Value) -> HttpResponse {
    let payload = frame.to_string();
    let auth = key.map(|k| format!("Bearer {k}"));
    let mut headers: Vec<(&str, &str)> = vec![("accept", "application/json, text/event-stream")];
    if let Some(value) = auth.as_deref() {
        headers.push(("authorization", value));
    }
    http_request(port, "POST", "/mcp", &headers, Some(&payload)).expect("mcp request")
}

/// Call one MCP tool and return the parsed JSON-RPC response.
///
/// `arguments` follows autumn's projection of the handler signature: path/query
/// parameters are top-level keys, and a JSON request body is nested under
/// `body` (see the parity suite).
fn tools_call(port: u16, key: Option<&str>, id: u64, name: &str, arguments: Value) -> Value {
    let response = mcp_call(
        port,
        key,
        &json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }),
    );
    assert_eq!(
        response.status, 200,
        "tools/call {name} failed: {}",
        response.body
    );
    response.json()
}

/// Extract the text payload of a `tools/call` result and parse it as JSON.
fn tool_payload(response: &Value) -> Value {
    assert_eq!(
        response["result"]["isError"],
        json!(false),
        "tools/call reported an error: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no text content: {response}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("tool text is not JSON ({e}): {text}"))
}

// ─────────────────────────── daemon process harness ─────────────────────────

/// A running `aletheia-daemon` process.
#[derive(Debug)]
struct Daemon {
    child: Child,
    port: u16,
}

impl Daemon {
    fn binary() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_aletheia-daemon"))
    }

    /// Start the daemon on a free port against `data_dir`, waiting until it is
    /// serving. Returns `Err` with the exit status if it refuses to start.
    fn try_start(data_dir: &Path) -> Result<Self, String> {
        let port = free_port();
        let mut child = Command::new(Self::binary())
            .env_remove("ALETHEIADB_CONFIG")
            .env("ALETHEIADB_DATA_DIR", data_dir)
            .env("ALETHEIADB_HOST", "127.0.0.1")
            .env("ALETHEIADB_PORT", port.to_string())
            .env("ALETHEIADB_BOOTSTRAP_ADMIN_KEY", ADMIN_KEY)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn aletheia-daemon");

        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!("daemon exited during startup: {status}"));
            }
            if let Ok(response) =
                http_request(port, "GET", "/status", &[("x-api-key", ADMIN_KEY)], None)
                && response.status == 200
            {
                return Ok(Self { child, port });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("daemon did not become ready within 60s".to_string());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Start a daemon, retrying past an ephemeral-port collision.
    ///
    /// `free_port` reserves and releases, so another test (or anything else on
    /// the machine) can take the port in between. Retrying keeps that race from
    /// masquerading as a product failure.
    fn start(data_dir: &Path) -> Self {
        let mut last = String::new();
        for _ in 0..3 {
            match Self::try_start(data_dir) {
                Ok(daemon) => return daemon,
                Err(message) => last = message,
            }
        }
        panic!("daemon must start: {last}");
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reserve then release a port, returning its number.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind for free port");
    listener.local_addr().expect("local addr").port()
}

// ───────────────────────────────── tests ────────────────────────────────────

/// AC: the daemon is MCP-capable over HTTP — `initialize` + `tools/list`
/// succeed against `/mcp`, so an HTTP-transport MCP client needs no per-session
/// database.
#[test]
fn daemon_serves_the_mcp_endpoint_over_http() {
    let data_dir = TempDir::new().expect("tempdir");
    let daemon = Daemon::start(data_dir.path());

    let init = mcp_call(
        daemon.port(),
        Some(ADMIN_KEY),
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "t", "version": "0" }
            }
        }),
    );
    assert_eq!(init.status, 200, "initialize over /mcp: {}", init.body);
    assert!(
        init.json()["result"]["serverInfo"].is_object(),
        "initialize returns server info: {}",
        init.body
    );

    let list = mcp_call(
        daemon.port(),
        Some(ADMIN_KEY),
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    );
    assert_eq!(list.status, 200, "tools/list over /mcp: {}", list.body);
    let tools = list.json()["result"]["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        tools.len() > 20,
        "the daemon advertises the AletheiaDB tool inventory, got {}",
        tools.len()
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in ["create_node", "get_node", "traverse"] {
        assert!(
            names.contains(&expected),
            "tool `{expected}` is advertised; got {names:?}"
        );
    }
}

/// **The success metric.** Two independent MCP client sessions write and read
/// the *same* state through one daemon — no session owns a database.
#[test]
fn two_mcp_sessions_share_one_database_through_the_daemon() {
    let data_dir = TempDir::new().expect("tempdir");
    let daemon = Daemon::start(data_dir.path());

    // Session A: initialize, then create a node.
    let init_a = mcp_call(
        daemon.port(),
        Some(ADMIN_KEY),
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": "session-a", "version": "0" } }
        }),
    );
    assert_eq!(init_a.status, 200);
    let created = tools_call(
        daemon.port(),
        Some(ADMIN_KEY),
        2,
        "create_node",
        json!({ "body": { "label": "Person", "properties": { "name": "Alice" } } }),
    );
    let node_id = tool_payload(&created)["id"]
        .as_u64()
        .unwrap_or_else(|| panic!("create_node returned no node_id: {created}"));

    // Session B: a *separate* initialize handshake — a different client.
    let init_b = mcp_call(
        daemon.port(),
        Some(ADMIN_KEY),
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": "session-b", "version": "0" } }
        }),
    );
    assert_eq!(init_b.status, 200);
    let read = tools_call(
        daemon.port(),
        Some(ADMIN_KEY),
        2,
        "get_node",
        json!({ "id": node_id }),
    );
    let payload = tool_payload(&read);
    assert_eq!(
        payload["properties"]["name"], "Alice",
        "session B sees the node session A created — one database: {payload}"
    );

    // And the count is 1, not 2 (no per-session database was created).
    let counted = tools_call(daemon.port(), Some(ADMIN_KEY), 3, "count_nodes", json!({}));
    assert_eq!(
        tool_payload(&counted)["count"],
        json!(1),
        "exactly one node exists across both sessions"
    );
}

/// The daemon — not the client — is the authentication and RBAC decision point.
#[test]
fn daemon_mcp_endpoint_enforces_authentication_and_rbac() {
    let data_dir = TempDir::new().expect("tempdir");
    let daemon = Daemon::start(data_dir.path());

    // No credential → uniform 401 envelope.
    let anon = mcp_call(
        daemon.port(),
        None,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    );
    assert_eq!(anon.status, 401, "unauthenticated /mcp: {}", anon.body);
    assert_eq!(anon.json()["error"]["code"], "UNAUTHENTICATED");

    // Mint a reader key through the admin surface, then try to write with it.
    let minted = http_request(
        daemon.port(),
        "POST",
        "/admin/keys",
        &[("x-api-key", ADMIN_KEY)],
        Some(&json!({ "name": "reader", "role": "reader" }).to_string()),
    )
    .expect("mint key");
    assert_eq!(minted.status, 200, "mint reader key: {}", minted.body);
    let reader_key = minted.json()["data"]["key"]
        .as_str()
        .unwrap_or_else(|| panic!("no key in mint response: {}", minted.body))
        .to_string();

    let denied = mcp_call(
        daemon.port(),
        Some(&reader_key),
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "create_node",
                        "arguments": { "body": { "label": "Person", "properties": {} } } }
        }),
    );
    assert_eq!(
        denied.status, 403,
        "a reader may not call a write tool: {}",
        denied.body
    );
    assert_eq!(denied.json()["error"]["code"], "PERMISSION_DENIED");
    assert_eq!(
        denied.json()["error"]["details"]["principal_role"],
        "reader"
    );
}

/// No accidental multi-writer: a second daemon must refuse a data directory a
/// live daemon already owns — and say *why*, so the test cannot be satisfied by
/// an unrelated startup failure (a port collision, a panic, a timeout).
#[test]
fn second_daemon_refuses_a_data_dir_owned_by_a_live_daemon() {
    let data_dir = TempDir::new().expect("tempdir");
    let first = Daemon::start(data_dir.path());

    let output = Command::new(Daemon::binary())
        .env_remove("ALETHEIADB_CONFIG")
        .env("ALETHEIADB_DATA_DIR", data_dir.path())
        .env("ALETHEIADB_HOST", "127.0.0.1")
        .env("ALETHEIADB_PORT", free_port().to_string())
        .env("ALETHEIADB_BOOTSTRAP_ADMIN_KEY", ADMIN_KEY)
        .stdin(Stdio::null())
        .output()
        .expect("spawn second daemon");

    assert!(
        !output.status.success(),
        "a second daemon must refuse to co-own the data directory"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("owned by a running AletheiaDB daemon"),
        "it refuses for the RIGHT reason: {stderr}"
    );
    assert!(
        stderr.contains(&first.pid().to_string()),
        "the refusal names the owning pid {}: {stderr}",
        first.pid()
    );

    // And the converse: once the owner is gone, the directory is claimable
    // again — a crashed daemon must not brick its own data directory.
    drop(first);
    let successor = Daemon::try_start(data_dir.path());
    assert!(
        successor.is_ok(),
        "a released directory must be claimable: {successor:?}"
    );
}

/// The stdio↔HTTP relay that `aletheia-mcp`'s daemon-client mode runs, driven
/// **in-process** against the real daemon: two relay sessions that are live at
/// the same time interleave reads and writes against one database.
///
/// Interleaved rather than sequential on purpose — two sessions that never
/// overlap could be served by one database *or* by a database created and
/// destroyed per session. Here each session reads what the *other* wrote while
/// both are still connected, which only one shared database can satisfy.
#[tokio::test(flavor = "multi_thread")]
async fn two_relay_sessions_share_one_database() {
    use aletheiadb::mcp::proxy::{DaemonClientConfig, run_stdio_proxy};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};

    let data_dir = TempDir::new().expect("tempdir");
    let daemon = Daemon::start(data_dir.path());
    let config = DaemonClientConfig::new(daemon.base_url()).with_api_key(ADMIN_KEY);

    /// One live relay session: frames go in, replies come out, in order.
    struct Session {
        stdin: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        stdout: tokio::io::Lines<TokioBufReader<tokio::io::DuplexStream>>,
        relay: tokio::task::JoinHandle<Result<(), aletheiadb::mcp::proxy::ProxyError>>,
    }

    impl Session {
        fn open(config: DaemonClientConfig) -> Self {
            let (client_side, proxy_stdin) = tokio::io::duplex(64 * 1024);
            let (proxy_stdout, client_reader) = tokio::io::duplex(64 * 1024);
            let relay = tokio::spawn(async move {
                run_stdio_proxy(config, TokioBufReader::new(proxy_stdin), proxy_stdout).await
            });
            let (read_half, stdin) = tokio::io::split(client_side);
            drop(read_half);
            Self {
                stdin,
                stdout: TokioBufReader::new(client_reader).lines(),
                relay,
            }
        }

        async fn call(&mut self, frame: Value) -> Value {
            self.stdin
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .expect("write frame");
            self.stdin.flush().await.expect("flush");
            let line = tokio::time::timeout(Duration::from_secs(30), self.stdout.next_line())
                .await
                .expect("the relay must answer within 30s")
                .expect("read relay line")
                .expect("relay closed early");
            serde_json::from_str(&line).expect("relay line is JSON")
        }

        async fn close(self) {
            drop(self.stdin);
            let _ = self.relay.await;
        }
    }

    fn initialize(id: u64, name: &str) -> Value {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": name, "version": "0" } }
        })
    }

    fn create(id: u64, title: &str) -> Value {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": "create_node",
                        "arguments": { "body": { "label": "Doc",
                                                 "properties": { "title": title } } } }
        })
    }

    fn get(id: u64, node_id: u64) -> Value {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": "get_node", "arguments": { "id": node_id } }
        })
    }

    // Both sessions are open for the whole exchange.
    let mut a = Session::open(config.clone());
    let mut b = Session::open(config);
    a.call(initialize(1, "session-a")).await;
    b.call(initialize(1, "session-b")).await;

    // A writes, B reads it.
    let from_a = tool_payload(&a.call(create(2, "written-by-a")).await)["id"]
        .as_u64()
        .expect("node id from A");
    assert_eq!(
        tool_payload(&b.call(get(2, from_a)).await)["properties"]["title"],
        "written-by-a",
        "session B reads session A's write while both are connected"
    );

    // B writes, A reads it — the same database in both directions.
    let from_b = tool_payload(&b.call(create(3, "written-by-b")).await)["id"]
        .as_u64()
        .expect("node id from B");
    assert_eq!(
        tool_payload(&a.call(get(3, from_b)).await)["properties"]["title"],
        "written-by-b",
        "session A reads session B's write"
    );

    // And exactly two nodes exist — not two nodes per session.
    let counted = tools_call(daemon.port(), Some(ADMIN_KEY), 9, "count_nodes", json!({}));
    assert_eq!(
        tool_payload(&counted)["count"],
        json!(2),
        "both sessions wrote into ONE database"
    );

    a.close().await;
    b.close().await;
}

/// The CLI cannot depend on this crate (that would be a dependency cycle), so
/// it duplicates the lock file's name to decide whether a daemon owns a data
/// directory. If the two ever diverge, the CLI silently stops detecting the
/// daemon and becomes a second writer — so pin them together here.
#[test]
fn the_cli_and_the_daemon_agree_on_the_lock_file_name() {
    const CLI_SRC: &str = include_str!("../../../src/bin/aletheia.rs");
    assert!(
        CLI_SRC.contains(&format!(
            "const DAEMON_LOCK_FILE_NAME: &str = \"{}\"",
            aletheia_server::daemon::LOCK_FILE_NAME
        )),
        "the CLI's DAEMON_LOCK_FILE_NAME must equal aletheia_server::daemon::LOCK_FILE_NAME \
         ('{}')",
        aletheia_server::daemon::LOCK_FILE_NAME
    );
}

/// The daemon writes the lock file the CLI's guard reads: the name, location,
/// and content (a bare pid) must line up end to end.
#[test]
fn a_running_daemon_writes_the_lock_file_the_cli_guard_reads() {
    let data_dir = TempDir::new().expect("tempdir");
    let daemon = Daemon::start(data_dir.path());

    let lock = data_dir
        .path()
        .join(aletheia_server::daemon::LOCK_FILE_NAME);
    let recorded = std::fs::read_to_string(&lock)
        .unwrap_or_else(|e| panic!("daemon must write {}: {e}", lock.display()));
    let pid: u32 = recorded
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("lock must hold a bare pid, got {recorded:?}: {e}"));
    assert_eq!(
        pid,
        daemon.pid(),
        "the lock records the owning daemon's pid"
    );
}

/// An unrecognized tool name is refused fail-closed — even for an admin — and
/// the daemon keeps serving afterwards. This is what makes an unclassified
/// routable tool a startup/CI problem rather than a privilege-escalation hole.
#[test]
fn daemon_refuses_an_unknown_tool_fail_closed_and_keeps_serving() {
    let data_dir = TempDir::new().expect("tempdir");
    let daemon = Daemon::start(data_dir.path());

    let bad = mcp_call(
        daemon.port(),
        Some(ADMIN_KEY),
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                 "params": { "name": "no_such_tool", "arguments": {} } }),
    );
    assert_eq!(
        bad.status, 403,
        "an unclassified tool name is denied, never allowed through: {}",
        bad.body
    );
    assert_eq!(bad.json()["error"]["code"], "PERMISSION_DENIED");

    // The daemon still serves afterwards.
    let ok = mcp_call(
        daemon.port(),
        Some(ADMIN_KEY),
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    );
    assert_eq!(ok.status, 200, "daemon still healthy: {}", ok.body);
}

/// One process serves REST *and* MCP over the same database: a node created
/// through an MCP `tools/call` is readable through the plain HTTP route. This
/// is the "one local durable database owner for CLI, HTTP, MCP, and SDK-style
/// workflows" acceptance criterion.
#[test]
fn daemon_serves_plain_http_alongside_mcp() {
    let data_dir = TempDir::new().expect("tempdir");
    let daemon = Daemon::start(data_dir.path());

    let created = tools_call(
        daemon.port(),
        Some(ADMIN_KEY),
        1,
        "create_node",
        json!({ "body": { "label": "Person", "properties": { "name": "Bob" } } }),
    );
    let node_id = tool_payload(&created)["id"].as_u64().expect("node id");

    let http = http_request(
        daemon.port(),
        "GET",
        &format!("/nodes/{node_id}"),
        &[("x-api-key", ADMIN_KEY)],
        None,
    )
    .expect("http get node");
    assert_eq!(http.status, 200, "REST read of an MCP write: {}", http.body);
    assert_eq!(http.json()["properties"]["name"], "Bob");
}
