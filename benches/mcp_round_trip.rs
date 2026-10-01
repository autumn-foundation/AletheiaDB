//! MCP round-trip latency benchmark harness (Issue #3361).
//!
//! This is a **black-box** latency harness for the AletheiaDB MCP server. It
//! spawns the *shipped* `aletheia-mcp` binary and drives it over its real
//! stdio transport (newline-delimited JSON-RPC, rmcp 1.7), measuring the
//! **end-to-end round-trip** latency of every registered tool: request bytes
//! serialized by the client -> written to the child's stdin -> dispatched by
//! the server against a seeded fixture -> response line read back -> parsed.
//! It touches **no** product code (nothing under `src/`); the design is
//! deliberately black-box so it exercises exactly what an LLM client hits.
//!
//! It reports per-scenario `p50/p95/p99/max` (nearest-rank), an **MCP-overhead
//! sub-metric** (round-trip minus the in-process direct-call floor), enforces
//! **registry completeness** (every advertised tool must have a scenario), and
//! gates representative scenarios two ways: an **absolute** `p99 < 5ms` ceiling
//! (catches gross blowups) plus a **relative** committed-baseline regression
//! gate (`current p50 > 2x baseline p50` fails — median is stable run-to-run,
//! whereas p99 drifts ±10%+ and would false-fail). See
//! `docs/guides/mcp-latency-benchmarks.md` for the published targets, fixture
//! shape, hardware baseline, and the JSON artifact schema.
//!
//! Because it is `harness = false` this is a plain `fn main()` timing loop
//! (Criterion cannot emit p95/p99/max). It requires the `mcp-server` feature so
//! `CARGO_BIN_EXE_aletheia-mcp` is set and `AletheiaMcpServer` is available for
//! the direct-call floor.
//!
//! ## Environment knobs
//! - `MCP_BENCH_SAMPLE_SIZE` / `BENCH_SAMPLE_SIZE`: measured calls per scenario (default 200)
//! - `MCP_BENCH_WARMUP` / `BENCH_WARMUP_TIME`: warm-up calls per scenario (default 20)
//! - `MCP_BENCH_SCALE`: `smoke` (default) or `nightly` (fixture scale)
//! - `MCP_BENCH_ENFORCE_LATENCY=1`: hard-fail (exit 3) if a gated scenario's p99 >= 5ms
//! - `MCP_BENCH_BASELINE=<path>`: committed reference baseline JSON of gated
//!   median (p50) latencies; enables the **relative** regression gate.
//! - `MCP_BENCH_ENFORCE_RELATIVE=1`: hard-fail (exit 4) if a gated scenario's
//!   current p50 exceeds `MCP_BENCH_REGRESSION_MULTIPLIER` x its baseline p50.
//! - `MCP_BENCH_REGRESSION_MULTIPLIER=<f>`: relative-gate threshold (default 2.0).
//! - `MCP_BENCH_WRITE_BASELINE=<path>`: after the run, write the gated
//!   scenarios' p50 to this path in the reference-baseline schema (regenerator).
//! - `MCP_BENCH_JSON=<path>`: write the machine-readable artifact here
//! - `MCP_BENCH_INJECT_LATENCY_MS=<f>`: SENSITIVITY PROOF — add this synthetic
//!   per-call latency (ms) to the injected gated scenario, so a regression can
//!   be demonstrated to trip both the absolute and the relative gate.
//! - `MCP_BENCH_INJECT_SCENARIO=<name>`: which scenario to inject into
//!   (default `gate_read__get_node`).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::mcp::AletheiaMcpServer;
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, AletheiaDBConfig, PropertyMapBuilder};
use serde_json::{Value, json};

/// Build the shared low-latency durability profile used for the fixture, the
/// spawned server (via a generated TOML config), and the direct-call floor DB.
///
/// The benchmark deliberately measures under **`Async`** durability so the
/// reported latencies isolate MCP round-trip + dispatch cost from the
/// operator's fsync/durability choice. A `GroupCommit`/`Synchronous` deployment
/// adds a fixed per-commit fsync/batch-wait latency (~2-10ms) that is a
/// durability tradeoff, **not** MCP overhead (it is paid equally by the
/// in-process direct-call floor, so it cancels out of the overhead sub-metric).
///
/// `MCP_BENCH_DURABILITY=group_commit` overrides this to the default durable
/// `GroupCommit { max_delay_ms: 10 }` profile, used ONLY to capture the
/// **observational** (ungated) durable-write round-trip number documented
/// alongside the Async-profile gated numbers — it is durability-bound, not an
/// MCP-layer cost.
fn bench_config(dir: &std::path::Path) -> AletheiaDBConfig {
    let durability = match std::env::var("MCP_BENCH_DURABILITY")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "group_commit" | "groupcommit" => DurabilityMode::GroupCommit {
            max_delay_ms: 10,
            max_batch_size: 200,
        },
        _ => DurabilityMode::Async {
            flush_interval_ms: 50,
        },
    };
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(dir.join("wal"))
                .durability_mode(durability)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: dir.join("indexes"),
            load_on_startup: true,
            ..Default::default()
        })
        .build()
}

// ============================================================================
// Configuration
// ============================================================================

/// Runtime configuration, resolved from environment variables.
struct Config {
    sample_size: usize,
    warmup: usize,
    scale_nightly: bool,
    enforce_latency: bool,
    enforce_relative: bool,
    regression_multiplier: f64,
    baseline_path: Option<String>,
    write_baseline_path: Option<String>,
    json_path: Option<String>,
    inject_latency_ms: f64,
    inject_scenario: String,
}

impl Config {
    fn from_env() -> Self {
        fn parse<T: std::str::FromStr>(primary: &str, fallback: &str, default: T) -> T {
            std::env::var(primary)
                .or_else(|_| std::env::var(fallback))
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default)
        }
        let scale = std::env::var("MCP_BENCH_SCALE").unwrap_or_default();
        Config {
            sample_size: parse("MCP_BENCH_SAMPLE_SIZE", "BENCH_SAMPLE_SIZE", 200usize).max(1),
            warmup: parse("MCP_BENCH_WARMUP", "BENCH_WARMUP_TIME", 20usize),
            scale_nightly: scale.eq_ignore_ascii_case("nightly"),
            enforce_latency: std::env::var("MCP_BENCH_ENFORCE_LATENCY")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            enforce_relative: std::env::var("MCP_BENCH_ENFORCE_RELATIVE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            regression_multiplier: parse(
                "MCP_BENCH_REGRESSION_MULTIPLIER",
                "MCP_BENCH_REGRESSION_MULTIPLIER",
                2.0f64,
            )
            .max(1.0),
            baseline_path: std::env::var("MCP_BENCH_BASELINE")
                .ok()
                .filter(|s| !s.is_empty()),
            write_baseline_path: std::env::var("MCP_BENCH_WRITE_BASELINE")
                .ok()
                .filter(|s| !s.is_empty()),
            json_path: std::env::var("MCP_BENCH_JSON").ok(),
            inject_latency_ms: std::env::var("MCP_BENCH_INJECT_LATENCY_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0),
            inject_scenario: std::env::var("MCP_BENCH_INJECT_SCENARIO")
                .unwrap_or_else(|_| "gate_read__get_node".to_string()),
        }
    }
}

/// Published latency gate: representative scenarios must keep p99 under this.
const P99_GATE_MS: f64 = 5.0;
/// Published MCP-overhead budget for single-entity reads.
const OVERHEAD_BUDGET_MS: f64 = 1.0;

// ============================================================================
// Percentile statistics (nearest-rank over sorted per-call nanos)
// ============================================================================

#[derive(Clone)]
struct Stats {
    n: usize,
    min_ns: u64,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    max_ns: u64,
    mean_ns: u64,
}

impl Stats {
    fn from_samples(mut samples: Vec<u64>) -> Stats {
        samples.sort_unstable();
        let n = samples.len();
        let sum: u128 = samples.iter().map(|&x| x as u128).sum();
        // Nearest-rank percentile: rank = ceil(p/100 * n), 1-indexed.
        let nr = |p: f64| -> u64 {
            if n == 0 {
                return 0;
            }
            let rank = (p / 100.0 * n as f64).ceil() as usize;
            let idx = rank.clamp(1, n) - 1;
            samples[idx]
        };
        Stats {
            n,
            min_ns: *samples.first().unwrap_or(&0),
            p50_ns: nr(50.0),
            p95_ns: nr(95.0),
            p99_ns: nr(99.0),
            max_ns: *samples.last().unwrap_or(&0),
            mean_ns: if n == 0 { 0 } else { (sum / n as u128) as u64 },
        }
    }
    fn p99_ms(&self) -> f64 {
        self.p99_ns as f64 / 1e6
    }
    fn to_json_us(&self) -> Value {
        let us = |ns: u64| (ns as f64) / 1000.0;
        json!({
            "min_us": us(self.min_ns),
            "p50_us": us(self.p50_ns),
            "p95_us": us(self.p95_ns),
            "p99_us": us(self.p99_ns),
            "max_us": us(self.max_ns),
            "mean_us": us(self.mean_ns),
            "samples": self.n,
        })
    }
}

