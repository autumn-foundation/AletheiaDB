# Belief-Revision Audit — when and why the database changed its mind (Issue #3362)

Status: **draft (wave-9 lane C)** · Cohort: **experimental `semantic-temporal`** · Author: implementation agent

> A belief-revision audit answers, for one node or edge, the question an LLM
> keeps asking today with 3+ stitched calls: *"why does the database now say Y
> when it used to say X, and who says so?"* It walks the entity's already-stored
> bi-temporal version history and classifies each transition — correction vs
> world-change vs retraction vs reaffirmation — attaching the provenance and a
> confidence trajectory. It is a **pure read**: no writes, no new storage
> format, no WAL change.

---

## 1. Problem & framing

Every "belief change" today is already recorded: one new `VersionInfo` appended
to an entity's `EntityHistory`, each carrying its bi-temporal interval,
properties, and optional `Provenance`. What is missing is the *interpretation*
layer: nothing in-tree tells a caller whether a new version **corrected** what we
had recorded about the same real-world period, or recorded that **the world
itself changed**, or **retracted** the fact, or merely **re-asserted** it from a
different source. This issue adds exactly that classifier, plus the ordered
revision sequence + confidence trajectory the ACs demand.

### Success metric (verbatim from the issue)

- ≤100-version audit completes in **<10 ms** (temporal-reconstruction target).
- On a scripted 20-write fixture covering all five categories, classification
  accuracy is **100%** (exact match), enforced as a test.
- An LLM answers "why Y not X, and who says so?" in **1 tool call** (vs ≥3).

---

## 2. Brainstorming (green hat)

- Reuse `VersionDiff::compute` for prior/new values — the diff primitive already
  exists (`src/core/history.rs`), NaN-safe via `semantically_equal`.
