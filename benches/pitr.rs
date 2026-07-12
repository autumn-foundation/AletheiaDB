//! Benchmark for point-in-time restore (PITR, Issue #3374).
//!
//! Times an end-to-end `AletheiaDB::restore_to_data_dir_at` — materialize a base
//! `.albk`, replay an archived WAL tail, and persist the target state — so the
//! <30s recovery metric can be checked with a real wall-clock number.
//!
//! The fixture aims toward the 10K-node / 50K-edge + WAL-tail AC target. The
//! synchronous fsync-per-commit source makes the *setup* the dominant cost, so
//! the sizes below are tunable constants: raise them toward the AC target on a
//! machine that can absorb the one-time build cost. The measured quantity — one
//! full PITR — is unchanged by the fixture size choice.

use std::path::Path;

use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::{AletheiaDB, DurabilityMode, PitrTarget, PropertyMapBuilder, Timestamp};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tempfile::TempDir;

/// Base graph size captured in the `.albk` (materialized on every restore).
/// Scale toward the 10K-node / 50K-edge AC target as the host allows.
const BASE_NODES: usize = 3_000;
const BASE_EDGES: usize = 6_000;
/// Post-backup transactions forming the archived WAL tail that PITR replays.
const POST_TX: usize = 800;

fn source_config(wal_dir: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir.to_path_buf())
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .build()
}

fn copy_wal_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            std::fs::copy(&path, dst.join(entry.file_name())).unwrap();
        }
    }
}

/// A built PITR fixture: a base backup + an archived WAL tail, plus the
/// mid-stream target timestamp to restore to.
struct Fixture {
    _tmp: TempDir,
    albk: std::path::PathBuf,
    archive: std::path::PathBuf,
    target: Timestamp,
}

fn build_fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let wal = tmp.path().join("wal");
    let db = AletheiaDB::with_unified_config(source_config(&wal)).unwrap();

    // Base graph captured in the `.albk`.
    let mut node_ids = Vec::with_capacity(BASE_NODES);
    for i in 0..BASE_NODES {
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("n{i}"))
                    .insert("phase", "base")
                    .build(),
            )
            .unwrap();
        node_ids.push(id);
    }
    for i in 0..BASE_EDGES {
        let a = node_ids[i % BASE_NODES];
        let b = node_ids[(i * 7 + 1) % BASE_NODES];
        db.create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
    }

    let albk = tmp.path().join("base.albk");
    db.backup(&albk).unwrap();

    // Post-backup WAL tail: the transactions PITR replays over the base.
    let mut target = None;
    for i in 0..POST_TX {
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("post{i}"))
                    .insert("phase", "post")
                    .build(),
            )
            .unwrap();
        if i == POST_TX / 2 {
            target = Some(db.get_node(id).unwrap().metadata.commit_timestamp.unwrap());
        }
    }

    let archive = tmp.path().join("archive");
    copy_wal_dir(&wal, &archive);
    drop(db);

    Fixture {
        _tmp: tmp,
        albk,
        archive,
        target: target.unwrap(),
    }
}

fn bench_pitr_restore(c: &mut Criterion) {
    let fixture = build_fixture();

    let mut group = c.benchmark_group("pitr_restore");
    // Restoring a multi-thousand-node base per iteration is expensive; a small
    // sample keeps the run bounded while still yielding a stable wall-clock.
    group.sample_size(10);

    group.bench_function(format!("{BASE_NODES}n_{BASE_EDGES}e_{POST_TX}tx"), |b| {
        b.iter(|| {
            // Each restore needs a fresh, empty target directory.
            let dst = TempDir::new().unwrap();
            let data_dir = dst.path().join("restored");
            let db = AletheiaDB::restore_to_data_dir_at(
                &fixture.albk,
                &fixture.archive,
                PitrTarget::AsOf(fixture.target),
                &data_dir,
            )
            .unwrap();
            black_box(db.node_count());
        });
    });

    group.finish();
}

criterion_group!(benches, bench_pitr_restore);
criterion_main!(benches);
