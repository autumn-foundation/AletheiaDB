//! Daemon-client (proxy) mode for the `aletheia-mcp` binary — Issue #2905.
//!
//! These tests drive the **real** `aletheia-mcp` process against a **stub
//! daemon** (a tiny in-test HTTP server), so they pin the contract that matters
//! operationally:
//!
//! * with `ALETHEIADB_DAEMON_URL` set, the process is a *relay*: it never opens
//!   local storage, and every JSON-RPC frame it reads on stdin is forwarded
//!   verbatim to the daemon's `/mcp` endpoint;
//! * the session credential travels as `Authorization: Bearer`, so the daemon
//!   stays the single RBAC decision point;
//! * a JSON-RPC *notification* (no `id`) produces no stdout line;
//! * the proxy exits when its client disconnects (stdin EOF) — the daemon is
//!   the long-lived process, the proxy is not;
//! * with no daemon URL configured, the embedded mode still works (fallback).
//!
//! The stub daemon is deliberately hand-rolled over `TcpListener` so the test
//! asserts on the *bytes on the wire* (headers included) rather than on a
//! client library's view of them.

#![cfg(feature = "mcp-server")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

// ───────────────────────────── stub daemon ──────────────────────────────────

/// One request the stub daemon received, captured verbatim.
#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// A minimal HTTP/1.1 server that answers every `POST` with a canned JSON-RPC
/// response derived from the request's `id`, recording what it saw.
struct StubDaemon {
    addr: SocketAddr,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
    shutdown: Arc<AtomicBool>,
}

impl StubDaemon {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
        let addr = listener.local_addr().expect("stub addr");
        listener
            .set_nonblocking(true)
            .expect("stub listener nonblocking");
        let captured = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread_captured = Arc::clone(&captured);
        let thread_shutdown = Arc::clone(&shutdown);
        std::thread::spawn(move || {
            while !thread_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let captured = Arc::clone(&thread_captured);
                        let shutdown = Arc::clone(&thread_shutdown);
                        std::thread::spawn(move || {
                            serve_connection(stream, &captured, &shutdown);
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            captured,
            shutdown,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn captured(&self) -> Vec<CapturedRequest> {
        self.captured.lock().expect("captured lock").clone()
    }
}

impl Drop for StubDaemon {
    /// Stop accepting **and** stop serving: a connection-pooling client would
    /// otherwise keep using an already-established connection long after the
    /// "daemon" was supposed to be gone, so a test simulating a daemon crash
    /// would silently keep succeeding.
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Serve one connection: read request(s), record, reply. Keeps the connection
/// alive so a pooled HTTP client can reuse it across JSON-RPC frames.
fn serve_connection(
    stream: TcpStream,
    captured: &Arc<Mutex<Vec<CapturedRequest>>>,
    shutdown: &Arc<AtomicBool>,
) {
    // Poll for shutdown between requests rather than blocking forever on a
    // keep-alive connection.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut reader = BufReader::new(stream.try_clone().expect("clone stub stream"));
    let mut writer = stream;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            // Closing the socket is what makes a pooled client observe the
            // daemon's disappearance.
            let _ = writer.shutdown(std::net::Shutdown::Both);
            return;
        }
        let mut request_line = String::new();
        match reader.read_line(&mut request_line) {
            Ok(0) => return,
            Ok(_) => {}
            // A read timeout just means "nothing yet"; loop back and re-check
            // the shutdown flag.
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return,
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();

        let mut headers = Vec::new();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                let name = name.trim().to_string();
                let value = value.trim().to_string();
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.parse().unwrap_or(0);
                }
                headers.push((name, value));
            }
        }

        let mut body = vec![0u8; content_length];
        if content_length > 0 && reader.read_exact(&mut body).is_err() {
            return;
        }
        let body = String::from_utf8_lossy(&body).into_owned();

        let request = CapturedRequest {
            method,
            path,
            headers,
            body,
        };
        let parsed = request.json();
        captured.lock().expect("captured lock").push(request);

        // A frame naming the `slow` method stalls, so a test can prove that one
        // long call does not block the rest of the session.
        if parsed.get("method").and_then(Value::as_str) == Some("slow") {
            std::thread::sleep(Duration::from_millis(750));
        }

        // A notification (no `id`) gets a bodiless 202, exactly like autumn's
        // MCP endpoint; anything else gets a JSON-RPC result echoing the id.
        let response = if parsed.get("id").is_none() {
            "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n".to_string()
        } else {
            let payload = json!({
                "jsonrpc": "2.0",
                "id": parsed.get("id").cloned().unwrap_or(Value::Null),
                "result": {
                    "served_by": "stub-daemon",
                    "method": parsed.get("method").cloned().unwrap_or(Value::Null),
                },
            })
            .to_string();
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nmcp-session-id: stub-session-1\r\ncontent-length: {}\r\n\r\n{}",
                payload.len(),
                payload
            )
        };
        if writer.write_all(response.as_bytes()).is_err() || writer.flush().is_err() {
            return;
        }
    }
}