// ============================================================================
// JSON-RPC stdio client for the spawned aletheia-mcp binary
// ============================================================================

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn spawn(bin: &str, config_path: &std::path::Path) -> McpClient {
        let mut child = Command::new(bin)
            .env("ALETHEIADB_AUTH_MODE", "anonymous")
            .env("ALETHEIADB_CONFIG", config_path)
            .env_remove("ALETHEIADB_DATA_DIR")
            .env_remove("ALETHEIADB_MCP_API_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn aletheia-mcp binary");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        McpClient {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn write_line(&mut self, v: &Value) {
        let mut s = serde_json::to_string(v).expect("serialize request");
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).expect("write to child");
        self.stdin.flush().expect("flush child stdin");
    }

    /// Read JSON-RPC response lines until one carries the given id.
    fn read_for_id(&mut self, id: u64) -> Value {
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("read child stdout");
            if n == 0 {
                panic!("aletheia-mcp closed stdout before responding to id {id}");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue, // ignore any non-JSON diagnostic line
            };
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                return v;
            }
        }
    }

    /// Perform the MCP handshake (`initialize` + `notifications/initialized`).
    fn handshake(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "mcp-round-trip-bench", "version": "1.0.0"}
            }
        }));
        let resp = self.read_for_id(id);
        assert!(resp.get("result").is_some(), "initialize failed: {resp}");
        self.write_line(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    /// Enumerate the server's live tool registry via `tools/list`.
    fn list_tools(&mut self) -> Vec<String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_line(&json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":{}}));
        let resp = self.read_for_id(id);
        resp.get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Issue one `tools/call`, returning the raw response Value.
    fn call_tool(&mut self, name: &str, args: &Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.write_line(&json!({
            "jsonrpc":"2.0","id":id,"method":"tools/call",
            "params": {"name": name, "arguments": args}
        }));
        self.read_for_id(id)
    }

    /// Extract the tool's inner JSON text (`result.content[0].text`), if present.
    fn tool_text(resp: &Value) -> Option<String> {
        resp.get("result")?
            .get("content")?
            .as_array()?
            .first()?
            .get("text")?
            .as_str()
            .map(String::from)
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ============================================================================
// Fixture (seeded deterministically; AC1)
// ============================================================================

/// Identifiers and coordinates captured from the seeded fixture, used to build
/// scenario arguments.
#[derive(Clone)]
struct Fixture {
    nodes: usize,
    edges: usize,
    embedding_dim: usize,
    // stable read/traversal targets
    person_id: u64,
    person_id2: u64,
    hub_id: u64,
    edge_id: u64,
    // dedicated mutable targets (idempotent-repeatable writes)
    update_node_id: u64,
    update_edge_id: u64,
    retract_node_id: u64,
    retract_edge_id: u64,
    // history targets (size-sensitive get_node_history)
    history_short_node: u64,
    history_long_node: u64,
    history_edge: u64,
    // disposable pools (consumed one id per iteration by delete scenarios)
    del_nodes: Vec<u64>,
    del_edges: Vec<u64>,
    cascade_nodes: Vec<u64>,
    // temporal coordinates (RFC3339)
    seed_start: String,
    now: String,
    // query embedding
    query_embedding: Vec<f32>,
    // discovered at runtime (prelude)
    node_from_version: u64,
    node_to_version: u64,
    edge_from_version: u64,
    edge_to_version: u64,
    person_version: u64,
}

fn now_rfc3339() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    micros_to_rfc3339(d.as_micros() as i64)
}

/// Minimal micros-since-epoch -> RFC3339 (UTC). Avoids a chrono dependency.
fn micros_to_rfc3339(micros: i64) -> String {
    let secs = micros.div_euclid(1_000_000);
    let usec = micros.rem_euclid(1_000_000);
    // days since epoch
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // civil date from days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}.{usec:06}Z")
}

/// Seed a durable fixture at `dir`. Runs the identical deterministic sequence
/// on both the binary's data dir and the direct-floor's data dir, so entity ids
/// and version ids line up between the two.
fn seed_fixture(dir: &std::path::Path, cfg: &Config) -> Fixture {
    // Disposable-delete pool sizing (Issue #3361 review FIX 5): the round-trip
    // consumes ids `warmup + i` for `i in 0..sample_size`, while the direct-call
    // floor (a SEPARATE db copy) consumes `warmup + sample_size + i` for
    // `i in 0..sample_size`. So the highest index touched is
    // `warmup + 2*sample_size - 1`; size the pool to cover that (plus margin) so
    // no delete scenario wraps past the pool end and re-deletes an already-gone
    // id (which would skew the direct floor toward NOT_FOUND fast paths).
    let del_pool = cfg.warmup + 2 * cfg.sample_size + 200;
    let (n_person, n_edge, hist_long) = if cfg.scale_nightly {
        (10_000usize, 50_000usize, 20usize)
    } else {
        (500usize, 2_000usize, 12usize)
    };
    let embedding_dim = 16usize;
    let seed_start = now_rfc3339();

    let db = AletheiaDB::with_unified_config(bench_config(dir)).expect("open fixture db");

    db.vector_index("embedding")
        .hnsw(HnswConfig::new(embedding_dim, DistanceMetric::Cosine))
        .enable()
        .expect("enable vector index");

    let mk_vec = |seed: usize| -> Vec<f32> {
        (0..embedding_dim)
            .map(|j| ((seed * 31 + j * 7) % 97) as f32 / 97.0)
            .collect()
    };

    // Core Person nodes (deterministic).
    let mut person_ids = Vec::with_capacity(n_person);
    for i in 0..n_person {
        let v = mk_vec(i);
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Person {i}"))
            .insert("age", (20 + (i % 60)) as i64)
            .insert("city", if i % 2 == 0 { "London" } else { "Berlin" })
            .insert_vector("embedding", &v)
            .build();
        person_ids.push(
            db.create_node("Person", props)
                .expect("create person")
                .as_u64(),
        );
    }
    let person_id = person_ids[0];
    let person_id2 = person_ids[1];
    // Hub node with many outgoing edges (index 2).
    let hub_id = person_ids[2];

    // KNOWS edges. Wire the hub to many targets, then fill the rest randomly.
    let mut first_edge_id = None;
    let mut created_edges = 0usize;
    let hub_fanout = (n_edge / 10).min(n_person - 3);
    let hub_nid = aletheiadb::NodeId::new(hub_id).unwrap();
    for t in 0..hub_fanout {
        let target = aletheiadb::NodeId::new(person_ids[3 + t]).unwrap();
        let e = db
            .create_edge(
                hub_nid,
                target,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .expect("create hub edge");
        first_edge_id.get_or_insert(e.as_u64());
        created_edges += 1;
    }
    let mut lcg = 0x1234_5678u64;
    while created_edges < n_edge {
        lcg = lcg
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let s = (lcg >> 33) as usize % n_person;
        lcg = lcg
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let t = (lcg >> 33) as usize % n_person;
        if s == t {
            continue;
        }
        let sn = aletheiadb::NodeId::new(person_ids[s]).unwrap();
        let tn = aletheiadb::NodeId::new(person_ids[t]).unwrap();
        let e = db
            .create_edge(
                sn,
                tn,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2021i64).build(),
            )
            .expect("create edge");
        first_edge_id.get_or_insert(e.as_u64());
        created_edges += 1;
    }
    let edge_id = first_edge_id.unwrap();

    // Dedicated mutable targets (edgeless so retract/update never trip DETACH).
    let update_node_id = db
        .create_node(
            "Mutable",
            PropertyMapBuilder::new().insert("counter", 0i64).build(),
        )
        .expect("update node")
        .as_u64();
    let retract_node_id = db
        .create_node(
            "Retractable",
            PropertyMapBuilder::new().insert("state", "open").build(),
        )
        .expect("retract node")
        .as_u64();
    // A standalone edge between two fresh nodes for update/retract-edge targets.
    let a = db
        .create_node("EndA", PropertyMapBuilder::new().build())
        .unwrap();
    let b = db
        .create_node("EndB", PropertyMapBuilder::new().build())
        .unwrap();
    let update_edge_id = db
        .create_edge(
            a,
            b,
            "LINK",
            PropertyMapBuilder::new().insert("counter", 0i64).build(),
        )
        .expect("update edge")
        .as_u64();
    let c = db
        .create_node("EndC", PropertyMapBuilder::new().build())
        .unwrap();
    let d = db
        .create_node("EndD", PropertyMapBuilder::new().build())
        .unwrap();
    let retract_edge_id = db
        .create_edge(
            c,
            d,
            "LINK",
            PropertyMapBuilder::new().insert("state", "open").build(),
        )
        .expect("retract edge")
        .as_u64();

    // History targets: short (2 versions) and long (hist_long versions).
    let history_short_node = db
        .create_node(
            "Historic",
            PropertyMapBuilder::new().insert("rev", 0i64).build(),
        )
        .unwrap()
        .as_u64();
    let hs_nid = aletheiadb::NodeId::new(history_short_node).unwrap();
    db.update_node_with_valid_time(
        hs_nid,
        PropertyMapBuilder::new().insert("rev", 1i64).build(),
        None,
    )
    .expect("hist short update");

    let history_long_node = db
        .create_node(
            "Historic",
            PropertyMapBuilder::new().insert("rev", 0i64).build(),
        )
        .unwrap()
        .as_u64();
    let hl_nid = aletheiadb::NodeId::new(history_long_node).unwrap();
    for r in 1..hist_long {
        db.update_node_with_valid_time(
            hl_nid,
            PropertyMapBuilder::new().insert("rev", r as i64).build(),
            None,
        )
        .expect("hist long update");
    }

    // History edge (several versions) for get_edge_history / diff_edge_versions.
    let he = db
        .create_edge(
            a,
            b,
            "HISTED",
            PropertyMapBuilder::new().insert("rev", 0i64).build(),
        )
        .unwrap();
    let history_edge = he.as_u64();
    for r in 1..5 {
        db.update_edge_with_valid_time(
            he,
            PropertyMapBuilder::new().insert("rev", r as i64).build(),
            None,
        )
        .expect("hist edge update");
    }

    // Disposable pools for delete scenarios.
    let mut del_nodes = Vec::with_capacity(del_pool);
    for i in 0..del_pool {
        del_nodes.push(
            db.create_node(
                "Ephemeral",
                PropertyMapBuilder::new().insert("i", i as i64).build(),
            )
            .unwrap()
            .as_u64(),
        );
    }
    let mut del_edges = Vec::with_capacity(del_pool);
    for _ in 0..del_pool {
        del_edges.push(
            db.create_edge(a, b, "EPH", PropertyMapBuilder::new().build())
                .unwrap()
                .as_u64(),
        );
    }
    let mut cascade_nodes = Vec::with_capacity(del_pool);
    for i in 0..del_pool {
        cascade_nodes.push(
            db.create_node(
                "Cascade",
                PropertyMapBuilder::new().insert("i", i as i64).build(),
            )
            .unwrap()
            .as_u64(),
        );
    }

    // Ensure durability before we drop and hand the dir to the binary.
    drop(db);

    Fixture {
        nodes: n_person,
        edges: n_edge,
        embedding_dim,
        person_id,
        person_id2,
        hub_id,
        edge_id,
        update_node_id,
        update_edge_id,
        retract_node_id,
        retract_edge_id,
        history_short_node,
        history_long_node,
        history_edge,
        del_nodes,
        del_edges,
        cascade_nodes,
        seed_start,
        now: now_rfc3339(),
        query_embedding: mk_vec(0),
        node_from_version: 0,
        node_to_version: 0,
        edge_from_version: 0,
        edge_to_version: 0,
        person_version: 0,
    }
}

// ============================================================================
// Scenario table
// ============================================================================

type ArgFn = Box<dyn Fn(usize) -> Value>;

struct Scenario {
    name: &'static str,
    tool: &'static str,
    category: &'static str, // read|write|temporal|vector|query|traversal|provenance|lineage|admin
    size_class: &'static str, // typical|small|medium|large
    gated: bool,
    args: ArgFn,
}

/// Build the full scenario table (>= 1 scenario per registered tool; AC3).
fn build_scenarios(fx: &Fixture) -> Vec<Scenario> {
    let f = fx.clone();
    macro_rules! s {
        ($name:expr, $tool:expr, $cat:expr, $sz:expr, $gated:expr, $args:expr) => {
            Scenario {
                name: $name,
                tool: $tool,
                category: $cat,
                size_class: $sz,
                gated: $gated,
                args: Box::new($args),
            }
        };
    }
    let emb: Vec<Value> = f.query_embedding.iter().map(|x| json!(x)).collect();
    let emb2 = emb.clone();
    let emb3 = emb.clone();

    vec![
        // ---- Gated representative set (read / write / temporal / vector / query) ----
        s!(
            "gate_read__get_node",
            "get_node",
            "read",
            "typical",
            true,
            {
                let id = f.person_id;
                move |_| json!({"node_id": id})
            }
        ),
        s!(
            "gate_write__create_node",
            "create_node",
            "write",
            "typical",
            true,
            {
                move |i| json!({"label": "BenchTmp", "properties": {"name": format!("n{i}"), "idx": i as i64}})
            }
        ),
        s!(
            "gate_temporal__get_node_at_time",
            "get_node_at_time",
            "temporal",
            "typical",
            true,
            {
                let id = f.person_id;
                let vt = f.now.clone();
                move |_| json!({"node_id": id, "valid_time": vt})
            }
        ),
        s!(
            "gate_vector__find_similar",
            "find_similar",
            "vector",
            "typical",
            true,
            {
                let e = emb.clone();
                move |_| json!({"property_name": "embedding", "embedding": e, "k": 10})
            }
        ),
        s!("gate_query__aql", "query", "query", "typical", true, {
            move |_| json!({"language": "aql", "query": "MATCH (n:Person) RETURN n LIMIT 10"})
        }),
        // ---- Reads ----
        s!("read__get_edge", "get_edge", "read", "typical", false, {
            let id = f.edge_id;
            move |_| json!({"edge_id": id})
        }),
        s!(
            "read__count_nodes",
            "count_nodes",
            "read",
            "typical",
            false,
            |_| json!({})
        ),
        s!(
            "read__count_edges",
            "count_edges",
            "read",
            "typical",
            false,
            |_| json!({})
        ),
        s!(
            "read__list_edges",
            "list_edges",
            "read",
            "typical",
            false,
            |_| json!({"limit": 100})
        ),
        s!(
            "read__get_outgoing_edges",
            "get_outgoing_edges",
            "read",
            "typical",
            false,
            {
                let id = f.hub_id;
                move |_| json!({"node_id": id})
            }
        ),
        s!(
            "read__get_incoming_edges",
            "get_incoming_edges",
            "read",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"node_id": id})
            }
        ),
        s!(
            "read__get_schema",
            "get_schema",
            "read",
            "typical",
            false,
            |_| json!({})
        ),
        s!(
            "read__database_stats",
            "database_stats",
            "read",
            "typical",
            false,
            |_| json!({})
        ),
        s!(
            "read__list_vector_indexes",
            "list_vector_indexes",
            "read",
            "typical",
            false,
            |_| json!({})
        ),
        s!(
            "read__list_unique_constraints",
            "list_unique_constraints",
            "read",
            "typical",
            false,
            |_| json!({})
        ),
        // list_nodes size sweep (page size: AC3)
        s!(
            "read__list_nodes_small",
            "list_nodes",
            "read",
            "small",
            false,
            |_| json!({"label": "Person", "limit": 10})
        ),
        s!(
            "read__list_nodes_medium",
            "list_nodes",
            "read",
            "medium",
            false,
            |_| json!({"label": "Person", "limit": 100})
        ),
        s!(
            "read__list_nodes_large",
            "list_nodes",
            "read",
            "large",
            false,
            |_| json!({"label": "Person", "limit": 1000})
        ),
        // get_node_history length sweep (AC3)
        s!(
            "read__get_node_history_short",
            "get_node_history",
            "read",
            "small",
            false,
            {
                let id = f.history_short_node;
                move |_| json!({"node_id": id})
            }
        ),
        s!(
            "read__get_node_history_long",
            "get_node_history",
            "read",
            "large",
            false,
            {
                let id = f.history_long_node;
                move |_| json!({"node_id": id})
            }
        ),
        s!(
            "read__get_edge_history",
            "get_edge_history",
            "read",
            "typical",
            false,
            {
                let id = f.history_edge;
                move |_| json!({"edge_id": id})
            }
        ),
        // Belief-revision audit (Issue #3362). The bench build lacks the
        // `semantic-temporal` feature, so this returns a tolerated
        // FAILED_PRECONDITION (isError) — present only to satisfy the runtime
        // registry-completeness assertion, like `semantic__semantic_path`.
        s!(
            "temporal__get_belief_revisions",
            "get_belief_revisions",
            "temporal",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"entity_kind": "node", "id": id})
            }
        ),
        // Deferred MCP-registry batch tools (Issue #3367 / #3352 / #3357 /
        // #3382). The bench build lacks the `semantic-temporal` /
        // `semantic-reasoning` features, so each returns a tolerated
        // FAILED_PRECONDITION (isError) — present only to satisfy the runtime
        // registry-completeness assertion (AC2), like `get_belief_revisions`.
        s!(
            "temporal__create_drift_monitor",
            "create_drift_monitor",
            "temporal",
            "typical",
            false,
            move |_| json!({
                "property_key": "embedding",
                "metric": "cosine",
                "threshold": 0.5,
                "window_micros": 3_600_000_000_u64
            })
        ),
        s!(
            "temporal__list_drift_monitors",
            "list_drift_monitors",
            "temporal",
            "typical",
            false,
            move |_| json!({})
        ),
        s!(
            "temporal__delete_drift_monitor",
            "delete_drift_monitor",
            "temporal",
            "typical",
            false,
            move |_| json!({"id": 1})
        ),
        s!(
            "temporal__query_drift_alarms",
            "query_drift_alarms",
            "temporal",
            "typical",
            false,
            move |_| json!({})
        ),
        s!(
            "temporal__resolve_drift_alarm",
            "resolve_drift_alarm",
            "temporal",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"alarm_id": id})
            }
        ),
        s!(
            "temporal__contradiction_genealogy",
            "contradiction_genealogy",
            "temporal",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"entity_kind": "node", "id": id, "property": "name"})
            }
        ),
        s!(
            "temporal__find_contradictions",
            "find_contradictions",
            "temporal",
            "typical",
            false,
            move |_| json!({})
        ),
        s!(
            "temporal__counterfactual_replay",
            "counterfactual_replay",
            "temporal",
            "typical",
            false,
            move |_| json!({"name": "cf-bench", "exclude_source": "poisoned-feed"})
        ),
        s!(
            "reasoning__trust_breakdown",
            "trust_breakdown",
            "reasoning",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"entity_kind": "node", "id": id, "version": 1})
            }
        ),
        s!(
            "reasoning__list_trust_policies",
            "list_trust_policies",
            "reasoning",
            "typical",
            false,
            move |_| json!({})
        ),
        // ---- Traversal depth sweep (AC3) ----
        s!(
            "traverse__depth1",
            "traverse",
            "traversal",
            "small",
            false,
            {
                let id = f.hub_id;
                move |_| json!({"start_node_id": id, "edge_label": "KNOWS", "depth": 1, "limit": 50})
            }
        ),
        s!(
            "traverse__depth2",
            "traverse",
            "traversal",
            "medium",
            false,
            {
                let id = f.hub_id;
                move |_| json!({"start_node_id": id, "edge_label": "KNOWS", "depth": 2, "limit": 50})
            }
        ),
        s!(
            "traverse__depth3",
            "traverse",
            "traversal",
            "large",
            false,
            {
                let id = f.hub_id;
                move |_| json!({"start_node_id": id, "edge_label": "KNOWS", "depth": 3, "limit": 50})
            }
        ),
        // ---- Vector k sweep (AC3) ----
        s!(
            "vector__find_similar_k1",
            "find_similar",
            "vector",
            "small",
            false,
            {
                let e = emb2.clone();
                move |_| json!({"property_name": "embedding", "embedding": e, "k": 1})
            }
        ),
        s!(
            "vector__find_similar_k50",
            "find_similar",
            "vector",
            "large",
            false,
            {
                let e = emb3.clone();
                move |_| json!({"property_name": "embedding", "embedding": e, "k": 50})
            }
        ),
        s!(
            "vector__hybrid_query",
            "hybrid_query",
            "vector",
            "typical",
            false,
            {
                let e: Vec<Value> = f.query_embedding.iter().map(|x| json!(x)).collect();
                move |_| json!({"vector_property": "embedding", "query_embedding": e, "top_k": 10, "filter_label": "Person", "limit": 10})
            }
        ),
        // ---- Query complexity sweep (AC3), all AQL (cypher may be off) ----
        s!(
            "query__aql_simple",
            "query",
            "query",
            "small",
            false,
            |_| json!({"language": "aql", "query": "MATCH (n:Person) RETURN n LIMIT 5"})
        ),
        s!(
            "query__aql_filtered",
            "query",
            "query",
            "medium",
            false,
            |_| json!({"language": "aql", "query": "MATCH (n:Person) WHERE n.age > 40 RETURN n LIMIT 25"})
        ),
        s!(
            "query__aql_traversal",
            "query",
            "query",
            "large",
            false,
            |_| json!({"language": "aql", "query": "MATCH (n:Person)-[:KNOWS]->(m:Person) RETURN m LIMIT 50"})
        ),
        // ---- Temporal ----
        s!(
            "temporal__get_edge_at_time",
            "get_edge_at_time",
            "temporal",
            "typical",
            false,
            {
                let id = f.edge_id;
                let vt = f.now.clone();
                move |_| json!({"edge_id": id, "valid_time": vt})
            }
        ),
        s!(
            "temporal__get_node_at_valid_time",
            "get_node_at_valid_time",
            "temporal",
            "typical",
            false,
            {
                let id = f.person_id;
                let vt = f.now.clone();
                move |_| json!({"node_id": id, "valid_time": vt})
            }
        ),
        s!(
            "temporal__get_node_at_transaction_time",
            "get_node_at_transaction_time",
            "temporal",
            "typical",
            false,
            {
                let id = f.person_id;
                let tt = f.now.clone();
                move |_| json!({"node_id": id, "transaction_time": tt})
            }
        ),
        s!(
            "temporal__get_edge_at_valid_time",
            "get_edge_at_valid_time",
            "temporal",
            "typical",
            false,
            {
                let id = f.edge_id;
                let vt = f.now.clone();
                move |_| json!({"edge_id": id, "valid_time": vt})
            }
        ),
        s!(
            "temporal__get_edge_at_transaction_time",
            "get_edge_at_transaction_time",
            "temporal",
            "typical",
            false,
            {
                let id = f.edge_id;
                let tt = f.now.clone();
                move |_| json!({"edge_id": id, "transaction_time": tt})
            }
        ),
        s!(
            "temporal__find_nodes_at_time",
            "find_nodes_at_time",
            "temporal",
            "typical",
            false,
            {
                let vt = f.now.clone();
                move |_| json!({"label": "Person", "valid_time": vt, "limit": 25})
            }
        ),
        s!(
            "temporal__list_changes",
            "list_changes",
            "temporal",
            "typical",
            false,
            {
                let from = f.seed_start.clone();
                let to = f.now.clone();
                move |_| json!({"tx_from": from, "tx_to": to, "limit": 100})
            }
        ),
        s!(
            "temporal__await_changes",
            "await_changes",
            "temporal",
            "typical",
            false,
            // timeout_ms:0 makes recv_timeout return instantly (Ok(empty)) so the
            // long-poll never blocks the bench; with no concurrent writes this
            // resolves as timed_out:true with an empty changes array.
            |_| json!({"timeout_ms": 0})
        ),
        s!(
            "temporal__temporal_extent",
            "temporal_extent",
            "temporal",
            "typical",
            false,
            |_| json!({})
        ),
        s!(
            "temporal__diff_node_versions",
            "diff_node_versions",
            "temporal",
            "typical",
            false,
            {
                let id = f.history_long_node;
                move |_| json!({"node_id": id, "from_version": f.node_from_version, "to_version": f.node_to_version})
            }
        ),
        s!(
            "temporal__diff_edge_versions",
            "diff_edge_versions",
            "temporal",
            "typical",
            false,
            {
                let id = f.history_edge;
                move |_| json!({"edge_id": id, "from_version": f.edge_from_version, "to_version": f.edge_to_version})
            }
        ),
        // ---- Lineage (empty closure on a real version is a valid typical query) ----
        s!(
            "lineage__upstream",
            "lineage_upstream",
            "lineage",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"entity_kind": "node", "id": id, "version": f.person_version})
            }
        ),
        s!(
            "lineage__downstream",
            "lineage_downstream",
            "lineage",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"entity_kind": "node", "id": id, "version": f.person_version})
            }
        ),
        // ---- Writes (non-gated) ----
        s!(
            "write__create_edge",
            "create_edge",
            "write",
            "typical",
            false,
            {
                let (s, t) = (f.person_id, f.person_id2);
                move |i| json!({"source_id": s, "target_id": t, "label": "BENCH_KNOWS", "properties": {"i": i as i64}})
            }
        ),
        s!(
            "write__update_node",
            "update_node",
            "write",
            "typical",
            false,
            {
                let id = f.update_node_id;
                move |i| json!({"node_id": id, "properties": {"counter": i as i64}})
            }
        ),
        s!(
            "write__update_edge",
            "update_edge",
            "write",
            "typical",
            false,
            {
                let id = f.update_edge_id;
                move |i| json!({"edge_id": id, "properties": {"counter": i as i64}})
            }
        ),
        s!(
            "write__delete_node",
            "delete_node",
            "write",
            "typical",
            false,
            {
                let pool = f.del_nodes.clone();
                move |i| json!({"node_id": pool[i % pool.len()]})
            }
        ),
        s!(
            "write__delete_edge",
            "delete_edge",
            "write",
            "typical",
            false,
            {
                let pool = f.del_edges.clone();
                move |i| json!({"edge_id": pool[i % pool.len()]})
            }
        ),
        s!(
            "write__delete_node_cascade",
            "delete_node_cascade",
            "write",
            "typical",
            false,
            {
                let pool = f.cascade_nodes.clone();
                move |i| json!({"node_id": pool[i % pool.len()]})
            }
        ),
        s!(
            "write__retract_node",
            "retract_node",
            "write",
            "typical",
            false,
            {
                let id = f.retract_node_id;
                move |_| json!({"node_id": id})
            }
        ),
        s!(
            "write__retract_edge",
            "retract_edge",
            "write",
            "typical",
            false,
            {
                let id = f.retract_edge_id;
                move |_| json!({"edge_id": id})
            }
        ),
        s!(
            "write__apply_batch",
            "apply_batch",
            "write",
            "typical",
            false,
            {
                move |i| json!({"operations": [{"op": "create_node", "label": "BatchTmp", "properties": {"i": i as i64}}]})
            }
        ),
        s!(
            "write__enable_vector_index",
            "enable_vector_index",
            "admin",
            "typical",
            false,
            {
                move |i| json!({"property_name": format!("bench_vec_{i}"), "dimensions": 8, "distance_metric": "cosine"})
            }
        ),
        s!(
            "write__enable_unique_constraint",
            "enable_unique_constraint",
            "admin",
            "typical",
            false,
            move |i| json!({"label": format!("UniqProbe{i}"), "property": "email"})
        ),
        // ---- Namespace management (Issue #3349) — admin/read-class ----
        // create_namespace is a write; a fresh unique name per iteration keeps
        // every call a clean success (a duplicate name would be a tolerated
        // tool-level CONFLICT, but unique names measure the success path).
        s!(
            "admin__create_namespace",
            "create_namespace",
            "admin",
            "typical",
            false,
            move |i| json!({"name": format!("agent:bench_{i}"), "description": "round-trip bench namespace"})
        ),
        s!(
            "admin__list_namespaces",
            "list_namespaces",
            "read",
            "typical",
            false,
            |_| json!({})
        ),
        // describe_namespace on the implicit 'default' namespace always resolves.
        s!(
            "admin__describe_namespace",
            "describe_namespace",
            "read",
            "typical",
            false,
            |_| json!({"name": "default"})
        ),
        // ---- GDPR crypto-shred admin tools (Issue #3359) ----
        // The smoke build configures no encryption key material, so both tools
        // return the structured FAILED_PRECONDITION unavailable response (a
        // tool-level `isError`, NOT a JSON-RPC error), which `measure_scenario`
        // tolerates. They are present to satisfy registry-completeness (AC2);
        // they are non-gated and assert no success. Args are well-formed so the
        // request deserializes cleanly (only malformed args trip a JSON-RPC
        // error / `sample_response_ok`).
        s!(
            "admin__designate_subject",
            "designate_subject",
            "admin",
            "typical",
            false,
            {
                let id = f.person_id;
                move |i| {
                    json!({
                        "subject_id": format!("bench-subject-{i}"),
                        "targets": [{"entity_kind": "node", "id": id}]
                    })
                }
            }
        ),
        s!(
            "admin__erase_subject",
            "erase_subject",
            "admin",
            "typical",
            false,
            move |i| json!({"subject_id": format!("bench-subject-{i}")})
        ),
        // ---- Provenance / audit ----
        s!(
            "provenance__verify_chain",
            "verify_chain",
            "provenance",
            "typical",
            false,
            |_| json!({})
        ),
        s!(
            "provenance__export_chain_head",
            "export_chain_head",
            "provenance",
            "typical",
            false,
            |_| json!({})
        ),
        s!(
            "provenance__audit_export",
            "audit_export",
            "provenance",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"entity_type": "node", "entity_id": id})
            }
        ),
        // ---- Semantic-search analysis tools (Issue #2907) ----
        // These are advertised on every build, but the PR-smoke bench compiles
        // WITHOUT the `semantic-search` feature, so each returns the structured
        // FAILED_PRECONDITION unavailable-feature response. The round-trip
        // harness treats a tool-level (isError) response as a valid round-trip
        // (only a JSON-RPC transport error trips `sample_response_ok`), so these
        // are present purely to satisfy registry-completeness (AC2); they are
        // non-gated and assert no success.
        s!(
            "semantic__semantic_path",
            "semantic_path",
            "vector",
            "typical",
            false,
            {
                let start = f.person_id;
                let end = f.hub_id;
                move |_| json!({"start": start, "end": end, "property_name": "embedding"})
            }
        ),
        s!(
            "semantic__concept_analogy",
            "concept_analogy",
            "vector",
            "typical",
            false,
            {
                let a = f.person_id;
                let b = f.hub_id;
                let c = f.update_node_id;
                move |_| json!({"a": a, "b": b, "c": c, "property_name": "embedding", "k": 10})
            }
        ),
        s!(
            "semantic__concept_mean",
            "concept_mean",
            "vector",
            "typical",
            false,
            {
                let a = f.person_id;
                let b = f.hub_id;
                move |_| json!({"nodes": [a, b], "property_name": "embedding", "k": 10})
            }
        ),
        s!(
            "semantic__find_duplicate_candidates",
            "find_duplicate_candidates",
            "vector",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"node_id": id, "property_name": "embedding", "threshold": 0.9, "limit": 10})
            }
        ),
        s!(
            "semantic__semantic_horizon",
            "semantic_horizon",
            "vector",
            "typical",
            false,
            {
                let seed = f.person_id;
                move |_| json!({"seed": seed, "property_name": "embedding", "threshold": 0.5, "max_depth": 3})
            }
        ),
        s!(
            "semantic__context_aspects",
            "context_aspects",
            "vector",
            "typical",
            false,
            {
                let id = f.person_id;
                move |_| json!({"node_id": id, "property_name": "embedding", "k": 3})
            }
        ),
        // ---- Embedding generation & text semantic search (Issue #2906) ----
        // These tools require a configured embedder / the `embeddings` feature.
        // The smoke build has neither, so each round-trip returns a structured
        // FAILED_PRECONDITION (a tool-level `isError`, NOT a JSON-RPC error), which
        // `measure_scenario` tolerates. They are present purely to satisfy the
        // registry-completeness gate (AC2); they are not gated on latency.
        s!(
            "embedding__embed_query",
            "embed_query",
            "vector",
            "typical",
            false,
            move |_| json!({"text": "benchmark embedding probe"})
        ),
        s!(
            "embedding__embed_text",
            "embed_text",
            "vector",
            "typical",
            false,
            move |_| json!({"texts": ["benchmark embedding probe"]})
        ),
        s!(
            "embedding__semantic_search",
            "semantic_search",
            "vector",
            "typical",
            false,
            {
                move |_| json!({"property_name": "embedding", "query_text": "benchmark probe", "k": 10})
            }
        ),
        s!(
            "embedding__create_node_with_embedding",
            "create_node_with_embedding",
            "vector",
            "typical",
            false,
            {
                move |i| {
                    json!({
                        "label": "BenchEmbed",
                        "text": format!("benchmark probe {i}"),
                        "embedding_property": "embedding"
                    })
                }
            }
        ),
        s!(
            "embedding__update_node_embedding",
            "update_node_embedding",
            "vector",
            "typical",
            false,
            {
                let id = f.person_id;
                move |i| {
                    json!({
                        "node_id": id,
                        "text": format!("benchmark probe {i}"),
                        "embedding_property": "embedding"
                    })
                }
            }
        ),
    ]
}

