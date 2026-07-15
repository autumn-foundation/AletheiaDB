# Migrating from XTDB to AletheiaDB

> **Who this is for.** You run [XTDB](https://xtdb.com) (v1 Crux-lineage or v2)
> today and want its bi-temporality *without the JVM/Clojure lock-in* — plus a
> native graph model, vector search, and an LLM surface XTDB doesn't offer. The
> hard part of leaving XTDB is that **your history is the asset**: a naive
> export/import would flatten years of valid-time and transaction-time into a
> single "imported now" timestamp, destroying exactly what you came for. This
> guide shows a **history-preserving** path.

This is a **concepts + workflow** guide. It maps XTDB's data and temporal model
onto AletheiaDB's real, current (trunk) API, and every Rust snippet here is
compiled against that API. It does **not** translate Datalog *code* — it maps
the *concepts* so you can rebuild your queries on AletheiaDB's Cypher/AQL and
MCP surfaces.

Related reading: [Core Concepts](core-concepts.md) ·
[Getting Started](getting-started.md) ·
[MCP Query Tool](mcp-query-tool.md) ·
[Migrating from Datomic](migrating-from-datomic.md).

---

## Concept mapping

| XTDB | AletheiaDB | Notes |
|------|------------|-------|
| Document (an EDN map with `:xt/id`) | **Node** (label + `PropertyMap`) | XTDB is schemaless documents; AletheiaDB nodes carry a label + typed properties. Choose a label from a document field (e.g. `:type`). |
| `:xt/id` (business key) | Node **business key** during import + `NodeId` after | The importer resolves your `:xt/id` to an AletheiaDB `NodeId` and remembers the mapping (`resolve_key`). Keep `:xt/id` as a property if you query by it. |
| A `:db.type/ref`-style field pointing at another `:xt/id` | **Edge** (typed relationship) | XTDB models relationships as document fields holding another doc's id; AletheiaDB promotes them to first-class edges with their own properties and temporal history. |
| **Valid time** (`::xt/valid-time`) | **Valid time** (`valid_from` / `valid_to`) | **Preserved fully.** Mapped through the #3211/#3221 backfill path. |
| **Transaction time** (`::xt/tx-time`, `::xt/tx-id`) | **Provenance** (`Provenance { source, note, correlation_id, … }`) | See [Temporal semantics](#temporal-semantics-what-is-preserved) — original tx-time is recorded as provenance, *queryable but not on the tx axis*, because AletheiaDB assigns transaction time at import (an invariant, never forged). |
| `[::xt/put doc valid-from valid-to]` | `create_node_with_valid_time` / `create_node_with_options(.with_valid_from(..))` | One source version → one AletheiaDB version at the same valid time. |
| `[::xt/delete eid valid-from]` | `retract_node(id, valid_to)` (edge-free) / `retract_node_detach(id, valid_to)` (connected) | Closes the valid-time interval **without erasing history**. Plain `retract_node` **refuses** if the entity has any connected edge; use `retract_node_detach` to co-retract those edges at the same valid time. |
| `[::xt/evict eid]` (GDPR hard-erase) | *no direct equivalent* | AletheiaDB is append-only for audit integrity. See [What doesn't map](#what-doesnt-map). |
| `(xt/db node valid-time tx-time)` | `get_node_at_time(id, valid, tx)` / `AS OF VALID_TIME … AS OF SYSTEM_TIME …` | Bi-temporal point read. |
| `(xt/entity-history db eid …)` | `get_node_history(id)` → `EntityHistory` | Full per-entity version list. |
| `(xt/q db '{:find … :where …})` Datalog | **Cypher** (`db.execute_cypher`) / **AQL** / MCP `traverse` + `query` | Graph pattern queries; see [Query translation](#query-translation). |

---

## Temporal semantics: what is preserved

AletheiaDB and XTDB are both **bi-temporal**, so the core promise — *"what did
we believe was true, as of when"* — transfers directly. The one asymmetry to
internalize:

- **Valid time is preserved with full fidelity.** Every `::xt/valid-time` on
  every source version becomes the `valid_from` of an AletheiaDB version, as a
  half-open `[valid_from, valid_to)` interval. Retractions close the interval
  (`valid_to`). For an entity that never changed value, `AS OF VALID_TIME`
  queries return equivalent answers directly; for an entity whose value changed,
  read the subtlety in [update = supersession](#the-one-subtlety-that-bites-update--supersession)
  below — the history is all there, but a superseded segment is recalled by
  anchoring both temporal dimensions.
- **Transaction time is *re-recorded*, not forged.** AletheiaDB's transaction
  time is **system-assigned at commit** and cannot be set by a caller — this is
  a correctness invariant (transaction time is the database's own audit axis; a
  forgeable tx-time is worthless for audit). So your original XTDB `::xt/tx-time`
  and `::xt/tx-id` are written into **[provenance](derivation-lineage.md)**
  (`source` / `note` / `correlation_id`), where they remain queryable — you can
  filter and display them — but they live *beside* AletheiaDB's tx axis, not
  *on* it.

Practically: an XTDB "as of tx-time T" query (auditing *when the database
learned* a fact) maps to a **provenance filter** on the recorded original
tx-time, not to AletheiaDB's `AS OF SYSTEM_TIME` (which reflects when the
*import* happened). This is called out honestly in
[What doesn't map](#what-doesnt-map).

### The one subtlety that bites: update = supersession

There is a genuine model difference in how the two engines record a *changed*
fact, and it is the single most important thing to understand before you replay
history.

- In **XTDB**, `[::xt/put {…:title "CEO"} #inst"2023"]` adds a **new valid-time
  segment**: at the latest transaction time the timeline reads
  *Engineer during `[2021, 2023)`, CEO during `[2023, ∞)`* — **both segments
  coexist**. A valid-time-only query at 2022 returns Engineer.
- In **AletheiaDB**, the same change replayed as
  `update_node_with_options(alice, {title: "CEO"}, with_valid_from(2023))` is a
  **bi-temporal supersession** (a *correction as of the import transaction
  time*). At the **latest transaction time**, the current-belief timeline is the
  newest version's valid interval `[2023, ∞)` = CEO; a valid-time-only read at
  2022 returns **NotFound**, not Engineer.

**Nothing is lost** — the earlier "Engineer" segment is fully preserved on the
*transaction-time* axis. You recover it by anchoring **both** coordinates:

```rust
// Superseded valid segment: valid-time 2022 AND a transaction time recorded
// *before* the "became CEO" correction was replayed.
let engineer = db.get_node_at_time(alice, valid_2022, tx_before_correction)?;
// -> "Engineer"   (valid-time alone at the latest tx would be NotFound)
```

`get_node_history(alice)` lists every source version, and
`get_node_at_version(alice, n)` fetches each one directly — so a fidelity report
that walks versions sees 100% of them.

**Consequence for query equivalence.** An XTDB *valid-time-only* `as-of` query
against a value that later changed maps to an AletheiaDB read that anchors *both*
the valid time **and** a transaction time inside that era. Because the importer
records each source version's original `::xt/tx-time` in provenance and preserves
per-entity transaction ordering, your probe set can pick the right
transaction-time anchor per era. This is the same "recall a superseded state by
anchoring both dimensions" rule AletheiaDB's point-in-time reads document
generally — it is not specific to migration. It is enumerated in
[What doesn't map](#what-doesnt-map).

---

## Migration workflow

Three steps: **extract** history from XTDB → **transform** to a flat artifact →
**load** into AletheiaDB.

### 1. Extract — dump per-entity history from XTDB

XTDB already exposes the full bi-temporal history of every entity through
`xtdb.api/entity-history`. Dump each version (with its document body, valid
time, tx time, and tx id) to a line-oriented artifact. A small Clojure script:

```clojure
(require '[xtdb.api :as xt]
         '[clojure.data.json :as json])

;; For each entity id, emit one JSON line per historical version, ascending.
(with-open [w (clojure.java.io/writer "xtdb-history.jsonl")]
  (doseq [eid (all-entity-ids node)]           ; your enumeration of :xt/id values
    (doseq [v (xt/entity-history (xt/db node) eid :asc {:with-docs? true})]
      (.write w (str (json/write-str
                       {:xt/id       eid
                        :valid-time  (str (::xt/valid-time v))
                        :tx-time     (str (::xt/tx-time v))
                        :tx-id       (::xt/tx-id v)
                        :doc         (::xt/doc v)})   ; nil doc = a delete/retraction
                     "\n"))))))
```

Each line is one **source version**: the document as it was, the valid time it
took effect, and the original transaction coordinates. A `nil`/absent `:doc` is
a delete (interval close).

> **XTDB v2** exposes the same information through its history/temporal SQL
> surface (`system_time` / `valid_time` columns and `FOR ALL SYSTEM_TIME` /
> `FOR ALL VALID_TIME` — the SQL:2011 `FOR SYSTEM_TIME ALL` form). Export the
> equivalent per-row history to the same JSONL shape; the load step is
> identical. The *ingestion artifact* — one JSON object per historical version
> with `valid-time`, original `tx-time`/`tx-id`, and the value — is the contract,
> not any specific XTDB API version.

### 2 & 3. Transform + load into AletheiaDB

You have two load paths depending on how much history you need to preserve.

#### Path A — bulk snapshot with valid time (fast, CSV/JSONL)

If you only need the **current value of each entity, stamped at its valid-time
origin** (a common "start fresh but keep effective dates" migration), flatten to
one row per entity and use the bulk importer (`--features import`). It maps a
`valid_time` column straight onto the backfill path:

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::api::import::{ColumnType, EdgeMapping, FailureMode, LabelSource, NodeMapping};

let db = AletheiaDB::new()?;
let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);

// people.csv: xt_id,name,role,valid_from
let node_report = importer.nodes_from_csv(
    "people.csv",
    NodeMapping::new(LabelSource::fixed("Person"), "xt_id")
        .property("name", "name", ColumnType::String)
        .property("role", "role", ColumnType::String)
        .valid_time_column("valid_from"),      // ISO date/timestamp → valid_from
)?;

// knows.csv: src,dst,valid_from  (endpoints resolved by :xt/id business key)
let edge_report = importer.edges_from_csv(
    "knows.csv",
    EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst").valid_time_column("valid_from"),
)?;

println!(
    "{} nodes, {} edges, {} skipped, {} unresolved",
    node_report.nodes_imported, edge_report.edges_imported,
    node_report.skipped.len(), edge_report.unresolved_endpoints.len(),
);
```

`FailureMode::SkipAndReport` honors the bulk-import failure contract (#3211):
malformed rows and unresolved endpoints are **collected with their locations**
in the returned `ImportReport` rather than aborting the load. Use
`FailureMode::Abort` (the default) if you want all-or-nothing.

#### Path B — full history replay with provenance (fidelity-preserving)

##### Recommended: the built-in `xtdb_import` (Issue #3384)

Since Issue #3384 you do **not** hand-write the replay loop. Dump your entity
history as **EDN** (XTDB's native serialization — a single top-level vector of
per-entity `{:xt/id … :history [ … ]}` maps taken with
`(xt/entity-history … {:with-docs? true})`) and hand the file to the importer:

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::api::import::XtdbOptions;

let db = AletheiaDB::new()?;
let mut importer = db.import();

// One call: replays every version of every entity, preserving valid-time,
// provenance (source `xtdb-import::<file>`, note `xtdb tx-id=… tx-time=…`,
// correlation `xt:tx:<id>`), supersession, ref→edge, and nil-doc deletes.
let report = importer.xtdb_import("xtdb-history.edn", &XtdbOptions::default())?;

assert!(report.zero_loss);              // nothing skipped / unresolved / unsupported
println!("entities  = {}", report.entities_read);
println!("versions  = {}", report.node_versions_written); // creates + updates
println!("edges     = {}", report.edges_created);
```

**Options** (`XtdbOptions`): `label_field` (doc field used as the node label,
default `"type"`), `default_label` (fallback `"Entity"`, reported as a
`default_label` coercion), `id_property` (where the `:xt/id` is stored, default
`"xt/id"`), `auto_detect_refs` (default `true` — a scalar field whose value
resolves to another entity's `:xt/id` becomes an edge), `ref_fields`
(field→edge-label overrides; a vector-of-ids field fans out to one edge per
element), and `failure_mode` (`Abort` — the default — or `SkipAndReport`).

**CLI** (`--features import`):

```bash
aletheia import --format xtdb --history xtdb-history.edn \
  [--label-field type] [--default-label Entity] \
  [--ref-field employer=EMPLOYER ...] [--on-error abort|skip] \
  [--report report.json]
```

The importer keys are matched by **local name ignoring namespace**, so both the
canonical `:xtdb.api/valid-time` and a short `:xt/valid-time` are accepted; a
`#crux/id` (or any other) tagged literal in an ignored field is tolerated. The
EDN reader is panic-free and returns a typed, line/column-tagged error on
malformed input (truncation, an unbalanced brace, a bad `#inst`, a pathological
deeply-nested value).

**AS-OF probe set — verify the migration landed.** After importing, sample a
grid of bi-temporal coordinates and confirm each reconstructs the source
version. Recall that a **superseded** segment needs *both* dimensions anchored
(see [update = supersession](#the-one-subtlety-that-bites-update--supersession)):

```rust
use aletheiadb::core::temporal::time;
let alice = importer.resolve_key("alice").unwrap();
let hist  = db.get_node_history(alice)?;
let v0_tx = hist.versions[0].temporal.transaction_time().start(); // Engineer recorded

// Superseded "Engineer" era — anchor valid-time AND v0's transaction time:
let engineer = db.get_node_at_time(alice, time::from_secs(1_654_041_600), v0_tx)?; // 2022-06
// Current-knowledge "CEO" era — valid-time + now is enough:
let ceo = db.get_node_at_valid_time(alice, time::from_secs(1_685_577_600))?;        // 2023-06
// After the delete: not valid at current knowledge (NotFound):
assert!(db.get_node_at_valid_time(alice, time::from_secs(1_719_792_000)).is_err()); // 2024-07
// Before creation: NotFound at every tx.
assert!(db.get_node_at_time(alice, time::from_secs(1_577_836_800), v0_tx).is_err()); // 2020-01
```

The library test suite runs this grid at **≥20 coordinates per fixture**
(before-create / each era / the supersession boundary / after-delete),
asserting the reconstructed `title` equals the source segment.

##### Under the hood — the manual equivalent

The importer does exactly what the loop below does — replay in ascending
valid-time order per entity, `create_node_with_options` for the first version
and `update_node_with_options` for each subsequent one, **each in its own
commit** so the valid-time segments stay independently reconstructable. This is
where the original tx-time/tx-id land as provenance:

```rust
use aletheiadb::{AletheiaDB, PropertyMapBuilder, Provenance};
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::temporal::time;

let db = AletheiaDB::new()?;

// Source version 1 (from the JSONL): valid-from 2021-03-01, XTDB tx-id 42.
let alice = db.create_node_with_options(
    "Person",
    PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("title", "Engineer")
        .build(),
    WriteRequestOptions::new()
        .with_valid_from(time::from_secs(1_614_556_800))   // 2021-03-01
        .with_provenance(
            Provenance::builder()
                .source("xtdb-import")
                .note("xtdb tx-id=42 tx-time=2021-03-01T00:00:00Z")
                .correlation_id("xt:tx:42")
                .confidence(1.0)
                .build()?,
        ),
)?;

// Source version 2 of the SAME entity: became CEO, valid-from 2023-01-01, tx-id 99.
db.update_node_with_options(
    alice,
    PropertyMapBuilder::new().insert("title", "CEO").build(),
    WriteRequestOptions::new()
        .with_valid_from(time::from_secs(1_672_531_200))   // 2023-01-01
        .with_provenance(
            Provenance::builder()
                .source("xtdb-import")
                .note("xtdb tx-id=99 tx-time=2023-01-01T00:00:00Z")
                .correlation_id("xt:tx:99")
                .build()?,
        ),
)?;
```

An XTDB `[::xt/delete eid valid-from]` (a version with no document) becomes a
retraction that closes the valid-time interval without erasing history:

```rust
use aletheiadb::core::temporal::time;
// The fact stopped being true on ~2024-06-01. AS OF VALID_TIME before that
// instant still returns the node; at/after it does not.
//
// `retract_node` is for edge-free entities: it **refuses** (a `ValidationFailed`
// error) if the node has ANY connected edge. A normal graph entity has
// relationships, so use `retract_node_detach`, which co-retracts the connected
// edges at the same valid time and reports how many in `edges_retracted`.
let result = db.retract_node_detach(alice, time::from_secs(1_717_200_000))?;
println!("edges_retracted = {}", result.edges_retracted);
```

> **Timestamps.** The `time` module offers `from_secs` / `from_millis` /
> `now()`. Convert your exported ISO-8601 valid-times to epoch seconds when
> replaying via the Rust API. The **CSV/JSONL bulk importer** (Path A) parses
> ISO date/timestamp strings in the `valid_time` column for you.

#### Determinism & idempotency (AC #5)

- **Per-entity ordering:** replay each entity's versions in **ascending
  valid-time** order (the `:asc` dump above guarantees it). AletheiaDB preserves
  that order as the entity's version chain. This ordering is **enforced, not
  advisory**: an `update_node…` whose `valid_from` precedes the node's creation
  `valid_from` is **rejected** at write time (`validate_valid_from_not_before_creation`),
  so a mis-ordered replay fails loudly instead of silently corrupting the chain.
- **Re-runs:** `xtdb_import` is **idempotent-or-refused** — before any write it
  refuses (with an `AlreadyImported` error) if the business key already exists.
  The guard is **durable**, not merely in-session: it probes the target
  database's current state for a node already carrying this importer's
  business-key property (`xt/id` by default), so a *fresh-process* re-run against
  a persistent target is refused too — not just a repeat call on the same
  `Importer`. (One caveat: a node that was *deleted* in the first import is
  absent from current state, so re-importing an all-deleted dataset is not
  detected by the current-state probe; any surviving node triggers the refusal.)
  Import into a **fresh** database and restart from empty rather than re-running
  over a partial import. (The hand-written loop has no such guard — check
  `importer.resolve_key(xt_id)` yourself if you replay manually.)

---

## Query translation

XTDB Datalog → AletheiaDB. AletheiaDB's graph surface is **Cypher** (with
temporal + vector extensions; `--features cypher`), **AQL**, and the **MCP**
tools. A few representative translations:

**1. Current-state entity lookup by attribute.**

```clojure
;; XTDB
(xt/q (xt/db node)
      '{:find [(pull ?p [*])]
        :where [[?p :name "Alice"] [?p :type :Person]]})
```
```cypher
// AletheiaDB (Cypher)
MATCH (p:Person {name: 'Alice'}) RETURN p
```

**2. One-hop relationship traversal.**

```clojure
;; XTDB — :knows is a ref field pointing at another :xt/id
(xt/q (xt/db node)
      '{:find [?fname]
        :where [[?p :name "Alice"] [?p :knows ?f] [?f :name ?fname]]})
```
```cypher
// AletheiaDB — the ref field became a KNOWS edge
MATCH (:Person {name: 'Alice'})-[:KNOWS]->(f:Person) RETURN f.name
```
Or via MCP without writing a query: `traverse` from Alice's `NodeId` over
`KNOWS`.

**3. Bi-temporal AS OF (the payoff).**

```clojure
;; XTDB — "who did Alice know, as of valid-time 2022-01-01?"
(xt/q (xt/db node #inst "2022-01-01")
      '{:find [?fname]
        :where [[?p :name "Alice"] [?p :knows ?f] [?f :name ?fname]]})
```
```cypher
// AletheiaDB (Cypher temporal extension)
AS OF VALID_TIME '2022-01-01T00:00:00Z'
MATCH (:Person {name: 'Alice'})-[:KNOWS]->(f:Person) RETURN f.name
```
```rust
// AletheiaDB (Rust API) — bi-temporal point read of a single entity.
// For a value that never changed, valid-time + now is enough:
let now = time::now();
let alice_2022 = db.get_node_at_time(alice, time::from_secs(1_640_995_200), now)?;

// For a value that WAS later superseded, anchor both dimensions to recall the
// old segment (see "update = supersession"):
let engineer = db.get_node_at_time(alice, time::from_secs(1_640_995_200), tx_before_change)?;
```
Via **MCP**, `traverse` accepts `as_of_valid_time` / `as_of_transaction_time`
(set both to recall superseded state), and `find_nodes_at_time` resolves
*"the Person named Alice, as of 2022-01-01"* without a prior `NodeId`.

**4. Auditing when the database learned a fact (the asymmetry).**

XTDB's `(xt/db node valid-time tx-time)` with a **tx-time** coordinate asks
"what did the DB know at tx-time T". On AletheiaDB the equivalent is a
**provenance filter on the recorded original tx-time**, because AletheiaDB's own
tx axis reflects the import, not XTDB's original write. Read it back with
`get_node_provenance` (Rust) or the provenance fields on MCP read responses.

---

## What doesn't map

Enumerated honestly — nothing is silently dropped.

- **`::xt/evict` (hard erasure).** AletheiaDB is append-only by design (audit
  integrity), so there is no import-time equivalent of XTDB's GDPR "evict".
  Erasure requests are handled operationally, not modeled as a historical
  version. Records that were *evicted* in XTDB simply won't be in your export.
- **Original transaction time on the tx axis.** As above: preserved as
  provenance, not as AletheiaDB transaction time. `AS OF SYSTEM_TIME` on
  imported data reflects **import** time, uniformly across all migrated
  versions.
- **Coexisting valid-time segments at the latest transaction time.** XTDB keeps
  every valid-time segment of a changed entity live at the latest tx; AletheiaDB
  represents a change as a **supersession**, so an earlier segment lives on the
  transaction-time axis and is recalled by anchoring both dimensions (see
  [update = supersession](#the-one-subtlety-that-bites-update--supersession)).
  The data is fully preserved; the *single-coordinate query ergonomics differ*.
- **Disjoint valid-time intervals for one `:xt/id` (reincarnation).** An entity
  that is valid, then has an explicit `::xt/valid-to` gap, then becomes valid
  *again* under the same id cannot be fully reconstructed: closing the first
  interval **retracts** the node, and the engine removes a retracted node from
  current state, so it cannot be "revived" through the write API. The importer
  honors the first interval's close (so an in-gap `AS OF VALID_TIME` correctly
  returns **absent**, never a stale fact) and **reports** each un-revivable later
  segment as `disjoint_reincarnation` in the fidelity report — it is never
  silently dropped, and `zero_loss` becomes `false`. Contiguous supersessions
  (no gap) are unaffected.
- **Schemaless heterogeneity within one `:xt/id`.** If a single XTDB entity's
  document shape changes radically across versions (different property sets),
  you must pick one AletheiaDB **label** for it; per-version properties still
  vary freely, but the label is stable.
- **Speculative / open transactions & `with-tx`.** XTDB's speculative
  transactions have no persisted history to export and are out of scope.
- **Document sub-structure / nested maps.** AletheiaDB properties are typed
  scalars and vectors. Flatten nested EDN maps to dotted property keys, or model
  the nested entity as its own node + edge.

---

## What you gained

Once your history is in AletheiaDB, you have capabilities XTDB never offered on
the same data:

**1. Time-travel, same as before — verify equivalence.**

```rust
let now = time::now();
// Current-belief timeline: what is true at valid-time 2024, as best known now.
let alice_2024 = db.get_node_at_time(alice, time::from_secs(1_717_200_000), now)?;

// A superseded valid segment: anchor valid-time AND a tx-time inside that era.
let alice_2022 = db.get_node_at_time(alice, time::from_secs(1_640_995_200), tx_before_change)?;

// The full recorded version chain (one entry per source version) — nothing lost.
let history = db.get_node_history(alice)?;
println!("versions = {}", history.version_count());
```

> Verified end-to-end: replaying two versions (Engineer @2021 → CEO @2023) and
> reading them back yields `versions = 2`, `alice@2022 = Engineer` (both
> dimensions anchored), `alice@2024 = CEO` — the exact behavior this guide's
> snippets were compiled and run against.

**2. Provenance filtering — your original XTDB tx metadata, queryable.**

```rust
if let Some(prov) = db.get_node_provenance(alice)? {
    println!("origin: {:?} / {:?}", prov.source(), prov.note());
}
```

**3. A vector index over migrated text — semantic search XTDB can't do.**

```rust
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::PropertyMapBuilder;

db.vector_index("embedding")
    .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    .enable()?;

let doc = db.create_node(
    "Document",
    PropertyMapBuilder::new()
        .insert("title", "Migrated note")
        .insert_vector("embedding", &embedding)   // your &[f32] from any embedder
        .build(),
)?;
use aletheiadb::SimilarityQuery;
let similar = db.similarity_search(SimilarityQuery::from_node(doc).k(10))?; // k-NN
```

You can now combine all three — graph traversal + vector similarity +
bi-temporal `AS OF` — in a single [hybrid query](hybrid-query-guide.md), on the
history you brought over from XTDB, with no JVM in sight.
