//! Provenance-enabled write-throughput benchmark suite (Issue #3383).
//!
//! # What this measures — "the cost of trust"
//!
//! Recording *why* a fact is trustworthy is not free: attaching #3224
//! provenance to a write serializes extra bytes into the WAL, and enabling a
//! #3218 uniqueness constraint adds a reservation-index check on the commit
//! path. This suite quantifies that overhead by measuring GroupCommit write
//! throughput and single-write latency across a **matrix** of trust
//! configurations, all measured **back-to-back in the same process run** so the
//! ratios are immune to cross-machine / cross-CI hardware variance:
//!
//! | Config              | Provenance (#3224) | Unique constraint (#3218) |
//! |---------------------|:------------------:|:-------------------------:|
//! | `baseline`          |         no         |            no             |
//! | `provenance_only`   |        yes         |            no             |
//! | `constraint_active` |         no         |            yes            |
//! | `composed`          |        yes         |            yes            |
//!
//! For each config the suite reports sustained **throughput (ops/sec)** and
//! single-write **latency p50/p99**, plus each config's throughput **ratio vs
//! the same-run baseline**. The success contract (AC3) is that the fully
//! trust-enabled `composed` config sustains **≥ 80 %** of same-run baseline
//! throughput, with per-feature bounds for the intermediate configs.
//!
//! # Deterministic, seeded fixture (AC2 — reproducibility)
//!
//! The workload is driven by a fixed-seed [`rand::rngs::SmallRng`]
//! (`WORKLOAD_SEED`), so every run issues the identical sequence of writes.
//! Each write creates a `"Bench"` node carrying:
//!
//! - `uid`: i64, a **process-unique** monotonic id (satisfies the uniqueness
//!   constraint so `constraint_active`/`composed` never error);
//! - `seq`: i64, the seeded per-write sequence value;
//! - `name`: String, `NAME_BYTES` (16) ASCII bytes;
//! - `payload`: String, `PAYLOAD_BYTES` (64) ASCII bytes.
//!
//! The provenance payload (configs 2 & 4) is a #3224 bundle:
//! `source` = `PROV_SOURCE` (18 bytes), `confidence` = `PROV_CONFIDENCE`
//! (0.95), `note` = `NOTE_BYTES` (64) ASCII bytes. Every config writes the
//! identical property shape — only the provenance attachment and the
//! constraint declaration differ — so the measured delta is exactly the cost
//! of the trust feature and nothing else.
//!
//! # Durability
//!
//! All configs use a **durable GroupCommit** database (WAL fsync +
//! index persistence), never the ephemeral `AletheiaDB::new()`, so the numbers
//! reflect real fsync-batched commit cost. GroupCommit with a single writer is
//! bounded by the batch timer, so the trust-feature CPU cost shows up primarily
//! in the latency percentiles while throughput ratios stay close to 1.0 — an
//! honest "fsync dominates, trust is cheap" result.
//!
//! # Running
//!
//! ```bash
//! # Full statistical run (Criterion arms + matrix table + JSON artifact):
//! cargo bench --bench provenance_write_throughput
//!
//! # Fast reduced-scale smoke:
//! BENCH_SAMPLE_SIZE=10 BENCH_MEASUREMENT_TIME=1 BENCH_WARMUP_TIME=1 \
//!   PROV_BENCH_WRITES=120 cargo bench --bench provenance_write_throughput
//!
//! # Self-gating mode (AC3): assert the same-run ratio bounds, non-zero exit on
//! # violation. Used by the SCHEDULED CI lane (hard-fail alerts, never blocks a PR):
//! PROV_BENCH_GATE=1 cargo bench --bench provenance_write_throughput
//!
//! # Verify the gate actually fails on a regression (injects a synthetic 25 %
//! # throughput hit into the `composed` row):
//! PROV_BENCH_GATE=1 PROV_BENCH_INJECT_REGRESSION=0.25 \
//!   cargo bench --bench provenance_write_throughput
//! ```
//!
//! Every matrix run also writes a machine-readable JSON results artifact (AC6)
//! to `$PROV_BENCH_JSON_OUT` (default `$CARGO_TARGET_DIR/provenance_write_throughput.json`).

