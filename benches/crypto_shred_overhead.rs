//! Crypto-shred designation overhead + non-designated zero-regression
//! benchmarks (Issue #3665, AC8).
//!
//! These criterion benches quantify the two AC8 guarantees:
//!
//! 1. **Designated-write overhead** — the latency/throughput cost of a write to
//!    a crypto-shred *designated* (sealed) property versus a plain,
//!    non-designated write, plus a mixed workload at 0% vs 10% designated so the
//!    throughput-retention ratio is derivable. Target: **>=90% write throughput
//!    retained at 10% designated**.
//! 2. **Non-designated zero-regression** — single-hop traversal latency on
//!    non-designated data (target **<2us single-hop**) and non-designated write
//!    throughput matched against a crypto-shred-*disabled* baseline.
//!
//! A fourth group, `seal_overhead_isolated`, measures the incremental **crypto**
//! cost a designated write adds (encode + AEAD seal) in a tight loop with fixed
//! inputs prepared outside the timed closure and **no** disk / WAL / scheduler
//! involvement. Its µs-scale delta is trustworthy even on a loaded shared host,
//! where the full-path medians above are dominated by scheduling + WAL noise —
//! so the designated-write overhead behind the >=90%-throughput target has a
//! clean number regardless of host load.
//!
//! ## Timing notes
//!
//! All benches run against an **in-process** database configured with WAL in
//! `Async` durability mode (no per-commit fsync wait), so the measured cost
//! reflects the seal/unseal work rather than disk latency. The *ratio* between
//! the designated and plain variants is far more robust to a noisy host than the
//! absolute medians — read the ratios, not the microsecond values, when the
//! sandbox is under load.
//!
//! The write benches use criterion's `iter_batched`: each timed iteration
//! replaces a **fresh** node (or fresh batch of nodes) created untimed in the
//! setup step. This isolates a single sealed-vs-plain write and keeps any one
//! node well under the per-node version cap, so the plain and designated sides
//! differ only in the seal work — the comparison stays apples-to-apples.

use std::hint::black_box;

use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::core::NodeId;
use aletheiadb::db::DesignationTarget;
use aletheiadb::db::crypto_shred::envelope;
use aletheiadb::encryption::{Aes256GcmCipher, Cipher, EncryptionConfig, FileKeyProvider};
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, PropertyMap, PropertyMapBuilder, PropertyValue};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tempfile::TempDir;
use zeroize::Zeroizing;

/// A representative designated payload: a handful of string/int properties that
/// a `WholeNode` designation seals on every write.
fn sealed_payload(tag: i64) -> PropertyMap {
    PropertyMapBuilder::new()
        .insert("email", "alice.example.person@example-corp.test")
        .insert("full_name", "Alice Q. Example")
        .insert("ssn", "123-45-6789")
        .insert("revision", tag)
        .build()
}

/// Build an **encrypted** (crypto-shred-capable) in-process database rooted at a
/// fresh tempdir, with WAL in `Async` mode so fsync latency does not swamp the
/// seal cost. Returns the DB and the owning tempdir (kept alive by the caller).
fn shred_capable_db() -> (AletheiaDB, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix("aletheia-ac8-enc-")
        .tempdir()
        .expect("tempdir");
    let key_file = dir.path().join("mek.key");
    FileKeyProvider::generate_key_file(&key_file).expect("generate mek");
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(dir.path().join("wal"))
                .durability_mode(DurabilityMode::Async {
                    // 1h: these benches are ephemeral/in-process, so no periodic
                    // durability flush is needed; a long interval keeps a
                    // background WAL flush from firing inside the measurement
                    // window, for more stable, reproducible timings.
                    flush_interval_ms: 3_600_000,
                })
                .build(),
        )
        .encryption(EncryptionConfig::file_based(&key_file))
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("encrypted db");
    (db, dir)
}

