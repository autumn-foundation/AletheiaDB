//! Production-stack integration test: the `413 Payload Too Large` body-size
//! limit is enforced through the REAL `run_server` / autumn layer stack, over
//! a real TCP socket.
//!
//! The `warden_http_param_dos.rs` regression test asserts the `413` behavior
//! through `build_test_router`, which *mirrors* — but is not — the production
//! router assembled in `src/http/server.rs::run_server`. A future refactor of
//! the production layer stack (the `DefaultBodyLimit::max(...)` layer) could
//! drop or disable the limit while the mirrored test router still passes. This
//! test boots the actual `run_server` on an ephemeral port and behaviorally
//! asserts an oversized body is rejected with `413` over the wire, so that
//! regression is caught.
//!
//! Regression test for Issue #3426 (item 2).
//!
//! Run with: `cargo test --test http_run_server_body_limit --features http-server`

#![cfg(feature = "http-server")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use aletheiadb::auth::AuthMode;
use aletheiadb::http::{ServerConfig, run_server};

/// Body-size limit for the booted server: small enough that a terse oversized
/// array trips it, comfortably above the tiny positive-control request.
const TEST_BODY_LIMIT: usize = 4 * 1024;

/// Reserve an ephemeral localhost port by binding (then dropping) a listener,
/// returning the port number the OS chose. There is an inherent small race
/// between releasing the port here and `run_server` re-binding it; on a test
/// host this is negligible and the readiness loop below tolerates the startup
/// window.
fn reserve_ephemeral_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("read local addr").port();
    drop(listener);
    port
}

/// Send a raw HTTP/1.1 POST and return the numeric status code, or `None` if
/// the connection could not be established / completed (server not up yet).
fn post_status(addr: &str, path: &str, body: &[u8]) -> Option<u16> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;

    let host = addr;
    let request_head = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n",
        len = body.len()
    );

    stream.write_all(request_head.as_bytes()).ok()?;
    // Race: the server may reject the oversized body and RST/close the
    // connection before we finish writing it, so `write_all`/`flush` can fail
    // with BrokenPipe/ConnectionReset even though a valid `413` status line is
    // already waiting to be read. Tolerate those write errors and still attempt
    // the read — the status line, not a clean write, is the real assertion.
    #[allow(clippy::collapsible_if)]
    if let Err(e) = stream.write_all(body) {
        if !matches!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ) {
            return None;
        }
    }
    let _ = stream.flush();

    let mut response = Vec::new();
    // Read until EOF (server sends `Connection: close`) or the read times out.
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&buf[..n]);
                // The status line is in the first packet; stop once we have the
                // full header block to avoid blocking on a large/slow body.
                if response.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    parse_status_code(&response)
}

/// Parse the numeric status code from an HTTP/1.x status line
/// (`HTTP/1.1 413 Payload Too Large`).
fn parse_status_code(response: &[u8]) -> Option<u16> {
    let text = String::from_utf8_lossy(response);
    let first_line = text.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let _version = parts.next()?;
    parts.next()?.parse::<u16>().ok()
}

/// Poll `post_status` until the server accepts connections, then return the
/// status of the request. Fails the test if the server never comes up.
fn post_when_ready(addr: &str, path: &str, body: &[u8]) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = post_status(addr, path, body) {
            return status;
        }
        if Instant::now() >= deadline {
            panic!("server at {addr} never became ready within timeout");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Plain (non-tokio) test: the autumn/axum `run_server` future cannot be
/// `tokio::spawn`ed (its higher-ranked lifetimes trip `FnOnce is not general
/// enough`), so we run the REAL server on its own OS thread with a dedicated
/// current-thread runtime and drive it with blocking `std::net` client I/O.
/// This still exercises the exact production layer stack over a real socket.
#[test]
fn run_server_enforces_413_over_the_wire() {
    let port = reserve_ephemeral_port();
    let addr = format!("127.0.0.1:{port}");

    // Boot the REAL production server stack on a background thread. Anonymous
    // mode (Issue #3350) so the body-limit layer — which runs before handler
    // auth — is what produces the 413, not an auth rejection. No data_dir =>
    // in-memory.
    let config = ServerConfig::builder()
        .host("127.0.0.1")
        .port(port)
        .auth_mode(AuthMode::Anonymous)
        .max_request_body_bytes(TEST_BODY_LIMIT)
        .build();

    let server_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build server runtime");
        // Runs until the process ends / thread is torn down; we never join it.
        let _ = rt.block_on(run_server(config));
    });

    // ── Oversized body must be rejected with 413 through the production stack. ──
    // Kept modest (~8 KiB, just over the 4 KiB limit) so it reliably fits a
    // single socket send and does not depend on the write completing after the
    // server has already RST the connection (see the write-tolerance in
    // `post_status`).
    let big_array: Vec<serde_json::Value> = (0..4_000).map(|_| serde_json::json!(1)).collect();
    let oversized = serde_json::to_vec(&serde_json::json!({
        "operation": "execute_query",
        "query": "MATCH (n) RETURN n",
        "parameters": { "v": big_array }
    }))
    .unwrap();
    assert!(
        oversized.len() > TEST_BODY_LIMIT && oversized.len() < 2 * 1024 * 1024,
        "oversized body ({} bytes) must exceed the configured limit but stay \
         under axum's historical 2 MiB default",
        oversized.len()
    );

    let status = post_when_ready(&addr, "/query", &oversized);
    assert_eq!(
        status, 413,
        "the production run_server stack must reject an oversized body with \
         413 Payload Too Large, got {status}"
    );

    // ── A small request under the limit must still be served (proves the
    // server is genuinely up and the limit is not rejecting everything). ──
    let small = serde_json::to_vec(&serde_json::json!({
        "operation": "create_node",
        "label": "Person",
        "properties": { "name": "Alice" }
    }))
    .unwrap();
    assert!(
        small.len() < TEST_BODY_LIMIT,
        "positive-control body ({} bytes) must be under the limit",
        small.len()
    );
    let ok_status = post_when_ready(&addr, "/query", &small);
    assert_eq!(
        ok_status, 200,
        "a legitimate small request must still succeed through run_server, got {ok_status}"
    );

    // The server thread is detached (it runs `run_server` forever); the test
    // process exits and reaps it. Nothing to join.
    drop(server_thread);
}
