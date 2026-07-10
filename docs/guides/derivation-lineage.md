# Derivation Lineage Between Facts (Issue #3371)

Write-time [provenance](../../src/core/provenance.rs) answers *"where did this
fact come from?"* for a fact written directly from an external source. It
cannot answer the question that dominates real knowledge pipelines: **"which
facts was this fact computed from?"** When an ETL job merges three records into
one, when an agent summarizes ten documents into a claim, or when
entity-resolution collapses duplicates, the output fact's true provenance is
*other facts in this database*, not a source string.

**Derivation lineage** records that fact-to-fact structure at write time and
makes it queryable in both directions:

- **Upstream** — *"what was fact B derived from?"* (the evidence chain, for
  citation-grade answers).
- **Downstream** — *"what has been derived from fact A?"* (the **blast
  radius**, for enumerating contamination when an input is found wrong or
  retracted).

Because every lineage reference pins a specific **version** (not just an
entity), lineage refers to exactly the fact that was read. Later updates to an
input never silently rewrite what a derivation was based on — the failure mode
of modelling lineage as ordinary graph edges pointing at *current* nodes.

## Concepts

| Type | Meaning |
|------|---------|
| [`LineageRef`](../../src/core/lineage.rs) | A version-pinned reference to one fact: `{ entity: NodeId \| EdgeId, version: VersionId }`. |
| `LineageRecord` | An immutable declaration: `derived` was computed from `sources`, at transaction time `recorded_at`. |
| `LineageView` | A resolved closure: entries with `depth` (min hops from the root) and a current-state `FactStatus`. |
| `FactStatus` | `Current` (referenced version is live), `Superseded` (a newer version exists), or `Absent` (entity deleted/retracted). |

### Version-space is a DAG

A declared source must already exist when the derived version is written, so
its `VersionId` was always allocated by an *earlier* write than the derived
version's. Lineage edges therefore always point from a higher-numbered version
to lower-numbered versions, which makes the lineage graph **acyclic by
construction**. The store still enforces self-derivation and transitive-cycle
guards (`LineageError::SelfDerivation` / `CycleDetected`) as defence-in-depth;
they cannot fire through the normal create/update write path.

## Rust API

Declaring lineage on writes:

```rust
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use aletheiadb::core::lineage::{LineageRef, LineageQueryOptions};

let db = AletheiaDB::new()?;

// Two source facts.
let doc_a = db.create_node("Doc", PropertyMapBuilder::new().insert("t", "A").build())?;
let doc_b = db.create_node("Doc", PropertyMapBuilder::new().insert("t", "B").build())?;

// Pin the exact versions that were read.
let a_ref = db.node_lineage_ref(doc_a).expect("current version");
let b_ref = db.node_lineage_ref(doc_b).expect("current version");

// A summary fact derived from both documents.
let summary = db.create_node_with_lineage(
    "Summary",
    PropertyMapBuilder::new().insert("text", "A+B merged").build(),
    &[a_ref, b_ref],
)?;
```

Querying the closure in both directions:

```rust
let summary_ref = db.node_lineage_ref(summary).unwrap();

// Upstream: what was the summary derived from? (evidence chain)
let evidence = db.upstream_lineage(summary_ref, LineageQueryOptions::new());
for entry in &evidence.entries {
    println!("depth {} -> {:?} [{}]", entry.depth, entry.reference, entry.status.as_str());
}

// Downstream: what has been derived from document A? (blast radius)
let blast = db.downstream_lineage(a_ref, LineageQueryOptions::new());
println!("{} downstream facts affected", blast.count());
```

`LineageQueryOptions` bounds every closure:

- `with_max_depth(n)` — transitive hop cap (`1` = direct parents/children).
- `with_limit(n)` — maximum returned entries; hitting it sets `has_more: true`.
- `with_as_of(ts)` — only follow lineage records whose `recorded_at <= ts`, so
  the closure reflects lineage as it was recorded by that transaction time.