/// Build a crypto-shred-*disabled* baseline database (no encryption config, so
/// designation is impossible and every seal/unseal hook is compiled out of the
/// hot path). Same `Async` WAL config as [`shred_capable_db`] so the only
/// difference is the presence of the crypto-shred subsystem.
fn disabled_baseline_db() -> (AletheiaDB, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix("aletheia-ac8-plain-")
        .tempdir()
        .expect("tempdir");
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(dir.path().join("wal"))
                .durability_mode(DurabilityMode::Async {
                    // 1h: see the note in `shred_capable_db` — a long flush
                    // interval keeps a background WAL flush out of the
                    // measurement window for reproducible timings.
                    flush_interval_ms: 3_600_000,
                })
                .build(),
        )
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("baseline db");
    (db, dir)
}

/// Create a node and return its id.
fn seed_node(db: &AletheiaDB) -> NodeId {
    db.create_node("Person", sealed_payload(0)).expect("create")
}

// ── Group 1: designated vs plain single-node write ─────────────────────────

/// Create a fresh node and designate it under a fresh single-target subject, so
/// the subsequent write to it is sealed. Each call uses a unique node + subject
/// so `subjects_for_entity` stays an O(1) lookup and no single node approaches
/// the per-node version cap. `nonce` disambiguates the subject id.
fn make_designated_node(db: &AletheiaDB, nonce: u64) -> NodeId {
    let id = db.create_node("Person", sealed_payload(0)).expect("create");
    db.designate_subject(
        format!("subject-{nonce}"),
        vec![DesignationTarget::WholeNode(id.as_u64())],
    )
    .expect("designate");
    id
}