// ============================================================================
// Direct-call floor (AC6) — in-process typed methods, no transport
// ============================================================================

/// Call a tool's public typed method on the in-process server, bypassing the
/// JSON-RPC/stdio transport. Returns `None` for tools whose request type is not
/// publicly re-exported from `aletheiadb::mcp` (their overhead is reported as
/// null; extending coverage only needs Lane 4 to re-export the remaining
/// request types — no change to this harness's logic).
fn direct_call(server: &AletheiaMcpServer, tool: &str, args: &Value) -> Option<String> {
    use aletheiadb::mcp::*;
    macro_rules! call {
        ($ty:ty, $method:ident) => {{
            let req: $ty = serde_json::from_value(args.clone()).ok()?;
            Some(server.$method(req))
        }};
    }
    match tool {
        "get_node" => call!(GetNodeRequest, get_node),
        "create_node" => call!(CreateNodeRequest, create_node),
        "update_node" => call!(UpdateNodeRequest, update_node),
        "delete_node" => call!(DeleteNodeRequest, delete_node),
        "delete_node_cascade" => call!(DeleteNodeCascadeRequest, delete_node_cascade),
        "list_nodes" => call!(ListNodesRequest, list_nodes),
        "count_nodes" => call!(CountNodesRequest, count_nodes),
        "get_edge" => call!(GetEdgeRequest, get_edge),
        "create_edge" => call!(CreateEdgeRequest, create_edge),
        "update_edge" => call!(UpdateEdgeRequest, update_edge),
        "delete_edge" => call!(DeleteEdgeRequest, delete_edge),
        "list_edges" => call!(ListEdgesRequest, list_edges),
        "count_edges" => call!(CountEdgesRequest, count_edges),
        "get_outgoing_edges" => call!(GetOutgoingEdgesRequest, get_outgoing_edges),
        "get_incoming_edges" => call!(GetIncomingEdgesRequest, get_incoming_edges),
        "traverse" => call!(TraverseRequest, traverse),
        "find_similar" => call!(FindSimilarRequest, find_similar),
        "enable_vector_index" => call!(EnableVectorIndexRequest, enable_vector_index),
        "list_vector_indexes" => call!(ListVectorIndexesRequest, list_vector_indexes),
        "get_node_at_time" => call!(GetNodeAtTimeRequest, get_node_at_time),
        "get_edge_at_time" => call!(GetEdgeAtTimeRequest, get_edge_at_time),
        "find_nodes_at_time" => call!(FindNodesAtTimeRequest, find_nodes_at_time),
        "list_changes" => call!(ListChangesRequest, list_changes),
        "hybrid_query" => call!(HybridQueryRequest, hybrid_query),
        "query" => call!(QueryRequest, query),
        "apply_batch" => call!(ApplyBatchRequest, apply_batch),
        _ => None,
    }
}

