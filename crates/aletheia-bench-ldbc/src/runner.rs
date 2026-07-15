//! The benchmark runner: times each workload operation and assembles a
//! [`BenchmarkReport`].
//!
//! Timing uses a monotonic `Instant` per iteration, a warmup phase whose
//! samples are discarded, and `std::hint::black_box` around inputs and results
//! so the optimizer cannot elide the measured work. Inputs are varied
//! deterministically across iterations (cycling through a sample of ids) to
//! avoid single-key cache bias.

use crate::generator::{self, Scale};
use crate::loader::{self, LoadedGraph};
use crate::report::{
    BenchmarkReport, Category, HardwareInfo, OperationResult, RunConfig, format_rfc3339, now_unix,
};
use crate::stats::LatencyStats;
use aletheiadb::{AletheiaDB, NodeId, PropertyMapBuilder, SimilarityQuery, time};
use std::hint::black_box;
use std::time::Instant;

/// Options controlling a suite run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Scale point to generate and run.
    pub scale: Scale,
    /// PRNG seed.
    pub seed: u64,
    /// Warmup iterations (discarded).
    pub warmup: usize,
    /// Measured iterations per operation.
    pub iterations: usize,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            scale: Scale::Smoke,
            seed: 42,
            warmup: 20,
            iterations: 200,
        }
    }
}

/// Run the full suite: generate → load → benchmark → report.
///
/// # Errors
///
/// Returns any error from graph loading or the underlying database queries.
pub fn run_suite(opts: &RunOptions) -> aletheiadb::Result<BenchmarkReport> {
    let graph = generator::generate(opts.scale, opts.seed);
    let loaded = loader::load(&graph)?;

    let mut operations = Vec::new();
    operations.extend(short_reads(&loaded, opts));
    operations.extend(complex_reads(&loaded, opts));
    operations.extend(insert_update_stream(&loaded, opts)?);
    operations.extend(temporal_extension(&loaded, opts));
    operations.extend(vector_extension(&loaded, opts)?);

    let unix = now_unix();
    Ok(BenchmarkReport {
        schema_version: crate::report::SCHEMA_VERSION,
        suite: "ldbc-style-snb-subset".to_string(),
        disclaimer: "LDBC-STYLE, NOT AN AUDITED LDBC RESULT. Built-in synthetic \
             generator (not official LDBC Datagen); single-node; incumbents not run \
             in this environment. See docs/METHODOLOGY.md for every deviation."
            .to_string(),
        generated_at: format_rfc3339(unix),
        generated_at_unix: unix,
        hardware: HardwareInfo::capture(),
        config: RunConfig {
            scale: opts.scale.label().to_string(),
            seed: opts.seed,
            warmup: opts.warmup,
            iterations: opts.iterations,
            node_count: loaded.node_count,
            edge_count: loaded.edge_count,
            vector_count: loaded.vector_count,
            vector_dim: loaded.embedding_dim,
        },
        operations,
    })
}

/// Time an operation. `f` receives the measured-iteration index so it can vary
/// its input. Returns per-iteration latencies in microseconds.
fn bench<F: FnMut(usize)>(warmup: usize, iterations: usize, mut f: F) -> Vec<f64> {
    for i in 0..warmup {
        f(i);
    }
    let mut samples = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let start = Instant::now();
        f(i);
        let elapsed = start.elapsed();
        samples.push(elapsed.as_nanos() as f64 / 1000.0);
    }
    samples
}

/// Like [`bench`], but for fallible operations (writes, vector queries) whose
/// `Result` is otherwise discarded for timing purposes.
///
/// The **first measured iteration**'s result is checked: if it failed (a broken
/// setup, e.g. a missing vector index or a rejected write) the error propagates
/// as a harness error instead of being silently timed as an error path. The
/// remaining measured iterations run in the same unbranched hot loop as
/// [`bench`], with their results discarded. Warmup results are always discarded.
fn bench_guarded<F>(warmup: usize, iterations: usize, mut f: F) -> aletheiadb::Result<Vec<f64>>
where
    F: FnMut(usize) -> aletheiadb::Result<()>,
{
    for i in 0..warmup {
        let _ = f(i);
    }
    let mut samples = Vec::with_capacity(iterations);
    if iterations > 0 {
        // First measured iteration: time it, then assert it succeeded so a
        // broken setup fails loudly rather than being timed as an error path.
        let start = Instant::now();
        let r = f(0);
        let elapsed = start.elapsed();
        r?;
        samples.push(elapsed.as_nanos() as f64 / 1000.0);
        // Remaining iterations: unbranched hot loop, results discarded.
        for i in 1..iterations {
            let start = Instant::now();
            let _ = f(i);
            let elapsed = start.elapsed();
            samples.push(elapsed.as_nanos() as f64 / 1000.0);
        }
    }
    Ok(samples)
}

