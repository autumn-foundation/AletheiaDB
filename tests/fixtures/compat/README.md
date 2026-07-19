# Cross-version compatibility fixtures

This directory holds on-disk data directories written by **older published
AletheiaDB releases**, used to test that newer code can open them. The point is
to exercise the *real* released byte layouts, not layouts synthesized in-process
by the current code.

## `aletheiadb-0.1.1/` — a 0.1.1-written data directory

A small bi-temporal graph (**52K** total, well under a 1 MiB cap) written by the
published `aletheiadb = "=0.1.1"` crate from crates.io. It contains an
**unreplayed WAL tail**: data present in the WAL that the last index-persistence
snapshot did NOT capture, so reopening requires WAL replay.

Consumed by [`tests/compat_0_1_1_datadir.rs`](../../compat_0_1_1_datadir.rs).

> ### ⚠️ Regeneration rule
>
> This fixture MUST only ever be regenerated via the **pinned
> `aletheiadb = "=0.1.1"` crate** (a throwaway generator project), **never**
> from the AletheiaDB trunk working tree. Trunk's on-disk formats and APIs
> differ; a trunk-written directory would not test 0.1.1 → newer compatibility.
> The `wal/000001.log` file is force-added to git (`git add -f`) because the
> repo `.gitignore` ignores `*.log`.