// ============================================================================
// Measurement
// ============================================================================

struct ScenarioResult {
    name: String,
    tool: String,
    category: String,
    size_class: String,
    gated: bool,
    round_trip: Stats,
    direct: Option<Stats>,
    injected_ms: f64,
    sample_response_ok: bool,
}

impl ScenarioResult {
    /// MCP overhead (round-trip minus direct floor), clamped at 0, per percentile.
    fn overhead_ns(&self) -> Option<(u64, u64, u64)> {
        self.direct.as_ref().map(|d| {
            let sub = |a: u64, b: u64| a.saturating_sub(b);
            (
                sub(self.round_trip.p50_ns, d.p50_ns),
                sub(self.round_trip.p95_ns, d.p95_ns),
                sub(self.round_trip.p99_ns, d.p99_ns),
            )
        })
    }
}

fn measure_scenario(
    client: &mut McpClient,
    server: &AletheiaMcpServer,
    sc: &Scenario,
    cfg: &Config,
) -> ScenarioResult {
    let inject = if sc.gated && sc.name == cfg.inject_scenario {
        cfg.inject_latency_ms
    } else {
        0.0
    };
    let inject_dur = Duration::from_secs_f64(inject / 1000.0);

    // Warm-up (round-trip).
    for i in 0..cfg.warmup {
        let _ = client.call_tool(sc.tool, &(sc.args)(i));
    }
    // Measured round-trip.
    let mut rt = Vec::with_capacity(cfg.sample_size);
    let mut sample_ok = true;
    for i in 0..cfg.sample_size {
        let args = (sc.args)(cfg.warmup + i);
        let start = Instant::now();
        let resp = client.call_tool(sc.tool, &args);
        if inject > 0.0 {
            std::thread::sleep(inject_dur); // synthetic regression, inside the timed region
        }
        rt.push(start.elapsed().as_nanos() as u64);
        if i == 0 {
            // A JSON-RPC-level error means our args are malformed; a tool-level
            // isError (business error) is still a valid round-trip.
            sample_ok = resp.get("error").is_none();
        }
    }

    // Direct-call floor (no transport), when a typed method exists.
    let direct = if direct_call(server, sc.tool, &(sc.args)(0)).is_some() {
        for i in 0..cfg.warmup {
            let _ = direct_call(server, sc.tool, &(sc.args)(i));
        }
        let mut d = Vec::with_capacity(cfg.sample_size);
        for i in 0..cfg.sample_size {
            let args = (sc.args)(cfg.warmup + cfg.sample_size + i);
            let start = Instant::now();
            let _ = direct_call(server, sc.tool, &args);
            d.push(start.elapsed().as_nanos() as u64);
        }
        Some(Stats::from_samples(d))
    } else {
        None
    };

    ScenarioResult {
        name: sc.name.to_string(),
        tool: sc.tool.to_string(),
        category: sc.category.to_string(),
        size_class: sc.size_class.to_string(),
        gated: sc.gated,
        round_trip: Stats::from_samples(rt),
        direct,
        injected_ms: inject,
        sample_response_ok: sample_ok,
    }
}

