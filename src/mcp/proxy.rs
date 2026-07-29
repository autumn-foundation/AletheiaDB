//! Daemon-client mode for `aletheia-mcp`: a stdio ⇄ HTTP MCP relay (Issue #2905).
//!
//! # Why a relay and not a translator
//!
//! Command-based MCP clients spawn one server process per session. When that
//! process is an *embedded* AletheiaDB, every session owns storage — so either
//! each session gets its own ephemeral database, or several processes open the
//! same data directory as unsupported concurrent writers.
//!
//! Daemon-client mode removes the ownership from the session entirely. The
//! process launched by the MCP client becomes a **relay**: it reads JSON-RPC
//! messages on stdin, forwards them verbatim to the daemon's MCP endpoint over
//! HTTP, and writes the daemon's replies to stdout. It never constructs an
//! [`AletheiaDB`](crate::AletheiaDB) — the module is deliberately free of any
//! storage import, and a test asserts that at the source level.
//!
//! Relaying rather than translating is what keeps this small and future-proof:
//! `initialize`, capability negotiation, `tools/list`, `tools/call`, and any
//! method a future protocol revision adds all flow through with **no
//! per-method code**. There is no tool table here to drift out of sync with the
//! daemon's.
//!
//! # Transport mapping
//!
//! | stdio (MCP) | HTTP (MCP Streamable HTTP) |
//! |---|---|
//! | one newline-delimited JSON message | one `POST` to `{base_url}/mcp` |
//! | request (has `id`) | response body → one stdout line |
//! | notification (no `id`) | `202 Accepted`, no stdout line |
//! | — | `text/event-stream` reply → one stdout line per `data:` frame |
//!
//! The session credential (`ALETHEIADB_MCP_API_KEY`) travels as
//! `Authorization: Bearer`, so the **daemon** stays the single authentication
//! and RBAC decision point; the relay makes no access-control decisions.
//! An `Mcp-Session-Id` returned by the daemon is remembered and echoed on
//! subsequent requests, so a session-stateful daemon sees one continuous
//! session per relay process.
//!
//! # Lifecycle
//!
//! The relay exits when stdin reaches EOF — i.e. when the MCP client
//! disconnects. The daemon is the long-lived process; relays are disposable.
//! That is what stops a fleet of stale `aletheia-mcp.exe` handles from pinning
//! the installed binary on Windows.

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::auth::SecretString;

/// Environment variable selecting daemon-client mode.
pub const DAEMON_URL_ENV: &str = "ALETHEIADB_DAEMON_URL";

/// Command-line flag selecting daemon-client mode (takes precedence over the
/// environment variable).
pub const DAEMON_URL_FLAG: &str = "--daemon-url";

/// Default time budget for one relayed JSON-RPC call.
///
/// Generous: some MCP tools (`await_changes`, large traversals) legitimately
/// take tens of seconds. The point of the timeout is that a dead daemon
/// surfaces as a JSON-RPC error instead of hanging the agent forever.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Time budget for establishing the TCP/TLS connection to the daemon.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum size of a single response body accepted from the daemon (32 MiB).
///
/// Comfortably above the daemon's own 8 MiB response cap, so a legitimate reply
/// is never refused; the bound exists to keep a wrong or hostile
/// `ALETHEIADB_DAEMON_URL` from exhausting the relay's memory.
pub const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

/// Maximum size of a single JSON-RPC frame read from stdin (32 MiB).
///
/// Bounds what a client can make this process buffer for one frame; without it
/// a newline-free write of arbitrary length would grow a `String` without
/// limit.
pub const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

/// Maximum JSON-RPC frames relayed concurrently.
///
/// Bounds the memory one client session can force this process to hold, and
/// back-pressures stdin rather than queueing without limit. Generous relative to
/// how many tool calls an agent has outstanding at once.
pub const MAX_IN_FLIGHT_FRAMES: usize = 32;

/// JSON-RPC error code reported when the daemon cannot be reached or answers
/// unusably. `-32000` is the reserved implementation-defined server-error range.
const JSONRPC_TRANSPORT_ERROR: i64 = -32000;

