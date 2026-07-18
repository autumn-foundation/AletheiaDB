# Design: Configurable String-Interner Cap + Eliminate Background-Persist Infinite Retry

- **Date**: 2026-07-18
- **Branch**: `fix/configurable-interner-cap`
- **Status**: Design (implementation to follow in the same PR)
- **Related code**: `src/core/interning.rs`, `src/storage/index_persistence/{mod.rs,strings.rs,formats.rs,graph.rs,worker.rs,api.rs}`, `src/mcp/error.rs`, `src/http/error.rs`

---

## 1. Problem

Two compounding defects turn a *legitimately large* dataset into an unrecoverable, CPU-burning hang.

### Defect 1 — hardcoded 100K cap (two of them, in different layers)

The interner has **two independent 100K caps** that must move in lockstep:

1. **Runtime intern-time cap** — `StringInterner.max_capacity`, default
   `DEFAULT_MAX_INTERNED_STRINGS = 100_000` (`src/core/interning.rs:20`). Already
   env-configurable (`ALETHEIADB_MAX_INTERNED_STRINGS`, `interning.rs:504-534`)
   and via `with_max_capacity`. `intern()` (`interning.rs:140-185`) returns
   `Error::Storage(StorageError::CapacityExceeded { resource, current, limit })`
   (`src/core/error.rs`) once the reserved id `>= max_capacity`.
2. **Persist-load validation cap** — hardcoded `MAX_STRING_COUNT = 100_000`
   (`src/storage/index_persistence/mod.rs:157`), checked only when **loading** a
   persisted interner in `validate_string_interner`
   (`src/storage/index_persistence/strings.rs:391-399`), returning
   `IndexPersistenceError::SizeLimitExceeded`.

**Why both must move together (prod-upgrade blocker):** if we raise the runtime
cap but leave `MAX_STRING_COUNT` at 100K, a DB that interns >100K strings *saves
fine* but **fails to reload** — `validate_string_interner` rejects
`string_count > MAX_STRING_COUNT`. A raised runtime cap without a raised load cap
bricks the next restart. This is why a reopen-with-higher-cap regression test is
mandatory.

### Defect 2 — infinite background-retry hang

The background persist worker (`src/storage/index_persistence/worker.rs:203-314`)
loops on a 1-second `check_interval`. Mutation counters **reset only on success**
(`Ok(_) => any_index_persisted = true`), so a *failing* persist re-attempts
**forever**, every ~1s, with only `eprintln!` diagnostics — no backoff, no
circuit-breaker, the error flattened to a string, never surfaced to MCP/HTTP.

The observed failure was an **egregore hang** on a code-graph import (~299k
records, high-cardinality file paths / symbol names). Those file paths / symbol
names are property **VALUES**, and property values intern **lazily inside the
persist thread** (`persist_property_value`, `src/storage/index_persistence/graph.rs:82-93`),
*not* on the write path. So the write path succeeded, the writer saw no error,
and the background thread spun on `CapacityExceeded` indefinitely — 1 CPU pegged,
`eprintln!` spam, zero actionable signal.

> Observed: writes "succeed", then the process wedges — a persist thread retrying
> a `CapacityExceeded` string-interner save once per second forever, with the only
> symptom an endless `Background persistence: Failed to persist ...` log line.

### What interns where (the crux)

| Interned data | Where | Timing | Writer sees error? |
|---|---|---|---|
| Node/edge **labels** | `src/storage/current/mod.rs:494` | synchronous, write path | **Yes** — `CapacityExceeded` |
| Property **keys** | `src/core/property/map.rs:518` | synchronous, write path | **Yes** — `CapacityExceeded` |
| Property string **VALUES** | `src/storage/index_persistence/graph.rs:82-93` | **lazy, background persist thread** | **No** — flattened to `eprintln!` |

The value path is the one that hangs, and it is invisible to the caller. Fixing
the cap alone is insufficient; the background thread's failure semantics must
change too.

---

## 2. Validated technical questions

### Q1 — How does per-DB config reach the process-global interner?

**Findings (from `src/core/interning.rs`):**