> ### 🚑 Current status: this fixture reproduces a RELEASE BLOCKER
>
> As of this commit, opening `aletheiadb-0.1.1/` under trunk (0.2.0) **corrupts
> the interned-string labels of every entity recovered from the WAL tail** (see
> the test's `//!` header for the verbatim reproduction). The test is therefore
> committed `#[ignore]`d as an executable reproduction. Do NOT treat the
> migration guide's in-place-open path as "tested safe" until this is fixed.

### Version pinned

- `aletheiadb = "=0.1.1"` (exact), edition 2024, `rust-version = 1.92`.
- Built with toolchain `rustc 1.94.1` (newer than 1.92 — fine).

### What the fixture contains

- **Batch 1** (captured by the index snapshot): 9 nodes across 3 labels
  (`Person`, `Company`, `City`) with `String`/`Int`/`Float`/`Bool` props, and
  9 edges across 2 types (`KNOWS`, `WORKS_AT`) each with a property. One node
  (Bob) is UPDATED (age 41 → 42), producing a superseded historical version.
- **Batch 2** (WAL-only tail, NOT in any snapshot): 3 more nodes + 3 more edges,
  written with `DurabilityMode::Synchronous` so the tail is complete and
  deterministic after the non-graceful exit.

On reopen with 0.1.1, the index snapshot restores 9 nodes / 9 edges, then the
WAL tail replays to yield the full **12 nodes / 12 edges**.

### File layout

```
aletheiadb-0.1.1/
├── wal/
│   └── 000001.log              # one WAL segment (contains the batch-2 tail)
└── indexes/
    └── indexes/                # 0.1.1 nests an inner indexes/ dir
        ├── manifest.idx
        ├── graph/adjacency.idx
        ├── strings/interner.idx
        └── temporal/versions.idx   # batch-1 versions incl. Bob's superseded v1
```

### The two documented 0.1.1 quirks baked into the fixture

**Quirk (1) — snapshot-boundary sentinel (node id 9 is absent under 0.1.1).**
0.1.1 has an off-by-one at the index-snapshot LSN boundary: `persist_indexes()`
records a watermark equal to the LSN the *next* WAL append will take, and replay
starts strictly **after** the watermark. The first post-snapshot WAL entry is
therefore neither snapshotted nor replayed — silently dropped on reopen. The
generator absorbs that one boundary slot with a throwaway `Sentinel` node (node
id 9) so the real batch-2 entities land past the watermark and replay
completely. **Under 0.1.1, node id 9 is intentionally absent after reopen and is
not part of the ground truth.**

> Trunk does NOT reproduce this off-by-one: it replays the boundary slot too, so
> node 9 comes back (node_count = 13, not 12) — but trunk cannot resolve its
> interned label. This divergence is part of the blocker the test documents.

**Quirk (2) — `get_node_at_time` on restored nodes.** After reopen, 0.1.1's
`get_node_history()` correctly returns Bob's two versions with correct
bi-temporal intervals, but `get_node_at_time()` point-reconstruction returns
`Storage(NodeNotFound)` for **restored** nodes (it worked in-process during
generation). The test asserts temporal state via `get_node_history()` (robust
across reopen) and treats `get_node_at_time()` as a non-fatal probe. On the
observed trunk run this limitation is **still not fixed** (same `NodeNotFound`).

### Ground truth (as 0.1.1 recovers it)

Current-state after reopen (WAL tail replayed):

| Fact | Value |
|------|-------|
| `node_count` | **12** |
| `edge_count` | **12** |
| batch-1 node/edge count (from snapshot) | 9 / 9 |
| batch-2 node/edge count (from WAL tail) | 3 / 3 |
| sentinel node id 9 | **absent** (dropped boundary slot) |

Specific node tuples (label, property → value):

- `alice` (NodeId 0) — `Person`: name=`"Alice"`, age=`30`, score=`4.5`, active=`true`
- `acme`  (NodeId 4) — `Company`: name=`"Acme"`, founded=`1999`, public=`true`
- `eve`   (NodeId 10, **BATCH2**) — `Person`: name=`"Eve"`, age=`34`, score=`5.0`, active=`true`

Node ids: alice=0, bob=1, carol=2, dave=3, acme=4, globex=5, initech=6,
london=7, boston=8, (sentinel=9, absent), eve=10, frank=11, umbrella=12.

Specific edges:

- `(Person:Alice) -[WORKS_AT role="Engineer"]-> (Company:Acme)`
- **BATCH2:** `(Person:Eve) -[WORKS_AT role="Researcher"]-> (Company:Umbrella)`

Temporal (Bob, NodeId 1 — age update):

- `before_update_valid_micros = 1784435154774824` (authoritative pre-update
  bi-temporal coordinate captured at generation time)
- `bob_age_before_update = Int(41)`, `bob_age_after_update = Int(42)`
- After reopen+replay, `get_node_history(bob)` returns **2 versions**:
  - v1 age=`Int(41)`, valid=`[1784435154567337 .. 1784435154785012)`,
    tx=`[1784435154567347 .. MAX)`
  - v2 age=`Int(42)`, valid=`[1784435154785012 .. MAX)`,
    tx=`[1784435154785044 .. MAX)`

## 0.1.1 public API used (verbatim signatures, from 0.1.1 source)

```rust
// Durable open (0.1.1 has NO `open(path)`):
aletheiadb::config::durable_config_for_data_dir(data_dir: impl Into<PathBuf>) -> AletheiaDBConfig
AletheiaDB::with_unified_config(config: AletheiaDBConfig) -> Result<Self>

// Writes (convenience, on AletheiaDB):
db.create_node(label: &str, properties: PropertyMap) -> Result<NodeId>
db.create_edge(source: NodeId, target: NodeId, label: &str, properties: PropertyMap) -> Result<EdgeId>

// Transactional writes (trait aletheiadb::WriteOps on WriteTransaction):
db.write(|tx| tx.update_node(node_id: NodeId, properties: PropertyMap) -> Result<()>)
db.write_with_options(options: WriteOptions, f: FnOnce(&mut WriteTransaction) -> Result<T>) -> Result<T>
WriteOptions::new().with_durability(DurabilityMode::Synchronous)   // aletheiadb::{WriteOptions, DurabilityMode}

// Index-persistence snapshot (force mid-run):
db.persist_indexes() -> Result<()>                                 // aletheiadb::db::admin

// Reads:
db.get_node(node_id: NodeId) -> Result<Node>
db.node_count() -> usize   /   db.edge_count() -> usize
Node::get_property(&self, key: &str) -> Option<&PropertyValue>

// Temporal reads:
db.get_node_at_time(node_id: NodeId, valid_time: Timestamp, transaction_time: Timestamp) -> Result<Node>
db.get_node_history(node_id: NodeId) -> Result<EntityHistory>      // { pub versions: Vec<VersionInfo> }

// Time helpers (aletheiadb::time):
time::now() -> Timestamp        // Timestamp == HybridTimestamp
Timestamp::wallclock() -> i64   // micros since epoch

// Properties:
PropertyMapBuilder::new().insert(key: &str, value: impl Into<PropertyValue>).build() -> PropertyMap
// PropertyValue variants: Null | Bool(bool) | Int(i64) | Float(f64) | String(Arc<str>) | ...
```

## Regenerating the fixture (pinned 0.1.1 crate ONLY)

Create a throwaway generator project **outside** the AletheiaDB repo, pinned to
`=0.1.1`, run it to write `./fixture_data`, and copy that directory here BEFORE
any reopen (reopening replays/re-snapshots and would mutate it).

### Generator `Cargo.toml`

```toml
[package]
name = "fixture_gen_011"
version = "0.1.0"
edition = "2024"
rust-version = "1.92"

[dependencies]
aletheiadb = "=0.1.1"
```

### Generator `src/main.rs`

```rust
// Fixture generator written against published AletheiaDB v0.1.1 (crates.io).
//
// Produces an on-disk data dir (wal/ + indexes/) with an UNREPLAYED WAL TAIL:
// batch 1 is written and captured by an index-persistence snapshot; batch 2 is
// written AFTER the snapshot and the process exits WITHOUT a second snapshot or
// graceful shutdown (std::process::exit skips Drop), so batch 2 lives only in
// the WAL and requires replay on reopen.
//
// Modes:
//   (default)   generate the fixture at ./fixture_data
//   --verify    re-open ./fixture_data with the SAME 0.1.1 binary and print
//               the recovered node/edge counts (proves the WAL tail replays)

use aletheiadb::core::property::PropertyValue;
use aletheiadb::config::durable_config_for_data_dir;
use aletheiadb::{AletheiaDB, DurabilityMode, PropertyMapBuilder, WriteOps, WriteOptions, time};
use std::path::Path;

fn prop_dbg(v: Option<&PropertyValue>) -> String {
    match v {
        Some(PropertyValue::Bool(b)) => format!("Bool({b})"),
        Some(PropertyValue::Int(i)) => format!("Int({i})"),
        Some(PropertyValue::Float(f)) => format!("Float({f})"),
        Some(PropertyValue::String(s)) => format!("String({s:?})"),
        Some(other) => format!("{other:?}"),
        None => "<absent>".to_string(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = Path::new("./fixture_data");
    let verify = std::env::args().any(|a| a == "--verify");

    if verify {
        let db = AletheiaDB::with_unified_config(durable_config_for_data_dir(data_dir))?;
        println!("VERIFY: reopened 0.1.1 fixture at {}", data_dir.display());
        println!("VERIFY node_count = {}", db.node_count());
        println!("VERIFY edge_count = {}", db.edge_count());
        std::mem::forget(db);
        return Ok(());
    }

    // Fresh generation: remove any prior fixture dir.
    let _ = std::fs::remove_dir_all(data_dir);

    let db = AletheiaDB::with_unified_config(durable_config_for_data_dir(data_dir))?;

    // ---------------------------------------------------------------
    // BATCH 1 (will be captured by the index-persistence snapshot)
    // ---------------------------------------------------------------
    let alice = db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("score", 4.5f64)
            .insert("active", true)
            .build(),
    )?;
    let bob = db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("age", 41i64)
            .insert("score", 2.25f64)
            .insert("active", false)
            .build(),
    )?;
    let carol = db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Carol")
            .insert("age", 27i64)
            .insert("score", 3.75f64)
            .insert("active", true)
            .build(),
    )?;
    let dave = db.create_node(
        "Person",
        PropertyMapBuilder::new()
            .insert("name", "Dave")
            .insert("age", 52i64)
            .insert("score", 1.5f64)
            .insert("active", true)
            .build(),
    )?;

    let acme = db.create_node(
        "Company",
        PropertyMapBuilder::new()
            .insert("name", "Acme")
            .insert("founded", 1999i64)
            .insert("public", true)
            .build(),
    )?;
    let globex = db.create_node(
        "Company",
        PropertyMapBuilder::new()
            .insert("name", "Globex")
            .insert("founded", 2010i64)
            .insert("public", false)
            .build(),
    )?;
    let initech = db.create_node(
        "Company",
        PropertyMapBuilder::new()
            .insert("name", "Initech")
            .insert("founded", 1985i64)
            .insert("public", true)
            .build(),
    )?;

    let london = db.create_node(
        "City",
        PropertyMapBuilder::new()
            .insert("name", "London")
            .insert("population", 8900000i64)
            .build(),
    )?;
    let boston = db.create_node(
        "City",
        PropertyMapBuilder::new()
            .insert("name", "Boston")
            .insert("population", 654000i64)
            .build(),
    )?;

    // Edges: KNOWS (Person->Person) and WORKS_AT (Person->Company), with props.
    db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().insert("since", 2015i64).build())?;
    db.create_edge(alice, carol, "KNOWS", PropertyMapBuilder::new().insert("since", 2018i64).build())?;
    db.create_edge(bob, carol, "KNOWS", PropertyMapBuilder::new().insert("since", 2019i64).build())?;
    db.create_edge(carol, dave, "KNOWS", PropertyMapBuilder::new().insert("since", 2021i64).build())?;
    db.create_edge(dave, alice, "KNOWS", PropertyMapBuilder::new().insert("since", 2022i64).build())?;
    db.create_edge(alice, acme, "WORKS_AT", PropertyMapBuilder::new().insert("role", "Engineer").build())?;
    db.create_edge(bob, globex, "WORKS_AT", PropertyMapBuilder::new().insert("role", "Manager").build())?;
    db.create_edge(carol, acme, "WORKS_AT", PropertyMapBuilder::new().insert("role", "Designer").build())?;
    db.create_edge(dave, initech, "WORKS_AT", PropertyMapBuilder::new().insert("role", "Analyst").build())?;

    let node_count_after_batch1 = db.node_count();
    let edge_count_after_batch1 = db.edge_count();

    // Capture a bi-temporal point BEFORE the update so the test can read the
    // pre-update value. Sleep a hair to guarantee monotonic wallclock separation.
    std::thread::sleep(std::time::Duration::from_millis(10));
    let before_update = time::now();
    std::thread::sleep(std::time::Duration::from_millis(10));

    // UPDATE Bob's age 41 -> 42 (PATCH; creates a superseded historical version).
    let bob_age_before = prop_dbg(db.get_node(bob)?.get_property("age"));
    db.write(|tx| tx.update_node(bob, PropertyMapBuilder::new().insert("age", 42i64).build()))?;
    let bob_age_after = prop_dbg(db.get_node(bob)?.get_property("age"));

    // Read Bob's age AS OF before_update (should still be 41).
    let bob_age_at_before =
        prop_dbg(db.get_node_at_time(bob, before_update, before_update)?.get_property("age"));

    // ---------------------------------------------------------------
    // FORCE INDEX-PERSISTENCE SNAPSHOT ("batch 1, persisted")
    // ---------------------------------------------------------------
    db.persist_indexes()?;

    // ---------------------------------------------------------------
    // BATCH 2 (WAL-only tail; NOT captured by any snapshot)
    //
    // Written with Synchronous durability (fsync per commit) so the entire
    // batch is DURABLY in the WAL before the non-graceful exit. It is NOT
    // captured by any index snapshot (we never call persist_indexes again),
    // so reopening MUST replay these WAL entries.
    // ---------------------------------------------------------------
    let sync_opts = || WriteOptions::new().with_durability(DurabilityMode::Synchronous);

    // Boundary-slot sentinel: in 0.1.1, persist_indexes records a WAL watermark
    // equal to the LSN the *next* append will take, and replay starts strictly
    // after the watermark -- so the first post-snapshot WAL entry is neither in
    // the snapshot nor replayed (it is dropped). We absorb that one boundary
    // slot with a sentinel node so the real batch-2 tail below is at LSNs past
    // the watermark and replays completely. The sentinel is EXPECTED to be
    // absent after reopen; it is not part of the fixture's ground truth.
    let sentinel = db.write_with_options(sync_opts(), |tx| {
        tx.create_node(
            "Sentinel",
            PropertyMapBuilder::new().insert("note", "boundary-slot, expected dropped on replay").build(),
        )
    })?;

    let eve = db.write_with_options(sync_opts(), |tx| {
        tx.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Eve")
                .insert("age", 34i64)
                .insert("score", 5.0f64)
                .insert("active", true)
                .build(),
        )
    })?;
    let frank = db.write_with_options(sync_opts(), |tx| {
        tx.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Frank")
                .insert("age", 29i64)
                .insert("score", 3.1f64)
                .insert("active", false)
                .build(),
        )
    })?;
    let umbrella = db.write_with_options(sync_opts(), |tx| {
        tx.create_node(
            "Company",
            PropertyMapBuilder::new()
                .insert("name", "Umbrella")
                .insert("founded", 2003i64)
                .insert("public", false)
                .build(),
        )
    })?;

    db.write_with_options(sync_opts(), |tx| {
        tx.create_edge(eve, frank, "KNOWS", PropertyMapBuilder::new().insert("since", 2023i64).build())
    })?;
    db.write_with_options(sync_opts(), |tx| {
        tx.create_edge(eve, umbrella, "WORKS_AT", PropertyMapBuilder::new().insert("role", "Researcher").build())
    })?;
    // Link a batch-2 node to a batch-1 node too.
    db.write_with_options(sync_opts(), |tx| {
        tx.create_edge(frank, boston, "WORKS_AT", PropertyMapBuilder::new().insert("role", "Contractor").build())
    })?;
    let _ = (london, sentinel);

    // node_count/edge_count here are the LIVE (in-process) counts and include
    // the sentinel node. The fixture's on-disk ground truth (after reopen) is
    // one node fewer, because the sentinel occupies the dropped boundary slot.
    let node_count_final_live = db.node_count();
    let edge_count_final = db.edge_count();
    let node_count_final = node_count_final_live - 1;

    // ---------------------------------------------------------------
    // GROUND TRUTH (stdout)
    // ---------------------------------------------------------------
    println!("=== AletheiaDB 0.1.1 fixture ground truth ===");
    println!("nodes_final (batch1+batch2) = {node_count_final}");
    println!("edges_final (batch1+batch2) = {edge_count_final}");
    println!("before_update_valid_micros = {}", before_update.wallclock());
    println!("bob_age_before_update = {bob_age_before}");
    println!("bob_age_after_update  = {bob_age_after}");
    println!("bob_age_AS_OF_before_update = {bob_age_at_before}");
    let _ = (node_count_after_batch1, edge_count_after_batch1);

    // ---------------------------------------------------------------
    // EXIT WITHOUT SNAPSHOT / WITHOUT GRACEFUL SHUTDOWN
    // std::process::exit skips Drop, so no Drop-time persistence runs and
    // batch 2 stays only in the WAL as an unreplayed tail.
    // ---------------------------------------------------------------
    std::io::Write::flush(&mut std::io::stdout()).ok();
    std::process::exit(0);
}
```

After generating, copy the produced `fixture_data/` directory to
`tests/fixtures/compat/aletheiadb-0.1.1/` (force-adding the `wal/*.log` file),
and validate with the pinned binary: `./fixture_gen_011 --verify` should print
`node_count = 12 edge_count = 12`.