/// Configuration for the daemon-client relay.
#[derive(Debug, Clone)]
pub struct DaemonClientConfig {
    endpoint: String,
    api_key: Option<SecretString>,
    connect_timeout: Duration,
    request_timeout: Duration,
}

impl DaemonClientConfig {
    /// Build a configuration targeting a daemon at `base_url`.
    ///
    /// The URL may be given with or without the `/mcp` path: an operator who
    /// copies the daemon's base URL out of `aletheia daemon status` and one who
    /// copies the MCP endpoint both end up pointing at the same place.
    #[must_use]
    pub fn new(base_url: impl AsRef<str>) -> Self {
        Self {
            endpoint: normalize_endpoint(base_url.as_ref()),
            api_key: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    /// Attach the session credential forwarded to the daemon as a bearer token.
    #[must_use]
    pub fn with_api_key(mut self, key: impl AsRef<str>) -> Self {
        let key = key.as_ref().trim();
        self.api_key = (!key.is_empty()).then(|| SecretString::new(key));
        self
    }

    /// Override the per-call request timeout.
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Override the connection timeout.
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Resolve daemon-client mode from CLI arguments and the environment.
    ///
    /// `--daemon-url <URL>` (or `--daemon-url=<URL>`) wins over
    /// [`DAEMON_URL_ENV`]; when neither is present the caller stays in embedded
    /// mode. The credential comes from `ALETHEIADB_MCP_API_KEY`, the same
    /// variable the embedded server uses, so one client configuration works in
    /// both modes.
    ///
    /// # Errors
    ///
    /// Returns a message when `--daemon-url` is passed without a value.
    pub fn resolve(args: &[String]) -> Result<Option<Self>, String> {
        let from_flag = daemon_url_from_args(args)?;
        let url = match from_flag {
            Some(url) => Some(url),
            None => std::env::var(DAEMON_URL_ENV)
                .ok()
                .map(|raw| raw.trim().to_string())
                .filter(|raw| !raw.is_empty()),
        };
        let Some(url) = url else {
            return Ok(None);
        };

        let mut config = Self::new(url);
        if let Ok(key) = std::env::var("ALETHEIADB_MCP_API_KEY") {
            config = config.with_api_key(key);
        }
        Ok(Some(config))
    }

    /// The resolved MCP endpoint this relay posts to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Whether a session credential is configured.
    #[must_use]
    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }
}

/// Extract `--daemon-url <URL>` / `--daemon-url=<URL>` from CLI arguments.
fn daemon_url_from_args(args: &[String]) -> Result<Option<String>, String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(inline) = arg.strip_prefix(&format!("{DAEMON_URL_FLAG}=")) {
            let value = inline.trim();
            if value.is_empty() {
                return Err(format!("{DAEMON_URL_FLAG} requires a URL"));
            }
            return Ok(Some(value.to_string()));
        }
        if arg == DAEMON_URL_FLAG {
            let value = iter
                .next()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{DAEMON_URL_FLAG} requires a URL"))?;
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// Normalize a user-supplied daemon URL into the MCP endpoint to post to.
///
/// Accepts a bare authority (`127.0.0.1:1963`), a base URL
/// (`http://127.0.0.1:1963`, with or without a trailing slash), or the endpoint
/// itself (`http://127.0.0.1:1963/mcp`). Any other explicit path is respected
/// verbatim so a daemon behind a reverse proxy at `/aletheia/mcp` still works.
fn normalize_endpoint(raw: &str) -> String {
    let trimmed = raw.trim();
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (scheme.to_string(), rest.to_string()),
        None => ("http".to_string(), trimmed.to_string()),
    };

    // Drop any `user:password@` prefix. Embedded credentials would otherwise be
    // interpolated into the transport-error messages the agent reads, and the
    // relay authenticates with a bearer token rather than basic auth anyway.
    let rest = match rest.split_once('@') {
        Some((_userinfo, remainder)) => {
            eprintln!(
                "aletheia-mcp: ignoring credentials embedded in the daemon URL; \
                 use ALETHEIADB_MCP_API_KEY instead."
            );
            remainder.to_string()
        }
        None => rest,
    };

    let rest = rest.trim_end_matches('/');
    warn_if_plaintext_to_remote_host(&scheme, rest);

    // Path-less URL → append the MCP mount path; any explicit path (a daemon
    // behind a reverse proxy at `/aletheia/mcp`) is respected verbatim.
    if rest.contains('/') {
        format!("{scheme}://{rest}")
    } else {
        format!("{scheme}://{rest}/mcp")
    }
}

/// Warn when the session credential would cross a network in cleartext.
///
/// A bare authority (`db.internal:1963`) resolves to `http://`, which is right
/// for the loopback default and wrong — silently — for a remote daemon: the
/// bearer token would travel unencrypted.
fn warn_if_plaintext_to_remote_host(scheme: &str, authority: &str) {
    if !scheme.eq_ignore_ascii_case("http") {
        return;
    }
    let host = authority
        .split('/')
        .next()
        .unwrap_or_default()
        .rsplit_once(':')
        .map_or(authority, |(host, _port)| host)
        .trim_matches(|c| c == '[' || c == ']');
    let is_loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !is_loopback {
        eprintln!(
            "WARNING: the daemon URL uses plaintext http:// to the non-local host '{host}'; \
             the API key will cross the network unencrypted. Use https:// or an SSH tunnel."
        );
    }
}

/// Errors that end a relay session.
#[derive(Debug)]
pub enum ProxyError {
    /// The HTTP client could not be constructed from the configuration.
    Client(String),
    /// stdin/stdout failed.
    Io(std::io::Error),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Client(message) => write!(f, "failed to build daemon client: {message}"),
            Self::Io(e) => write!(f, "stdio failure: {e}"),
        }
    }
}

