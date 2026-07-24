//! TCP wire transport for asynchronous replication (Issue #3355, Slice C).
//!
//! Everything above this module (feed, source trait, applier) is transport
//! agnostic; this module is the one concrete "real network" implementation:
//! [`ReplicationServer`] (primary side, serves [`crate::storage::replication::feed::ReplicationFeed`]
//! requests over `std::net::TcpStream` connections) and [`TcpSource`]
//! (replica side, implements [`ReplicationSource`] by driving the same wire
//! protocol as a client).
//!
//! # Wire protocol
//!
//! Every message is a length-prefixed frame:
//!
//! ```text
//! [u32 LE payload_len][u8 msg_type][payload; payload_len bytes]
//! ```
//!
//! `payload_len` is capped at [`MAX_FRAME_SIZE`] on both sides -- a frame
//! claiming to be larger is a protocol error and the connection is closed.
//! Control payloads (handshake, fetch requests, resync/snapshot headers) are
//! `serde_json`. The one payload that is NOT plain JSON is [`MSG_ENTRIES`]:
//! a JSON header (see [`EntriesHeaderMsg`]) followed by `count`
//! length-prefixed binary WAL entries, each serialized with the existing WAL
//! entry codec (`crate::storage::wal::serialization::serialize_operation_into`
//! / `crate::storage::wal::segment_reader::parse_entry_at`) under the fixed
//! wire version [`WIRE_WAL_VERSION`] -- reusing the exact bytes the WAL
//! itself persists, so there is no second serialization format to keep in
//! sync.
//!
//! Message sequence:
//!
//! - C→S [`MSG_HELLO`] `{token, protocol_version}` → S→C [`MSG_HELLO_ACK`]
//!   `{protocol_version}` or [`MSG_AUTH_FAILED`] (token mismatch; connection
//!   closed immediately after). The token comparison is constant-time (SHA-256
//!   digest + [`subtle::ConstantTimeEq`]), mirroring `src/auth/store.rs`'s
//!   API-key verification. The token itself is never logged.
//! - C→S [`MSG_FETCH_ENTRIES`] `{from_lsn, max_entries}` → S→C [`MSG_ENTRIES`]
//!   (see above) or [`MSG_RESYNC_REQUIRED`] `{min_available_lsn}`.
//! - C→S [`MSG_FETCH_SNAPSHOT`] (empty payload) → S→C [`MSG_SNAPSHOT_HEADER`]
//!   `{len, source_lsn}` followed by exactly `len` **raw, unframed** bytes of
//!   a `.albk` backup artifact (streamed in fixed 1 MiB chunks so the whole
//!   file is never buffered in memory), or [`MSG_SNAPSHOT_UNAVAILABLE`] (see
//!   below).
//!
//! # Two feed backends
//!
//! [`ReplicationServer::start`] -- the primary, fully-functional entry point
//! -- takes a real `Arc<AletheiaDB>` and serves both `FetchEntries` and
//! `FetchSnapshot` via a full [`ReplicationFeed`].
//!
//! `AletheiaDB::with_unified_config`'s config-driven auto-wiring
//! (`replication.listen_addr`) is a different situation: it runs *while*
//! the database is still a bare, not-yet-`Arc`-wrapped local value being
//! constructed -- there is no legal way to hand a background thread an
//! `Arc<AletheiaDB>` pointing at an object that hasn't been placed in an
//! `Arc` yet (and never will be, if the caller drops the returned value
//! instead of wrapping it). Rather than not auto-start the server at all,
//! that path uses [`FeedBackend::EntriesOnly`]: `FetchEntries` is served
//! directly from the `Arc<ConcurrentWalSystem>` the database already owns
//! (see [`crate::storage::replication::feed::fetch_entries_from_wal`]), which
//! needs no self-reference at all. `FetchSnapshot` genuinely needs the full
//! database (`AletheiaDB::backup` snapshots current + historical + WAL
//! state consistently), so on this backend it replies
//! [`MSG_SNAPSHOT_UNAVAILABLE`] with a remediation message pointing the
//! operator at [`ReplicationServer::start`] (called with a real
//! `Arc<AletheiaDB>` once the caller has one) for bootstrap support. This
//! keeps the auto-wired path honest (never silently hangs/fails a bootstrap
//! attempt) while still making the common "primary configured to accept
//! replica connections" (AC1) case fully automatic and zero-touch for
//! streaming.
//!
//! # Concurrency / lifecycle
//!
//! Follows this repo's background-thread idiom (`FlushThread`,
//! [`crate::storage::replication::applier`]): an accept-loop thread polls a
//! non-blocking [`std::net::TcpListener`] so it notices
//! [`ReplicationServerHandle::shutdown`] promptly and releases the listening
//! port for a subsequent rebind (needed for reconnect/resume tests). Each
//! accepted connection is handled on its own spawned thread (capped at
//! [`MAX_CONCURRENT_CONNECTIONS`]; connections beyond the cap are refused
//! immediately with no response) and is registered in a shared table so
//! [`ReplicationServerHandle::shutdown`]/`Drop` can force-close every live
//! socket (`TcpStream::shutdown`), unblocking any in-flight read/write on
//! both this end and the peer's immediately -- this is what makes "stop the
//! server mid-stream" actually observable to a connected [`TcpSource`]
//! instead of leaving a stale connection quietly still serving requests.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::core::error::{Error, Result, StorageError};
use crate::db::AletheiaDB;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::storage::wal::entry::WalEntry;
use crate::storage::wal::segment_reader::{WAL_VERSION_STRING_LABELS, parse_entry_at};
use crate::storage::wal::serialization::serialize_operation_into;

