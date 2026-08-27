# Making AletheiaDB Multi-Reader and Multi-Writer

**Status**: design exploration — no implementation proposed for merge yet
**Date**: 2026-08-26
**Harness**: [`examples/concurrency_scaling.rs`](../../examples/concurrency_scaling.rs)

## TL;DR

AletheiaDB is not currently multi-reader or multi-writer *inside a single
process*, and the reasons are four specific, separable serialization points —
not a diffuse "it needs a rewrite" problem. One of them is an outright bug with
a one-line-ish fix. Measured on a 4-core box:

| Path | 1 thread | 4 threads | scaling |
|------|---------:|----------:|--------:|
| `write/group_commit` | 95/sec | 94/sec | **0.98x** |
| `write/async` | 175K/sec | 68K/sec | **0.39x** |
| `read/snapshot` (`read_transaction`) | 4.4M/sec | 444K/sec | **0.10x** |
| `read/current` (`get_node`) | 18.2M/sec | 4.9M/sec | **0.27x** |
| *control: pure CPU, no DB* | — | — | *3.80x* |

Every path is flat or **negative**. The control confirms the box itself scales
3.8x on 4 cores, so this is AletheiaDB, not the environment.

Connection pools do not appear anywhere in the fix list, and that is the
interesting part — see [On connection pools](#on-connection-pools). The short
version: there is no "connection" object in an embedded Rust database to pool.
The thing you are reaching for is real, but it is **write admission control**,
and it only becomes meaningful *after* Stage 3 below.

## First, three different questions wearing the same name

"Multi-reader, multi-writer" is three separate problems here, and the repo has
already solved two of them:

| Axis | Question | Status |
|------|----------|--------|
| **Multi-process** | Can N OS processes share one data directory? | **Solved, by refusing.** `daemon_lock.rs` makes one process the owner; everyone else connects to it over HTTP/MCP (Issue #2905). This is the right call — it is what real embedded stores do. |
| **Multi-node** | Can reads scale across machines? | **Solved.** Async replication gives read scale-out and a warm standby (Issue #3355). |
| **Multi-threaded** | Inside the one owning process, do concurrent readers and writers actually run concurrently? | **This is the gap.** |

That framing matters, because the daemon means *all* concurrency now funnels
into a single process. Every MCP session, every HTTP request, every replica
read lands on one `Arc<AletheiaDB>`. Whatever that shared object's internal
concurrency is, it is now the concurrency of the entire product. Right now it
is approximately one.

## The measured baseline

Reproduce with:

```bash
cargo run --release --example concurrency_scaling
```

Measured on a 4-core Firecracker VM, no cgroup CPU quota (`cpu.cfs_quota_us =
-1`). **Treat the ratios as the result and the absolute numbers as
environment-specific.** The harness releases all worker threads from a
`Barrier` (which parks rather than spins, so synchronization does not steal
cores) and sizes each workload so the timed window is long enough that
`thread::join` overhead is not what is being measured — an earlier draft of
this harness timed 0.4ms windows and produced garbage.

```
write/group_commit  (GroupCommit { max_delay_ms: 10, max_batch_size: 200 })
  threads         ops/sec     scaling         µs/op
        1              95       1.00x      10504.95
        2              95       1.00x      21050.52
        4              94       0.98x      42695.56
        8              95       1.00x      84400.61

write/async         (Async { flush_interval_ms: 1 })
        1          175496       1.00x          5.70
        2           61691       0.35x         32.42
        4           67878       0.39x         58.93
        8           65224       0.37x        122.65

read/snapshot  (read_transaction + get_node)
        1         4405275       1.00x          0.23
        2          629934       0.14x          3.17
        4          444503       0.10x          9.00
        8         1029885       0.23x          7.77

read/current   (db.get_node -> clones Node + bumps PropertyMap Arc)
        1        18219665       1.00x          0.05
        4         4952901       0.27x          0.81

read/borrow    (db.with_node, zero-copy, shared hot set)
        1        58634263       1.00x          0.02
        4         6171424       0.11x          0.65
```

The single-threaded numbers are excellent — 17ns for a zero-copy point read,
comfortably inside the documented 22–70ns. Nothing here is a claim that the
engine is slow. It is a claim that it is **exactly as fast with four cores as
with one**, and sometimes slower.

## Diagnosis: four serialization points

### 1. Every node and edge lives in DashMap shard 0 (a bug)

`CurrentIndexes` stores nodes and edges in `DashMap<NodeId, Node,
IdHashBuilder>` (`src/index/current.rs:55`). `IdHashBuilder` is
`BuildHasherDefault<IdentityHasher>` — the key's `u64` passed straight through,
introduced to avoid SipHash on the point-lookup hot path. That part is sound.

The problem is how DashMap picks a shard (`dashmap-6.1.0/src/lib.rs:429`):

```rust
pub(crate) fn determine_shard(&self, hash: usize) -> usize {
    // Leave the high 7 bits for the HashBrown SIMD tag.
    (hash << 7) >> self.shift
}
```

with `shift = 64 - log2(shard_amount)`. It selects the shard from the **high**
bits of the hash. With an identity hasher the hash *is* the node id, and node
ids are small sequential integers — so the high bits are all zero. On a 4-core
box (`shard_amount = 16`, `shift = 60`), a node only reaches shard 1 once its
id exceeds **2^53**. In practice every node and every edge in the database is
in shard 0. Fifteen of sixteen shards are permanently empty.

So all current-storage concurrency — reads *and* writes — serializes on a
single `RwLock` and ping-pongs a single cache line. And it gets **worse on
bigger machines**: a 64-core server allocates 256 shards and uses 1 of them.

Verified directly, outside AletheiaDB, on a bare `DashMap<u64,u64>` with 1000
sequential keys. These are *isolated-control* numbers used to identify the
mechanism — for the in-engine effect see [Stage 1, as landed](#stage-1-as-landed):

```
shards available = 16  |  shards occupied by ids 0..1000:
    identity = 1, fibonacci = 16, default(SipHash) ~ 16

identity-hashed DashMap  (AletheiaDB's configuration)
  threads  1:  60584948 ops/sec  1.01x
  threads  2:   7424810 ops/sec  0.12x     <-- collapses
  threads  4:   6455404 ops/sec  0.11x

fibonacci-hashed DashMap  (candidate fix)
  threads  1:  83655358 ops/sec  1.01x     <-- also faster single-threaded
  threads  2:  22937962 ops/sec  0.28x
  threads  4:  22271704 ops/sec  0.27x     <-- 3.5x better at 4 threads
```

The identity curve (60M → 7.4M → 6.5M) matches AletheiaDB's measured
`read/borrow` curve (58M → 7.3M → 6.2M) almost exactly. That is the whole
explanation for the current-state read collapse.

The fix is **not** to drop the cheap hasher — identity hashing genuinely is
faster than SipHash. It is to keep a ~1-cycle hash that puts entropy in the
high bits. Fibonacci hashing (`id.wrapping_mul(0x9E37_79B9_7F4A_7C15)`) is one
multiply and measured *faster single-threaded than identity* (83M vs 60M),
because it also fixes a second-order problem: with identity hashing every
entry shares the same hashbrown SIMD control tag, so every probe group
false-matches.

Note honestly what this does **not** buy: 0.27x is still not scaling. Sharding
raises the ceiling ~3.5x; it does not make DashMap reads scale, because a
DashMap read is still an atomic RMW on a shard lock word. Real read scaling
needs a structure without a per-read atomic RMW (arc-swap / left-right /
epoch-based reclamation, or a seqlock like the one already in
`apply_gate.rs`). That is a bigger project; the shard fix is the cheap 3.5x
that should happen first regardless.

### 2. Reads take a global exclusive mutex to get a snapshot

`snapshot_timestamp_for_read` (`src/db/transaction.rs:61`) locks
`current_timestamp: Arc<Mutex<Timestamp>>`, reads the frontier, computes a
strictly-greater stamp, and **writes it back**. Every `read_transaction()` —
and therefore every temporal read, every `AS OF` query, every snapshot-isolated
scan — takes the same global exclusive mutex the *writers* use.

The reservation is load-bearing, not incidental: the doc comment explains
correctly that without it a commit in the same wallclock tick would recompute
an identical stamp and a superseded version's `[C1, S)` interval would exclude
`S`, silently breaking snapshot isolation. So this cannot just be deleted.

But it does not need a mutex. The whole critical section is
read-compute-conditionally-write on a 96-bit value (`HybridTimestamp { wallclock:
i64, logical: u32 }`) — a textbook CAS loop. `portable-atomic::AtomicU128` is
lock-free on x86-64 (`cmpxchg16b`) and aarch64 (`casp`); alternatively pack into
an `AtomicU64` as 52-bit wallclock + 12-bit logical, or store the wallclock
relative to a database epoch. This removes readers from the writer's mutex
entirely and is contained, mechanical, and separately testable.

This is why `read/snapshot` (0.10x) scales *worse* than `read/current` (0.27x):
it pays the shard collapse **and** the global mutex.

### 3. Writers hold the commit clock across the fsync

This is the big one, and it is deliberate, documented, and load-bearing — so it
must be changed carefully rather than reverted.

`commit_with_timestamp_inner` (`src/api/transaction/write/mod.rs:511`) holds the
`current_timestamp` mutex across the **entire** commit:

```
acquire current_timestamp
  assign commit HLC
  precondition guards (#3416 orphan / dangling-endpoint, #3577 CAS-lease-fence)
  WAL append
  wal.commit()
  gc.wait_for_flush(epoch)        <-- the fsync. Still holding the mutex.
  apply_changes()                 <-- also takes historical.write()
  finalize_current_commit_timestamps()
release current_timestamp
```

Issue #3413 widened this on purpose: running the guards under the same held
lock that spans apply is what guarantees a guard which passed pre-WAL is still
valid at apply time, so a transaction rejected at runtime can never leave a
durable frame for crash recovery to reapply. That is a genuine correctness
property and any redesign has to preserve it. The plan doc even says so
plainly:

> `current_timestamp` is *already* held across `append → wal.commit() →
> wait_for_flush` (the dominant fsync cost), which already serializes committers
> — `docs/plans/2026-07-22-wal-abort-framing.md:139`

The consequence is that **group commit cannot ever batch**. The
`GroupCommitCoordinator` is a correct epoch-batching design — its own docs
describe `Epoch 0: [tx1, tx2, tx3] → flush` — but no two transactions can be
registered-and-unflushed simultaneously, because the first one is holding the
mutex the second needs in order to reach the WAL at all. Batch size is
structurally pinned at 1.

The measurement confirms it exactly: 8 concurrent writers, `max_delay_ms: 10`,
produce **95 commits/sec** — one per batch window. If batching worked, 8
writers sharing one flush would give ~800/sec. The `~100K+/sec GroupCommit`
figure in the docs is the WAL layer measured in isolation
(`benches/durability_modes.rs` drives the coordinator directly with 8 threads);
it is not reachable through `db.write()`.

The fix is the standard commit pipeline, and AletheiaDB already has every piece
of it:

- **Stage A — sequence (short, exclusive).** Assign commit HLC, run the
  precondition guards, append to WAL. Release.
- **Stage B — durability (concurrent, unordered).** Wait for the group flush.
  *This* is where N writers collapse into one fsync.
- **Stage C — apply (ordered, single-threaded).** Drain durable frames in LSN
  order, apply to current + historical, publish visibility.

Stage C is *already written*: it is the replica applier plus the `ApplyGate`
seqlock from Issue #3788, which exists precisely so point reads see
before-or-after a batch and never mid-apply. The primary would become, in
effect, a replica of its own WAL. `commit()` waits on `applied_lsn >= my_lsn`
via a condvar rather than by holding a global mutex — so writers pipeline
instead of queueing.

The correctness argument #3413 needs is preserved but relocated: guard validity
stops depending on "one mutex held across everything" and starts depending on
"guards are validated in Stage A against the write sets of transactions already
sequenced but not yet applied" — which is ordinary OCC validation, and the
codebase already does write-write conflict detection (`detect_conflicts`) for
snapshot isolation.

This is the stage that turns "multi-writer" from false into true, and it is
also by far the most invasive. It should not be attempted before Stages 1 and 2
are landed and the harness shows their effect.

### 4. Historical storage is one global RwLock

`historical: Arc<RwLock<HistoricalStorage>>` (`src/db/mod.rs:262`) protects a
struct of plain `FastHashMap`s. Every commit takes the write side; every
temporal read takes the read side. Under the Stage 3 pipeline a single applier
holds it, so this stops being a *writer* bottleneck — but it remains a
reader/writer interference point: a long temporal scan holding `read()` blocks
the applier, and vice versa.

Sharding it by entity id (the same way `CurrentIndexes` intends to shard, and
with the same care about which bits select the shard) is the natural follow-up,
but it is genuinely Stage 4 — there is no point paying for it while Stage 3 is
the binding constraint.

## Stage 1, as landed

`IdHashBuilder` is now backed by a new `IdHasher`: `IdentityHasher` plus one
finalizing `wrapping_mul` by the Fibonacci constant `0x9E37_79B9_7F4A_7C15`.
The multiplier is odd, so the finalizer is a bijection on `u64` and cannot
introduce a collision that identity hashing would have avoided.

**Scope: `DashMap` only.** An earlier draft also repointed the std-`HashMap`
aliases (`FastHashMap`/`FastHashSet` in `core/version.rs`, the write buffer's).
That was reverted: a std `HashMap` has no shard to repair — only the
control-byte effect — and sequential ids map to *sequential buckets* there,
which is good for locality. The evidence justifies the change where the shard
selector reads the hash, and nowhere else.

### Measured effect

Baseline vs. change, **interleaved across three passes on the same box** so
machine drift cancels. This methodology matters more than it sounds: a naive
before/after comparison run an hour apart reported a 5.5x gain and a 5% write
regression, and *both* were artifacts of drift. Medians:

| Workload | threads | baseline | with `IdHasher` | |
|----------|--------:|---------:|----------------:|--|
| `read/current` (`get_node`) | 1 | 17.6M/s | 17.3M/s | unchanged |
| | 2 | 11.6M/s | 19.7M/s | **1.69x** |
| | 4 | 11.6M/s | 28.2M/s | **2.44x** |
| | 8 | 11.5M/s | 28.9M/s | **2.50x** |
| `read/borrow` (`with_node`) | 4 | 15.8M/s | 36.6M/s | **2.32x** |
| `write/async` | 4 | 46.7K/s | 49.0K/s | within noise |
| `write/group_commit` | 4 | 93/s | 94/s | unchanged |

The headline is not the multiple, it is the **shape**: `read/current` went from
0.27x scaling (slower with more cores) to genuinely scaling with them. Writes
are unaffected in either direction, which is expected — they are bound by the
commit clock and the fsync, not by hashing. `read/snapshot` is also unchanged,
because it is still gated by the global commit-clock mutex; that is Stage 2's
job, and this is what "the stages depend on each other" looks like in practice.

A caution for whoever measures next: the fixed build is **bimodal** on this
box — some passes land at ~16M/s (baseline-like) and others at ~38M/s. The
baseline, by contrast, is tightly clustered. Removing one bottleneck exposes
the next, and what the next one is appears to depend on thread-to-core
placement. Take medians over interleaved passes; never trust a single run.

### Verification

`cargo test --lib` 4625 passed / 0 failed (up 7 from the new hasher tests),
`cargo test --doc` 419 passed / 0 failed, and the integration suite 1302 passed
/ 3 failed across 166 targets. All three integration failures
(`embedded_mcp_server_refuses_a_data_dir_a_live_daemon_owns`,
`havoc_flush_deadlock::test_flush_deadlock_on_io_error`,
`test_metadata_corruption_on_error`) were re-run against the unmodified
baseline and **fail there identically** — they are pre-existing/environmental
(daemon-subprocess and WAL fault-injection), not caused by this change. Notably
nothing broke on iteration order, which the change does alter throughout.

The integration suite had to be run in batches of 25 targets, purging linked
test executables between them: linking all 216 at once exceeds this session's
disk allowance (`target/` reaches 30G, and the first attempt failed with
`os error 28`, which surfaces as misleading `linking with cc failed` errors).

### A rejected alternative, and why

A "high-half scramble" finalizer — `(id & LOW) | (id * FIB & !LOW)` — was
built and measured alongside. It keeps the low 32 bits verbatim, so bucket
locality for sequential ids is preserved exactly, while still scrambling the
bits that shard selection and the control byte read. It looked strictly better
on paper. It measured worse and far less stable (`read/current` at 4 threads:
29.2M, 15.2M, 12.7M across passes, versus Fibonacci's steady 29.7M, 28.2M,
26.3M), and it would have required a bespoke collision argument. Rejected in
favor of the standard construction.

### Found along the way, deliberately not fixed

`IdentityHasher::update_state` assigns on the first write and then XOR-mixes.
XOR is commutative, so a two-field composite key hashes identically in either
field order — directly contradicting that method's own doc comment, which
claims it "ensures that `hash((1, 2))` is not the same as `hash((2, 1))`". The
existing `test_identity_hasher_composite_writes` even encodes the symmetric
value `(1 ^ 2) * FNV_PRIME` as expected.

It is **latent, not live**: no map in the crate is both tuple-keyed and
id-hashed (`prop_index`'s tuple-keyed outer `DashMap` uses the default
`RandomState`), so nothing currently collides because of it. Fixing it would
change composite hash values, hence iteration order, hence possibly tests —
unrelated to the shard bug and not worth folding into this change. It is now
pinned by a characterization test named
`known_limitation_two_field_composites_ignore_field_order`, so a future fix has
to be deliberate rather than accidental.

## On connection pools

You said "connection pools, not quite yet — but we're probably better off with
it." Half right, and the half that is wrong is the useful half.

**There is nothing to pool today.** A connection pool exists to amortize a
per-session resource that is expensive to create and unsafe to share — a TCP
socket, a server-side session, a file handle. AletheiaDB's Rust API has no such
object. Callers hold `Arc<AletheiaDB>`; cloning it is a refcount bump; every
thread already shares one instance. A pool of `Arc` clones would be an
allocator for integers. `read_transaction()` and `write_transaction()` are
cheap value types, not handles onto a scarce resource. Pooling them would add
bookkeeping and remove nothing, because the contention is not in *acquiring* a
transaction — it is inside `commit()`, and every pooled handle would queue on
the same mutex.

**The place a pool genuinely belongs is the daemon boundary, and it is an HTTP
concern.** Since Issue #2905, N MCP/HTTP clients talk to one `aletheia-daemon`.
Reusing those TCP/TLS connections instead of dialing per request is a real win —
but that is `reqwest`'s connection pool in the client, configuration rather than
architecture, and it says nothing about whether the daemon can use its cores.

**The thing you are actually reaching for is admission control, and it becomes
necessary at Stage 3.** Today, bounding concurrent writers is pointless: they
are already bounded at 1 by the commit mutex. The moment commits pipeline, an
unbounded number of in-flight commits becomes a real resource-exhaustion
question — memory held by buffered write sets, WAL ring pressure, applier lag.
At that point you want a bounded pool of write permits with a queue and a
timeout, which is a connection pool in everything but name.

There is already a precedent to copy rather than invent: `max_in_flight_queries`
(default 64, Issue #3368) does exactly this for reads, returning a bounded,
retriable `UNAVAILABLE` when the pool is exhausted. The write-side analogue
should mirror it — same error envelope, same retriable semantics, same
configuration shape. Note the existing caveat that the read pool is a single
shared budget across the `query` tool and the wrapped read tools; a write
admission pool should be a separate budget, not folded into that one.

So: **not a connection pool, an admission pool, and it is Stage 3's companion,
not its prerequisite.** Building it now would bound a queue of length one.

## Proposed staging

| Stage | Change | Unlocks | Risk |
|-------|--------|---------|------|
| **0** | Scaling harness (`examples/concurrency_scaling.rs`) | A number to argue with; regression detection | none — additive |
| **1** | ~~Fix shard selection (fibonacci hash for id keys)~~ **LANDED** | 2.4-2.5x concurrent `get_node` throughput; reads now scale with cores | low, contained; regression tests pin shard occupancy |
| **2** | CAS-loop commit clock, remove the reader mutex | Readers stop contending with writers | low-medium; SI reservation invariant must be preserved exactly |
| **3** | Commit pipeline: sequence → durability → ordered apply | **Real multi-writer.** Group commit finally batches | high — touches #3413's correctness argument |
| **3b** | Write admission pool, mirroring `max_in_flight_queries` | Bounded in-flight commits | low, once 3 exists |
| **4** | Shard historical storage | Temporal reads stop blocking the applier | medium |

Stages 1 and 2 are independently valuable, independently testable, and do not
depend on Stage 3. They are also the ones where the effort-to-payoff ratio is
absurd — Stage 1 is a hasher change that is strictly better on both axes.

## What must not break

- **Issue #3413's guarantee**: a runtime-rejected transaction leaves no durable
  WAL frame. Stage 3 relocates this argument; it must not weaken it.
- **Snapshot isolation's reservation invariant** (`transaction.rs:42-60`): every
  issued snapshot must be strictly greater than the prior frontier, or
  superseded versions vanish from temporal reads.
- **The documented lock order** in `CLAUDE.md` (`current_timestamp → wal →
  historical → temporal_indexes → id generators → outgoing → incoming`). Stage 3
  shortens the first hold; it should not reorder.
- **Single-threaded performance.** The <1µs single-hop target is the headline
  claim. Stage 1 improves it; Stages 2–4 must be shown not to regress it.
- **`ApplyGate` semantics** (#3788): bulk scans and iterators are deliberately
  not gated. If the primary adopts the applier, that caveat now applies to the
  primary too and needs documenting.

## Open questions

1. Should Stage 3's applier be a dedicated thread, or should the last committer
   in a flush epoch drain the queue (avoiding a context switch on the
   uncontended path)?
2. Does `commit()` need to wait for apply at all? Callers expecting
   read-your-writes do; a `commit_async()` returning a durable-but-not-yet-visible
   receipt might serve high-throughput ingest better.
3. Is the shard count worth configuring explicitly rather than inheriting
   DashMap's `ncpu * 4`? A daemon serving many small tenants may want a
   different tradeoff than a single large graph.
4. Multi-tenancy (#3365) gives one `AletheiaDB` per tenant, so per-tenant
   commit mutexes already partition writers by tenant. Does that change Stage 3's
   priority for tenant-heavy deployments? (It does not help the single-large-graph
   case, which is the one the benchmarks target.)