/// Build an [`OperationResult`] from a timing run.
#[allow(clippy::too_many_arguments)]
fn op(
    key: &str,
    description: &str,
    snb_analogue: &str,
    category: Category,
    iterations: usize,
    samples: Vec<f64>,
    extra: Vec<(&str, serde_json::Value)>,
) -> OperationResult {
    let mut map = serde_json::Map::new();
    for (k, v) in extra {
        map.insert(k.to_string(), v);
    }
    OperationResult {
        key: key.to_string(),
        description: description.to_string(),
        snb_analogue: snb_analogue.to_string(),
        category,
        iterations,
        stats: LatencyStats::from_samples(&samples),
        extra: map,
    }
}

// ---- Traversal helpers (built on the public adjacency API) ----

/// Friends of a person: `KNOWS` out-neighbours.
fn friends(db: &AletheiaDB, person: NodeId) -> Vec<NodeId> {
    db.get_outgoing_edges_with_label(person, "KNOWS")
        .into_iter()
        .filter_map(|e| db.get_edge_target(e).ok())
        .collect()
}

/// Messages authored by a person: sources of incoming `HAS_CREATOR` edges.
fn messages_by(db: &AletheiaDB, person: NodeId) -> Vec<NodeId> {
    db.get_incoming_edges_with_label(person, "HAS_CREATOR")
        .into_iter()
        .filter_map(|e| db.get_edge_source(e).ok())
        .collect()
}

/// Friends within two hops (node-distinct BFS), excluding the root.
fn friends_2hop(db: &AletheiaDB, person: NodeId) -> Vec<NodeId> {
    let mut seen = std::collections::BTreeSet::new();
    seen.insert(person);
    let mut frontier = vec![person];
    for _ in 0..2 {
        let mut next = Vec::new();
        for &n in &frontier {
            for f in friends(db, n) {
                if seen.insert(f) {
                    next.push(f);
                }
            }
        }
        frontier = next;
    }
    seen.remove(&person);
    seen.into_iter().collect()
}

fn pick<T: Copy>(items: &[T], i: usize) -> Option<T> {
    if items.is_empty() {
        None
    } else {
        Some(items[i % items.len()])
    }
}

// ---- Short reads (SNB IS-series analogues) ----

fn short_reads(g: &LoadedGraph, opts: &RunOptions) -> Vec<OperationResult> {
    let db = &g.db;
    let mut out = Vec::new();

    out.push(op(
        "is1_person_profile",
        "Fetch a single person's profile node",
        "IS1",
        Category::ShortRead,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(p) = pick(&g.person_ids, i) {
                let _ = black_box(db.get_node(p));
            }
        }),
        vec![],
    ));

    out.push(op(
        "is2_person_recent_messages",
        "List messages (posts/comments) authored by a person",
        "IS2",
        Category::ShortRead,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(p) = pick(&g.person_ids, i) {
                let _ = black_box(messages_by(db, p));
            }
        }),
        vec![],
    ));

    out.push(op(
        "is3_person_friends",
        "List a person's direct KNOWS friends",
        "IS3",
        Category::ShortRead,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(p) = pick(&g.person_ids, i) {
                let _ = black_box(friends(db, p));
            }
        }),
        vec![],
    ));

    out.push(op(
        "is5_message_creator",
        "Resolve the creator of a post (HAS_CREATOR)",
        "IS5",
        Category::ShortRead,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(post) = pick(&g.post_ids, i) {
                let creator = db
                    .get_outgoing_edges_with_label(post, "HAS_CREATOR")
                    .into_iter()
                    .filter_map(|e| db.get_edge_target(e).ok())
                    .next();
                if let Some(c) = creator {
                    let _ = black_box(db.get_node(c));
                }
            }
        }),
        vec![],
    ));

    out.push(op(
        "is6_forum_of_message",
        "Resolve the forum containing a post (CONTAINER_OF)",
        "IS6",
        Category::ShortRead,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(post) = pick(&g.post_ids, i) {
                let forum = db
                    .get_incoming_edges_with_label(post, "CONTAINER_OF")
                    .into_iter()
                    .filter_map(|e| db.get_edge_source(e).ok())
                    .next();
                let _ = black_box(forum);
            }
        }),
        vec![],
    ));

    out
}

