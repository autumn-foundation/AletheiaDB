# Migrating from Datomic to AletheiaDB

> **Who this is for.** You run [Datomic](https://www.datomic.com) today and want
> to keep its immutable, time-aware audit trail *without the JVM/Clojure lock-in*
> — and you'd like a native graph model, vector search, and an LLM surface
> Datomic doesn't offer. Datomic's history *is the asset*: a naive dump/reload
> would collapse years of `:db/txInstant` audit history into a single "imported
> now". This guide shows a **history-preserving** path.

This is a **concepts + workflow** guide. It maps Datomic's data and temporal
model onto AletheiaDB's real, current (trunk) API, and every Rust snippet here is
compiled against that API. It does **not** translate Datalog *code* — it maps
the *concepts* so you can rebuild your queries on AletheiaDB's Cypher/AQL and
MCP surfaces.

> **The headline difference:** Datomic is **single-time-axis** — it records
> *transaction time* (`:db/txInstant`) only; it has no separate valid-time axis.
> AletheiaDB is **bi-temporal**. So Datomic's history maps onto AletheiaDB's
> **valid-time** axis by a documented, configurable rule (default:
> `:db/txInstant → valid_from`), while the original transaction id/instant are
> *also* recorded as **provenance** and AletheiaDB's own transaction axis records
> the import. This choice, and its consequences, are spelled out in
> [Temporal semantics](#temporal-semantics-the-single-axis-mapping-rule).

Related reading: [Core Concepts](core-concepts.md) ·
[Getting Started](getting-started.md) ·
[MCP Query Tool](mcp-query-tool.md) ·
[Migrating from XTDB](migrating-from-xtdb.md).

---

## Concept mapping

| Datomic | AletheiaDB | Notes |
|---------|------------|-------|
| **Entity** (set of datoms sharing an `E`) | **Node** (label + `PropertyMap`) | Pick a node **label** from an entity's type attribute (e.g. `:person/…` namespace, or a `:type` attribute). |
| `:db/id` (entity id) / `:db/ident` | Node **business key** at import + `NodeId` after | Keep `:db/id` (or a `:db.unique/identity` attribute) as a property so you can resolve entities post-import. |
| **Attribute** `:ns/name` (non-ref) | **Property** `ns/name` | The `A` of a datom becomes a typed property on the node. |
| **Ref attribute** (`:db/valueType :db.type/ref`) | **Edge** (typed relationship) | A datom whose value is another entity id becomes a first-class edge. You provide the ref-attribute → edge-label mapping (explicit mapping config). |
| **Assertion** `[:db/add e a v tx]` | `create_node` / `update_node…` (a new version) | First assertion of an entity → create; later ones → an update (a new version). |
| **Retraction** `[:db/retract e a v tx]` (whole entity) | `retract_node(id, valid_to)` (edge-free) / `retract_node_detach(id, valid_to)` (connected) | Closes the valid-time interval without erasing history. Plain `retract_node` **refuses** if the entity has any connected edge; use `retract_node_detach` to co-retract those edges at the same valid time. |
| **Cardinality-many** (`:db.cardinality/many`) | multiple **edges** (ref) / a list-valued property or fan-out nodes (scalar) | AletheiaDB properties are single-valued; see [What doesn't map](#what-doesnt-map). |
| **`:db/txInstant`** (transaction wall-clock) | **`valid_from`** (default rule) **+** provenance | The core mapping: Datomic's only time axis becomes AletheiaDB valid time; the raw `txInstant`/tx-id are *also* kept as provenance. |
| **Transaction id** (`tx` / `:db/id` of the tx entity) | **Provenance** (`correlation_id` / `note`) | Recorded so you can group facts by their original Datomic transaction. |
| `d/as-of db t` | `get_node_at_time(id, valid=t, tx=now)` / `AS OF VALID_TIME t` | Because Datomic tx-time maps to AletheiaDB **valid** time, Datomic `as-of` becomes an AletheiaDB **valid-time** `AS OF` (anchor **both** dims for a *superseded* value — `tx=now` returns NotFound for a value later changed; see the [update = supersession](#the-one-subtlety-that-bites-update--supersession) section). |
| `d/since db t` | `AS OF VALID_TIME` range / `BETWEEN t AND now` | Complement of `as-of`; expressed as a valid-time bound. |
| `d/history db` | `get_node_history(id)` → `EntityHistory` | Full per-entity version list. |
| `d/q '[:find …]` Datalog | **Cypher** / **AQL** / MCP `traverse` + `query` | Graph pattern queries; see [Query translation](#query-translation). |
| Schema (`:db/valueType`, `:db/cardinality`) | Node **labels** + typed **properties** | AletheiaDB is not schema-first; encode types at import via the mapping's `ColumnType`. |

---

## Temporal semantics: the single-axis mapping rule

Datomic and AletheiaDB both treat data as **immutable and accumulate-only**, so
the audit promise transfers. The essential asymmetry is dimensionality:

- **Datomic has one time axis** — transaction time, stamped as `:db/txInstant`
  on each transaction. "When was this true in the real world?" is not a separate,
  first-class coordinate.
- **AletheiaDB has two** — valid time *and* transaction time.

**The mapping rule (configurable).** By default, each datom's transaction time
(`:db/txInstant`) is mapped to the AletheiaDB **`valid_from`** of the resulting
version. This makes Datomic's `d/as-of`/`d/history` audit queries land on
AletheiaDB's valid-time axis, where they read naturally. Simultaneously:

- The raw `:db/txInstant` and Datomic transaction id are recorded as
  **[provenance](derivation-lineage.md)** (`note` / `correlation_id`), so the
  original audit metadata is queryable.
- AletheiaDB's **own** transaction time is **system-assigned at import** (an
  invariant — never forged). `AS OF SYSTEM_TIME` on imported data therefore
  reflects **import** time, uniformly.

If your Datomic data *also* carried a domain "effective date" attribute
(a common pattern precisely *because* Datomic lacks a valid axis), you can point
the rule at that attribute instead of `:db/txInstant` — that recovers a true
valid-time axis for those facts. That is the "configurable rule" knob.

### The one subtlety that bites: update = supersession

Just as in the [XTDB guide](migrating-from-xtdb.md#the-one-subtlety-that-bites-update--supersession),
replaying a *changed* attribute value as a backdated AletheiaDB update is a
**bi-temporal supersession** (a correction as of the import transaction time),
**not** a coexisting new segment:

- Replay `title = Engineer` (valid_from 2021) then `title = CEO`
  (valid_from 2023) via `update_node_with_options`.
- At the **latest** transaction time, `get_node_at_time(alice, valid=2022, now)`
  returns **NotFound** — the current-belief timeline starts the new segment at
  2023.
- The earlier "Engineer" segment is **not lost**: recover it by anchoring
  **both** dimensions — `get_node_at_time(alice, valid=2022,
  tx=before_the_correction)` → Engineer — or via `get_node_at_version` /
  `get_node_history`.

This is verified end-to-end (the snippets below were compiled and run): a
two-version replay yields `versions = 2`, `alice@(2022, before-correction) =
Engineer`, `alice@(2024, now) = CEO`. It is enumerated in
[What doesn't map](#what-doesnt-map).

---

## Migration workflow

Three steps: **extract** the datom/transaction stream from Datomic → **transform**
to a flat artifact → **load** into AletheiaDB.

### 1. Extract — dump the transaction log / datom stream

Datomic exposes the complete, ordered datom history through `d/tx-range` (the
transaction log) and `d/datoms` / `d/history`. Dump each datom as
entity/attribute/value/tx/op, resolving `:db/txInstant` per transaction, to a
line-oriented artifact. A small Clojure sketch:

```clojure
(require '[datomic.api :as d])

;; Emit one JSON line per datom, in transaction order, with the tx wall-clock.
(with-open [w (clojure.java.io/writer "datomic-log.jsonl")]
  (doseq [{:keys [t data]} (d/tx-range (d/log conn) nil nil)]
    (let [tx-inst (:v (first (filter #(= :db/txInstant (d/ident db (:a %))) data)))]
      (doseq [datom data]
        (.write w (str (clojure.data.json/write-str
                         {:e   (:e datom)
                          :a   (d/ident db (:a datom))   ; keyword attr name
                          :v   (:v datom)
                          :tx  t
                          :tx-instant (str tx-inst)
                          :op  (:added datom)})           ; true=assert, false=retract
                       "\n"))))))
```

Each line is one datom. `:op false` is a retraction. Group by `:e` (entity) and
order by `:tx` to reconstruct each entity's version sequence.

### 2 & 3. Transform + load into AletheiaDB

Two load paths, exactly as in the [XTDB guide](migrating-from-xtdb.md#2--3-transform--load-into-aletheiadb):

#### Path A — bulk snapshot with valid time (CSV/JSONL, `--features import`)

Collapse the datom stream to one row per entity (its current attributes) plus an
edge row per current ref-datom, carrying the origin `:db/txInstant` in a
`valid_from` column, and use the bulk importer:

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::api::import::{ColumnType, EdgeMapping, FailureMode, LabelSource, NodeMapping};

let db = AletheiaDB::new()?;
let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);

// people.csv: db_id,name,title,valid_from   (valid_from = origin :db/txInstant)
let node_report = importer.nodes_from_csv(
    "people.csv",
    NodeMapping::new(LabelSource::fixed("Person"), "db_id")
        .property("name", "name", ColumnType::String)
        .property("title", "title", ColumnType::String)
        .valid_time_column("valid_from"),
)?;

// employs.csv: src,dst,valid_from  — a :db.type/ref datom becomes an EMPLOYS edge
let edge_report = importer.edges_from_csv(
    "employs.csv",
    EdgeMapping::new(LabelSource::fixed("EMPLOYS"), "src", "dst").valid_time_column("valid_from"),
)?;

println!(
    "{} nodes, {} edges, {} skipped, {} unresolved",
    node_report.nodes_imported, edge_report.edges_imported,
    node_report.skipped.len(), edge_report.unresolved_endpoints.len(),
);
```

`FailureMode::SkipAndReport` collects malformed rows and unresolved ref endpoints
**with their locations** in the returned `ImportReport` instead of aborting
(the #3211 bulk-import failure contract); `FailureMode::Abort` (default) is
all-or-nothing.

#### Path B — full history replay with provenance (fidelity-preserving)

##### Recommended: the built-in `datomic_import` (Issue #3384)

Since Issue #3384 you do **not** hand-write the replay loop. Export two **EDN**
artifacts — a flat, tx-ordered **datom stream** (`d/tx-range` → a vector of
`{:e … :a … :v … :tx … :tx-instant #inst … :op true/false}` maps) and an
**attribute-schema map** (`ident → {:db/valueType … :db/cardinality …}`, which
tells the importer which attributes are refs) — and hand both to the importer:

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::api::import::DatomicOptions;

let db = AletheiaDB::new()?;
let mut importer = db.import();

// One call: groups datoms by :e, orders by :tx, makes each tx a node version
// stamped at its :db/txInstant, promotes :db.type/ref attrs to edges, and stamps
// provenance (source `datomic-import::<file>`, note `datomic tx=… txInstant=…`,
// correlation `datomic:tx:<tx>`).
let report = importer.datomic_import(
    "datomic-log.edn",
    "datomic-schema.edn",
    &DatomicOptions::default(),
)?;

assert!(report.zero_loss);
println!("entities = {}", report.entities_read);
println!("versions = {}", report.node_versions_written);
println!("edges    = {}", report.edges_created);
```

**Options** (`DatomicOptions`): `label_attr` (attribute whose value is the node
label; default derives the label from the attribute **namespace** —
`:person/name` → `Person`), `default_label` (fallback `"Entity"`), `id_property`
(where `:e` is stored, default `"db/id"`), `valid_time_attr` (redirect the
single-axis rule off `:db/txInstant` onto a domain effective-date attribute),
`cardinality_many_scalar` (`LastValue` — the default, reported — or `List`, which
accumulates all asserted values into an array), and `failure_mode` (`Abort` or
`SkipAndReport`).

**Reported, never dropped.** Schema-alteration datoms (`:db/ident`,
`:db.install/attribute`, …), transaction functions (`:db/fn`), and excision
(`:db/excise`) are skipped and enumerated in `report.unsupported` (with
`zero_loss = false`); a scalar `:db/retract` with no same-tx re-assert becomes an
explicit `Null` reported as `attr_retract_as_null`; an attribute absent from the
schema is kept as a scalar string property and reported as `unknown_attribute`
(never silently guessed to be a ref); and a `:db.type/ref` datom whose `:v` is
**not** a scalar entity key (a map, vector, …) cannot form an edge and is
reported as `ref_non_scalar_value` rather than silently dropped.

**CLI** (`--features import`):

```bash
aletheia import --format datomic --datoms datomic-log.edn --schema datomic-schema.edn \
  [--valid-time-attr person/effective-date] [--default-label Entity] \
  [--label-attr person/kind] [--on-error abort|skip] [--report report.json]
```

**AS-OF probe set — verify the migration landed.** As with XTDB, a **superseded**
segment needs both dimensions anchored; the current segment reads at `now`:

```rust
use aletheiadb::core::temporal::time;
let person = importer.resolve_key("200").unwrap();
let v0_tx  = db.get_node_history(person)?.versions[0].temporal.transaction_time().start();

// Superseded "Engineer" era (txInstant 2021-03) — anchor valid-time AND v0's tx:
let engineer = db.get_node_at_time(person, time::from_secs(1_622_505_600), v0_tx)?; // 2021-06
// Current-knowledge "CEO" era (txInstant 2023-01):
let ceo = db.get_node_at_valid_time(person, time::from_secs(1_685_577_600))?;       // 2023-06
// Before creation is NotFound at v0's tx:
assert!(db.get_node_at_time(person, time::from_secs(1_577_836_800), v0_tx).is_err()); // 2020-01
```

The test suite runs this grid at **≥20 coordinates**, asserting each
reconstructed segment.

##### Under the hood — the manual equivalent

The importer does exactly what the loop below does — replay each entity's
assertions in transaction order, `create_node_with_options` for the first tx and
`update_node_with_options` per subsequent change, **each in its own commit**.
Each carries the origin `:db/txInstant` as `valid_from` (the mapping rule) and
the Datomic tx metadata as provenance:

```rust
use aletheiadb::{AletheiaDB, PropertyMapBuilder, Provenance};
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::temporal::time;

let db = AletheiaDB::new()?;

// Datom set for entity 17592186045418, transaction t=1000, txInstant 2021-03-01.
let alice = db.create_node_with_options(
    "Person",
    PropertyMapBuilder::new()
        .insert("db/id", 17_592_186_045_418_i64)   // preserve the Datomic entity id
        .insert("name", "Alice")
        .insert("title", "Engineer")
        .build(),
    WriteRequestOptions::new()
        .with_valid_from(time::from_secs(1_614_556_800))   // :db/txInstant → valid_from
        .with_provenance(
            Provenance::builder()
                .source("datomic-import")
                .note("datomic tx=1000 txInstant=2021-03-01T00:00:00Z")
                .correlation_id("datomic:tx:1000")
                .confidence(1.0)
                .build()?,
        ),
)?;

// Later transaction t=2000 (txInstant 2023-01-01) asserts :person/title "CEO".
db.update_node_with_options(
    alice,
    PropertyMapBuilder::new().insert("title", "CEO").build(),
    WriteRequestOptions::new()
        .with_valid_from(time::from_secs(1_672_531_200))   // this tx's txInstant
        .with_provenance(
            Provenance::builder()
                .source("datomic-import")
                .note("datomic tx=2000 txInstant=2023-01-01T00:00:00Z")
                .correlation_id("datomic:tx:2000")
                .build()?,
        ),
)?;
```

A whole-entity retraction (`[:db/retract e … tx]` that removes the entity)
becomes a valid-time retraction that closes the interval without erasing history:

```rust
use aletheiadb::core::temporal::time;
// `retract_node` is for edge-free entities: it **refuses** (a
// `ValidationFailed` error) if the entity has ANY connected edge. A normal
// graph entity carries ref-derived edges, so use `retract_node_detach`, which
// co-retracts the connected edges at the same valid time and reports how many
// in `edges_retracted`.
let result = db.retract_node_detach(alice, time::from_secs(1_717_200_000))?; // ~2024-06-01
println!("edges_retracted = {}", result.edges_retracted);
```

> **Attribute-level retractions (a v1 limitation).** A `[:db/retract e a v tx]`
> that removes a *single* attribute (not the whole entity) does **not** have a
> faithful equivalent through the public update API. `update_node…` has
> **PATCH-merge** semantics: it starts from the existing property map and only
> *inserts* the keys you pass, so a key you omit keeps its **old value** — it is
> not dropped. There is no public way to delete a property key via
> `update_node`. The closest achievable is setting the attribute to
> `PropertyValue::Null` in the update map (recording an explicit null, which is
> *not* the same as a true removal), with the retraction's `txInstant` as the
> new version's `valid_from`. True attribute-granular retraction (dropping a
> key) is a v1 limitation.

> **Timestamps.** The `time` module offers `from_secs` / `from_millis` /
> `now()`; convert exported ISO-8601 `:db/txInstant`s to epoch seconds for the
> Rust replay path. The CSV/JSONL bulk importer parses ISO date/timestamp
> strings in the `valid_time` column directly.

#### Determinism & idempotency (re-runs)

- **Per-entity ordering:** replay each entity's datoms in **ascending Datomic
  `tx`** order (the `d/tx-range` dump is already transaction-ordered). AletheiaDB
  preserves that as the entity's version chain. This ordering is **enforced, not
  advisory**: an `update_node…` whose `valid_from` precedes the node's creation
  `valid_from` is **rejected** at write time (`validate_valid_from_not_before_creation`),
  so a mis-ordered replay fails loudly rather than silently corrupting the chain.
- **Re-runs:** `datomic_import` is **idempotent-or-refused** — before any write
  it refuses (with an `AlreadyImported` error) if the `:e` already exists. The
  guard is **durable**: it probes the target database's current state for a node
  already carrying this importer's business-key property (`db/id` by default), so
  a *fresh-process* re-run against a persistent target is refused too, not just a
  repeat call on the same `Importer`. Import into a **fresh** database and restart
  from empty. (The hand-written loop has no such guard — check
  `importer.resolve_key(db_id)` yourself if you replay manually.)

---

## Query translation

Datomic Datalog → AletheiaDB. AletheiaDB's graph surface is **Cypher** (with
temporal + vector extensions; `--features cypher`), **AQL**, and the **MCP**
tools.

**1. Entity lookup by attribute.**

```clojure
;; Datomic
(d/q '[:find (pull ?p [*])
       :where [?p :person/name "Alice"]]
     db)
```
```cypher
// AletheiaDB
MATCH (p:Person {name: 'Alice'}) RETURN p
```

**2. Ref-attribute traversal (a datom whose value is another entity).**

```clojure
;; Datomic — :person/employer is a :db.type/ref attribute
(d/q '[:find ?cname
       :where [?p :person/name "Alice"]
              [?p :person/employer ?c]
              [?c :company/name ?cname]]
     db)
```
```cypher
// AletheiaDB — the ref attribute became an EMPLOYS/EMPLOYER edge
MATCH (:Person {name: 'Alice'})-[:EMPLOYER]->(c:Company) RETURN c.name
```
Or via MCP `traverse` from Alice's `NodeId` over the edge type.

**3. Time-travel `d/as-of` (the payoff).**

Because Datomic's tx-time maps to AletheiaDB **valid** time, a Datomic `as-of`
becomes a **valid-time** `AS OF`:

```clojure
;; Datomic — Alice's title as of tx-time 2022-01-01
(d/q '[:find ?title
       :where [?p :person/name "Alice"] [?p :person/title ?title]]
     (d/as-of db #inst "2022-01-01"))
```
```cypher
// AletheiaDB — Datomic tx-time -> AletheiaDB valid time
AS OF VALID_TIME '2022-01-01T00:00:00Z'
MATCH (p:Person {name: 'Alice'}) RETURN p.title
```
```rust
// Rust API. For a value that never changed, valid-time + now suffices.
// For a value later superseded, anchor BOTH dimensions to recall the old
// segment (see "update = supersession"):
let engineer = db.get_node_at_time(alice, time::from_secs(1_640_995_200), tx_before_change)?;
```

Via **MCP**, `traverse` accepts `as_of_valid_time` / `as_of_transaction_time`,
and `find_nodes_at_time` resolves *"the Person named Alice, as of 2022-01-01"*
without a prior `NodeId`.

**4. `d/history` — full audit of an attribute.**

```clojure
;; Datomic
(d/q '[:find ?title ?tx
       :where [?p :person/name "Alice"] [?p :person/title ?title ?tx]]
     (d/history db))
```
```rust
// AletheiaDB — every recorded version of the entity
let history = db.get_node_history(alice)?;
println!("versions = {}", history.version_count());
```

---

## What doesn't map

Enumerated honestly — nothing is silently dropped.

- **The second time axis is synthesized, not native to the source.** Datomic has
  no valid-time; AletheiaDB's valid axis is *derived* from `:db/txInstant` (or a
  chosen effective-date attribute) by the mapping rule. Facts whose real-world
  effective date genuinely differed from their record date cannot be recovered
  unless Datomic stored that date as an attribute.
- **Coexisting segments at latest tx / update = supersession.** As in XTDB, a
  changed value becomes a supersession; the earlier segment lives on the
  transaction-time axis and is recalled by anchoring both dimensions. Data is
  preserved; single-coordinate query ergonomics differ. (See
  [update = supersession](#the-one-subtlety-that-bites-update--supersession).)
- **Original transaction time on AletheiaDB's tx axis.** Preserved as provenance,
  not on the tx axis; `AS OF SYSTEM_TIME` reflects import time.
- **Cardinality-many attributes.** AletheiaDB properties are single-valued.
  Cardinality-many **ref** attributes map cleanly to multiple **edges**;
  cardinality-many **scalar** attributes must be modeled as a list-valued
  property or fanned out into child nodes — you choose per attribute.
- **Transaction functions, schema-alteration history, and excision records.**
  Datomic transaction functions (`:db/fn`), the history of schema changes, and
  `:db/excise` records are **not** imported (excision is a hard-erase with no
  append-only equivalent). They are enumerated in the fidelity report rather than
  guessed at.
- **Reified transaction annotations beyond `:db/txInstant`.** Custom attributes
  asserted *on the transaction entity* (audit user, source system, etc.) map to
  provenance fields (`note` / `source` / `principal`) where they fit; arbitrary
  transaction-entity graphs are flattened, not preserved as their own entities.
- **Component / `isComponent` cascade semantics.** Datomic's `:db/isComponent`
  cascade-on-retract is not automatic; use `retract_node_detach` (or
  `delete_node_cascade`) to co-retract connected edges explicitly.

---

## What you gained

Once your Datomic history is in AletheiaDB, you have capabilities the source
never offered on the same data:

**1. Bi-temporal time-travel — verify equivalence.**

```rust
let now = time::now();
// Current-belief timeline at valid-time 2024 (Datomic as-of, mapped to valid).
let alice_2024 = db.get_node_at_time(alice, time::from_secs(1_717_200_000), now)?;

// A superseded segment: anchor valid-time AND a tx-time inside that era.
let alice_2022 = db.get_node_at_time(alice, time::from_secs(1_640_995_200), tx_before_change)?;

let history = db.get_node_history(alice)?;         // full version chain, nothing lost
println!("versions = {}", history.version_count());
```

**2. Provenance filtering — your original Datomic tx metadata, queryable.**

```rust
if let Some(prov) = db.get_node_provenance(alice)? {
    // e.g. "datomic tx=2000 txInstant=2023-01-01T00:00:00Z", correlation "datomic:tx:2000"
    println!("origin: {:?} / {:?}", prov.source(), prov.note());
}
```

**3. A vector index over migrated text — semantic search Datomic can't do.**

```rust
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::{PropertyMapBuilder, SimilarityQuery};

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
let similar = db.similarity_search(SimilarityQuery::from_node(doc).k(10))?;
```

You can now combine all three — graph traversal + vector similarity +
bi-temporal `AS OF` — in a single [hybrid query](hybrid-query-guide.md), on the
history you brought over from Datomic, with no JVM in sight.