impl std::error::Error for ProxyError {}

impl From<std::io::Error> for ProxyError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Run the stdio ⇄ HTTP relay until `input` reaches EOF.
///
/// Each newline-delimited JSON-RPC message read from `input` is posted to the
/// daemon; each reply is written to `output` as one newline-delimited message.
///
/// # Concurrency
///
/// Frames are relayed **concurrently**, up to [`MAX_IN_FLIGHT_FRAMES`]. JSON-RPC
/// correlates by `id`, so replies may complete out of order — and they must be
/// allowed to. A strictly sequential relay would let one long call block the
/// whole session: `await_changes` legitimately blocks for tens of seconds, and
/// an agent issuing parallel tool calls would see them serialized. Reading stops
/// (back-pressuring the client) once the in-flight limit is reached, so a
/// misbehaving client cannot make this process buffer without bound.
///
/// Returning `Ok(())` means the client disconnected — the normal shutdown for a
/// proxy process. In-flight requests are awaited first, so a reply is never
/// dropped on the way out.
///
/// # Errors
///
/// Returns [`ProxyError::Client`] if the HTTP client cannot be built, or
/// [`ProxyError::Io`] if stdin/stdout fails. A daemon that is unreachable or
/// answers with an error status is **not** an error here: it is reported to the
/// client as a JSON-RPC error response, so the agent sees a diagnosable failure
/// instead of a dead pipe.
pub async fn run_stdio_proxy<R, W>(
    config: DaemonClientConfig,
    input: R,
    output: W,
) -> Result<(), ProxyError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let client = reqwest::Client::builder()
        .connect_timeout(config.connect_timeout)
        .timeout(config.request_timeout)
        .build()
        .map_err(|e| ProxyError::Client(e.to_string()))?;

    let session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut reader = input;
    // Owned by the loop, not by the read future, so a `select!` cancellation
    // cannot discard a partially-read frame.
    let mut partial_frame: Vec<u8> = Vec::new();
    let mut output = output;
    let mut in_flight = FuturesUnordered::new();
    let mut client_connected = true;

    loop {
        tokio::select! {
            // Bias toward draining completed work, so replies reach the client
            // as soon as they are ready even under a steady inbound stream.
            biased;

            Some(replies) = in_flight.next() => {
                write_replies(&mut output, replies).await?;
            }

            frame = read_frame(&mut reader, &mut partial_frame), if client_connected
                && in_flight.len() < MAX_IN_FLIGHT_FRAMES =>
            {
                match frame? {
                    Some(frame) if frame.is_empty() => continue,
                    Some(frame) => {
                        in_flight.push(relay_frame(&client, &config, &session_id, frame));
                    }
                    // stdin EOF: the client disconnected. Stop reading, but keep
                    // draining what is already in flight.
                    None => client_connected = false,
                }
            }

            else => break,
        }

        if !client_connected && in_flight.is_empty() {
            break;
        }
    }

    Ok(())
}