mod common;

use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::config::WalConfigBuilder;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, AletheiaDBConfig, NodeId, PropertyMapBuilder, Provenance};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tempfile::TempDir;

// ============================================================================
// Fixture constants (AC2: documented, deterministic)
// ============================================================================

/// Fixed RNG seed for the deterministic workload.
const WORKLOAD_SEED: u64 = 0x3383_C057_0F74_0500; // "3383 COST OF TRUST" mnemonic
/// Bytes in each node's `name` property.
const NAME_BYTES: usize = 16;
/// Bytes in each node's `payload` property.
const PAYLOAD_BYTES: usize = 64;
/// Provenance `source` string (18 bytes).
const PROV_SOURCE: &str = "prov-write-bench-3383";
/// Provenance `confidence`.
const PROV_CONFIDENCE: f64 = 0.95;
/// Bytes in the provenance `note`.
const NOTE_BYTES: usize = 64;
/// Label all benchmark nodes are created under.
const BENCH_LABEL: &str = "Bench";
/// Property carrying the unique id (target of the uniqueness constraint).
const UNIQUE_PROP: &str = "uid";

/// Default number of writes measured per config in the standalone matrix.
const DEFAULT_MATRIX_WRITES: u64 = 240;

/// Process-wide unique id source so `uid` never collides across configs,
/// databases, warmup, or the constraint pre-flight — every constraint-checked
/// write carries a fresh value and thus never errors on uniqueness.
static UID: AtomicU64 = AtomicU64::new(1);

fn next_uid() -> i64 {
    UID.fetch_add(1, Ordering::Relaxed) as i64
}

// ============================================================================
// Config matrix
// ============================================================================

/// The four trust configurations measured back-to-back in one process run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Config {
    /// Plain `create_node`, no trust features.
    Baseline,
    /// `create_node_with_options` carrying #3224 provenance on every write.
    ProvenanceOnly,
    /// #3218 uniqueness constraint enabled on the written label; plain writes.
    ConstraintActive,
    /// Provenance + uniqueness constraint together.
    Composed,
}

impl Config {
    const ALL: [Config; 4] = [
        Config::Baseline,
        Config::ProvenanceOnly,
        Config::ConstraintActive,
        Config::Composed,
    ];

    fn key(self) -> &'static str {
        match self {
            Config::Baseline => "baseline",
            Config::ProvenanceOnly => "provenance_only",
            Config::ConstraintActive => "constraint_active",
            Config::Composed => "composed",
        }
    }

    fn has_provenance(self) -> bool {
        matches!(self, Config::ProvenanceOnly | Config::Composed)
    }

    fn has_constraint(self) -> bool {
        matches!(self, Config::ConstraintActive | Config::Composed)
    }

    /// Declared throughput lower bound vs same-run baseline (AC3 / #3383).
    ///
    /// - `composed` ≥ 0.80 is the issue's hard success metric.
    /// - `provenance_only` / `constraint_active` ≥ 0.85 are conservative
    ///   per-feature bounds (mirroring the ≥ 0.90 pattern the #3351 provenance
    ///   chain gate uses, relaxed for GroupCommit timing noise on shared CI).
    /// - `baseline` is the reference (ratio == 1.0 by construction).
    fn ratio_bound(self) -> f64 {
        match self {
            Config::Baseline => 1.0,
            Config::ProvenanceOnly => 0.85,
            Config::ConstraintActive => 0.85,
            Config::Composed => 0.80,
        }
    }
}

// ============================================================================
// Durable GroupCommit database builder
// ============================================================================