/// Fetch a node's / edge's version ids from the running server so the diff and
/// lineage scenarios can name real versions (prelude before measurement).
fn discover_versions(client: &mut McpClient, fx: &mut Fixture) {
    // Node history (long node) -> two version ids for diff_node_versions.
    let resp = client.call_tool(
        "get_node_history",
        &json!({"node_id": fx.history_long_node}),
    );
    if let Some(vers) = extract_versions(&resp).filter(|v| v.len() >= 2) {
        fx.node_from_version = vers[0];
        fx.node_to_version = *vers.last().unwrap();
    }
    let resp = client.call_tool("get_edge_history", &json!({"edge_id": fx.history_edge}));
    if let Some(vers) = extract_versions(&resp).filter(|v| v.len() >= 2) {
        fx.edge_from_version = vers[0];
        fx.edge_to_version = *vers.last().unwrap();
    }
    // Person version (for lineage root).
    let resp = client.call_tool("get_node_history", &json!({"node_id": fx.person_id}));
    if let Some(v) = extract_versions(&resp).and_then(|v| v.first().copied()) {
        fx.person_version = v;
    }
}

/// Pull every integer `version_id`/`version` field out of a history response.
fn extract_versions(resp: &Value) -> Option<Vec<u64>> {
    let text = McpClient::tool_text(resp)?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let mut out = Vec::new();
    collect_versions(&v, &mut out);
    Some(out)
}