/// Read one newline-delimited frame into `partial`, bounded by
/// [`MAX_FRAME_BYTES`].
///
/// `AsyncBufReadExt::lines` grows one `String` without limit, so a newline-free
/// write of arbitrary length would exhaust memory — and `AsyncReadExt::take`
/// bounds the *whole stream*, which would silently end the session mid-run.
/// This bounds each frame individually: bytes past the cap are dropped and the
/// truncated frame fails to parse, which the daemon answers with a JSON-RPC
/// parse error — diagnosable, unlike an OOM.
///
/// # Cancellation safety
///
/// Partially-read bytes accumulate in `partial`, which the **caller** owns, so
/// dropping this future (as `tokio::select!` does when another branch wins)
/// loses nothing: the next call resumes where this one stopped. Keeping the
/// accumulator inside the future — the obvious shape — would silently truncate
/// frames that span more than one read.
async fn read_frame<R>(reader: &mut R, partial: &mut Vec<u8>) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let (consumed, complete) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                // EOF. A trailing frame with no newline is still a frame.
                return Ok((!partial.is_empty()).then(|| take_frame(partial)));
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(newline) => {
                    append_capped(partial, &available[..newline]);
                    (newline + 1, true)
                }
                None => {
                    let len = available.len();
                    append_capped(partial, available);
                    (len, false)
                }
            }
        };
        reader.consume(consumed);
        if complete {
            return Ok(Some(take_frame(partial)));
        }
    }
}

/// Append `bytes` to `frame`, silently dropping anything past
/// [`MAX_FRAME_BYTES`].
fn append_capped(frame: &mut Vec<u8>, bytes: &[u8]) {
    let room = (MAX_FRAME_BYTES as usize).saturating_sub(frame.len());
    frame.extend_from_slice(&bytes[..room.min(bytes.len())]);
}

/// Consume the accumulated bytes as a trimmed frame, leaving the buffer empty
/// and ready for the next one.
fn take_frame(frame: &mut Vec<u8>) -> String {
    let text = String::from_utf8_lossy(frame).trim().to_string();
    frame.clear();
    text
}

/// Relay one frame, turning a transport failure into the replies the client is
/// owed (or into nothing, for a notification).
async fn relay_frame(
    client: &reqwest::Client,
    config: &DaemonClientConfig,
    session_id: &Arc<Mutex<Option<String>>>,
    frame: String,
) -> Vec<Value> {
    // Peek at the ids so a transport failure is reported against the right
    // requests, and so notifications are never answered.
    let ids = request_ids(&frame);

    match relay_one(client, config, session_id, &frame).await {
        Ok(replies) => replies,
        // Every request must be answered, or the client hangs forever.
        Err(message) if !ids.is_empty() => ids
            .into_iter()
            .map(|id| transport_error(id, &message))
            .collect(),
        // Only notifications: they expect no reply, so report to stderr rather
        // than fabricating a response the client never asked for.
        Err(message) => {
            eprintln!("aletheia-mcp: failed to relay notification: {message}");
            Vec::new()
        }
    }
}

/// The JSON-RPC ids a frame expects answers for.
///
/// Handles the **batch** form (a top-level array) as well as a single object.
/// `Value::get("id")` returns `None` for an array, so treating a batch as one
/// object would classify it as a notification — and on a transport failure the
/// client would wait forever for replies that were never sent.
fn request_ids(frame: &str) -> Vec<Value> {
    let Ok(value) = serde_json::from_str::<Value>(frame) else {
        return Vec::new();
    };
    match value {
        Value::Array(messages) => messages
            .iter()
            .filter_map(|message| message.get("id").cloned())
            .filter(|id| !id.is_null())
            .collect(),
        other => other
            .get("id")
            .cloned()
            .filter(|id| !id.is_null())
            .into_iter()
            .collect(),
    }
}