- `GLOBAL_INTERNER` **is** a process-global static:
  `pub static GLOBAL_INTERNER: LazyLock<StringInterner>` (`interning.rs:525-534`).
  It is seeded **once, at first access**, from `ALETHEIADB_MAX_INTERNED_STRINGS`
  (or `DEFAULT_MAX_INTERNED_STRINGS`), then `warm_common_strings()`.
- `max_capacity` is a **plain `usize`** field (`interning.rs:111`), set only in
  `with_max_capacity` (`121-128`) and **immutable after construction**. It is read
  in exactly one place: the capacity check in `intern()` (`interning.rs:164`,
  `if id_value >= self.max_capacity as u32`). `intern_unchecked` bypasses it.

**Recommended mechanism:**

1. Change `max_capacity: usize` → `max_capacity: AtomicUsize`.
2. Add `pub fn set_max_capacity(&self, n: usize)` (store `Relaxed`) and read it in
   `intern()` via `self.max_capacity.load(Relaxed)`. The fast-path (already
   interned) is untouched; the check is only on the slow insert path, so the added
   atomic load is off the hot path and negligible.
3. At DB construction (where `PersistenceConfig` is available), call
   `GLOBAL_INTERNER.set_max_capacity(effective_cap)`.
4. **Precedence:** explicit `persistence.max_interned_strings` (config/TOML/builder)
   `>` `ALETHEIADB_MAX_INTERNED_STRINGS` env var `>` raised default (10M). The env
   seed still applies for embedded/`GLOBAL_INTERNER`-only users who never open a DB.

**v1 caveat (document loudly):** the interner is **process-global**. If a process
opens multiple DBs with different caps, **last `open()` wins** for the shared
`GLOBAL_INTERNER.max_capacity`. A per-DB interner is out of scope; the shared cap
is a documented v1 limitation. `set_max_capacity` only ever *raises* effective
headroom for already-interned data (it never evicts), so a lower subsequent cap
cannot corrupt existing ids — it merely refuses *new* interns past the lower bound.

### Q2 — Does a value-string persist failure lose data?

**No. WAL is ground truth; index persistence is an optimization.**

Cited evidence:

- `src/storage/recovery.rs` (module doc): *"Upon restart, this module reads those
  persisted WAL entries and replays them. It reconstructs the exact state of the
  database by systematically applying every recorded Create, Update, and Delete
  operation to both the CurrentStorage ... and the HistoricalStorage."* — i.e. the
  optimized read structures (including the interner) are **rebuilt from the WAL**,
  which is *"a simple, sequential log of operations"* that is synced on every write.
- `src/db/admin.rs` (checkpoint/persist comments): index snapshots are taken at a
  conservative **manifest LSN `n`**, and on restart *"replay re-indexes every entry
  with lsn >= n"* (idempotent). A persisted index that lags behind the WAL is simply
  a shorter replay from an earlier `n`.
- Property string **values** live as real `Arc<str>` inside the in-memory /
  WAL-serialized property maps on the hot path; they are interned to `u32` **only**
  inside the persist thread (`graph.rs:82-93`). The interner is therefore a
  *persistence-side accelerator for the graph-index snapshot*, not a store of record.

**Conclusion:** a background persist failure means the on-disk index snapshot is
**stale or absent**, so the next restart replays a longer WAL tail — **slower
startup, never data loss**. Therefore "terminal-loud + back off" in the background
thread is **safe**: stopping the retry does not risk durability, because durability
was already achieved by the WAL at commit time.

### Q3 (bonus) — Does raising the cap change the persist format?

**No.** `StringInternerData` (`src/storage/index_persistence/formats.rs:147-157`)
encodes `magic`, `version`, `string_count: u64` (the **actual** count, not the cap),
`strings: Vec<String>`. The cap is a *validation threshold applied at load*, not a
stored field. Raising it changes no bytes on disk and no struct layout — the
`FORMAT-FROZEN` guarantee (it is embedded in `.albk` backup payloads) is preserved.
**No migration, no format bump.**

---

## 3. Brainstorming

Raw idea generation (unfiltered):