fn collect_versions(v: &Value, out: &mut Vec<u64>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                let is_ver = k == "version_id" || k == "version";
                if is_ver && val.is_u64() {
                    out.push(val.as_u64().unwrap());
                } else if is_ver && val.as_str().and_then(|s| s.parse::<u64>().ok()).is_some() {
                    // Version fields are sometimes serialized as decimal strings.
                    out.push(val.as_str().unwrap().parse().unwrap());
                } else {
                    collect_versions(val, out);
                }
            }
        }
        Value::Array(arr) => arr.iter().for_each(|x| collect_versions(x, out)),
        _ => {}
    }
}

// ============================================================================
// Reporting
// ============================================================================

fn git_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// A single gated scenario's relative-gate evaluation against the committed
/// baseline median.
struct RegressionRow {
    name: String,
    current_p50_us: f64,
    baseline_p50_us: f64,
    ratio: f64,
    regressed: bool,
}

/// Load the committed reference baseline (`gated_scenarios[name].p50_us`).
/// Returns `None` if the file is absent or unparseable (relative gate then
/// silently no-ops — it only ever fires when a baseline is present).
fn load_baseline(path: &str) -> Option<std::collections::BTreeMap<String, f64>> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let obj = v.get("gated_scenarios")?.as_object()?;
    let mut out = std::collections::BTreeMap::new();
    for (name, entry) in obj {
        if let Some(p50) = entry.get("p50_us").and_then(|x| x.as_f64()) {
            out.insert(name.clone(), p50);
        }
    }
    Some(out)
}