/// Build a durable, GroupCommit database rooted at `data_dir`. Every config
/// gets a fresh `TempDir` with its own `wal/` and `indexes/` subdirectories so
/// there is no cross-config interference. Settings are identical across configs
/// so the only measured difference is the trust feature under test.
fn build_db(data_dir: &std::path::Path) -> AletheiaDB {
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(data_dir.join("wal"))
                .durability_mode(DurabilityMode::GroupCommit {
                    max_delay_ms: 10,
                    max_batch_size: 200,
                })
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.join("indexes"),
            load_on_startup: true,
            ..Default::default()
        })
        .build();
    AletheiaDB::with_unified_config(config).expect("db init")
}

/// Deterministic ASCII string of `len` bytes drawn from `rng`.
fn rand_string(rng: &mut SmallRng, len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Build the fixed-shape property map for one write. Identical across all
/// configs; `uid` is process-unique, the rest is seeded-deterministic.
fn build_props(rng: &mut SmallRng) -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new()
        .insert(UNIQUE_PROP, next_uid())
        .insert("seq", rng.gen_range(0..i64::MAX))
        .insert("name", rand_string(rng, NAME_BYTES))
        .insert("payload", rand_string(rng, PAYLOAD_BYTES))
        .build()
}

/// Build the #3224 provenance bundle attached by provenance-carrying configs.
fn build_provenance(rng: &mut SmallRng) -> Provenance {
    Provenance::builder()
        .source(PROV_SOURCE)
        .confidence(PROV_CONFIDENCE)
        .note(rand_string(rng, NOTE_BYTES))
        .build()
        .expect("valid provenance")
}

/// Perform one write against `db` for `config`, returning the new node id.
fn one_write(db: &AletheiaDB, config: Config, rng: &mut SmallRng) -> NodeId {
    let props = build_props(rng);
    if config.has_provenance() {
        let prov = build_provenance(rng);
        db.create_node_with_options(
            BENCH_LABEL,
            props,
            WriteRequestOptions::new().with_provenance(prov),
        )
        .expect("create_node_with_options")
    } else {
        db.create_node(BENCH_LABEL, props).expect("create_node")
    }
}

// ============================================================================
// Standalone throughput + percentile measurement (not Criterion)
// ============================================================================

/// The measured result for one config in the matrix.
#[derive(Clone, Debug)]
struct ConfigResult {
    config: Config,
    throughput: f64,
    p50_us: f64,
    p99_us: f64,
    ratio_vs_baseline: f64,
}

/// Run `n_writes` timed writes against a fresh durable DB for `config`,
/// collecting per-op latencies and computing sustained throughput and
/// p50/p99. A short warmup (not timed) primes WAL/index structures.
fn measure_config(config: Config, n_writes: u64) -> ConfigResult {
    let dir = TempDir::new().expect("temp dir");
    let db = build_db(dir.path());

    // Enable the uniqueness constraint BEFORE any measured write so
    // constraint-checked configs pay the real per-write reservation cost.
    if config.has_constraint() {
        db.unique_constraint(BENCH_LABEL, UNIQUE_PROP)
            .enable()
            .expect("enable unique constraint");
    }

    let mut rng = SmallRng::seed_from_u64(WORKLOAD_SEED);

    // Warmup (untimed): a fraction of the measured volume.
    let warmup = (n_writes / 10).max(5);
    for _ in 0..warmup {
        black_box(one_write(&db, config, &mut rng));
    }

    // Measured loop: collect per-op latency in microseconds.
    let mut latencies_us: Vec<f64> = Vec::with_capacity(n_writes as usize);
    let start = Instant::now();
    for _ in 0..n_writes {
        let op_start = Instant::now();
        black_box(one_write(&db, config, &mut rng));
        latencies_us.push(op_start.elapsed().as_secs_f64() * 1_000_000.0);
    }
    let elapsed = start.elapsed().as_secs_f64();
    let throughput = n_writes as f64 / elapsed;

    latencies_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50_us = percentile(&latencies_us, 0.50);
    let p99_us = percentile(&latencies_us, 0.99);

    ConfigResult {
        config,
        throughput,
        p50_us,
        p99_us,
        ratio_vs_baseline: f64::NAN, // filled in by the matrix once baseline is known
    }
}