// ─────────────────────────── proxy process harness ──────────────────────────

/// A running `aletheia-mcp` process plus its piped stdio.
struct ProxyProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl ProxyProcess {
    /// Spawn `aletheia-mcp`. `daemon_url` selects daemon-client mode; `data_dir`
    /// is exported as `ALETHEIADB_DATA_DIR` so the "no local storage" assertion
    /// is meaningful (a storage-opening process would populate it).
    fn spawn(daemon_url: Option<&str>, data_dir: &Path, api_key: Option<&str>) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aletheia-mcp"));
        cmd.env_remove("ALETHEIADB_CONFIG");
        cmd.env("ALETHEIADB_DATA_DIR", data_dir);
        cmd.env("ALETHEIADB_AUTH_MODE", "anonymous");
        match daemon_url {
            Some(url) => {
                cmd.env("ALETHEIADB_DAEMON_URL", url);
            }
            None => {
                cmd.env_remove("ALETHEIADB_DAEMON_URL");
            }
        }
        match api_key {
            Some(key) => {
                cmd.env("ALETHEIADB_MCP_API_KEY", key);
            }
            None => {
                cmd.env_remove("ALETHEIADB_MCP_API_KEY");
            }
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn aletheia-mcp");
        let stdin = child.stdin.take().expect("proxy stdin");
        let stdout = BufReader::new(child.stdout.take().expect("proxy stdout"));
        Self {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin still open");
        writeln!(stdin, "{message}").expect("write to proxy stdin");
        stdin.flush().expect("flush proxy stdin");
    }

    /// Read one newline-delimited JSON-RPC message from stdout.
    ///
    /// The read runs on a helper thread with a deadline: a regression that drops
    /// a reply must fail this test with a clear message, not hang until the
    /// harness kills the whole run.
    fn recv(&mut self) -> Value {
        self.recv_within(Duration::from_secs(30))
    }

    fn recv_within(&mut self, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        let mut line = String::new();
        loop {
            line.clear();
            // `read_line` on a pipe blocks; bound it by polling the child and
            // the clock between reads on a helper thread.
            let (tx, rx) = std::sync::mpsc::channel();
            let stdout = &mut self.stdout;
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let result = stdout.read_line(&mut line);
                    let _ = tx.send(result.map(|n| n > 0));
                });
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(Ok(true)) => {}
                    Ok(Ok(false)) => panic!("proxy closed stdout before answering"),
                    Ok(Err(e)) => panic!("failed to read proxy stdout: {e}"),
                    Err(_) => {
                        panic!("proxy did not answer within {timeout:?} — a reply was dropped")
                    }
                }
            });
            if line.trim().is_empty() {
                continue;
            }
            return serde_json::from_str(line.trim())
                .unwrap_or_else(|e| panic!("proxy stdout line is not JSON ({e}): {line:?}"));
        }
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Wait up to `timeout` for the process to exit; `None` on timeout.
    fn wait_for_exit(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => return Some(status),
                None if Instant::now() >= deadline => return None,
                None => std::thread::sleep(Duration::from_millis(25)),
            }
        }
    }
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialize_frame(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "0.0.0" }
        }
    })
}