// ---- Complex reads (SNB IC-series analogues) ----

fn complex_reads(g: &LoadedGraph, opts: &RunOptions) -> Vec<OperationResult> {
    let db = &g.db;
    let mut out = Vec::new();

    out.push(op(
        "ic1_friends_within_2_hops",
        "Friends of friends within two hops (node-distinct BFS)",
        "IC1",
        Category::ComplexRead,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(p) = pick(&g.person_ids, i) {
                let _ = black_box(friends_2hop(db, p));
            }
        }),
        vec![],
    ));

    out.push(op(
        "ic2_recent_messages_by_friends",
        "Messages authored by a person's direct friends",
        "IC2",
        Category::ComplexRead,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(p) = pick(&g.person_ids, i) {
                let mut msgs = Vec::new();
                for f in friends(db, p) {
                    msgs.extend(messages_by(db, f));
                }
                let _ = black_box(msgs);
            }
        }),
        vec![],
    ));

    out.push(op(
        "ic9_messages_by_friends_of_friends",
        "Messages authored by friends-of-friends (2-hop) of a person",
        "IC9",
        Category::ComplexRead,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(p) = pick(&g.person_ids, i) {
                let mut msgs = Vec::new();
                for f in friends_2hop(db, p) {
                    msgs.extend(messages_by(db, f));
                }
                let _ = black_box(msgs);
            }
        }),
        vec![],
    ));

    out
}

// ---- Insert/update stream (SNB INS-series analogues) ----

fn insert_update_stream(
    g: &LoadedGraph,
    opts: &RunOptions,
) -> aletheiadb::Result<Vec<OperationResult>> {
    let db = &g.db;
    let mut out = Vec::new();

    out.push(op(
        "ins1_insert_person",
        "Insert a new Person node",
        "INS1",
        Category::InsertUpdate,
        opts.iterations,
        bench_guarded(opts.warmup, opts.iterations, |i| {
            let id = db.create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("StreamPerson{i}").as_str())
                    .insert("city", "Springfield")
                    .build(),
            )?;
            black_box(id);
            Ok(())
        })?,
        vec![],
    ));

    out.push(op(
        "ins6_insert_post",
        "Insert a new Post node with a creator edge",
        "INS6",
        Category::InsertUpdate,
        opts.iterations,
        bench_guarded(opts.warmup, opts.iterations, |i| {
            let Some(creator) = pick(&g.person_ids, i) else {
                return Ok(());
            };
            let post = db.create_node(
                "Post",
                PropertyMapBuilder::new().insert("length", 100i64).build(),
            )?;
            let edge = db.create_edge(
                post,
                creator,
                "HAS_CREATOR",
                PropertyMapBuilder::new().build(),
            )?;
            black_box(edge);
            Ok(())
        })?,
        vec![],
    ));

    out.push(op(
        "upd2_update_person_city",
        "Update a person's city property (bi-temporal revision)",
        "UPD-PERSON",
        Category::InsertUpdate,
        opts.iterations,
        bench_guarded(opts.warmup, opts.iterations, |i| {
            let Some(p) = pick(&g.person_ids, i) else {
                return Ok(());
            };
            // Returns `()`; the write's DB mutation is an observable side
            // effect, so no `black_box` is needed to prevent elision.
            db.update_node_with_valid_time(
                p,
                PropertyMapBuilder::new()
                    .insert("city", "Rivertown")
                    .build(),
                None,
            )?;
            Ok(())
        })?,
        vec![],
    ));

    Ok(out)
}