use super::feed::{FetchOutcome, ReplicationFeed, fetch_entries_from_wal};
use super::source::ReplicationSource;

/// Wire-format WAL entry version. Fixed (not negotiated) because this wire
/// protocol -- unlike an on-disk segment, which must keep reading whatever
/// version it was written at -- always serializes with the current writer,
/// so both sides of a live connection agree on the newest plaintext layout.
/// Bump alongside the WAL's own newest-plaintext-version bumps (see
/// `crate::storage::wal::segment_reader`'s history-of-versions doc comment).
const WIRE_WAL_VERSION: u8 = WAL_VERSION_STRING_LABELS;

/// Protocol handshake version. Bump on any wire-incompatible change.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum accepted frame payload size (sanity cap against a malformed or
/// hostile peer claiming an enormous length prefix).
///
/// Sized as [`crate::storage::wal::entry::MAX_WAL_ENTRY_SIZE`] plus headroom
/// for the entries-payload framing (header JSON + per-entry length
/// prefixes), so that any single legal WAL entry -- up to the WAL's own
/// 64 MiB cap -- can always be shipped in a one-entry batch. With the two
/// caps equal (the original shape), a near-max entry could NEVER fit a
/// frame once framing overhead was added, permanently stalling replication
/// at that LSN.
pub const MAX_FRAME_SIZE: u32 = (crate::storage::wal::entry::MAX_WAL_ENTRY_SIZE as u32) + 64 * 1024;

/// Pre-authentication frame cap: before a peer has authenticated, the only
/// legal frame is a small `MSG_HELLO` control message, so the server refuses
/// to allocate more than this for it. Without a distinct pre-auth cap, 16
/// unauthenticated connections each claiming a `MAX_FRAME_SIZE` length
/// prefix could pin ~1 GiB of allocation before ever presenting a token.
const MAX_HELLO_FRAME_SIZE: u32 = 64 * 1024;

/// Maximum concurrently-served connections per [`ReplicationServer`].
/// Connections beyond this are refused (socket dropped, no response).
pub const MAX_CONCURRENT_CONNECTIONS: usize = 16;

/// Server-side per-request cap on `FetchEntriesMsg::max_entries`, regardless
/// of what a client requests -- defensive against a buggy/hostile client
/// asking for an unbounded batch.
const MAX_SERVED_ENTRIES_PER_FETCH: u32 = 100_000;

const MSG_HELLO: u8 = 1;
const MSG_HELLO_ACK: u8 = 2;
const MSG_AUTH_FAILED: u8 = 3;
const MSG_FETCH_ENTRIES: u8 = 4;
const MSG_ENTRIES: u8 = 5;
const MSG_RESYNC_REQUIRED: u8 = 6;
const MSG_FETCH_SNAPSHOT: u8 = 7;
const MSG_SNAPSHOT_HEADER: u8 = 8;
/// Sent instead of [`MSG_SNAPSHOT_HEADER`] when this server connection's feed
/// backend cannot serve a snapshot (see [`FeedBackend::EntriesOnly`]).
const MSG_SNAPSHOT_UNAVAILABLE: u8 = 9;

// ---------------------------------------------------------------------------
// Control message payloads (serde_json)
// ---------------------------------------------------------------------------

// Encoded/decoded by hand via `serde_json::Value` rather than serde derives:
// the `serde` crate is an optional dependency, while `serde_json` (and its
// `Value` impls) is always available, keeping this module compiling under
// `--no-default-features`.
trait ControlMsg: Sized {
    fn to_value(&self) -> Value;
    fn from_value(v: &Value) -> Option<Self>;
}

#[derive(Debug)]
struct HelloMsg {
    token: String,
    protocol_version: u32,
}

impl ControlMsg for HelloMsg {
    fn to_value(&self) -> Value {
        json!({ "token": self.token, "protocol_version": self.protocol_version })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            token: v.get("token")?.as_str()?.to_string(),
            protocol_version: u32::try_from(v.get("protocol_version")?.as_u64()?).ok()?,
        })
    }
}

#[derive(Debug)]
struct HelloAckMsg {
    protocol_version: u32,
}

impl ControlMsg for HelloAckMsg {
    fn to_value(&self) -> Value {
        json!({ "protocol_version": self.protocol_version })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            protocol_version: u32::try_from(v.get("protocol_version")?.as_u64()?).ok()?,
        })
    }
}

#[derive(Debug)]
struct FetchEntriesMsg {
    from_lsn: u64,
    max_entries: u32,
}