The write surface has `create_node_with_lineage`, `create_edge_with_lineage`,
`update_node_with_lineage`, and `update_edge_with_lineage` (an update records
lineage against the *new* version). An edge may be derived from nodes and vice
versa — lineage is cross-kind.

## Worked scenarios

### Merge

An entity-resolution step collapses duplicate `Person` records into a canonical
one. The canonical fact declares the duplicates as its sources:

```rust
let dup1 = db.node_lineage_ref(person_a).unwrap();
let dup2 = db.node_lineage_ref(person_b).unwrap();
let canonical = db.create_node_with_lineage(
    "Person",
    PropertyMapBuilder::new().insert("name", "Ada Lovelace").build(),
    &[dup1, dup2],
)?;
// Later: upstream_lineage(canonical) cites exactly the records that were merged.
```

### Summarize

An LLM agent summarizes ten source documents into one claim. The claim's
`derived_from` is the ten version-pinned document refs, so the agent can later
produce the full evidence chain for the claim with a single
`upstream_lineage` call — grounding no longer stops one hop short of the actual
sources.

### Retraction blast radius

An input fact is found wrong and retracted (#3230). One `downstream_lineage`
call enumerates every transitively derived fact, with per-hop depth:

```rust
let poisoned = db.node_lineage_ref(bad_input).unwrap();
db.retract_node(bad_input, aletheiadb::time::now())?;

// Lineage is immutable — retraction never deletes records pointing at the input.
let contaminated = db.downstream_lineage(poisoned, LineageQueryOptions::new());
for entry in &contaminated.entries {
    // The retracted input itself resolves as `Absent`; downstream facts still resolve.
    println!("affected: {:?} (depth {})", entry.reference, entry.depth);
}
```

## Immutability and retraction

Lineage records are **immutable once written** — they are part of recorded
history. Deleting or retracting a fact never deletes lineage records pointing at
it; the closure over a retracted input still resolves and marks the input's
status `Absent`. Lineage **informs**, it never auto-mutates: there is no cascade
deletion along lineage edges (out of scope per the issue).

## Errors

All are caller faults (never retriable). On the MCP surface they map to the
[#3234 structured error codes](mcp-query-tool.md#structured-error-codes-and-the-retriable-contract):

| Rust `LineageError` | MCP code |
|---------------------|----------|
| `SourceNotFound { reference }` — a declared source does not resolve to an existing version | `NOT_FOUND` |
| `SelfDerivation` / `CycleDetected` | `INVALID_ARGUMENT` |
| `AlreadyRecorded` — lineage is write-once per version | `FAILED_PRECONDITION` |

Declaring derivation from a nonexistent reference fails the write **before any
commit** — no silent dangling lineage. Omitting `derived_from` (or passing an
empty slice) reproduces today's behavior exactly.

## Limitations (v1)

- **Durability**: to keep the WAL entry format untouched while it is being
  restructured (#3413), the v1 lineage index is **in-memory**. Records are
  immutable and survive supersession/retraction within a process, but do **not
  yet survive a process restart**. Rehydrating lineage from a persisted log is a
  tracked follow-up; the record shape is deliberately serialisation-friendly for
  that work. This mirrors the honest attribution caveat in #3427.
- **Cold tier**: reference validation and status resolution consult hot-tier
  history and current state; a source version already cold-migrated is not
  resolvable for a *new* declaration until it is warmed.
- **Out of scope** (separate specs): confidence/trust propagation along lineage,
  automatic lineage capture from query patterns, lineage for facts derived
  outside the database, lineage-aware retrieval ranking, and cross-shard lineage
  federation.

## See also

- [`src/core/lineage.rs`](../../src/core/lineage.rs) — the immutable lineage store and closure algorithms.
- [`src/db/lineage.rs`](../../src/db/lineage.rs) — the database write/query API.
- [Provenance (#3224)](../../src/core/provenance.rs) — write-time attributive provenance (the external-source complement).
