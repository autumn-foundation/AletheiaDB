//! Fusion scoring micro-benchmark (Issue #3372).
//!
//! Measures the cost of the pure `FusionPolicy::fuse()` core over a synthetic
//! candidate pool — the per-candidate work `similarity_search_fused` does after
//! the vector over-fetch. This is a lightweight, DB-free micro-bench: it isolates
//! the arithmetic + breakdown-construction hot loop, not the vector search or the
//! metadata reads.
//!
//! **Scope:** this validates the fuse-core is cheap (well under a microsecond per
//! candidate). The full graduation gate — fused k-NN p99 < 20 ms at 1M vectors
//! with provenance on 100% of candidates — rides with the #3366 eval harness,
//! which does not exist in-tree yet (see
//! `docs/plans/2026-07-18-3372-provenance-weighted-retrieval.md` §7).

use aletheiadb::FusionPolicy;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// A synthetic candidate: (similarity, confidence, recency), covering the
/// defaulted-confidence path (`None`) as well.
fn synthetic_candidates(n: usize) -> Vec<(f64, Option<f64>, f64)> {
    (0..n)
        .map(|i| {
            let f = (i as f64) / (n as f64);
            let confidence = if i % 7 == 0 { None } else { Some(1.0 - f) };
            (f, confidence, 1.0 - f * 0.5)
        })
        .collect()
}

fn bench_fuse_core(c: &mut Criterion) {
    let policy = FusionPolicy::builder()
        .w_similarity(1.0)
        .w_confidence(1.0)
        .w_recency(1.0)
        .build()
        .expect("valid policy");

    // Small synthetic pool — the fuse-core is O(candidates) with a tiny constant.
    let candidates = synthetic_candidates(256);

    c.bench_function("fusion_fuse_core_256", |b| {
        b.iter(|| {
            let mut acc = 0.0f64;
            for &(s, conf, r) in &candidates {
                let breakdown = policy.fuse(black_box(s), black_box(conf), black_box(r));
                acc += breakdown.fused;
            }
            black_box(acc)
        });
    });
}

criterion_group!(benches, bench_fuse_core);
criterion_main!(benches);