impl ControlMsg for FetchEntriesMsg {
    fn to_value(&self) -> Value {
        json!({ "from_lsn": self.from_lsn, "max_entries": self.max_entries })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            from_lsn: v.get("from_lsn")?.as_u64()?,
            max_entries: u32::try_from(v.get("max_entries")?.as_u64()?).ok()?,
        })
    }
}

#[derive(Debug)]
struct EntriesHeaderMsg {
    primary_flushed_lsn: u64,
    primary_wallclock_micros: i64,
    count: u32,
}

impl ControlMsg for EntriesHeaderMsg {
    fn to_value(&self) -> Value {
        json!({
            "primary_flushed_lsn": self.primary_flushed_lsn,
            "primary_wallclock_micros": self.primary_wallclock_micros,
            "count": self.count,
        })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            primary_flushed_lsn: v.get("primary_flushed_lsn")?.as_u64()?,
            primary_wallclock_micros: v.get("primary_wallclock_micros")?.as_i64()?,
            count: u32::try_from(v.get("count")?.as_u64()?).ok()?,
        })
    }
}

#[derive(Debug)]
struct ResyncRequiredMsg {
    min_available_lsn: u64,
}

impl ControlMsg for ResyncRequiredMsg {
    fn to_value(&self) -> Value {
        json!({ "min_available_lsn": self.min_available_lsn })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            min_available_lsn: v.get("min_available_lsn")?.as_u64()?,
        })
    }
}

#[derive(Debug)]
struct SnapshotHeaderMsg {
    len: u64,
    source_lsn: u64,
}

impl ControlMsg for SnapshotHeaderMsg {
    fn to_value(&self) -> Value {
        json!({ "len": self.len, "source_lsn": self.source_lsn })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            len: v.get("len")?.as_u64()?,
            source_lsn: v.get("source_lsn")?.as_u64()?,
        })
    }
}

#[derive(Debug)]
struct SnapshotUnavailableMsg {
    reason: String,
}

impl ControlMsg for SnapshotUnavailableMsg {
    fn to_value(&self) -> Value {
        json!({ "reason": self.reason })
    }

    fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            reason: v.get("reason")?.as_str()?.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Frame I/O
// ---------------------------------------------------------------------------

fn protocol_error(msg: impl Into<String>) -> Error {
    Error::Storage(StorageError::CorruptedData(msg.into()))
}

fn write_frame(stream: &mut TcpStream, msg_type: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME_SIZE as usize {
        return Err(io::Error::other(format!(
            "replication frame payload of {} bytes exceeds MAX_FRAME_SIZE ({} bytes)",
            payload.len(),
            MAX_FRAME_SIZE
        )));
    }
    stream.write_all(&(payload.len() as u32).to_le_bytes())?;
    stream.write_all(&[msg_type])?;
    stream.write_all(payload)?;
    stream.flush()
}

fn read_frame(stream: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
    read_frame_capped(stream, MAX_FRAME_SIZE)
}

/// [`read_frame`] with an explicit payload cap, so pre-authentication reads
/// (which only ever legitimately carry a tiny `MSG_HELLO`) can refuse large
/// claimed lengths outright instead of allocating for them.
fn read_frame_capped(stream: &mut TcpStream, max_len: u32) -> io::Result<(u8, Vec<u8>)> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    if len > max_len {
        return Err(io::Error::other(format!(
            "peer announced a replication frame of {len} bytes, exceeding the \
             {max_len}-byte cap for this read; closing connection"
        )));
    }
    let mut type_buf = [0u8; 1];
    stream.read_exact(&mut type_buf)?;
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload)?;
    Ok((type_buf[0], payload))
}

fn to_json(value: &impl ControlMsg) -> Result<Vec<u8>> {
    Ok(value.to_value().to_string().into_bytes())
}

fn from_json<T: ControlMsg>(payload: &[u8]) -> Result<T> {
    let v: Value = serde_json::from_slice(payload)
        .map_err(|e| protocol_error(format!("replication control message decode failed: {e}")))?;
    T::from_value(&v).ok_or_else(|| {
        protocol_error("replication control message decode failed: missing or mistyped field")
    })
}

fn read_u32_at(buf: &[u8], offset: usize) -> Result<u32> {
    let bytes = buf
        .get(offset..offset + 4)
        .ok_or_else(|| protocol_error("truncated length prefix in replication entries payload"))?;
    Ok(u32::from_le_bytes(bytes.try_into().expect("checked len 4")))
}

