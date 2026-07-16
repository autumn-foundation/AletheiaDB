# Named Snapshots — Reproducible Reads (Issue #3370)

A **named snapshot** pins a human-readable name to a bi-temporal coordinate
`(valid_time, transaction_time)`. Reads issued through the resulting handle are
evaluated at that coordinate via the deterministic historical (`*_at_time`)
read path, so the **same handle returns identical results regardless of writes
that land afterward**. This is the reproducibility primitive behind "run this
analysis against exactly the state I saw a moment ago".

> **Scope.** This wave ships the **Rust API only**. Surfacing named snapshots
> through the MCP server and an `AS OF SNAPSHOT <name>` query DDL clause is a
> deliberately coordinated follow-up.

## Quick start

```rust
use aletheiadb::AletheiaDB;

let db = AletheiaDB::open("data/mydb")?;
// ... writes ...

// Pin "the world as of right now".
let snap = db.create_snapshot("nightly-run", Some("2026-07-15 report".into()))?;

// Resolve to a read-only, borrowed handle and read at the pin.
let view = db.snapshot("nightly-run")?;
let alice = view.get_node(alice_id)?;          // as of the pin
let people = view.find_nodes("Person")?;       // as of the pin
let out    = view.get_outgoing_edges(alice_id); // adjacency as of the pin

// A pre-pinned query builder (traversal, filtering, vector ranking):
let results = view.query().start(alice_id).traverse("KNOWS").execute(&db)?;
```

## API

| Method | Purpose |
|--------|---------|
| `create_snapshot(name, description)` | Pin "now": valid-time = wallclock now, transaction-time = the commit frontier. |
| `create_snapshot_at(name, valid_time, transaction_time, description)` | Pin an explicit (possibly backdated) coordinate. |
| `snapshot(name) -> Snapshot` | Resolve a name to a borrowed read handle. |
| `get_snapshot(name) -> NamedSnapshot` | Fetch the coordinate/metadata without a handle. |
| `list_snapshots() -> Vec<NamedSnapshot>` | List all pins (stable order: created_at, then name). |
| `delete_snapshot(name)` | Remove a pin. |

The `Snapshot<'_>` handle exposes `get_node`, `get_edge`, `find_nodes`,
`find_nodes_by_property`, `get_outgoing_edges` / `get_incoming_edges` (adjacency
at the pin), and `query()` (a `QueryBuilder` already pinned via
`as_of(valid_time, transaction_time)` — use it for multi-hop traversal at the
pin). Every read routes through the historical `*_at_time` path, never the
current-state hot path.

Errors use the project's structured codes (Issue #3234): a duplicate name is a
**CONFLICT**, an unknown name is a **NOT_FOUND** (with the name in the message).

## A snapshot is a coordinate, not a held resource

Creating a snapshot records two timestamps in a small sidecar registry. It

- **pins no storage** — there is no retention obligation and nothing is kept
  alive on your behalf;
- **adds no lasting write-path overhead** — the registry is entirely off the
  data write path (`create_node` / `create_edge` never touch it).

Creation is effectively free but **not literally a no-op** against concurrent
committers: it takes the commit-clock lock (`current_timestamp`) just long
enough to copy a single `Timestamp` — nanosecond-scale contention on the hot
commit path, released immediately. This mirrors the #3360 cursor, which
likewise captures a `(vt, tt)` pair once and re-reads deterministically.

A snapshot **created at an instant racing an in-flight commit** inherits the
engine's standard committed-but-not-yet-applied visibility window — the same
caveat #3225 / #3236 point-in-time reads carry. Once created, the coordinate is
fixed and every read through it is deterministic.

## Defaulting the coordinate ("now")

`create_snapshot` defaults the two dimensions **differently**, each to the
correct notion of "now":

- **transaction time = the commit frontier** (the value guarded by
  `current_timestamp`). The commit path advances that frontier strictly
  monotonically under the same lock, so every transaction committed **before**
  the snapshot has a transaction-time start `≤` the frontier (**visible**) and
  every transaction committed **after** has a start strictly `>` it
  (**invisible**). The frontier equals "now" for all committed data, but is
  chosen over wallclock precisely for this race-free monotonicity.
- **valid time = wallclock `time::now()`**, matching the engine's "now"
  convention (`get_node_at_valid_time`, `find_nodes_at_time`). Defaulting valid
  time to the (idle-lagging) frontier would silently **exclude** facts that are
  genuinely valid at creation — including #3221 future-dated facts already valid
  by now.

So "as of the moment of creation" is precise: valid-time-now = wallclock-now,
transaction-time-now = the commit frontier. A node created after the pin is
invisible, a node updated after the pin reads as its pre-update version, and a
node deleted after the pin is still visible.

## Caveats (shared with `temporal_extent` / point-in-time reads)

- **Cold-tier / truncation visibility.** A snapshot enjoys no retention
  guarantee. If the versions it observes are later evicted (cold tier not
  configured, or history truncated) a read through the handle can return
  "not found" for a fact that was visible at creation — exactly the
  visibility caveat that governs `temporal_extent` (Issue #3238) and every
  other point-in-time read. Keep history (or a configured cold tier) for as
  long as you intend a snapshot to remain readable.
- **Future-valid facts.** Pinning "now" excludes facts whose `valid_from` lies
  in the future of the pin — the same tradeoff documented for #3236 /
  point-in-time reads. Use `create_snapshot_at` with an explicit future
  `valid_time` if you need to observe forward-dated facts.
- **Backdated pins.** `create_snapshot_at` does **not** reject coordinates
  outside the current temporal extent; a coordinate before any recorded
  history simply resolves to an empty world.

## Durability

When index persistence is enabled (e.g. via `AletheiaDB::open(path)`), the
registry is persisted atomically (temp file + rename + parent fsync) **inside**
the persistence directory, at `{persistence.data_dir}/snapshots.json` — under
the canonical durable config that is `{data_dir}/indexes/snapshots.json`. Keeping
the sidecar inside the data dir (rather than a stripped-parent sibling) means a
power user who points `data_dir` at their data root never gets a file written
outside it.

Coordinates are stored as the **full HLC timestamp** (`{wallclock, logical}`),
not bare microseconds. Persisting the logical counter is required for
restart-fidelity: two commits within one wallclock microsecond receive distinct
logical counters, so a pin at `(W, 1)` that dropped its logical counter would
collapse to `(W, 0)` on reload and resolve the **wrong** (superseded) version.
The sidecar is versioned (`version: 2`); a legacy `version: 1` file (bare-integer
timestamps) still loads, reconstructed with logical `0`.

A corrupt, unparseable, or unknown-future-version `snapshots.json` does **not**
brick startup (unlike the security-critical auth key store, which correctly
hard-fails): the database logs a warning, quarantines the bad file aside
(`snapshots.json.corrupt`, preserving the bytes), and starts with an empty
registry — snapshots are non-critical bookmarks whose loss costs at most a
re-created pin.

Ephemeral databases (`AletheiaDB::new()`) keep the registry in memory only.