/// Write relayed messages to the client as newline-delimited JSON.
async fn write_replies<W>(output: &mut W, replies: Vec<Value>) -> Result<(), ProxyError>
where
    W: AsyncWrite + Unpin,
{
    if replies.is_empty() {
        return Ok(());
    }
    for reply in replies {
        output.write_all(reply.to_string().as_bytes()).await?;
        output.write_all(b"\n").await?;
    }
    output.flush().await?;
    Ok(())
}

/// Forward one JSON-RPC frame and return the messages to emit for it.
///
/// An empty vector means "no reply" (a notification, answered `202`).
async fn relay_one(
    client: &reqwest::Client,
    config: &DaemonClientConfig,
    session_id: &Arc<Mutex<Option<String>>>,
    frame: &str,
) -> Result<Vec<Value>, String> {
    let mut request = client
        .post(&config.endpoint)
        .header("content-type", "application/json")
        // Accept both Streamable-HTTP reply shapes: a buffered JSON body, and
        // the SSE channel a streaming tool answers on.
        .header("accept", "application/json, text/event-stream")
        .body(frame.to_string());

    if let Some(key) = &config.api_key {
        request = request.header("authorization", format!("Bearer {}", key.expose()));
    }
    if let Some(id) = session_id.lock().await.as_deref() {
        request = request.header("mcp-session-id", id);
    }

    let response = request.send().await.map_err(|e| {
        format!(
            "cannot reach the AletheiaDB daemon at {}: {e}. Is it running \
             (`aletheia daemon status`)?",
            config.endpoint
        )
    })?;

    // Remember the daemon's session id for subsequent frames.
    if let Some(id) = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
    {
        *session_id.lock().await = Some(id);
    }

    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_event_stream = content_type.contains("text/event-stream");

    let body = read_body_capped(response).await?;

    if body.trim().is_empty() {
        // `202 Accepted` with no body: the daemon acknowledged a notification.
        if status.is_success() {
            return Ok(Vec::new());
        }
        return Err(format!("daemon returned HTTP {status} with an empty body"));
    }

    if is_event_stream {
        return Ok(parse_sse_messages(&body));
    }

    match serde_json::from_str::<Value>(&body) {
        // A JSON-RPC batch reply is emitted as-is: the client correlates it.
        Ok(value) => Ok(vec![value]),
        // A non-JSON body means the daemon (or something in front of it)
        // answered outside the protocol — surface the status and a bounded
        // excerpt rather than corrupting the client's stream.
        Err(_) => Err(format!(
            "daemon returned HTTP {status} with a non-JSON body: {}",
            excerpt(&body)
        )),
    }
}

/// Extract the JSON messages carried by an SSE reply's `data:` frames.
fn parse_sse_messages(body: &str) -> Vec<Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|payload| !payload.is_empty())
        .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
        .collect()
}

/// Read a response body, refusing anything over [`MAX_RESPONSE_BYTES`].
///
/// `Response::text()` buffers without limit, and up to [`MAX_IN_FLIGHT_FRAMES`]
/// of these run concurrently — so a misconfigured `ALETHEIADB_DAEMON_URL` that
/// lands on a large-file host would exhaust the memory of a process running
/// inside the user's agent session. The declared `Content-Length` is rejected up
/// front when it is already over cap; chunked replies are bounded as they
/// stream.
async fn read_body_capped(response: reqwest::Response) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES)
    {
        return Err(format!(
            "the daemon's response exceeds the {MAX_RESPONSE_BYTES}-byte relay limit"
        ));
    }

    let mut response = response;
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("failed to read the daemon's response body: {e}"))?
    {
        if buffer.len() as u64 + chunk.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(format!(
                "the daemon's response exceeds the {MAX_RESPONSE_BYTES}-byte relay limit"
            ));
        }
        buffer.extend_from_slice(&chunk);
    }
    String::from_utf8(buffer).map_err(|_| "the daemon's response was not valid UTF-8".to_string())
}