/// Encode an `Entries` response payload: `[u32 json_len][json][repeat:
/// u32 entry_len][entry_bytes]`.
///
/// Packs the longest PREFIX of `entries` that fits [`MAX_FRAME_SIZE`] and
/// builds the header (with the packed count) itself, rather than erroring
/// when the whole batch doesn't fit. Erroring closed the connection with no
/// structured response, and since neither side ever shrank the batch, an
/// embedding-heavy batch at the default 500 entries would reconnect-loop at
/// the same `from_lsn` forever (Issue #3355 review). Truncating to a dense
/// prefix is always safe: the client's applier accumulates partial frames
/// across polls and re-requests from the first unsent LSN. `MAX_FRAME_SIZE`
/// exceeds the WAL's own per-entry cap plus framing overhead, so any legal
/// single entry fits and progress is guaranteed (a batch whose FIRST entry
/// somehow exceeds it still errors -- that entry could never be shipped).
fn encode_entries_payload(
    primary_flushed_lsn: u64,
    primary_wallclock_micros: i64,
    entries: &[WalEntry],
) -> Result<Vec<u8>> {
    // The header JSON's length varies only with the digit counts of its
    // fields; 256 bytes is comfortably above its maximum, and reserving it
    // up front lets entries be packed against a fixed budget before the
    // real header (with the final count) is known.
    const HEADER_BUDGET: usize = 256;

    let mut packed = Vec::new();
    let mut entry_buf = Vec::new();
    let mut packed_count: u32 = 0;
    for entry in entries {
        entry_buf.clear();
        serialize_operation_into(entry.lsn, entry.timestamp, &entry.operation, &mut entry_buf)
            .map_err(|e| protocol_error(format!("failed to serialize WAL entry for wire: {e}")))?;
        let would_be = packed.len() + 4 + entry_buf.len() + HEADER_BUDGET;
        if would_be > MAX_FRAME_SIZE as usize {
            if packed_count == 0 {
                return Err(protocol_error(
                    "a single WAL entry exceeds MAX_FRAME_SIZE and can never be shipped",
                ));
            }
            break;
        }
        packed.extend_from_slice(&(entry_buf.len() as u32).to_le_bytes());
        packed.extend_from_slice(&entry_buf);
        packed_count += 1;
    }

    let header = EntriesHeaderMsg {
        primary_flushed_lsn,
        primary_wallclock_micros,
        count: packed_count,
    };
    let json = to_json(&header)?;
    let mut out = Vec::with_capacity(4 + json.len() + packed.len());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&packed);
    Ok(out)
}

/// Decode an `Entries` response payload produced by [`encode_entries_payload`].
fn decode_entries_payload(payload: &[u8]) -> Result<(EntriesHeaderMsg, Vec<WalEntry>)> {
    let mut offset = 0usize;
    let json_len = read_u32_at(payload, offset)? as usize;
    offset += 4;
    let json_bytes = payload
        .get(offset..offset + json_len)
        .ok_or_else(|| protocol_error("truncated Entries header in replication payload"))?;
    let header: EntriesHeaderMsg = from_json(json_bytes)?;
    offset += json_len;

    // Never pre-allocate from a peer-claimed count alone: a hostile header
    // claiming u32::MAX entries would reserve GBs before any bounds check.
    // Each entry occupies at least 4 payload bytes (its length prefix), so
    // the payload length itself bounds the plausible count.
    let plausible = payload.len().saturating_sub(offset) / 4;
    let mut entries = Vec::with_capacity((header.count as usize).min(plausible));
    for _ in 0..header.count {
        let entry_len = read_u32_at(payload, offset)? as usize;
        offset += 4;
        let entry_bytes = payload
            .get(offset..offset + entry_len)
            .ok_or_else(|| protocol_error("truncated WAL entry in replication payload"))?;
        let (entry, _consumed) = parse_entry_at(entry_bytes, 0, WIRE_WAL_VERSION)
            .map_err(|e| protocol_error(format!("failed to parse WAL entry from wire: {e}")))?;
        offset += entry_len;
        entries.push(entry);
    }
    Ok((header, entries))
}

/// SHA-256 digest of a replication auth token, for constant-time comparison
/// (mirrors `crate::auth::store::digest_key`). The plaintext token is never
/// retained beyond this call and never logged.
fn digest_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Which primary-side data source serves a connection's requests. See the
/// module docs ("Two feed backends") for why this exists.
enum FeedBackend {
    /// A full [`ReplicationFeed`] over a real `Arc<AletheiaDB>`: serves both
    /// `FetchEntries` and `FetchSnapshot`.
    Full(ReplicationFeed),
    /// Serves `FetchEntries` directly from the WAL (no `Arc<AletheiaDB>`
    /// needed); `FetchSnapshot` always replies [`MSG_SNAPSHOT_UNAVAILABLE`].
    EntriesOnly(Arc<ConcurrentWalSystem>),
}

impl FeedBackend {
    fn fetch_entries(&self, from_lsn: u64, max_entries: usize) -> Result<FetchOutcome> {
        match self {
            FeedBackend::Full(feed) => feed.fetch_entries(from_lsn, max_entries),
            FeedBackend::EntriesOnly(wal) => fetch_entries_from_wal(wal, from_lsn, max_entries),
        }
    }

    fn snapshot_to(&self, path: &Path) -> Result<Option<u64>> {
        match self {
            FeedBackend::Full(feed) => feed.snapshot_to(path).map(|s| Some(s.source_lsn)),
            FeedBackend::EntriesOnly(_) => Ok(None),
        }
    }
}