- Classification is **pure interval geometry** over a `VersionInfo` pair (+ the
  version's provenance). Keep it a free function so it is trivially unit-testable
  with hand-built pairs and byte-reproducible.
- Model the engine on `TemporalDiff<'a> { db: &'a AletheiaDB }` — the blessed
  read-only "analysis over history" shape in the same cohort.
- Confidence trajectory is literally
  `versions.iter().map(|v| v.provenance.and_then(|p| p.confidence()))` — an
  `Option<f64>` per revision, `None` (JSON `null`) when absent. Never defaulted.
- `as_of_transaction_time` reuses the lineage `record.recorded_at > as_of`
  filter idiom: keep only versions whose tx-start ≤ the coordinate. Because
  versions append in tx-time order, that is a stable prefix → time-travelable
  and deterministic.

## 3. Reverse-brainstorming — how could the classifier be *wrong* or gamed?

| Attack / hazard | Consequence | Mitigation |
|---|---|---|
| **Clock skew / backdated `valid_from`** makes a correction look like a `world_change` | mislabels the revision | Rule keys off *recorded* geometry only (`valid_from` vs the max prior `valid_from`), documented as such; we classify what was *written*, never guess intent (Out-of-Scope forbids NLP/inference). A backdated write that advances `valid_from` **is** a world-change by our definition — deterministic, falsifiable. |
| **Tie**: new `valid_from == max prior valid_from` | ambiguous corr/world | Deterministic tie-break: `>` strictly ⇒ world_change, `<=` (incl. equal) ⇒ correction. Documented + tested both orderings (risk #1). |
| **No-op re-write, same source** | spurious revision? | Classified `reaffirmation` (no value delta). Source is surfaced so the caller sees "same source re-asserted"; we do not suppress it (append-only truth). |
| **Retraction that also changes value** (backdated delete w/ valid_time) | corr/world vs retraction race | Precedence: a **closed valid interval wins** → `Retraction`. Documented ordering. |
| **`HashMap` iteration leaking into output** | non-determinism (breaks AC4) | Sort `changes` by key; no map iteration in output; versions already ordered by `version_number`. |
| **Idempotent re-retraction** (`already_retracted`) appends no version | phantom double retraction | No new version ⇒ no extra revision. Verified by test. |
| **Property scope on a never-present key** | silent empty success | Full-history key scan ⇒ `INVALID_ARGUMENT` if the key never existed (AC6). |
| **Cold-tier / truncated history** | incomplete chain | Audit reflects *available* history (same caveat as `temporal_extent`/point-in-time reads); documented, not silently "complete". |

## 4. Six hats (condensed)

- **White (facts):** all inputs already persisted; zero new format. Provenance
  field is `note` not `reason` (the AC's `reason` maps to `Provenance::note()`).
- **Red (gut):** an engine mirroring `TemporalDiff` feels native to the cohort.
- **Black (caution):** putting a brand-new spec-churny classifier on the *stable*
  `AletheiaDB` surface before incubation is risky; the issue itself says
  "incubate in `semantic-temporal`".
- **Yellow (upside):** 1-call answer to a high-value LLM question; deterministic
  and falsifiable; O(versions), well under the 10 ms target.
- **Green (creative):** the classifier as a pure fn unlocks table-driven fixture
  testing and a future MCP tool with no re-derivation.
- **Blue (process):** TDD, gated behind an existing flag, MCP surface *designed
  but deferred* to an approved follow-up (registry + parity harness need sign-off).

---

## 5. Approaches considered

### Approach A — Experimental `BeliefRevisions<'a>` engine in `semantic-temporal` **(CHOSEN)**
New `src/experimental/temporal/belief_revision.rs`, gated
`#[cfg(feature = "semantic-temporal")]` (inherits the subtree gate). Engine
`BeliefRevisions<'a>` + a thin gated inherent `AletheiaDB::belief_revisions`
convenience so the AC's literal `db.belief_revisions(entity, options)` spelling
works. Reuses `VersionDiff`, `Provenance`, the `ChangeType::Deleted` tombstone
rule, and lineage-style `has_more` bounding.
- **Pros:** lands in the cohort the issue names; smallest surface; zero
  Cargo/ADR/format/registry churn; deterministic & byte-reproducible; ships this
  wave cleanly.
- **Cons:** capability behind an experimental flag; MCP tool deferred. Result
  types carry `PropertyValue` (not serde) so they compile under the standalone
  cohort without pulling `serde`/`audit-export`; JSON serialization for the MCP
  tool is a mechanical follow-up.

### Approach B — Core types (`src/core/belief.rs`) + inherent method + MCP tool now
Types unflagged in core, classifier on the stable facade, tool in the registry.
- **Pros:** literal AC1 surface incl. MCP in one wave.
- **Cons:** puts an un-incubated classifier on the stable surface (contradicts the
  issue's incubation note); touches the MCP registry + parity harness (coordinator
  approval required). Rejected for this wave.

### Approach C — Fold into `src/audit/` as a derived view
- **Cons:** `src/audit` is a **signed, compatibility-bearing** wire format; bolting
  a mutable analysis view on risks the stability contract; wrong altitude. Rejected
  (but we borrow its value/timestamp *conventions*).

### Decision
**Approach A.** Additive, in-cohort, no on-disk/registry change, deterministic.
The MCP `get_belief_revisions` tool is fully specified in §9 and deferred to an
approved follow-up PR.

---

## 6. Classification rules (the contract)

Let `v[0..n]` be the entity's versions ordered by `version_number` (oldest
first), **after** filtering to those with `transaction_time().start() ≤
as_of_transaction_time` (if the coordinate is set; else all). For revision `i`:

| Precedence | Class | Deterministic rule (pure fn of the version pair + this version's provenance) |
|---|---|---|
| 1 | `initial_assertion` | `i == 0` (first visible version — no predecessor). |
| 2 | `retraction` | This version's **valid interval is closed** (`valid_time().is_closed()`, i.e. `valid_to != TIMESTAMP_MAX`). Covers both a **delete tombstone** (empty `[t, t)`) and a **#3230 valid-time retraction** (`[valid_from, valid_to)`). |
| 3 | `reaffirmation` | Not closed, and `VersionDiff(v[i-1], v[i])` has **no value change** (`!has_changes()`). The provenance/source is surfaced so a caller sees who re-asserted. |
| 4 | `world_change` | Not closed, value changed, and `v[i].valid_from > max(v[j].valid_from for j<i)` — a **new/later** valid interval: the fact itself changed. |
| 5 | `correction` | Not closed, value changed, and `v[i].valid_from <= max prior valid_from` — rewriting an **already-recorded** valid period at a later transaction time (tx-time supersession). |

Rules 4/5 are mutually exclusive by the strict `>` tie-break (equal ⇒
correction). Precedence 2 dominates 4/5 (a closed interval is always a
retraction, even if properties also changed).

`changes` per revision (sorted by key for determinism):
- `initial_assertion`: `(key, None, Some(new))` for each property at `v[0]`.
- `retraction`: `(key, Some(old), None)` for each property of the predecessor
  (the values being retracted); for `i==0`\* n/a (a first version is never a
  retraction).
- `reaffirmation`: empty (no value delta by definition).
- `correction` / `world_change`: the `VersionDiff` — added `(key, None, Some)`,
  removed `(key, Some, None)`, modified `(key, Some(old), Some(new))`.

`confidence` per revision = `v[i].provenance.and_then(|p| p.confidence())`
(explicit `None`/null when the write carried none — both "no provenance" and
"provenance present, confidence absent" collapse to `None`, AC3).

Property-scoped audit (`property_key = Some(k)`): validate `k` appears in **at
least one version across the full history** (else `INVALID_ARGUMENT`); then emit
a revision only when `k` is involved (present at initial, in the diff, or
retracted while present in the predecessor), with `changes` filtered to `k`.

---

## 7. Determinism & bounding

- Output order = `version_number` ascending; `changes` sorted by key. No map
  iteration reaches the output. ⇒ two audits at the same `(entity, options,
  as_of)` are **byte-identical** (AC4), provable via struct `==` and
  `format!("{:?}", …)` equality (and JSON string equality where `serde` is on).
- `limit`: **default 100**, **max 1000** (`DEFAULT_REVISION_LIMIT` /
  `MAX_REVISION_LIMIT`). A `limit` of 0 is `INVALID_ARGUMENT`; a value above the
  max is clamped. `has_more = true` when more revisions exist than returned
  (chronological first-N; a paginating offset is a follow-up). (AC7)

## 8. Performance

Single `historical.read()` history fetch + O(n) pairwise classify; the running
`max prior valid_from` avoids any O(n²) overlap scan. Target **<10 ms for ≤100
versions**. A gated Criterion micro-bench (`benches/belief_revision.rs`,
`required-features=["semantic-temporal"]`) over a ~100-version synthetic history
guards it. A full CI perf gate rides with cohort graduation if the eval harness
is absent.

---

## 9. Deferred MCP tool spec — `get_belief_revisions` (DESIGNED, NOT IMPLEMENTED)

> Requires coordinator sign-off: adds a registry entry in `src/mcp/server.rs`
> plus parity-harness rows (`tests/parity_mcp.rs`, `tests/parity_http.rs`,
> `tests/parity/inventory.json`) and the CLAUDE.md tool table. **Not touched in
> this PR.** Reader-class (read-only). Mechanical once approved: serialize the
> `BeliefRevisionLog` (values via `audit::model::ExportedValue`, timestamps via
> `rfc3339_micros`).

- **Tool name:** `get_belief_revisions`
- **Class:** `reader`
- **`inputSchema.properties`:**
  - `entity_kind` (string, `"node"|"edge"`, required)
  - `id` (integer, required)
  - `property_key` (string, optional — scope to one property)
  - `as_of_transaction_time` (string RFC 3339 | integer micros, optional)
  - `limit` (integer, optional; default 100, max 1000)
- **Success output:**
  ```json
  {
    "entity": {"kind": "node", "id": 42},
    "property_key": null,
    "as_of_transaction_time": null,
    "revisions": [
      {
        "version_number": 2,
        "version_id": 7,
        "transaction_time": "2026-07-08T12:00:00.000000Z",
        "valid_from": "2026-01-01T00:00:00.000000Z",
        "valid_to": null,
        "classification": "correction",
        "changes": [{"key":"title","prior":{"type":"string","value":"X"},"new":{"type":"string","value":"Y"}}],
        "provenance": {"source":"editor","confidence":0.9,"note":"typo fix"},
        "confidence": 0.9
      }
    ],
    "confidence_trajectory": [null, 0.9],
    "has_more": false
  }
  ```
- **Errors (#3234 structured codes):**
  - unknown entity ⇒ `NOT_FOUND` (from `get_*_history`), `retriable:false`.
  - property key the entity never had, or `limit == 0` ⇒ `INVALID_ARGUMENT`
    (`QueryError::InvalidParameter`), `retriable:false`.
- Excluded from #3368/#3353/#3360 wrappers is **not** required (it is a bounded
  read); enrollment can be decided at follow-up time.

---

## 10. Risk / edge-case → test map

| # | Risk | Test |
|---|---|---|
| 1 | corr vs world tie-break (equal / earlier / later `valid_from`) | `classify_*` unit tests + fixture rows |
| 2 | retraction geometry (delete tombstone AND #3230 valid-time close) | fixture `retraction` rows + `retraction_detects_*` |
| 3 | idempotent re-retraction emits no extra revision | `reretraction_no_extra_revision` |
| 4 | confidence null: no-provenance AND provenance-without-confidence | `confidence_trajectory_surfaces_nulls` |
| 5 | `as_of_transaction_time` excludes later corrections; byte-identical repeat | `as_of_scopes_history` + `audit_is_byte_identical` |
| 6 | property scope: unknown key ⇒ INVALID_ARGUMENT; early-only key found | `property_scope_unknown_key_rejected` / `property_scope_early_key` |
| 7 | NOT_FOUND for unknown id; single-version entity ⇒ 1 initial_assertion | `unknown_entity_not_found` / `single_version_is_initial` |
| 8 | `limit`/`has_more` bounding; limit 0 rejected | `limit_bounds_and_has_more` / `limit_zero_rejected` |
| 9 | determinism (no map order leak) | `audit_is_byte_identical` |
| 10 | 20-write fixture, 100% classification accuracy | `scripted_20_write_fixture_100pct` (Success Metric) |
| 11 | edges audited symmetrically | `edge_belief_revisions` |
| 12 | <10 ms ≤100 versions | `benches/belief_revision.rs` |

---

## 11. Acceptance-criteria coverage

| AC | Summary | Status |
|---|---|---|
| AC1 | Rust API + ordered revision sequence w/ provenance + classification | **Done** (Rust API). MCP tool **designed, deferred** (§9). |
| AC2 | Deterministic 5-category classification; fixture 100% | **Done** (`scripted_20_write_fixture_100pct`). |
| AC3 | Confidence trajectory w/ explicit null | **Done** (`confidence_trajectory`). |
| AC4 | Pure read, byte-identical on repeat | **Done** (`audit_is_byte_identical`). |
| AC5 | `as_of_transaction_time` time-travel | **Done** (`as_of_scopes_history`). |
| AC6 | #3234 `NOT_FOUND` / `INVALID_ARGUMENT` | **Done** (Rust error variants that map to those codes); MCP wire mapping deferred w/ §9. |
| AC7 | Bounded `limit` + completeness signaling | **Done** (`has_more`). |
| AC8 | Docs w/ worked correction-vs-world-change example | **Done** (module docs + this plan §6/§9). |

Deferred to an approved follow-up: the `get_belief_revisions` MCP tool
(registry + parity + CLAUDE.md table) — spec frozen in §9.