/// Compare the current gated p50s against the baseline; a ratio above
/// `multiplier` is a regression. Gates on **p50/median** (stable run-to-run),
/// never p99 (which drifts on the tail and would false-fail).
fn evaluate_relative(
    results: &[ScenarioResult],
    baseline: &std::collections::BTreeMap<String, f64>,
    multiplier: f64,
) -> Vec<RegressionRow> {
    let mut rows = Vec::new();
    for r in results.iter().filter(|r| r.gated) {
        if let Some(&base) = baseline.get(&r.name) {
            let cur = r.round_trip.p50_ns as f64 / 1000.0;
            let ratio = if base > 0.0 {
                cur / base
            } else {
                f64::INFINITY
            };
            rows.push(RegressionRow {
                name: r.name.clone(),
                current_p50_us: cur,
                baseline_p50_us: base,
                ratio,
                regressed: ratio > multiplier,
            });
        }
    }
    rows
}

/// Serialize the gated scenarios' current p50 as a fresh reference baseline.
fn build_baseline_doc(cfg: &Config, fx: &Fixture, results: &[ScenarioResult]) -> Value {
    let mut gated = serde_json::Map::new();
    for r in results.iter().filter(|r| r.gated) {
        gated.insert(
            r.name.clone(),
            json!({
                "tool": r.tool,
                "p50_us": r.round_trip.p50_ns as f64 / 1000.0,
                "p99_us": r.round_trip.p99_ns as f64 / 1000.0,
            }),
        );
    }
    json!({
        "schema_version": 1,
        "benchmark": "mcp_round_trip",
        "issue": 3361,
        "kind": "reference_baseline",
        "note": "Reference baseline MEDIAN (p50) latencies for the GATED scenarios only. \
                 The relative regression gate fails a gated scenario whose current p50 \
                 exceeds regression_multiplier x its baseline p50. This is a REFERENCE \
                 baseline tied to the hardware/scale below; refresh it per release or when \
                 the reference runner changes (regenerate with MCP_BENCH_WRITE_BASELINE).",
        "generated_at": now_rfc3339(),
        "git_sha": git_sha(),
        "scale": if cfg.scale_nightly { "nightly" } else { "smoke" },
        "sample_size": cfg.sample_size,
        "warmup": cfg.warmup,
        "regression_multiplier": cfg.regression_multiplier,
        "gate_on": "p50",
        "hardware": {
            "arch": std::env::consts::ARCH,
            "os": std::env::consts::OS,
            "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        },
        "fixture": { "nodes": fx.nodes, "edges": fx.edges, "embedding_dim": fx.embedding_dim },
        "gated_scenarios": gated,
    })
}

fn main() {
    let cfg = Config::from_env();
    let bin = env!("CARGO_BIN_EXE_aletheia-mcp");

    println!("=== MCP round-trip latency benchmark (Issue #3361) ===");
    println!(
        "scale={} sample_size={} warmup={} enforce_latency={} enforce_relative={} mult={:.1}x baseline={} inject={}ms@{}",
        if cfg.scale_nightly {
            "nightly"
        } else {
            "smoke"
        },
        cfg.sample_size,
        cfg.warmup,
        cfg.enforce_latency,
        cfg.enforce_relative,
        cfg.regression_multiplier,
        cfg.baseline_path.as_deref().unwrap_or("<none>"),
        cfg.inject_latency_ms,
        cfg.inject_scenario,
    );

    // Two identical fixtures: dir_rt drives the spawned binary (round-trip);
    // dir_direct backs the in-process server (direct-call floor). Identical
    // deterministic seeding keeps entity/version ids aligned.
    let tmp_rt = tempfile::tempdir().expect("tempdir rt");
    let tmp_direct = tempfile::tempdir().expect("tempdir direct");
    println!("seeding fixtures...");
    let mut fx = seed_fixture(tmp_rt.path(), &cfg);
    let _ = seed_fixture(tmp_direct.path(), &cfg);
    println!(
        "fixture: {} nodes, {} edges, embedding_dim={}, disposable_pool={}",
        fx.nodes,
        fx.edges,
        fx.embedding_dim,
        fx.del_nodes.len()
    );

    // In-process server for the direct-call floor (same durability profile).
    let db_direct = Arc::new(
        AletheiaDB::with_unified_config(bench_config(tmp_direct.path())).expect("open direct db"),
    );
    let server = AletheiaMcpServer::new(db_direct);

    // Write the generated TOML config so the spawned binary serves the seeded
    // fixture under the same Async durability profile (via ALETHEIADB_CONFIG).
    let config_path = tmp_rt.path().join("bench_config.toml");
    let toml = bench_config(tmp_rt.path())
        .to_toml_string()
        .expect("serialize bench config to TOML");
    std::fs::write(&config_path, toml).expect("write bench config");

    // Spawn and handshake the shipped binary.
    let mut client = McpClient::spawn(bin, &config_path);
    client.handshake();

    // AC2: registry completeness against the LIVE registry.
    let advertised = client.list_tools();
    discover_versions(&mut client, &mut fx);

    let mut scenarios = build_scenarios(&fx);
    // Test hook: `MCP_BENCH_DROP_SCENARIO=<tool>` drops every scenario for that
    // tool, so the registry-completeness invariant (AC2) can be shown to fail
    // (exit 2) without editing the source.
    if let Ok(drop_tool) = std::env::var("MCP_BENCH_DROP_SCENARIO") {
        scenarios.retain(|sc| sc.tool != drop_tool);
        eprintln!("[test hook] dropped all scenarios for tool '{drop_tool}'");
    }
    let mut covered: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for sc in &scenarios {
        covered.insert(sc.tool.to_string());
    }
    let missing: Vec<String> = advertised
        .iter()
        .filter(|t| !covered.contains(*t))
        .cloned()
        .collect();
    let registry_complete = missing.is_empty();
    println!(
        "\nregistry: {} advertised, {} covered by scenarios, {} missing",
        advertised.len(),
        covered.len(),
        missing.len()
    );
    if !missing.is_empty() {
        println!("  MISSING SCENARIOS: {missing:?}");
    }

    // Run every scenario.
    println!("\nrunning {} scenarios...\n", scenarios.len());
    let mut results = Vec::new();
    for sc in &scenarios {
        let t0 = Instant::now();
        eprint!("  {:<40} ", sc.name);
        let r = measure_scenario(&mut client, &server, sc, &cfg);
        eprintln!(
            "p99={:.2}ms ({:.1}s)",
            r.round_trip.p99_ms(),
            t0.elapsed().as_secs_f64()
        );
        results.push(r);
    }
    drop(client); // stop the child before printing

    // ---- Table ----
    print_table(&results);

    // ---- Gate evaluation (AC5) ----
    let mut gated_failures = Vec::new();
    for r in &results {
        if r.gated && r.round_trip.p99_ms() >= P99_GATE_MS {
            gated_failures.push(r.name.clone());
        }
    }
    // ---- Overhead budget (AC6) informational check ----
    let mut overhead_failures = Vec::new();
    for r in &results {
        if !r.gated {
            continue;
        }
        if let Some((_, _, o99)) = r.overhead_ns()
            && (o99 as f64 / 1e6) >= OVERHEAD_BUDGET_MS
        {
            overhead_failures.push(r.name.clone());
        }
    }

    // ---- Relative committed-baseline regression gate (AC5) ----
    // Median (p50) based: stable ±3-9% run-to-run, so a real 2x regression trips
    // it while honest noise never does (p99 is deliberately NOT used here — its
    // tail drifts ±10%+ and would false-fail). Additive to the absolute p99 gate.
    let baseline = cfg.baseline_path.as_deref().and_then(load_baseline);
    let regression_rows = baseline
        .as_ref()
        .map(|b| evaluate_relative(&results, b, cfg.regression_multiplier))
        .unwrap_or_default();
    let regression_failures: Vec<String> = regression_rows
        .iter()
        .filter(|r| r.regressed)
        .map(|r| r.name.clone())
        .collect();

    println!("\n=== Gate summary ===");
    println!(
        "p99 < {}ms gate over {} gated scenarios: {}",
        P99_GATE_MS,
        results.iter().filter(|r| r.gated).count(),
        if gated_failures.is_empty() {
            "PASS"
        } else {
            "FAIL"
        }
    );
    if !gated_failures.is_empty() {
        println!("  FAILING GATED SCENARIOS: {gated_failures:?}");
    }
    match &baseline {
        None => println!(
            "relative regression gate (p50 <= {:.1}x baseline): SKIPPED (no baseline; set MCP_BENCH_BASELINE)",
            cfg.regression_multiplier
        ),
        Some(_) => {
            println!(
                "relative regression gate (p50 <= {:.1}x baseline) over {} baselined scenario(s): {}",
                cfg.regression_multiplier,
                regression_rows.len(),
                if regression_failures.is_empty() {
                    "PASS"
                } else {
                    "FAIL"
                }
            );
            for row in &regression_rows {
                println!(
                    "  {:<32} current p50={:.1}us baseline p50={:.1}us ratio={:.2}x{}",
                    row.name,
                    row.current_p50_us,
                    row.baseline_p50_us,
                    row.ratio,
                    if row.regressed {
                        "  <-- REGRESSION"
                    } else {
                        ""
                    },
                );
            }
            if !regression_failures.is_empty() {
                println!("  REGRESSED GATED SCENARIOS: {regression_failures:?}");
            }
        }
    }
    println!(
        "overhead < {}ms budget (informational): {}",
        OVERHEAD_BUDGET_MS,
        if overhead_failures.is_empty() {
            "PASS"
        } else {
            "FAIL"
        }
    );
    if !overhead_failures.is_empty() {
        println!("  OVER-BUDGET (informational): {overhead_failures:?}");
    }
    println!(
        "registry-completeness: {}",
        if registry_complete { "PASS" } else { "FAIL" }
    );

    // ---- JSON artifact (AC4) ----
    let artifact = build_artifact(
        &cfg,
        &fx,
        &advertised,
        &covered,
        &missing,
        &results,
        &gated_failures,
        &overhead_failures,
        &regression_rows,
        &regression_failures,
        baseline.is_some(),
        registry_complete,
    );
    if let Some(path) = &cfg.json_path {
        std::fs::write(path, serde_json::to_string_pretty(&artifact).unwrap())
            .expect("write JSON artifact");
        println!("\nJSON artifact written to {path}");
    }

    // ---- Reference-baseline regenerator ----
    if let Some(path) = &cfg.write_baseline_path {
        let doc = build_baseline_doc(&cfg, &fx, &results);
        std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap())
            .expect("write reference baseline");
        println!("reference baseline written to {path}");
    }

    // ---- Exit codes ----
    if !registry_complete {
        eprintln!(
            "\nERROR: registry-completeness FAILED — {} advertised tool(s) have no scenario: {:?}",
            missing.len(),
            missing
        );
        std::process::exit(2);
    }
    if cfg.enforce_latency && !gated_failures.is_empty() {
        eprintln!(
            "\nERROR: absolute p99 < {P99_GATE_MS}ms gate FAILED for gated scenario(s): {gated_failures:?}"
        );
        std::process::exit(3);
    }
    if cfg.enforce_relative && !regression_failures.is_empty() {
        eprintln!(
            "\nERROR: relative regression gate FAILED — gated scenario(s) exceeded {:.1}x baseline p50: {regression_failures:?}",
            cfg.regression_multiplier
        );
        std::process::exit(4);
    }
    println!("\nOK");
}