- Raise both constants to 1M / 5M / 10M / `u32::MAX`.
- Make the runtime cap an `AtomicUsize` settable at `open()`.
- Drive the load cap from the same config field as the runtime cap.
- Switch from a **count** cap to a **byte-budget** (total interned bytes) cap.
- Move property-value interning onto the write path so the writer gets the error
  synchronously (like labels/keys).
- Add a circuit-breaker to the background thread: after N deterministic failures,
  stop trying and mark the interner "sealed".
- Exponential backoff on persist failure (1s → 2s → … → 60s cap).
- Dedupe identical error log lines (log once, then a rate-limited summary).
- Classify errors: deterministic (`CapacityExceeded`, serialization) vs transient
  (I/O) — only back off / seal on deterministic ones.
- Surface a persist-health metric / `database_stats` field so operators see the
  stuck state instead of tailing logs.
- Auto-raise the cap on demand (rejected — unbounded memory is the DoS we're guarding).
- Evict LRU interned strings (rejected — ids are immutable, referenced everywhere).
- Hot-reload the cap without restart (deferred — needs a config-reload path).

## 4. Reverse brainstorming

*"How could we make this WORSE / guarantee the hang persists?"* — then invert each.

| How to make it worse | Inversion (what to actually do) |
|---|---|
| Keep the two caps independent so a raised runtime cap still fails to reload | Drive **both** caps from one config field |
| Keep retrying every 1s with no backoff | Bounded exponential backoff (cap 60s) on deterministic failure |
| Log the same line forever | Log once at ERROR with actionable text; dedupe/rate-limit thereafter |
| Flatten the error to a string so nobody can branch on it | Preserve structured `CapacityExceeded`; classify deterministic vs transient |
| Leave the failure invisible to the caller | Writer already gets it for labels/keys; make the message name the knob; add a health signal |
| Hide the config knob / undocument it | First-class `PersistenceConfig.max_interned_strings`, builder + TOML + docs |
| Make the operator guess the fix | Error message literally names `persistence.max_interned_strings` and the restart step |
| Reset counters only on success (so failure re-arms instantly) | On deterministic failure, stop hot-retrying regardless of counters |

## 5. Six Thinking Hats

- **White (facts):** Two 100K caps (`interning.rs:20`, `mod.rs:157`). Runtime cap
  already env-configurable; load cap is not. Background worker retries every 1s,
  counters reset only on success, error `eprintln!`-only. Labels/keys intern
  synchronously; property **values** intern in the background persist thread. WAL
  is ground truth (recovery.rs); persist failure ≠ data loss. Format unchanged by a
  cap raise. `id` space is `u32` (hard ceiling ~4.29B). MCP maps `CapacityExceeded`
  → `FailedPrecondition` (non-retriable, `mcp/error.rs:342`); HTTP → 400 BadRequest.
- **Red (gut/feelings):** The current behavior is *infuriating* — writes "succeed"
  and the process silently wedges, pegging a core and spamming logs, with no hint of
  the cause or the fix. It reads as a hang/DoS, not a clean, actionable limit. A user
  loading a normal-sized code graph feels ambushed.
- **Black (risks):** Raising the cap raises the worst-case memory ceiling (DoS
  surface). A byte-budget adds a running-total atomic on the lock-free intern
  fast path (contention / correctness risk). A too-high default could let a genuine
  runaway consume GBs before the cap bites. `set_max_capacity` on a process-global
  static means multi-DB processes share one cap (last-open-wins). Changing MCP error
  codes risks destabilizing conformance tests.
- **Yellow (benefits):** One knob fixes the real user (10M ≈ impossible to hit for a
  ~1–3M-unique-string graph). Terminal-loud + backoff turns an invisible hang into a
  bounded, self-explaining error. No format change → zero migration risk. Structured
  error + named knob → the operator self-serves the fix in one restart.
- **Green (alternatives):** count cap vs byte-budget (§6); move value interning to
  the write path (deferred — risky, changes hot-path latency and error surface);
  hot-reload the cap (deferred); circuit-breaker "seal" vs pure backoff (we do
  backoff + log-once, which is observably bounded without new state machine).
- **Blue (plan/process):** TDD — write the 8 regression tests (§10) first, then land
  the `AtomicUsize` cap + config field + load-cap unification + background
  terminal-loud/backoff, then docs. Draft PR up front (this doc) to avoid loss.

---

## 6. Cap shape — count cap vs byte-budget (with arithmetic)

**Memory model:** each *new* interned entry costs `L` string bytes (stored once
behind `Arc<str>`) **plus** map/pointer overhead. There are **two** DashMap slots
per string (`string_to_id: DashMap<Arc<str>, InternedString>` and
`id_to_string: DashMap<InternedString, Arc<str>>`), so overhead is roughly
**~70–120 bytes/entry** (two hash slots, two `Arc<str>` fat pointers + control
blocks, the `u32` id). Per-string length is already bounded by
`MAX_STRING_LENGTH = 10 MB` (`mod.rs:166`), and the persisted interner file is
bounded by `MAX_STRING_INTERNER_FILE_SIZE`.

Let overhead ≈ 100 B/entry and assume short-ish identifiers (file paths / symbol
names) averaging ~30–60 B:

| Cap (count) | Overhead only (~100 B) | + string bytes (~30–60 B avg) | Verdict |
|---|---|---|---|
| 100K (today) | ~10 MB | ~13–16 MB | **Too small** — the actual bug; a ~299k-record graph blows past it |
| 1M | ~100 MB | ~130–160 MB | Comfortable, but a large code graph (1–3M unique strings) can still hit it |
| 5M | ~500 MB | ~650–800 MB | Generous |
| **10M** | **~1.0 GB** | **~1.3–1.6 GB** | **Recommended default** — near-impossible to hit for the user's workload, still bounded |
| `u32::MAX` (~4.29B) | ~430 GB | — | Effectively "no cap" — reintroduces the DoS we guard against |

**Decision: keep a configurable COUNT cap, raise the default to `10_000_000` (10M).**

Justification:

- Per-string length is **already** bounded (10 MB) and the persist file is
  **already** size-bounded, so the residual unbounded vector is *count*, not size —
  a count cap targets exactly the remaining DoS dimension.
- 10M short strings ≈ ~1.0–1.6 GB is **proportionate to a legitimately large
  dataset**, while making the cap near-impossible to hit for the user's ~299k-record
  / ~1–3M-unique-string code graph.
- The `InternedString` id is a `u32`, so the absolute hard ceiling is ~4.29B; 10M
  sits comfortably below it with room for the knob to go higher if ever needed.

**Byte-budget: considered and deferred.** A total-interned-bytes budget would bound
memory more precisely, but it requires a **running-total atomic incremented on the
lock-free intern fast path** — a new point of cross-thread contention and a
correctness hazard (the `fetch_add`/rollback dance in `intern()` would need a second
atomic kept consistent with the first under the capacity race). The marginal benefit
over a count cap is small *because length is already capped*, so the worst-case bytes
are already `count × 10 MB`-bounded and, in practice, `count × avg_len`. We therefore
**defer the byte-budget** and ship the count cap. (If a future workload has extreme
length variance under the 10 MB cap, revisit — but default to the count cap.)

---

## 7. Implementation approaches (pick one)

### (A) Single `max_interned_strings` on `PersistenceConfig` — **RECOMMENDED**

- Add `pub max_interned_strings: usize` to `PersistenceConfig` (`api.rs:32`),
  default `10_000_000` (via a `DEFAULT_MAX_INTERNED_STRINGS` bump *and* the config
  default).
- At DB construction, drive **both** caps from it:
  - runtime: `GLOBAL_INTERNER.set_max_capacity(cap)` (new `AtomicUsize` mechanism, §Q1);
  - load: replace the hardcoded `MAX_STRING_COUNT` check in `validate_string_interner`
    with the configured cap (thread the cap into the load path, or make the load
    validator read the same effective cap).
- Background worker made **terminal-loud** (§8).
- **Tradeoffs:** one knob, one mental model, both caps guaranteed in lockstep,
  reopen-with-higher-cap works. Cost: threading the cap into the load validator (the
  only non-trivial wiring). This is the choice.

### (B) Byte-budget bound — **DEFERRED**

- Bound total interned bytes instead of count. Tighter memory guarantee.
- **Tradeoffs:** running-total atomic on the lock-free fast path (contention +
  correctness risk) for marginal benefit given the existing length cap. Deferred (§6).

### (C) Minimal: just raise the two constants, no config — **REJECTED**

- Bump `DEFAULT_MAX_INTERNED_STRINGS` and `MAX_STRING_COUNT` to 10M, done.
- **Tradeoffs:** no operator knob. The user explicitly needs a *configurable* cap
  (different deployments, per-dataset headroom, the ability to raise past a raised
  default without recompiling). Rejected — but note the constant bump is a *subset*
  of (A), so (A) includes "raise the default" anyway.

**Chosen: (A).**

---

## 8. Failure-semantics fix

Two halves — the synchronous writer path (already correct, make it actionable) and
the background thread (the actual hang).

**(i) Synchronous writer error (labels/keys) — already returned, make it actionable.**
`intern()` already returns structured `CapacityExceeded { resource, current, limit }`
for labels (`current/mod.rs:494`) and property keys (`property/map.rs:518`), so the
writer already gets a synchronous, non-retriable error. **Change:** make the message
**name the knob** — e.g. *"string interner at capacity (N/N); raise
`persistence.max_interned_strings` and restart"* — and ensure the structured
`details` carry `resource`/`current`/`limit`.

**(ii) Background thread — terminal-loud, suspend-until-restart.** In
`worker.rs:203-314`, for each `persist_*` call that currently does
`Err(e) => eprintln!(...)`:

1. **Classify** the error: *deterministic* (`CapacityExceeded`, the intern-string
   serialization wrap — retrying the identical state cannot succeed) vs *transient*
   (I/O — a later attempt may succeed).
2. On **deterministic** failure: log **once** via `eprintln!` (the module's
   established best-effort-stderr convention — no new dependency on an optional
   logging framework) with actionable text naming `persistence.max_interned_strings`,
   then **SUSPEND** background persistence for that affected index entirely — set a
   per-index "persist-suspended" latch so **no further attempts** are made until the
   process restarts. Because raising the cap *requires a restart* (the runtime and
   load caps are both read at `open()`), retrying — even with backoff — is futile;
   suspending is cleaner, testable, and provably bounded (attempt count for the
   affected index goes to exactly 1 and then stops). The log fires exactly once,
   guarded on the transition into the suspended state.
3. On **transient** failure: keep the existing 1s retry cadence (I/O may recover); do
   **not** suspend.

The per-index latch is a small `PersistSuspension` value (an `AtomicBool` suspended
flag plus atomic attempt / log-emit counters used as test hooks), one per index type
(vector/graph/temporal/strings), so a deterministic failure on one index never
silences the others.

**(iii) Manifest string count — exact written count, not a racy global sample.**
`persist_string_interner` previously stamped the manifest's `string_count` from a live
`GLOBAL_INTERNER.len()` read taken *after* the save completed, which can race a
concurrent writer that interned more strings between the file write and the count
read. The save functions now return the exact number of strings they serialized, and
`persist_string_interner` threads that written count through to
`tracker.update_last_persisted_string_count`, so the manifest describes exactly what is
on disk.

**Scope note:** property-**value** interning **stays at persist time** (moving it to
the write path is out of scope and risky — it changes hot-path latency and the write
error surface). The guarantee for the value path is therefore the **terminal-loud
background handling**: the hang is replaced by a bounded, self-explaining, backed-off
error, and — per Q2 — **a persist failure does not lose data** (WAL is ground truth;
worst case is a slower next restart).

---

## 9. Prod-upgrade procedure

The cap is read at **`open()`** (both the runtime `set_max_capacity` and the load
validator). Raising it **requires a RESTART** (hot-reload is out of scope).

Operator flow:

1. Hit the clean, actionable error (writer error for labels/keys; the once-logged
   ERROR line for the value path) that names `persistence.max_interned_strings`.
2. Bump `persistence.max_interned_strings` in config (TOML or builder).
3. Restart.
4. Existing on-disk data **loads clean** (the load validator now uses the raised cap;
   a previously-saved interner with >old-default strings is accepted).
5. Interning proceeds **past the old cap**.

**No migration. No format change.** (`StringInternerData` is unchanged; the cap is a
load-time threshold, §Q3.)

---

## 10. MCP / HTTP surface

Current: `StorageError::CapacityExceeded` → MCP `FailedPrecondition`, non-retriable
(`mcp/error.rs:342`); HTTP falls through to 400 BadRequest (`http/error.rs`).

**Decision: keep `FAILED_PRECONDITION` (non-retriable).** It is correct — the request
is well-formed but the system refuses in its current state, and retrying the identical
call cannot succeed; only operator action (raise cap + restart) resolves it.
`RESOURCE_EXHAUSTED` was considered but rejected: it risks destabilizing existing
conformance tests, and the semantics (needs operator action, non-transient) map more
faithfully to `FAILED_PRECONDITION`. **Change:** improve the message to name
`persistence.max_interned_strings` and ensure `details` carry structured
`resource`/`current`/`limit` (the #3234 envelope), so an LLM/operator gets an
actionable, machine-readable signal instead of a bare string.

---

## 11. Risks / edge cases as TEST CASES (drives TDD)

1. **Synchronous writer cap error** — with a low configured cap, a `create_node`
   whose label/property-key interning exceeds the cap returns a synchronous
   `CapacityExceeded` (structured) — the writer is *not* silently deferred.
2. **Background thread does not spin on cap exhaustion** — under a deterministic
   `CapacityExceeded` failure, assert the affected index's persist-**attempt count
   goes to exactly 1 and then STOPS** (the index is observably suspended), rather than
   re-attempting each cycle. A transient (I/O) failure, by contrast, does **not**
   suspend and keeps attempting.
3. **Log-once** — the actionable deterministic-failure line is emitted **exactly
   once** (asserted via the atomic emit-counter test hook on the transition into the
   suspended state).
4. **Prod-upgrade (the headline test)** — durable DB under a LOW configured cap;
   fill to/near the cap; close; reopen with a HIGHER cap → (a) existing data loads
   clean, (b) interning proceeds beyond the old cap, (c) no migration / no format
   change (same file bytes accepted).
5. **Config round-trip** — `PersistenceConfig.max_interned_strings` round-trips
   through the unified **builder** AND **TOML** (serde).
6. **Load validation uses the configured cap** — a persisted interner file with
   more strings than the *old default* (100K) loads successfully under a raised cap
   (proves `validate_string_interner` honors the configured cap, not the constant).
7. **MCP structured envelope** — a `create` that exceeds the cap returns the #3234
   structured envelope (`FAILED_PRECONDITION` + message naming the knob + structured
   `details.{resource,current,limit}`), **not** a 500/panic.
8. **Default cap = 10M** — assert `DEFAULT_MAX_INTERNED_STRINGS == 10_000_000` and
   that a default-config `PersistenceConfig.max_interned_strings == 10_000_000`.

---

## 12. Acceptance Criteria

| AC | Requirement | Proven by |
|---|---|---|
| **A** | Cap is configurable + default raised to 10M + builder + TOML + docs | Tests 5, 8; docs update (§CONFIGURATION.md) |
| **A′** | Load-validation cap driven by the same config field (lockstep) | Tests 4, 6 |
| **B(i)** | Synchronous **structured** writer error for labels/keys, message names the knob | Tests 1, 7 |
| **B(ii)** | Terminal-loud, **no-spin** background handling (bounded retries + log-once + backoff) | Tests 2, 3 |
| **C** | MCP/HTTP propagation: `FAILED_PRECONDITION` + actionable message + structured `details` | Test 7 |
| **Upgrade** | Reopen-with-higher-cap loads clean and interns past old cap | Test 4 |
| **No-format-change** | No migration, `StringInternerData` layout unchanged | Test 4(c); §Q3 |

---

## 13. Docs to update (impl phase)

- `docs/CONFIGURATION.md` — the **`[persistence]`** section (TOML example at
  `docs/CONFIGURATION.md:96`, and the "Configuration Parameters" table at
  `:170`) gains a `max_interned_strings` row/field. *Placeholder noted here; the
  implementation worker writes the final text.*
- `CLAUDE.md` persistence notes — mention the configurable cap + terminal-loud
  background behavior (optional, follow-up).
</content>
</invoke>