/// Recursively collect every entry under `dir` — files **and** directories, as
/// paths relative to `dir`.
///
/// Directories count: opening a durable database creates `wal/` and `indexes/`
/// before anything is written to them, so a files-only walk would report an
/// untouched directory for a process that had in fact taken ownership of it.
fn entries_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(rel) = path.strip_prefix(base) {
                out.push(rel.display().to_string());
            }
            if path.is_dir() {
                walk(&path, base, out);
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

// ───────────────────────────────── tests ────────────────────────────────────

/// AC: `aletheia-mcp` supports a daemon-client mode configured by
/// `ALETHEIADB_DAEMON_URL`, relaying JSON-RPC to the daemon's `/mcp` endpoint.
#[test]
fn daemon_mode_relays_jsonrpc_to_the_daemon_mcp_endpoint() {
    let stub = StubDaemon::start();
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(
        Some(&stub.base_url()),
        data_dir.path(),
        Some("session-key-abcdefghijklmnop"),
    );

    proxy.send(&initialize_frame(1));
    let response = proxy.recv();

    assert_eq!(response["id"], json!(1), "the id is preserved: {response}");
    assert_eq!(
        response["result"]["served_by"], "stub-daemon",
        "the response came from the DAEMON, not an embedded database: {response}"
    );

    let captured = stub.captured();
    assert_eq!(captured.len(), 1, "exactly one frame was forwarded");
    let first = &captured[0];
    assert_eq!(first.method, "POST", "MCP over HTTP uses POST");
    assert_eq!(
        first.path, "/mcp",
        "the proxy targets the daemon's /mcp mount"
    );
    assert_eq!(
        first.json()["method"],
        "initialize",
        "the frame is relayed verbatim: {}",
        first.body
    );
}

/// AC: the session credential is forwarded so the DAEMON stays the single
/// authentication/RBAC decision point.
#[test]
fn daemon_mode_forwards_the_session_credential_as_bearer() {
    let stub = StubDaemon::start();
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(
        Some(&stub.base_url()),
        data_dir.path(),
        Some("session-key-abcdefghijklmnop"),
    );

    proxy.send(&initialize_frame(7));
    let _ = proxy.recv();

    let captured = stub.captured();
    let first = captured.first().expect("one captured request");
    assert_eq!(
        first.header("authorization"),
        Some("Bearer session-key-abcdefghijklmnop"),
        "credential is forwarded as a bearer token: {:?}",
        first.headers
    );
    assert_eq!(
        first.header("content-type"),
        Some("application/json"),
        "JSON-RPC frames are posted as JSON"
    );
}

/// AC: in daemon mode the proxy "does not call `AletheiaDB::open_from_env()` or
/// open local storage" — with a data dir configured, it must stay untouched.
#[test]
fn daemon_mode_never_opens_local_storage() {
    let stub = StubDaemon::start();
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(Some(&stub.base_url()), data_dir.path(), None);

    proxy.send(&initialize_frame(1));
    let _ = proxy.recv();
    // A tool call too: no path through the proxy may touch storage.
    proxy.send(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "count_nodes", "arguments": {} }
    }));
    let _ = proxy.recv();

    let entries = entries_under(data_dir.path());
    assert!(
        entries.is_empty(),
        "daemon-mode proxy must not create ANY local storage, found: {entries:?}"
    );
}

/// A JSON-RPC notification (no `id`) expects no reply; the proxy must not
/// invent one, and must keep serving afterwards.
#[test]
fn daemon_mode_notification_produces_no_response_line() {
    let stub = StubDaemon::start();
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(Some(&stub.base_url()), data_dir.path(), None);

    proxy.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    proxy.send(&initialize_frame(42));

    let response = proxy.recv();
    assert_eq!(
        response["id"],
        json!(42),
        "the first stdout line answers the REQUEST, not the notification: {response}"
    );

    let captured = stub.captured();
    assert_eq!(captured.len(), 2, "both frames reached the daemon");
    // Frames are relayed concurrently, so arrival ORDER is not guaranteed —
    // assert the set. What matters is that the notification was forwarded and
    // produced no stdout line.
    let methods: Vec<String> = captured
        .iter()
        .filter_map(|request| request.json()["method"].as_str().map(str::to_owned))
        .collect();
    assert!(
        methods.iter().any(|m| m == "notifications/initialized"),
        "the notification was relayed: {methods:?}"
    );
    assert!(
        methods.iter().any(|m| m == "initialize"),
        "the request was relayed too: {methods:?}"
    );
}

/// One slow tool call must not block the rest of the session.
///
/// This matters concretely: `await_changes` blocks for tens of seconds by
/// design, and agents issue parallel tool calls. A relay that served frames
/// strictly one at a time would stall an entire agent session behind a single
/// long-poll.
#[test]
fn daemon_mode_a_slow_call_does_not_block_later_calls() {
    let stub = StubDaemon::start();
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(Some(&stub.base_url()), data_dir.path(), None);

    // Warm the connection so the assertion measures relay behavior, not TCP setup.
    proxy.send(&initialize_frame(1));
    assert_eq!(proxy.recv()["id"], json!(1));

    proxy.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "slow" }));
    proxy.send(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }));

    let first = proxy.recv();
    assert_eq!(
        first["id"],
        json!(3),
        "the fast call is answered while the slow one is still in flight: {first}"
    );
    let second = proxy.recv();
    assert_eq!(second["id"], json!(2), "the slow call still completes");
}