fn print_table(results: &[ScenarioResult]) {
    println!(
        "{:<38} {:<26} {:<10} {:>9} {:>9} {:>9} {:>9} {:>11} {:>10}",
        "scenario", "tool", "gated", "p50(us)", "p95(us)", "p99(us)", "max(us)", "overhd99us", "ok"
    );
    println!("{}", "-".repeat(140));
    for r in results {
        let us = |ns: u64| ns as f64 / 1000.0;
        let ov = match r.overhead_ns() {
            Some((_, _, o99)) => format!("{:.1}", us(o99)),
            None => "n/a".to_string(),
        };
        let inj = if r.injected_ms > 0.0 {
            format!(" [+{}ms]", r.injected_ms)
        } else {
            String::new()
        };
        println!(
            "{:<38} {:<26} {:<10} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>11} {:>10}{}",
            r.name,
            r.tool,
            if r.gated { "yes" } else { "" },
            us(r.round_trip.p50_ns),
            us(r.round_trip.p95_ns),
            us(r.round_trip.p99_ns),
            us(r.round_trip.max_ns),
            ov,
            if r.sample_response_ok { "ok" } else { "ERR" },
            inj,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn build_artifact(
    cfg: &Config,
    fx: &Fixture,
    advertised: &[String],
    covered: &std::collections::BTreeSet<String>,
    missing: &[String],
    results: &[ScenarioResult],
    gated_failures: &[String],
    overhead_failures: &[String],
    regression_rows: &[RegressionRow],
    regression_failures: &[String],
    baseline_present: bool,
    registry_complete: bool,
) -> Value {
    let scenarios: Vec<Value> = results
        .iter()
        .map(|r| {
            let overhead = r.overhead_ns().map(|(o50, o95, o99)| {
                json!({
                    "p50_us": o50 as f64 / 1000.0,
                    "p95_us": o95 as f64 / 1000.0,
                    "p99_us": o99 as f64 / 1000.0,
                })
            });
            json!({
                "name": r.name,
                "tool": r.tool,
                "category": r.category,
                "size_class": r.size_class,
                "gated": r.gated,
                "injected_latency_ms": r.injected_ms,
                "sample_response_ok": r.sample_response_ok,
                "round_trip": r.round_trip.to_json_us(),
                "direct": r.direct.as_ref().map(|d| d.to_json_us()),
                "overhead": overhead,
                "gate_pass": if r.gated { Some(r.round_trip.p99_ms() < P99_GATE_MS) } else { None },
            })
        })
        .collect();

    json!({
        "schema_version": 1,
        "benchmark": "mcp_round_trip",
        "issue": 3361,
        "generated_at": now_rfc3339(),
        "git_sha": git_sha(),
        "scale": if cfg.scale_nightly { "nightly" } else { "smoke" },
        "sample_size": cfg.sample_size,
        "warmup": cfg.warmup,
        "hardware": {
            "arch": std::env::consts::ARCH,
            "os": std::env::consts::OS,
            "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        },
        "fixture": {
            "nodes": fx.nodes,
            "edges": fx.edges,
            "embedding_dim": fx.embedding_dim,
            "disposable_pool_per_delete_scenario": fx.del_nodes.len(),
        },
        "gate": {
            "p99_threshold_ms": P99_GATE_MS,
            "overhead_budget_ms": OVERHEAD_BUDGET_MS,
            "enforced": cfg.enforce_latency,
            "injected_latency_ms": cfg.inject_latency_ms,
            "injected_scenario": cfg.inject_scenario,
        },
        "relative_gate": {
            "baseline_present": baseline_present,
            "baseline_path": cfg.baseline_path,
            "regression_multiplier": cfg.regression_multiplier,
            "gate_on": "p50",
            "enforced": cfg.enforce_relative,
            "scenarios": regression_rows.iter().map(|r| json!({
                "name": r.name,
                "current_p50_us": r.current_p50_us,
                "baseline_p50_us": r.baseline_p50_us,
                "ratio": r.ratio,
                "regressed": r.regressed,
            })).collect::<Vec<_>>(),
            "failures": regression_failures,
        },
        "registry": {
            "advertised": advertised,
            "covered": covered.iter().cloned().collect::<Vec<_>>(),
            "missing": missing,
            "complete": registry_complete,
        },
        "scenarios": scenarios,
        "gated_failures": gated_failures,
        "overhead_failures": overhead_failures,
        "regression_failures": regression_failures,
        "pass": registry_complete
            && (!cfg.enforce_latency || gated_failures.is_empty())
            && (!cfg.enforce_relative || regression_failures.is_empty()),
    })
}