// ---- Temporal extension (NON-STANDARD) ----

fn temporal_extension(g: &LoadedGraph, opts: &RunOptions) -> Vec<OperationResult> {
    let db = &g.db;
    let now = time::now();
    let mut out = Vec::new();

    // Point-in-time reconstruction of a person's historical state. Target: <10ms.
    out.push(op(
        "ext_temporal_reconstruction",
        "Point-in-time reconstruction of a revised person's historical state (NON-STANDARD)",
        "EXT-TEMPORAL",
        Category::TemporalExtension,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(pi) = pick(&g.revised_persons, i) {
                let node = g.person_ids[pi];
                let _ = black_box(db.get_node_at_time(node, g.early_ts, now));
            }
        }),
        vec![
            ("reconstruction_target_ms", serde_json::json!(10)),
            (
                "revised_person_count",
                serde_json::json!(g.revised_persons.len()),
            ),
        ],
    ));

    // AS OF traversal: a person's KNOWS neighbours as they were at `early_ts`.
    out.push(op(
        "ext_temporal_as_of_traversal",
        "AS OF traversal of KNOWS edges at a historical valid time (NON-STANDARD)",
        "EXT-TEMPORAL",
        Category::TemporalExtension,
        opts.iterations,
        bench(opts.warmup, opts.iterations, |i| {
            if let Some(p) = pick(&g.person_ids, i) {
                let edges = db.get_outgoing_edges_at_time(p, g.early_ts, now);
                let neighbours: Vec<_> = edges
                    .into_iter()
                    .filter_map(|e| db.get_edge_at_time(e, g.early_ts, now).ok())
                    .collect();
                let _ = black_box(neighbours);
            }
        }),
        vec![],
    ));

    out
}

// ---- Vector extension (NON-STANDARD) ----

fn vector_extension(
    g: &LoadedGraph,
    opts: &RunOptions,
) -> aletheiadb::Result<Vec<OperationResult>> {
    let db = &g.db;
    let dim = g.embedding_dim;
    let mut out = Vec::new();

    // k-NN over the post embeddings. Target: <10ms (at the scale actually run,
    // reported honestly — NOT the aspirational 1M-vector target).
    out.push(op(
        "ext_vector_knn",
        "k-NN (k=10) over post embeddings (NON-STANDARD)",
        "EXT-VECTOR",
        Category::VectorExtension,
        opts.iterations,
        bench_guarded(opts.warmup, opts.iterations, |i| {
            // Perturb the query embedding slightly per iteration so we are not
            // re-issuing one identical query.
            let mut q = g.query_embedding.clone();
            if !q.is_empty() {
                q[i % dim.max(1)] += 0.001 * (i as f32);
            }
            let hits = db.similarity_search(SimilarityQuery::from_embedding(q).k(10))?;
            black_box(hits);
            Ok(())
        })?,
        vec![
            ("knn_k", serde_json::json!(10)),
            ("vector_count", serde_json::json!(g.vector_count)),
            ("vector_dim", serde_json::json!(dim)),
            (
                "target_scale_note",
                serde_json::json!(
                    "1M-vector target is ASPIRATIONAL / NOT RUN in this environment; \
                 vector_count is the scale actually measured"
                ),
            ),
        ],
    ));

    // Hybrid graph+vector: traverse a forum's posts and rank them by embedding
    // similarity in one call.
    out.push(op(
        "ext_vector_hybrid_graph_vector",
        "Hybrid: traverse Forum CONTAINER_OF Posts then rank by embedding similarity (NON-STANDARD)",
        "EXT-VECTOR",
        Category::VectorExtension,
        opts.iterations,
        bench_guarded(opts.warmup, opts.iterations, |i| {
            let Some(forum) = pick(&g.forum_ids, i) else {
                return Ok(());
            };
            let ranked = db.traverse_and_rank(forum, "CONTAINER_OF", &g.query_embedding, 10)?;
            black_box(ranked);
            Ok(())
        })?,
        vec![("knn_k", serde_json::json!(10))],
    ));

    Ok(out)
}