/// The daemon's `Mcp-Session-Id` must be echoed on later frames, so a
/// session-stateful daemon sees one continuous session per relay rather than a
/// fresh one per call.
#[test]
fn daemon_mode_echoes_the_session_id_the_daemon_issued() {
    let stub = StubDaemon::start();
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(Some(&stub.base_url()), data_dir.path(), None);

    proxy.send(&initialize_frame(1));
    let _ = proxy.recv();
    proxy.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let _ = proxy.recv();

    let captured = stub.captured();
    assert_eq!(captured.len(), 2);
    assert!(
        captured[0].header("mcp-session-id").is_none(),
        "the first frame has no session yet"
    );
    assert_eq!(
        captured[1].header("mcp-session-id"),
        Some("stub-session-1"),
        "later frames carry the session the daemon issued: {:?}",
        captured[1].headers
    );
}

/// A daemon that dies **mid-session** must surface as a JSON-RPC error on the
/// next call — and the relay must still exit cleanly afterwards. A relay that
/// hangs here is the stale process that pins the executable on Windows: the
/// motivating bug.
#[test]
fn daemon_mode_survives_the_daemon_dying_mid_session() {
    let stub = StubDaemon::start();
    let base_url = stub.base_url();
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(Some(&base_url), data_dir.path(), None);

    proxy.send(&initialize_frame(1));
    assert_eq!(proxy.recv()["id"], json!(1));

    // The daemon goes away: it stops accepting AND closes live connections.
    drop(stub);
    std::thread::sleep(Duration::from_millis(500));

    proxy.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let reply = proxy.recv();
    assert_eq!(reply["id"], json!(2), "the request is still answered");
    assert!(
        reply["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("aletheia daemon status")),
        "the error is diagnosable: {reply}"
    );

    proxy.close_stdin();
    let status = proxy
        .wait_for_exit(Duration::from_secs(15))
        .expect("the relay still exits cleanly after the daemon vanished");
    assert!(status.success());
}

/// A reply for work already in flight must be delivered before the relay exits,
/// even when the client closes stdin immediately after sending.
#[test]
fn daemon_mode_delivers_in_flight_replies_before_exiting() {
    let stub = StubDaemon::start();
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(Some(&stub.base_url()), data_dir.path(), None);

    proxy.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "slow" }));
    // Disconnect while the slow call is still outstanding.
    proxy.close_stdin();

    let reply = proxy.recv_within(Duration::from_secs(30));
    assert_eq!(
        reply["id"],
        json!(1),
        "an in-flight reply is not dropped on the way out: {reply}"
    );
    let status = proxy
        .wait_for_exit(Duration::from_secs(15))
        .expect("the relay exits after draining");
    assert!(status.success());
}

/// AC: "stdio proxy processes exit when the client disconnects; the daemon
/// remains the long-lived process."
#[test]
fn daemon_mode_proxy_exits_when_client_disconnects() {
    let stub = StubDaemon::start();
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(Some(&stub.base_url()), data_dir.path(), None);

    proxy.send(&initialize_frame(1));
    let _ = proxy.recv();

    proxy.close_stdin();
    let status = proxy
        .wait_for_exit(Duration::from_secs(15))
        .expect("proxy must exit promptly after stdin EOF (client disconnect)");
    assert!(
        status.success(),
        "a client disconnect is a normal shutdown, not a failure: {status:?}"
    );

    // The daemon is unaffected: it still accepts connections.
    let probe = TcpStream::connect(stub.addr).expect("daemon still listening after proxy exit");
    drop(probe);
}

/// AC: "Direct embedded MCP mode still works as a fallback when no daemon URL
/// is configured."
#[test]
fn without_daemon_url_the_embedded_mode_still_serves() {
    let data_dir = TempDir::new().expect("tempdir");
    let mut proxy = ProxyProcess::spawn(None, data_dir.path(), None);

    proxy.send(&initialize_frame(1));
    let response = proxy.recv();

    assert_eq!(response["id"], json!(1));
    assert!(
        response["result"]["serverInfo"].is_object(),
        "embedded mode answers initialize itself: {response}"
    );
    assert!(
        response["result"]["capabilities"]["tools"].is_object(),
        "embedded mode advertises tools: {response}"
    );

    // ...and it DID open local storage — the contrast that makes the
    // `daemon_mode_never_opens_local_storage` assertion meaningful.
    let entries = entries_under(data_dir.path());
    assert!(
        entries.iter().any(|entry| entry.starts_with("wal")),
        "embedded mode creates the write-ahead log — the marker of a database \
         actually being opened, and the contrast that makes the daemon-mode \
         empty-directory assertion mean \"no database was opened\" rather than \
         \"no file happened to appear\": {entries:?}"
    );
}