/// Handle to a running [`ReplicationServer`]'s background accept-loop thread.
///
/// Dropping it (or calling [`Self::shutdown`] explicitly) stops accepting new
/// connections, force-closes every currently-connected socket, and joins the
/// accept thread -- mirroring [`crate::storage::replication::applier::ReplicaApplierHandle`]'s
/// `Arc<AtomicBool>` + `JoinHandle` + `Drop` idiom.
pub struct ReplicationServerHandle {
    local_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    connections: Arc<Mutex<HashMap<u64, TcpStream>>>,
    accept_handle: Option<JoinHandle<()>>,
}

impl ReplicationServerHandle {
    /// The actual bound local address -- in particular the OS-assigned port
    /// when the server was started with a `:0` listen address (used by tests
    /// so they never depend on a fixed port).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop accepting new connections and force-close every currently
    /// connected socket. Idempotent; safe to call more than once (also
    /// called by `Drop`).
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        if let Ok(mut conns) = self.connections.lock() {
            for (_, stream) in conns.drain() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

impl Drop for ReplicationServerHandle {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(handle) = self.accept_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Primary-side TCP replication server: accepts [`TcpSource`] connections and
/// serves [`crate::storage::replication::feed::ReplicationFeed`] requests
/// over the wire protocol documented at the module level.
pub struct ReplicationServer;

impl ReplicationServer {
    /// Start serving `db`'s replication feed on `listen_addr` (e.g.
    /// `"127.0.0.1:0"` to let the OS assign a port -- see
    /// [`ReplicationServerHandle::local_addr`]), authenticating connecting
    /// replicas against `token`.
    ///
    /// Serves both `FetchEntries` and `FetchSnapshot` (bootstrap) requests,
    /// regardless of `db`'s current [`crate::db::NodeRole`] -- a promoted
    /// primary keeps serving. Note that CHAINED replication (a replica
    /// re-serving the stream to a downstream replica) is NOT supported in
    /// v1: the applier applies replicated entries without appending them to
    /// this node's own local WAL, so a listen server running on a replica
    /// has an empty feed to serve. A replica may run a listen server so it
    /// is ready to serve the moment it is PROMOTED (its own post-promotion
    /// writes do land in its WAL), not to fan out mid-chain.
    ///
    /// # Errors
    ///
    /// Returns an error if binding `listen_addr` fails (e.g. address already
    /// in use, or unparsable).
    pub fn start(
        db: Arc<AletheiaDB>,
        listen_addr: &str,
        token: String,
    ) -> Result<ReplicationServerHandle> {
        Self::start_with_backend(
            FeedBackend::Full(ReplicationFeed::new(db)),
            listen_addr,
            token,
        )
    }

    /// Crate-internal entry point for `AletheiaDB::with_unified_config`'s
    /// config-auto-wired `replication.listen_addr` (Issue #3355, Slice C):
    /// serves `FetchEntries` directly from `wal` (no `Arc<AletheiaDB>`
    /// needed -- see the module docs), and refuses `FetchSnapshot`.
    pub(crate) fn start_entries_only(
        wal: Arc<ConcurrentWalSystem>,
        listen_addr: &str,
        token: String,
    ) -> Result<ReplicationServerHandle> {
        Self::start_with_backend(FeedBackend::EntriesOnly(wal), listen_addr, token)
    }

    fn start_with_backend(
        backend: FeedBackend,
        listen_addr: &str,
        token: String,
    ) -> Result<ReplicationServerHandle> {
        let listener = TcpListener::bind(listen_addr)?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let connections: Arc<Mutex<HashMap<u64, TcpStream>>> = Arc::new(Mutex::new(HashMap::new()));
        let token_hash = digest_token(&token);
        drop(token); // never retain the plaintext token past hashing

        let backend = Arc::new(backend);
        let shutdown_for_thread = Arc::clone(&shutdown);
        let connections_for_thread = Arc::clone(&connections);

        let accept_handle = thread::spawn(move || {
            accept_loop(
                listener,
                backend,
                token_hash,
                shutdown_for_thread,
                connections_for_thread,
            );
        });

        Ok(ReplicationServerHandle {
            local_addr,
            shutdown,
            connections,
            accept_handle: Some(accept_handle),
        })
    }
}

fn accept_loop(
    listener: TcpListener,
    backend: Arc<FeedBackend>,
    token_hash: [u8; 32],
    shutdown: Arc<AtomicBool>,
    connections: Arc<Mutex<HashMap<u64, TcpStream>>>,
) {
    let next_conn_id = Arc::new(AtomicU64::new(1));

    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _peer_addr)) => {
                let too_many = connections
                    .lock()
                    .map(|c| c.len() >= MAX_CONCURRENT_CONNECTIONS)
                    .unwrap_or(true);
                if too_many {
                    // Refuse silently: drop the accepted socket with no
                    // response, per the module docs.
                    drop(stream);
                    continue;
                }

                let conn_id = next_conn_id.fetch_add(1, Ordering::Relaxed);
                let registered_stream = match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if let Ok(mut conns) = connections.lock() {
                    conns.insert(conn_id, registered_stream);
                } else {
                    continue;
                }

                let backend = Arc::clone(&backend);
                let shutdown_for_conn = Arc::clone(&shutdown);
                let connections_for_conn = Arc::clone(&connections);
                thread::spawn(move || {
                    handle_connection(stream, &backend, token_hash, &shutdown_for_conn);
                    if let Ok(mut conns) = connections_for_conn.lock() {
                        conns.remove(&conn_id);
                    }
                });
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                // Transient accept() error (e.g. resource limits): back off
                // briefly and keep the server alive rather than exiting the
                // thread on a single hiccup.
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

const SERVER_IO_TIMEOUT: Duration = Duration::from_secs(30);

fn handle_connection(
    mut stream: TcpStream,
    backend: &FeedBackend,
    token_hash: [u8; 32],
    shutdown: &AtomicBool,
) {
    // The listener is non-blocking (so the accept loop can poll shutdown).
    // On BSD/macOS an accepted socket INHERITS the listener's non-blocking
    // flag (Linux always returns blocking sockets), which would make every
    // read race the client's bytes and fail with WouldBlock. Force blocking
    // mode so the read/write timeouts below govern instead.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(SERVER_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SERVER_IO_TIMEOUT));
    let _ = stream.set_nodelay(true);

    if !perform_handshake(&mut stream, token_hash) {
        return;
    }

    while !shutdown.load(Ordering::Acquire) {
        let (msg_type, payload) = match read_frame(&mut stream) {
            Ok(v) => v,
            Err(_) => return,
        };

        match msg_type {
            MSG_FETCH_ENTRIES => {
                if !serve_fetch_entries(&mut stream, backend, &payload) {
                    return;
                }
            }
            MSG_FETCH_SNAPSHOT => {
                if !serve_fetch_snapshot(&mut stream, backend) {
                    return;
                }
            }
            _ => return, // unexpected message type: protocol error, close.
        }
    }
}

/// Returns `true` if the handshake succeeded and the caller should proceed
/// to serve requests; `false` if the connection should be closed.
fn perform_handshake(stream: &mut TcpStream, token_hash: [u8; 32]) -> bool {
    // Pre-auth: cap the allocation at the hello-sized limit.
    let (msg_type, payload) = match read_frame_capped(stream, MAX_HELLO_FRAME_SIZE) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if msg_type != MSG_HELLO {
        return false;
    }
    let hello: HelloMsg = match from_json(&payload) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let presented_hash = digest_token(&hello.token);
    if !bool::from(presented_hash.ct_eq(&token_hash)) {
        let _ = write_frame(stream, MSG_AUTH_FAILED, &[]);
        return false;
    }

    let ack = HelloAckMsg {
        protocol_version: PROTOCOL_VERSION,
    };
    match to_json(&ack) {
        Ok(json) => write_frame(stream, MSG_HELLO_ACK, &json).is_ok(),
        Err(_) => false,
    }
}

fn serve_fetch_entries(stream: &mut TcpStream, backend: &FeedBackend, payload: &[u8]) -> bool {
    let req: FetchEntriesMsg = match from_json(payload) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let max_entries = req.max_entries.min(MAX_SERVED_ENTRIES_PER_FETCH) as usize;

    match backend.fetch_entries(req.from_lsn, max_entries) {
        Ok(FetchOutcome::Entries {
            entries,
            primary_flushed_lsn,
            primary_wallclock_micros,
        }) => {
            match encode_entries_payload(primary_flushed_lsn, primary_wallclock_micros, &entries) {
                Ok(payload) => write_frame(stream, MSG_ENTRIES, &payload).is_ok(),
                Err(_) => false,
            }
        }
        Ok(FetchOutcome::ResyncRequired { min_available_lsn }) => {
            let msg = ResyncRequiredMsg { min_available_lsn };
            match to_json(&msg) {
                Ok(json) => write_frame(stream, MSG_RESYNC_REQUIRED, &json).is_ok(),
                Err(_) => false,
            }
        }
        Err(_) => false, // internal error reading the WAL: close; client retries.
    }
}

fn serve_fetch_snapshot(stream: &mut TcpStream, backend: &FeedBackend) -> bool {
    let tmp_path = std::env::temp_dir().join(format!(
        "aletheia_replication_snapshot_{}_{}.albk",
        std::process::id(),
        instant_nonce()
    ));

    let result = backend.snapshot_to(&tmp_path);
    let outcome = match result {
        Ok(Some(source_lsn)) => stream_snapshot_file(stream, &tmp_path, source_lsn),
        Ok(None) => {
            let msg = SnapshotUnavailableMsg {
                reason: "this replication connection serves FetchEntries only (config \
                          auto-wired listen server); use ReplicationServer::start with a real \
                          Arc<AletheiaDB> for snapshot bootstrap support"
                    .to_string(),
            };
            to_json(&msg)
                .map(|json| write_frame(stream, MSG_SNAPSHOT_UNAVAILABLE, &json).is_ok())
                .unwrap_or(false)
        }
        Err(_) => false,
    };

    let _ = std::fs::remove_file(&tmp_path);
    outcome
}

fn stream_snapshot_file(stream: &mut TcpStream, path: &Path, source_lsn: u64) -> bool {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return false,
    };
    let header = SnapshotHeaderMsg { len, source_lsn };
    let json = match to_json(&header) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if write_frame(stream, MSG_SNAPSHOT_HEADER, &json).is_err() {
        return false;
    }

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return true,
            Ok(n) => {
                if stream.write_all(&buf[..n]).is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
}

fn instant_nonce() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Client (replica-side ReplicationSource)
// ---------------------------------------------------------------------------

const CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Replica-side [`ReplicationSource`] over a TCP connection to a
/// [`ReplicationServer`].
///
/// Connects lazily (on the first `fetch_entries`/`fetch_snapshot` call) and
/// re-handshakes on every reconnect. Any I/O or protocol error drops the
/// current connection and returns `Err`; the caller (the applier's existing
/// backoff/`Connecting` state machine) retries, which transparently
/// reconnects and re-handshakes on the next call -- this type never retries
/// silently on its own.
pub struct TcpSource {
    addr: String,
    token: String,
    stream: Option<TcpStream>,
}

impl TcpSource {
    /// Construct a source that will connect to `addr` (`host:port`),
    /// authenticating with `token`. No I/O happens until the first fetch
    /// call.
    #[must_use]
    pub fn new(addr: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            token: token.into(),
            stream: None,
        }
    }

    fn ensure_connected(&mut self) -> Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }

        // Resolve via `ToSocketAddrs` so DNS hostnames (`primary:9099` in
        // Docker/K8s) work, not just literal `IP:port`. First resolved
        // address wins (standard client behavior).
        let socket_addr: SocketAddr = std::net::ToSocketAddrs::to_socket_addrs(&self.addr.as_str())
            .map_err(|e| {
                Error::Storage(StorageError::io_error(format!(
                    "invalid replication primary address '{}': {e}",
                    self.addr
                )))
            })?
            .next()
            .ok_or_else(|| {
                Error::Storage(StorageError::io_error(format!(
                    "replication primary address '{}' resolved to no addresses",
                    self.addr
                )))
            })?;
        let mut stream = TcpStream::connect_timeout(&socket_addr, CLIENT_CONNECT_TIMEOUT)?;
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT))?;
        stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT))?;

        let hello = HelloMsg {
            token: self.token.clone(),
            protocol_version: PROTOCOL_VERSION,
        };
        write_frame(&mut stream, MSG_HELLO, &to_json(&hello)?)?;

        let (msg_type, payload) = read_frame(&mut stream)?;
        match msg_type {
            MSG_HELLO_ACK => {
                let _ack: HelloAckMsg = from_json(&payload)?;
                self.stream = Some(stream);
                Ok(())
            }
            MSG_AUTH_FAILED => Err(Error::Storage(StorageError::io_error(
                "replication authentication failed: token mismatch with primary",
            ))),
            _ => Err(protocol_error(
                "unexpected message during replication handshake",
            )),
        }
    }

    /// Run `body` against the connected stream; on ANY error, drop the
    /// connection so the next call reconnects and re-handshakes from
    /// scratch.
    fn with_connection<T>(&mut self, body: impl FnOnce(&mut TcpStream) -> Result<T>) -> Result<T> {
        let result = self.ensure_connected().and_then(|()| {
            let stream = self.stream.as_mut().expect("ensure_connected succeeded");
            body(stream)
        });
        if result.is_err() {
            self.stream = None;
        }
        result
    }
}

