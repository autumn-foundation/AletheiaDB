# Migrating from AletheiaDB 0.1.x to 0.2.0

This guide is for **embedded (Rust API) users** upgrading a working
deployment on published **0.1.0** or **0.1.1** — in particular one that built
a custom schema layer on top of AletheiaDB — to **0.2.0**. 0.2.0 is the first
published crates.io release since 0.1.1 and bundles a large surface of trunk
work, but **most changes are additive or opt-in**: for a typical embedded
graph/temporal app the actual upgrade is small. The sections below call out
exactly the handful of things that can affect you, and are honest about which
compatibility claims are verified against code versus asserted by the
maintainers and not independently re-checked.

---

## ⚠️ Before you upgrade (data safety)

**Drain the WAL before upgrading.** A cleanly-checkpointed 0.1.x data
directory opens under 0.2.0 with full integrity — this is verified by a
cross-version test that opens a real 0.1.1-written fixture (see
`tests/compat_0_1_1_datadir.rs`). Correct labels, all nodes/edges/properties,
and temporal history come back intact. The safe upgrade procedure:

1. On **0.1.x**, after your last write, call `db.persist_indexes()` to snapshot
   all state, then shut the database down **gracefully** (drop the `AletheiaDB`
   value; do not hard-kill the process). This leaves the WAL with no unreplayed
   tail.
2. **Verify the WAL is drained** (see the check below).
3. Copy the data directory.
4. Open the copy with 0.2.0 (`AletheiaDB::open(path)`), and validate known
   entities before decommissioning the original.

```rust
// On 0.1.x, before upgrading: snapshot state, then drop gracefully.
db.persist_indexes()?;
drop(db); // graceful shutdown — do NOT hard-kill the process
```

**Checking the WAL is drained (0.1.x):** re-open the directory once with your
0.1.x binary and watch its startup logs. If there is **no**
`Replaying N WAL entries` line, the WAL has no unreplayed tail and the
directory is safe to upgrade. (The `wal/` segment file is retained and
non-empty even when drained — it is the *replay log line*, not the file size,
that tells you. The entries it holds are all at or below the last
index-snapshot watermark.)

```text
# Drained (safe to upgrade) — no replay line at startup:
INFO aletheiadb: opened data directory, 0 WAL entries to replay

# NOT drained (do NOT upgrade this directory):
INFO aletheiadb: Replaying 42 WAL entries
```