/// Nearest-rank percentile of a pre-sorted slice.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = (q * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// Read the measured-write count from `PROV_BENCH_WRITES`, defaulting to
/// [`DEFAULT_MATRIX_WRITES`]. Gate mode uses a reduced default for a fast smoke.
fn matrix_writes(gate: bool) -> u64 {
    std::env::var("PROV_BENCH_WRITES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if gate { 120 } else { DEFAULT_MATRIX_WRITES })
}

/// Run all four configs back-to-back in this process, computing same-run
/// ratios. `inject_regression` (0.0 == none) synthetically deflates the
/// `composed` throughput to prove the gate fails on a regression.
fn run_matrix(n_writes: u64, inject_regression: f64) -> Vec<ConfigResult> {
    let mut results: Vec<ConfigResult> = Config::ALL
        .iter()
        .map(|&c| measure_config(c, n_writes))
        .collect();

    let baseline = results
        .iter()
        .find(|r| r.config == Config::Baseline)
        .map(|r| r.throughput)
        .expect("baseline measured");

    for r in &mut results {
        if inject_regression > 0.0 && r.config == Config::Composed {
            // Simulate a throughput regression on the composed row.
            r.throughput *= 1.0 - inject_regression;
        }
        r.ratio_vs_baseline = if baseline > 0.0 {
            r.throughput / baseline
        } else {
            f64::NAN
        };
    }
    results
}

/// Print the human-readable matrix table.
fn print_table(results: &[ConfigResult]) {
    println!("\n[prov-write] ===== Cost-of-Trust write matrix (same-run baseline) =====");
    println!(
        "[prov-write] {:<18} {:>14} {:>12} {:>12} {:>10} {:>8}",
        "config", "throughput/s", "p50 (us)", "p99 (us)", "ratio", "bound"
    );
    for r in results {
        println!(
            "[prov-write] {:<18} {:>14.1} {:>12.1} {:>12.1} {:>10.3} {:>8.2}",
            r.config.key(),
            r.throughput,
            r.p50_us,
            r.p99_us,
            r.ratio_vs_baseline,
            r.config.ratio_bound(),
        );
    }
    println!("[prov-write] ================================================================\n");
}

/// Resolve the JSON artifact output path (AC6).
fn json_out_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("PROV_BENCH_JSON_OUT") {
        return std::path::PathBuf::from(p);
    }
    let base = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string());
    std::path::PathBuf::from(base).join("provenance_write_throughput.json")
}

/// Write the machine-readable JSON results artifact (AC6). Schema is documented
/// in `docs/benchmarks/cost-of-trust.md`.
fn write_json_artifact(results: &[ConfigResult], n_writes: u64, gated: bool) {
    let configs: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let bound = r.config.ratio_bound();
            let pass = r.config == Config::Baseline || r.ratio_vs_baseline >= bound;
            serde_json::json!({
                "config": r.config.key(),
                "throughput": r.throughput,
                "p50_us": r.p50_us,
                "p99_us": r.p99_us,
                "ratio_vs_baseline": r.ratio_vs_baseline,
                "bound": bound,
                "pass": pass,
            })
        })
        .collect();

    let doc = serde_json::json!({
        "schema": "aletheiadb.provenance_write_throughput.v1",
        "issue": 3383,
        "durability_mode": "GroupCommit{max_delay_ms:10,max_batch_size:200}",
        "workload_seed": format!("{WORKLOAD_SEED:#x}"),
        "writes_per_config": n_writes,
        "gated": gated,
        "fixture": {
            "label": BENCH_LABEL,
            "unique_property": UNIQUE_PROP,
            "name_bytes": NAME_BYTES,
            "payload_bytes": PAYLOAD_BYTES,
            "provenance": {
                "source": PROV_SOURCE,
                "confidence": PROV_CONFIDENCE,
                "note_bytes": NOTE_BYTES,
            },
        },
        "configs": configs,
    });

    let path = json_out_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(
        &path,
        serde_json::to_vec_pretty(&doc).expect("serialize results"),
    ) {
        Ok(()) => println!(
            "[prov-write] wrote JSON results artifact: {}",
            path.display()
        ),
        Err(e) => eprintln!("[prov-write] WARNING: could not write JSON artifact: {e}"),
    }
}