/// Truncate and sanitize an upstream body for inclusion in an error message.
///
/// The bytes come from whatever answered at the daemon URL — a captive portal,
/// a reverse-proxy error page, the wrong host — and the resulting message is
/// read by an LLM as tool output. Control characters and newlines are stripped
/// so the excerpt cannot forge structure in the agent's transcript, and the
/// length is bounded.
fn excerpt(body: &str) -> String {
    const MAX: usize = 200;
    let sanitized: String = body
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let sanitized = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    if sanitized.chars().count() <= MAX {
        return sanitized;
    }
    let head: String = sanitized.chars().take(MAX).collect();
    format!("{head}…")
}

/// Build the JSON-RPC error response reported when the daemon is unreachable.
fn transport_error(id: Value, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": JSONRPC_TRANSPORT_ERROR,
            "message": message,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_url_gains_the_mcp_path() {
        assert_eq!(
            DaemonClientConfig::new("http://127.0.0.1:1963").endpoint(),
            "http://127.0.0.1:1963/mcp"
        );
    }

    #[test]
    fn a_trailing_slash_does_not_double_up() {
        assert_eq!(
            DaemonClientConfig::new("http://127.0.0.1:1963/").endpoint(),
            "http://127.0.0.1:1963/mcp"
        );
    }

    #[test]
    fn an_explicit_mcp_endpoint_is_kept() {
        assert_eq!(
            DaemonClientConfig::new("http://127.0.0.1:1963/mcp").endpoint(),
            "http://127.0.0.1:1963/mcp"
        );
    }

    #[test]
    fn a_reverse_proxy_path_is_respected() {
        assert_eq!(
            DaemonClientConfig::new("https://gateway.example/aletheia/mcp").endpoint(),
            "https://gateway.example/aletheia/mcp"
        );
    }

    #[test]
    fn a_bare_authority_gets_http_and_the_mcp_path() {
        assert_eq!(
            DaemonClientConfig::new("127.0.0.1:1963").endpoint(),
            "http://127.0.0.1:1963/mcp"
        );
    }

    #[test]
    fn https_is_preserved() {
        assert_eq!(
            DaemonClientConfig::new("https://db.internal:8443").endpoint(),
            "https://db.internal:8443/mcp"
        );
    }

    #[test]
    fn the_flag_supplies_the_url() {
        let args = vec!["--daemon-url".to_string(), "http://d:1963".to_string()];
        let resolved = DaemonClientConfig::resolve(&args)
            .expect("valid flag")
            .expect("daemon mode");
        assert_eq!(resolved.endpoint(), "http://d:1963/mcp");
    }

    #[test]
    fn the_inline_flag_form_supplies_the_url() {
        let args = vec!["--daemon-url=http://d:1963".to_string()];
        let resolved = DaemonClientConfig::resolve(&args)
            .expect("valid flag")
            .expect("daemon mode");
        assert_eq!(resolved.endpoint(), "http://d:1963/mcp");
    }

    #[test]
    fn a_valueless_flag_is_an_error_not_a_silent_embedded_fallback() {
        let args = vec!["--daemon-url".to_string()];
        let error = DaemonClientConfig::resolve(&args).expect_err("must not fall back silently");
        assert!(
            error.contains("--daemon-url"),
            "actionable message: {error}"
        );
    }

    #[test]
    fn an_empty_inline_flag_is_an_error() {
        let args = vec!["--daemon-url=".to_string()];
        assert!(DaemonClientConfig::resolve(&args).is_err());
    }

    #[test]
    fn no_flag_and_no_env_means_embedded_mode() {
        // Explicitly ignore any ambient env by asserting only the flag path
        // when the variable is absent; `resolve` reads the process env, which
        // the test harness shares, so this asserts the flag-free branch only
        // when the operator has not opted in.
        if std::env::var(DAEMON_URL_ENV).is_ok() {
            return;
        }
        assert!(
            DaemonClientConfig::resolve(&[]).expect("no flag").is_none(),
            "without a daemon URL the caller stays embedded"
        );
    }

    #[test]
    fn an_empty_api_key_is_not_forwarded() {
        let config = DaemonClientConfig::new("http://d:1963").with_api_key("   ");
        assert!(
            !config.has_api_key(),
            "a blank credential must not become an `Authorization: Bearer ` header"
        );
    }

    #[test]
    fn sse_frames_become_individual_messages() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n\
                    event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n";
        let messages = parse_sse_messages(body);
        assert_eq!(messages.len(), 2, "one message per data frame");
        assert_eq!(messages[0]["id"], json!(1));
        assert_eq!(messages[1]["method"], "notifications/progress");
    }

    #[test]
    fn sse_parsing_skips_non_data_lines_and_blank_frames() {
        let body = ": keepalive\nevent: message\ndata:\n\ndata: {\"ok\":true}\n\n";
        let messages = parse_sse_messages(body);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["ok"], json!(true));
    }

    #[test]
    fn transport_errors_are_reported_against_the_originating_request() {
        let error = transport_error(json!(17), "daemon unreachable");
        assert_eq!(error["id"], json!(17));
        assert_eq!(error["error"]["code"], json!(JSONRPC_TRANSPORT_ERROR));
        assert_eq!(error["error"]["message"], "daemon unreachable");
    }

    #[test]
    fn excerpts_are_bounded() {
        let long = "x".repeat(1000);
        let excerpt = excerpt(&long);
        assert!(excerpt.chars().count() <= 201, "bounded: {}", excerpt.len());
        assert!(excerpt.ends_with('…'));
    }

    #[tokio::test]
    async fn a_client_disconnect_ends_the_relay_cleanly() {
        // EOF on stdin with no frames: the relay returns Ok, which is what makes
        // the proxy process exit 0 when its MCP client disconnects.
        let input = tokio::io::BufReader::new(std::io::Cursor::new(Vec::<u8>::new()));
        let mut output = Vec::new();
        let result = run_stdio_proxy(
            DaemonClientConfig::new("http://127.0.0.1:1"),
            input,
            &mut output,
        )
        .await;
        assert!(result.is_ok(), "client disconnect is a clean shutdown");
        assert!(output.is_empty(), "no frames in, no frames out");
    }

    #[tokio::test]
    async fn an_unreachable_daemon_answers_with_a_jsonrpc_error_not_a_hang() {
        // Port 1 is never a live daemon. The relay must answer the request so
        // the agent sees a diagnosable failure instead of a dead pipe.
        let frame = "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/list\"}\n";
        let input = tokio::io::BufReader::new(std::io::Cursor::new(frame.as_bytes().to_vec()));
        let mut output = Vec::new();
        run_stdio_proxy(
            DaemonClientConfig::new("http://127.0.0.1:1")
                .with_connect_timeout(Duration::from_millis(500))
                .with_request_timeout(Duration::from_secs(2)),
            input,
            &mut output,
        )
        .await
        .expect("relay ends cleanly");

        let text = String::from_utf8(output).expect("utf8");
        let reply: Value = serde_json::from_str(text.trim()).expect("one JSON line");
        assert_eq!(reply["id"], json!(5), "answered against the right request");
        assert_eq!(reply["error"]["code"], json!(JSONRPC_TRANSPORT_ERROR));
        let message = reply["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("aletheia daemon status"),
            "the error tells the operator how to diagnose it: {message}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_daemon_does_not_fabricate_a_reply_to_a_notification() {
        let frame = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
        let input = tokio::io::BufReader::new(std::io::Cursor::new(frame.as_bytes().to_vec()));
        let mut output = Vec::new();
        run_stdio_proxy(
            DaemonClientConfig::new("http://127.0.0.1:1")
                .with_connect_timeout(Duration::from_millis(500))
                .with_request_timeout(Duration::from_secs(2)),
            input,
            &mut output,
        )
        .await
        .expect("relay ends cleanly");
        assert!(
            output.is_empty(),
            "a notification expects no response, even on failure"
        );
    }
}