/// AC8.1: latency of a write to a DESIGNATED (sealed) property vs a plain write,
/// plus the crypto-shred-disabled baseline for the zero-regression comparison.
///
/// Each timed iteration replaces a **fresh** node (produced untimed in the
/// `iter_batched` setup), so the measurement isolates a single write and never
/// grows one node past the version cap.
fn bench_designated_vs_plain_node_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("designated_vs_plain_node_write");

    let (enc_db, _enc_dir) = shred_capable_db();
    let (base_db, _base_dir) = disabled_baseline_db();
    let mut nonce: u64 = 0;

    // Plain write on the crypto-shred-capable DB (no designation → no seal work,
    // exercises the has_active_designations() short-circuit).
    group.bench_function("plain_write_shred_capable", |b| {
        b.iter_batched(
            || {
                enc_db
                    .create_node("Person", sealed_payload(0))
                    .expect("create")
            },
            |id| {
                enc_db
                    .replace_node(black_box(id), "Person", sealed_payload(1))
                    .expect("plain write");
            },
            BatchSize::SmallInput,
        )
    });

    // Designated write: the same replace_node, but every non-reserved property
    // is sealed under the subject key before it is persisted.
    group.bench_function("designated_write", |b| {
        b.iter_batched(
            || {
                nonce += 1;
                make_designated_node(&enc_db, nonce)
            },
            |id| {
                enc_db
                    .replace_node(black_box(id), "Person", sealed_payload(1))
                    .expect("designated write");
            },
            BatchSize::SmallInput,
        )
    });

    // Plain write on the crypto-shred-disabled baseline (subsystem absent).
    group.bench_function("plain_write_disabled_baseline", |b| {
        b.iter_batched(
            || {
                base_db
                    .create_node("Person", sealed_payload(0))
                    .expect("create")
            },
            |id| {
                base_db
                    .replace_node(black_box(id), "Person", sealed_payload(1))
                    .expect("baseline write");
            },
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

// ── Group 2: mixed workload, 0% vs 10% designated ──────────────────────────

/// Number of writes in one mixed-workload batch. Throughput is reported in
/// elements (writes) per second so the 10%-vs-0% ratio is directly derivable.
const BATCH: usize = 100;

/// Build a batch of `BATCH` fresh nodes on `db`; designate the first
/// `designated_count` of them under one subject (`nonce`-disambiguated).
fn build_batch(db: &AletheiaDB, designated_count: usize, nonce: u64) -> Vec<NodeId> {
    let ids: Vec<NodeId> = (0..BATCH)
        .map(|_| db.create_node("Person", sealed_payload(0)).expect("create"))
        .collect();
    if designated_count > 0 {
        let targets: Vec<DesignationTarget> = ids
            .iter()
            .take(designated_count)
            .map(|id| DesignationTarget::WholeNode(id.as_u64()))
            .collect();
        db.designate_subject(format!("subject-mixed-{nonce}"), targets)
            .expect("designate batch");
    }
    ids
}

/// AC8.1 (throughput): a batch of `BATCH` writes at 0% vs 10% designated. The
/// ratio of the two throughputs is the "write throughput retained at 10%
/// designated" figure (target >=90%). Each timed iteration writes a fresh batch
/// (built untimed in setup), so no node approaches the version cap.
fn bench_mixed_workload_designation_ratio(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_workload_designation_ratio");
    group.throughput(Throughput::Elements(BATCH as u64));

    let (db0, _dir0) = shred_capable_db();
    let (db10, _dir10) = shred_capable_db();
    let designated_count = BATCH / 10; // 10%
    let mut nonce: u64 = 0;

    // 0% designated: none of the batch is sealed.
    group.bench_with_input(BenchmarkId::new("designated_pct", 0), &(), |b, _| {
        b.iter_batched(
            || {
                nonce += 1;
                build_batch(&db0, 0, nonce)
            },
            |ids| {
                for id in &ids {
                    db0.replace_node(*id, "Person", sealed_payload(1))
                        .expect("write 0pct");
                }
                black_box(ids.len())
            },
            BatchSize::SmallInput,
        )
    });

    // 10% designated: the first 10% of the batch is sealed on write.
    group.bench_with_input(BenchmarkId::new("designated_pct", 10), &(), |b, _| {
        b.iter_batched(
            || {
                nonce += 1;
                build_batch(&db10, designated_count, nonce)
            },
            |ids| {
                for id in &ids {
                    db10.replace_node(*id, "Person", sealed_payload(1))
                        .expect("write 10pct");
                }
                black_box(ids.len())
            },
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

// ── Group 3: non-designated single-hop traversal (<2us) ────────────────────

/// AC8.2: single-hop traversal latency on non-designated data. Built on the
/// crypto-shred-capable DB (subsystem present, no designations) and, for
/// contrast, on the disabled baseline. Target: <2us single-hop.
fn bench_non_designated_single_hop(c: &mut Criterion) {
    let mut group = c.benchmark_group("non_designated_single_hop");

    // Crypto-shred-capable DB, non-designated graph: n1 -> n2, n1 -> n3, n1 -> n4.
    let (enc_db, _enc_dir) = shred_capable_db();
    let src = seed_node(&enc_db);
    for _ in 0..3 {
        let dst = seed_node(&enc_db);
        enc_db
            .create_edge(src, dst, "KNOWS", PropertyMapBuilder::new().build())
            .expect("edge");
    }

    // Disabled baseline graph.
    let (base_db, _base_dir) = disabled_baseline_db();
    let base_src = seed_node(&base_db);
    for _ in 0..3 {
        let dst = seed_node(&base_db);
        base_db
            .create_edge(base_src, dst, "KNOWS", PropertyMapBuilder::new().build())
            .expect("edge");
    }

    group.bench_function("shred_capable", |b| {
        b.iter(|| {
            let n = enc_db.get_outgoing_edges_iter(black_box(src)).count();
            black_box(n)
        })
    });

    group.bench_function("disabled_baseline", |b| {
        b.iter(|| {
            let n = base_db.get_outgoing_edges_iter(black_box(base_src)).count();
            black_box(n)
        })
    });

    group.finish();
}

// ── Group 4: isolated per-seal crypto overhead (host-noise-robust) ─────────

/// The designated properties one `WholeNode` write seals: the non-reserved
/// values of [`sealed_payload`]. Returned as owned `PropertyValue`s so the
/// timed closures below never touch the DB, WAL, or scheduler — only the
/// encode + AEAD-seal work that a designated write layers on top of a plain
/// write. Mirrors `sealed_payload(1)` so the isolated cost lines up with the
/// full-path `designated_write` bench.
fn designated_values() -> Vec<PropertyValue> {
    vec![
        PropertyValue::String("alice.example.person@example-corp.test".into()),
        PropertyValue::String("Alice Q. Example".into()),
        PropertyValue::String("123-45-6789".into()),
        PropertyValue::Int(1),
    ]
}

/// AC8.1 (isolated): the incremental **crypto** cost a designated write adds,
/// measured with zero disk / WAL / scheduler involvement so it is trustworthy
/// even when the shared sandbox is loaded (unlike the full-path medians, whose
/// absolute values are dominated by host noise).
///
/// The AEAD cipher, subject id, key version, write-site context, and the fixed
/// property values are all prepared **outside** the timed closures, so each
/// iteration times only `serialize` (plain path) or `seal_property_value`
/// (designated path). The delta between the two is the per-property seal
/// overhead; summed over a `WholeNode` payload it is the crypto cost of one
/// designated write. Compare this µs figure against the plain per-write cost
/// from `designated_vs_plain_node_write` to reason about the >=90%-throughput
/// target directly, without the host-noise floor.
fn bench_seal_overhead_isolated(c: &mut Criterion) {
    let mut group = c.benchmark_group("seal_overhead_isolated");

    // Fixed, deterministic subject cipher + AAD binding, built once.
    let key = Zeroizing::new([0x5Au8; 32]);
    let cipher = Aes256GcmCipher::new(&key);
    let cipher_ref: &dyn Cipher = &cipher;
    let subject_id = "bench-subject";
    let key_version: u32 = 1;
    // Representative write-site context (`entity_id` LE || property-key bytes),
    // shaped like the real seal-site binding; its length barely moves AEAD cost.
    let context: &[u8] = b"\x01\x00\x00\x00\x00\x00\x00\x00email";

    let values = designated_values();
    let single = values[0].clone(); // one representative string property

    // Plain path: encode a single property value (what a non-designated write
    // persists) — no AEAD.
    group.bench_function("plain_encode_property", |b| {
        b.iter(|| {
            let bytes = black_box(&single).serialize().expect("encode");
            black_box(bytes)
        })
    });

    // Designated path: encode + AEAD-seal a single property value. The delta
    // over `plain_encode_property` is the per-property seal overhead.
    group.bench_function("seal_property", |b| {
        b.iter(|| {
            let env = envelope::seal_property_value(
                black_box(&single),
                subject_id,
                key_version,
                context,
                cipher_ref,
            )
            .expect("seal");
            black_box(env)
        })
    });

    // Plain path over a whole `WholeNode` payload: encode every designated value.
    group.bench_function("plain_encode_payload", |b| {
        b.iter(|| {
            for v in &values {
                black_box(black_box(v).serialize().expect("encode"));
            }
        })
    });

    // Designated path over a whole `WholeNode` payload: seal every value. This
    // is the crypto work one designated write does on top of a plain write.
    group.bench_function("seal_payload", |b| {
        b.iter(|| {
            for v in &values {
                let env = envelope::seal_property_value(
                    black_box(v),
                    subject_id,
                    key_version,
                    context,
                    cipher_ref,
                )
                .expect("seal");
                black_box(env);
            }
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_designated_vs_plain_node_write,
    bench_mixed_workload_designation_ratio,
    bench_non_designated_single_hop,
    bench_seal_overhead_isolated,
);
criterion_main!(benches);