/// Apply the self-gate (AC3): fail (panic → non-zero exit) if any config's
/// same-run throughput ratio is below its declared bound, naming the offending
/// row.
fn apply_gate(results: &[ConfigResult]) {
    let mut failures: Vec<String> = Vec::new();
    for r in results {
        if r.config == Config::Baseline {
            continue;
        }
        let bound = r.config.ratio_bound();
        // NaN (measurement failure) must also fail the gate, hence the explicit
        // is_nan branch rather than a bare `<` comparison.
        if r.ratio_vs_baseline.is_nan() || r.ratio_vs_baseline < bound {
            failures.push(format!(
                "{}: ratio {:.3} < bound {:.2} (throughput {:.1}/s)",
                r.config.key(),
                r.ratio_vs_baseline,
                bound,
                r.throughput,
            ));
        }
    }
    if failures.is_empty() {
        println!("[prov-write] GATE PASS: all configs meet their same-run ratio bounds.");
    } else {
        panic!(
            "[prov-write] GATE FAIL ({} row(s) below bound):\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }
}

// ============================================================================
// Criterion arms — statistical per-config write throughput
// ============================================================================

/// Micro-benchmark each config's single-write path under Criterion. These give
/// the statistical throughput distribution; the standalone matrix (below)
/// supplies the p50/p99 and same-run ratios Criterion cannot.
fn bench_write_configs(c: &mut Criterion) {
    let mut group = c.benchmark_group("provenance_write_throughput");
    group.throughput(Throughput::Elements(1));

    for &config in &Config::ALL {
        let dir = TempDir::new().expect("temp dir");
        let db = build_db(dir.path());
        if config.has_constraint() {
            db.unique_constraint(BENCH_LABEL, UNIQUE_PROP)
                .enable()
                .expect("enable unique constraint");
        }
        let mut rng = SmallRng::seed_from_u64(WORKLOAD_SEED);
        group.bench_function(config.key(), |b| {
            b.iter(|| black_box(one_write(&db, config, &mut rng)));
        });
    }
    group.finish();
}

/// AC4 read spot-check: single-hop current-state read of a provenance-carrying
/// node (target < 1µs p99) and temporal reconstruction of a provenance-carrying
/// version (target < 10ms). These reuse the standard read paths; the point is
/// that carrying provenance does not slow the read hot path.
fn bench_read_spotchecks(c: &mut Criterion) {
    // Seed a durable DB with provenance-carrying nodes; update one repeatedly
    // to build reconstructable history.
    let dir = TempDir::new().expect("temp dir");
    let db = build_db(dir.path());
    let mut rng = SmallRng::seed_from_u64(WORKLOAD_SEED);

    let read_target = one_write(&db, Config::ProvenanceOnly, &mut rng);
    for _ in 0..32 {
        one_write(&db, Config::ProvenanceOnly, &mut rng);
    }

    // Capture a bi-temporal snapshot while the original provenance-carrying
    // version is still the tx-current head, then supersede the node several
    // times in later transactions. Reconstructing at this snapshot is a
    // transaction-time-travel read that returns the original (now superseded)
    // version — exercising the temporal reconstruction path.
    let t_snapshot = time::now();
    for _ in 0..8 {
        let prov = build_provenance(&mut rng);
        db.update_node_with_options(
            read_target,
            PropertyMapBuilder::new()
                .insert("seq", rng.gen_range(0..i64::MAX))
                .build(),
            WriteRequestOptions::new().with_provenance(prov),
        )
        .expect("update_node_with_options");
    }

    let mut group = c.benchmark_group("provenance_read_spotchecks");
    // AC4a: current-state single-hop read (<1µs p99 target).
    group.bench_function("current_read_provenance_node", |b| {
        b.iter(|| black_box(db.get_node(black_box(read_target)).expect("get_node")));
    });
    // AC4b: temporal reconstruction of a provenance-carrying version (<10ms).
    group.bench_function("temporal_reconstruct_provenance_version", |b| {
        b.iter(|| {
            black_box(
                db.get_node_at_time(black_box(read_target), t_snapshot, t_snapshot)
                    .expect("get_node_at_time"),
            )
        });
    });
    group.finish();
}

/// AC5 recovery spot-check: crash-recover a provenance + constraint dataset and
/// time the reopen. Runs at a reduced scale by default (`PROV_BENCH_RECOVERY_*`)
/// to stay light in sandboxed CI; the reference scale is 10K nodes / 50K edges
/// with a < 5s target (CI runs the full scale — see the guide).
fn bench_recovery_provenance(c: &mut Criterion) {
    let node_count: usize = std::env::var("PROV_BENCH_RECOVERY_NODES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000);
    let edge_count: usize = std::env::var("PROV_BENCH_RECOVERY_EDGES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4_000);

    // Populate a durable DB with provenance-carrying nodes + edges under an
    // active uniqueness constraint, then drop it so the WAL/index is on disk.
    let dir = TempDir::new().expect("temp dir");
    {
        let db = build_db(dir.path());
        db.unique_constraint(BENCH_LABEL, UNIQUE_PROP)
            .enable()
            .expect("enable unique constraint");
        let mut rng = SmallRng::seed_from_u64(WORKLOAD_SEED);
        let mut ids: Vec<NodeId> = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            ids.push(one_write(&db, Config::Composed, &mut rng));
        }
        for i in 0..edge_count {
            let src = ids[i % node_count];
            let tgt = ids[(i + 1) % node_count];
            let prov = build_provenance(&mut rng);
            db.create_edge_with_options(
                src,
                tgt,
                "CONNECTS",
                PropertyMapBuilder::new().insert("w", i as i64).build(),
                WriteRequestOptions::new().with_provenance(prov),
            )
            .expect("create_edge_with_options");
        }
        // Drop persists WAL/index to disk.
    }

    let mut group = c.benchmark_group("provenance_recovery");
    group.sample_size(10);
    group.bench_function(
        format!("reopen_{node_count}_nodes_{edge_count}_edges"),
        |b| {
            b.iter(|| {
                let db = build_db(dir.path());
                assert_eq!(db.node_count(), node_count);
                black_box(db);
            });
        },
    );
    group.finish();
}

/// The matrix driver: measures all four configs same-run, prints the table,
/// writes the JSON artifact, and (in `PROV_BENCH_GATE=1` mode) applies the
/// self-gate. Runs as a Criterion "benchmark" so it participates in
/// `cargo bench` and a gate panic yields a non-zero exit for CI.
fn bench_matrix_and_gate(_c: &mut Criterion) {
    let gate = std::env::var("PROV_BENCH_GATE").as_deref() == Ok("1");
    let inject = std::env::var("PROV_BENCH_INJECT_REGRESSION")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    let n_writes = matrix_writes(gate);
    let results = run_matrix(n_writes, inject);
    print_table(&results);
    write_json_artifact(&results, n_writes, gate);

    if gate {
        apply_gate(&results);
    }
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_matrix_and_gate,
        bench_write_configs,
        bench_read_spotchecks,
        bench_recovery_provenance,
);
criterion_main!(benches);