**⚠️ A directory with an unreplayed pre-v13 WAL tail is REFUSED on open
(Issue #3746).** If a 0.1.x directory is opened by 0.2.0 while it still has
un-checkpointed WAL entries (e.g. the process was killed, or you skipped
`persist_indexes()`), `AletheiaDB::open()` now **fails fast** with a
`FAILED_PRECONDITION`-class error
(`StorageError::PreV13WalTailRequiresMigration`) that names the pre-v13 tail,
explains it would otherwise silently corrupt interned labels, and points back to
this guide. It does **not** open the database and does **not** replay the tail.
This is the enforcement of the *drain-first* rule above: the guard scans only the
entries that would actually be replayed (the post-snapshot window), so a drained
directory (empty window) and a current-version v13+ tail both open normally.

**Scope of the guard.** The refusal keys on the WAL segment *format version*,
not on the "0.1.x" label per se: **any** pre-string-labels format — every
plaintext or encrypted WAL segment at version **< 13** (`WAL_VERSION_STRING_LABELS`),
including the intermediate 0.2.0-dev formats written before string labels
landed — is refused on open for exactly the same raw-interner-id reason. The
guard's predicate is `!carries_string_labels(version)` applied to the decoded
segment version. Because every segment (plaintext or encrypted-then-decrypted)
routes through a single decode site that stamps the payload version, the
encrypted pre-v13 path is **logically covered** by the same check; it simply
lacks a checked-in encrypted-pre-v13 fixture, so that specific path is a
**documented test gap** (the plaintext pre-v13 path is fixture-verified in
`tests/compat_0_1_1_datadir.rs`).

Before this guard, opening such a directory *succeeded* but the tail-replayed
entities came back with **silently corrupted labels** — a verified failure
reproduced in `tests/compat_0_1_1_datadir.rs`: tail nodes were mislabeled to
unrelated interned strings (for example, nodes coming back labeled with
property-key strings such as `founded`/`since`), and the recovered node count
also diverged. The cause is that 0.1.x's pre-v13 WAL stores labels as
process-local interner ids that 0.2.0's rebuilt interner resolves to different
strings; because the label string was never written to disk, there is nothing to
recover it from at replay time. There is no in-place fix — drain the WAL first
(above), which captures every label as a string in the index snapshot.

**There is no backup/restore off-ramp.** The `.albk` backup format did **not**
exist in 0.1.x — 0.1.1 has no `backup()` at all — so the usual "back up on the
old version, restore on the new version" migration path is impossible for this
upgrade. **In-place open (of a drained, copied directory) is the only path.**
(Once you are on 0.2.0, you *can* create `.albk` backups going forward.)

Keep the 0.1.1 binary available so you can re-read the original copy if
anything looks wrong.

### Known post-upgrade caveats

- **`get_node_at_time` on restored nodes may return `NotFound`.** After
  upgrading, point-in-time reads via `get_node_at_time` on nodes restored from
  a 0.1.x directory may return `NotFound` even at valid coordinates (a
  restore-path limitation carried from 0.1.x, not fixed in 0.2.0). Use
  `get_node_history`, which returns the correct versioned history. This applies
  to both the cleanly-checkpointed and WAL-tail cases and is tracked
  separately.
- **Recovered-node-count discrepancy vs 0.1.x — RESOLVED (Issue #3747).**
  0.2.0 was observed recovering a *different* (higher) node count than 0.1.x
  from the same WAL tail. This is now understood and is **not** a 0.2.0 bug:
  0.2.0's replay watermark (Issue #3419) is **inclusive** and its replay is
  idempotent, so it correctly replays every post-snapshot entry exactly once.
  0.1.1's watermark used an **exclusive** `.next()` boundary that dropped the
  first entry after the snapshot — a real recovery gap on the old version. (The
  test fixture deliberately parks a throwaway `Sentinel` node in that first
  post-snapshot slot, so 0.1.1's "12" silently omitted a real slot that 0.2.0
  recovers.) This count difference is orthogonal to the interned-label story
  above and is independent of the pre-v13 refusal — but it is moot in practice,
  because a pre-v13 tail is now refused outright: drain the WAL before upgrading
  and neither concern applies.

---

## Upgrade checklist

- [ ] **Copy the data directory** (see above) and open the copy first.
- [ ] Bump the dependency to `aletheiadb = "0.2"`.
- [ ] If you relied on the **implicit** default that persisted indexes to
      `./data`, switch to `AletheiaDB::open(path)` or set
      `PersistenceConfig { enabled: true, data_dir, .. }` — the default
      `enabled` flipped to `false` (Issue #3388).
- [ ] Update `ReadOps::get_outgoing_edges` / `get_incoming_edges` /
      `get_outgoing_edges_with_label` call sites to handle
      `Result<Vec<EdgeId>>` (append `?`) — Issue #359. The non-transactional
      `AletheiaDB` convenience getters of the same name are **unchanged**.
- [ ] **(0.1.0 only)** Rename `experimental::*` imports to `semantic_search::*`
      for the 14 graduated search modules. 0.1.1 users already did this.
- [ ] If you drive the **MCP** surface: adapt to the structured error envelope,
      the vector-elision default, and `delete_node` DETACH semantics.
- [ ] Rebuild, and run `cargo update` to refresh the lockfile.

---

## Constructors & entry points

**What changed / what to do:** A new durable one-liner, `AletheiaDB::open(path)`,
is the canonical way to open a persistent database. Adopt it if you like;
nothing you did before breaks.

- `AletheiaDB::open(path)` is **new** in 0.2.0. It did not exist at 0.1.1. It
  is exactly `with_unified_config(durable_config_for_data_dir(path))` — the
  same thing a 0.1.1 user wrote by hand — so you can switch to it with no
  change in behavior.
- `AletheiaDB::new()` is **unchanged**: still ephemeral / tempdir-backed. (The
  CHANGELOG note that `new()` is "explicitly tempdir-backed and ephemeral" was
  a **0.1.1** change, already in your baseline.)
- `open_from_env()`, `with_unified_config`, `durable_config_for_data_dir`, and
  `AletheiaDBConfig::builder()` are all present with the **same signatures**.
  `durable_config_for_data_dir` still lays out `wal/` and `indexes/`
  identically; the only internal difference is that its `..Default::default()`
  now also fills the new `max_interned_strings` field (see below).

```rust
// 0.1.1 (still works in 0.2.0):
let db = AletheiaDB::with_unified_config(durable_config_for_data_dir(&path))?;

// 0.2.0 canonical equivalent:
let db = AletheiaDB::open(&path)?;
```

---

## Persistence config & defaults

**What changed / what to do:** The `PersistenceConfig` default for `enabled`
flipped from `true` to `false`. If you built a config without touching
persistence and relied on implicit `./data` persistence, you must now set it
explicitly (or use `open()` / `durable_config_for_data_dir`, which are
unaffected).

- **`PersistenceConfig::default().enabled` flipped `true` → `false`
  (Issue #3388).** This is a behavioral break **only** for code that built a
  config without touching persistence and relied on implicit persistence to
  `./data`. `durable_config_for_data_dir` and `open()` set `enabled: true`
  explicitly, so they are **not** affected. (A TOML `[persistence]` table that
  omits `enabled` is now also treated as disabled.)

  ```rust
  // If you relied on the implicit default persisting to ./data, make it explicit:
  use aletheiadb::storage::index_persistence::PersistenceConfig;

  let persistence = PersistenceConfig {
      enabled: true,
      data_dir: path.join("indexes"),
      load_on_startup: true,
      ..Default::default()
  };
  // ...or simply switch to AletheiaDB::open(path).
  ```

- **`max_interned_strings` — new field, default cap raised 100K → 10M.** 0.1.1
  had no such config field; the interner cap was a hardcoded `100_000`,
  overridable only via the `ALETHEIADB_MAX_INTERNED_STRINGS` env var. 0.2.0
  adds `PersistenceConfig.max_interned_strings`, defaulting to `10_000_000`
  (~1 GB), read at `open()`. This is a **relaxation**: a 0.1.x database that
  was near the old 100K unique-string ceiling gains far more headroom, and its
  ≤100K persisted interner loads cleanly under the new cap. **No migration
  action is required.** Note the value is process-global and read at open, so
  changing it needs a restart.

- **Struct-literal `PersistenceConfig { .. }` constructors** must account for
  the new field: add `max_interned_strings` or use `..Default::default()`.

  ```rust
  let cfg = PersistenceConfig {
      enabled: true,
      data_dir: path.join("indexes"),
      ..Default::default() // supplies max_interned_strings and any other new fields
  };
  ```

- **Default Cargo features changed** from `["config-toml"]` to
  `["config-toml", "audit-export", "simsimd"]`. Both additions are additive
  (a new export module and a SIMD vector-distance backend) and build-only —
  they do not change runtime behavior of existing code. If you build with
  default features you now compile two extra dependencies; pin
  `default-features = false` to opt out. Encryption is **not** added to
  defaults.

---

## On-disk formats (reference)

| Artifact | Written by 0.1.x | 0.2.0 reader accepts | Notes |
|---|---|---|---|
| WAL segment | magic `GWAL`, **v1** (unencrypted) | `≤ 14` | v1 accepted; pre-v13 labels are raw interner ids. End-to-end tested (`tests/compat_0_1_1_datadir.rs`): a **drained** 0.1.x dir opens with full integrity; an **un-drained pre-v13 WAL tail** is now **refused** on open (`FAILED_PRECONDITION`, Issue #3746) instead of silently corrupting labels — drain first (see above). |
| Index-persistence manifest | **v1** | `≤ 4` | Magic `GGRP`/`GDLT` unchanged; persisted graph structs byte-identical → v1 decodes correctly. **Verified.** |
| `.albk` backup | **did not exist** in 0.1.x | writes v7, reads v1–v7 | No backup off-ramp for this upgrade; 0.2.0 can produce backups going forward. **Verified.** |
| Temporal index / cold storage records | (as written) | backward-compatible (`principal: None` for older records) | **Maintainer-asserted; not independently re-verified here.** A 0.1.x embedded user almost certainly has no cold storage (it is manual opt-in). |

Going forward, with no keyring configured, 0.2.0 writes **new** WAL segments at
v13 plaintext, producing a mixed-version directory (old v1 + new v13). The
reader handles both.

---

## New opt-in subsystems (no action needed)

**What changed / what to do:** 0.2.0 adds several subsystems that are all
inert unless you opt in. **Do nothing to keep 0.1.x behavior.**

- **Namespaces (#3349):** opt-in. Existing 0.1.x entities live in the implicit
  `default` namespace and carry no namespace stamp. Backward-compatible.
- **Encryption at rest (#3616):** feature-gated (`encryption` feature),
  **not** in default features. The WAL writes plaintext unless you configure a
  keyring. Off by default.
- **Cold storage (Redb):** manual opt-in, unchanged from 0.1.x — requires
  explicit `TieredStorage` / `RedbColdStorage` wiring.
- **Changefeed (#3375):** push subscriptions via `subscribe_changes` (and the
  MCP `await_changes` tool). The broadcast only runs to registered
  subscribers; with zero subscribers it is inert. In-memory, no WAL change.

---

## Public API removed/renamed

**What changed / what to do:** Nothing at the crate root was removed or
renamed for a 0.1.1 user — the re-export diff is purely additive. Two items
need attention: a path rename that affects **0.1.0** users only, and the
`ReadOps` edge-getter return-type change.

- **Crate-root re-exports are purely additive.** Every re-export a 0.1.1 user
  wrote at the crate root is still present; 0.2.0 only **adds** names
  (`ChangeFilter`, `Namespace`, `DatabaseStats`, `BackupError`,
  `TraverseDirection`, `ConstraintError`, `FactStatus`, …). No crate-root
  import you wrote was removed.
- **`experimental::* → semantic_search::*` (0.1.0 only).** The 14 search-cohort
  modules (`fishing`, `gestalt`, `cartographer`, `highlander`, `janus`,
  `chameleon`, `semantic_navigator`, `concept_algebra`, `serendipity`,
  `voyager`, `spectre`, `telepathy`, `tapestry`, `horizon`) moved out of
  `experimental::` **in 0.1.0**, before 0.1.1. So this rename is **already
  done** if you are on 0.1.1; it is only required for a **0.1.0** user
  (`aletheiadb::experimental::fishing` → `aletheiadb::semantic_search::fishing`,
  and add `"semantic-search"` alongside `"nova"` in features). These modules
  are behind the `semantic-search` feature; a plain graph/temporal app is
  unaffected either way.
- **`ReadOps` edge-getters now return `Result<Vec<EdgeId>>` (#359).**
  `ReadOps::get_outgoing_edges`, `get_incoming_edges`, and
  `get_outgoing_edges_with_label` changed from `Vec<EdgeId>` to
  `Result<Vec<EdgeId>>`. A missing/invisible node now returns
  `Err(NodeNotFound)`; an existing node with no edges returns `Ok(vec![])`
  (so you can distinguish the two). This affects only call sites that use the
  **`ReadOps` trait inside a transaction** — the non-transactional
  `AletheiaDB::get_outgoing_edges` / `get_incoming_edges` /
  `get_outgoing_edges_with_label` convenience methods are **unchanged**, so a
  user of the top-level convenience API may not be affected at all.

  ```rust
  // Before (0.1.x), inside a transaction using ReadOps:
  let edges: Vec<EdgeId> = txn.get_outgoing_edges(node_id);

  // After (0.2.0):
  let edges: Vec<EdgeId> = txn.get_outgoing_edges(node_id)?;
  // ...or keep the old silent-empty behavior:
  let edges = txn.get_outgoing_edges(node_id).unwrap_or_default();
  ```

  Two related riders: `retract_node_detach` on a plain-deleted node now
  co-retracts 0 edges, and `delete_node_cascade` on a node already deleted
  within the transaction fails fast with `NodeNotFound`.

---

## MCP surface (only if you use it)

**What changed / what to do:** If you drive AletheiaDB over MCP, update any
client that parsed `error` as a string, expected raw embedding arrays in read
responses, or relied on `delete_node` silently orphaning edges. If you only
use the embedded Rust API, skip this section.

- **Tool count grew 30 → 63.** MCP existed in 0.1.1 with 30 tools; 0.2.0 has 63.
- **Structured error envelope (#3234).** Every MCP error is now
  `{"error":{"code","message","retriable","details"?}}` instead of
  `{"error":"<string>"}`. **Breaking for consumers that read `error` as a
  string.**
- **Vector elision by default (#3220).** Read tools now return
  `{type, dim, elided: true}` instead of raw float arrays, unless the request
  passes `include_vectors: true`.
- **`delete_node` DETACH-or-refuse (#3209).** MCP `delete_node` now refuses
  when the node has connected edges (reporting `connected_edges`) unless
  `detach: true` is passed. Any client that previously deleted a connected
  node and got a plain success now receives a refusal instead.

---

## Getting help / where to look

- The [`## [0.2.0]`](../../CHANGELOG.md) section of the CHANGELOG lists every
  change with issue numbers, including the full breaking-changes list.
- [Persistence Guide](PERSISTENCE.md) — WAL, index persistence, cold storage.
- [Tiered Storage](tiered-storage-guide.md) — setting up cold storage.
- [Security Quickstart](security-quickstart.md) — authentication, RBAC, and
  encryption-at-rest key setup.