impl ReplicationSource for TcpSource {
    fn fetch_entries(&mut self, from_lsn: u64, max_entries: usize) -> Result<FetchOutcome> {
        self.with_connection(|stream| {
            let req = FetchEntriesMsg {
                from_lsn,
                max_entries: max_entries.min(u32::MAX as usize) as u32,
            };
            write_frame(stream, MSG_FETCH_ENTRIES, &to_json(&req)?)?;

            let (msg_type, payload) = read_frame(stream)?;
            match msg_type {
                MSG_ENTRIES => {
                    let (header, entries) = decode_entries_payload(&payload)?;
                    Ok(FetchOutcome::Entries {
                        entries,
                        primary_flushed_lsn: header.primary_flushed_lsn,
                        primary_wallclock_micros: header.primary_wallclock_micros,
                    })
                }
                MSG_RESYNC_REQUIRED => {
                    let msg: ResyncRequiredMsg = from_json(&payload)?;
                    Ok(FetchOutcome::ResyncRequired {
                        min_available_lsn: msg.min_available_lsn,
                    })
                }
                _ => Err(protocol_error(
                    "unexpected message type in response to FetchEntries",
                )),
            }
        })
    }

    fn fetch_snapshot(&mut self, dest: &Path) -> Result<u64> {
        self.with_connection(|stream| {
            write_frame(stream, MSG_FETCH_SNAPSHOT, &[])?;

            let (msg_type, payload) = read_frame(stream)?;
            match msg_type {
                MSG_SNAPSHOT_HEADER => {
                    let header: SnapshotHeaderMsg = from_json(&payload)?;
                    let mut file = std::fs::File::create(dest)?;
                    let mut remaining = header.len;
                    let mut buf = vec![0u8; 1024 * 1024];
                    while remaining > 0 {
                        let want = remaining.min(buf.len() as u64) as usize;
                        stream.read_exact(&mut buf[..want])?;
                        file.write_all(&buf[..want])?;
                        remaining -= want as u64;
                    }
                    file.flush()?;
                    Ok(header.source_lsn)
                }
                MSG_SNAPSHOT_UNAVAILABLE => {
                    let msg: SnapshotUnavailableMsg = from_json(&payload)?;
                    Err(Error::Storage(StorageError::io_error(format!(
                        "primary cannot serve a replication snapshot: {}",
                        msg.reason
                    ))))
                }
                _ => Err(protocol_error(
                    "unexpected message type in response to FetchSnapshot",
                )),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::id::NodeId;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
    use crate::storage::wal::{WalEntry, WalOperation};

    fn big_entry(lsn: u64, payload_bytes: usize) -> WalEntry {
        let props = PropertyMapBuilder::new()
            .insert(
                "blob",
                PropertyValue::String("x".repeat(payload_bytes).into()),
            )
            .build();
        WalEntry {
            lsn: crate::storage::wal::LSN(lsn),
            timestamp: HybridTimestamp::new_unchecked(1, 0),
            operation: WalOperation::CreateNode {
                node_id: NodeId::new(lsn).unwrap(),
                label: GLOBAL_INTERNER.intern("TcpTest").unwrap(),
                properties: props,
                valid_from: HybridTimestamp::new_unchecked(1, 0),
                provenance: None,
            },
            checksum: 0,
            framed: true,
            segment_version: None,
        }
    }

    fn small_entry(lsn: u64) -> WalEntry {
        WalEntry {
            lsn: crate::storage::wal::LSN(lsn),
            timestamp: HybridTimestamp::new_unchecked(1, 0),
            operation: WalOperation::CreateNode {
                node_id: NodeId::new(lsn).unwrap(),
                label: GLOBAL_INTERNER.intern("TcpTest").unwrap(),
                properties: PropertyMap::new(),
                valid_from: HybridTimestamp::new_unchecked(1, 0),
                provenance: None,
            },
            checksum: 0,
            framed: true,
            segment_version: None,
        }
    }

    /// Review finding #6 (Issue #3355): a batch whose full encoding exceeds
    /// MAX_FRAME_SIZE must be truncated to the prefix that fits (>= 1 entry)
    /// and shipped, never errored -- erroring closed the connection and
    /// neither side ever shrank the batch, stalling replication forever.
    #[test]
    fn oversize_batch_is_truncated_not_errored() {
        // Three ~28 MiB entries: any two fit a 64 MiB + headroom frame, all
        // three do not.
        let entries = vec![
            big_entry(1, 28 * 1024 * 1024),
            big_entry(2, 28 * 1024 * 1024),
            big_entry(3, 28 * 1024 * 1024),
        ];
        let payload = encode_entries_payload(3, 0, &entries).expect("must not error");
        assert!(payload.len() <= MAX_FRAME_SIZE as usize);

        let (header, decoded) = decode_entries_payload(&payload).expect("decode");
        assert_eq!(header.count as usize, decoded.len());
        assert!(
            !decoded.is_empty() && decoded.len() < 3,
            "expected a truncated non-empty prefix, got {} of 3",
            decoded.len()
        );
        // Dense prefix from the first entry -- never a mid-batch selection.
        assert_eq!(decoded[0].lsn.0, 1);
        for (i, e) in decoded.iter().enumerate() {
            assert_eq!(e.lsn.0, 1 + i as u64);
        }
    }

    /// A small batch round-trips exactly (count preserved, all entries).
    #[test]
    fn small_batch_roundtrips_completely() {
        let entries = vec![small_entry(7), small_entry(8), small_entry(9)];
        let payload = encode_entries_payload(9, 123, &entries).expect("encode");
        let (header, decoded) = decode_entries_payload(&payload).expect("decode");
        assert_eq!(header.count, 3);
        assert_eq!(header.primary_flushed_lsn, 9);
        assert_eq!(header.primary_wallclock_micros, 123);
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[2].lsn.0, 9);
    }

    /// Any single legal WAL entry (up to MAX_WAL_ENTRY_SIZE) must fit one
    /// frame: MAX_FRAME_SIZE carries headroom for the framing overhead.
    #[test]
    fn single_near_max_entry_ships() {
        // ~63.9 MiB payload: within the WAL's own 64 MiB entry cap.
        let entries = vec![big_entry(1, 63 * 1024 * 1024 + 900 * 1024)];
        let payload = encode_entries_payload(1, 0, &entries).expect("must fit");
        assert!(payload.len() <= MAX_FRAME_SIZE as usize);
        let (header, decoded) = decode_entries_payload(&payload).expect("decode");
        assert_eq!(header.count, 1);
        assert_eq!(decoded.len(), 1);
    }
}
