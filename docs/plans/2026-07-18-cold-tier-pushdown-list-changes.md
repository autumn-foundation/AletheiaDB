# True Cold-Tier Pushdown for `list_changes` (Approach B — Issue #3677)

Status: In progress
Base: origin/trunk @ 1f4219a
Follow-up to: PR #3685 (v1 filter+limit pushdown)

## Problem

`AletheiaDB::list_changes` (`src/db/temporal.rs`) serves a bi-temporal changefeed
window by merging a hot-tier scan with a cold-tier scan. After PR #3685 both tiers
push the tx-window / valid-window / label filter / resume-cursor / limit *down into*
their scans and retain only the `limit + 1`-smallest survivors by `ChangeCursor`. That
made the working set `O(page)` — but the **cold tier still decodes every stored version**
(`RedbColdStorage::collect_changes_filtered` → `scan_versions_into`), so cold I/O stays
`O(N_cold)` per query. On a database with a large cold history, a `limit = 10` "what
changed in the last minute" query still decrypts + decompresses + decodes the entire
cold store.

## The landmine (why the naive fix is unsound)

redb cold storage is keyed by `version_id` (`redb_cold_storage/mod.rs`,
`NODE_VERSIONS_TABLE`/`EDGE_VERSIONS_TABLE` are `TableDefinition<u64, &[u8]>`). It is
tempting to `range()` that ascending `version_id` key and early-stop once past the
window. **This is unsound.** A version's `version_id` is allocated at transaction *build*
time (`src/api/transaction/write/mod.rs`, `version_id_gen.next()`), but its commit
*transaction-time* — the coordinate that orders the changefeed via `ChangeCursor` — is
assigned later under the `current_timestamp` lock at commit
(`write/mod.rs` commit block). The two orderings are independent: two concurrent commits
can allocate ids in one order and commit in the opposite order, so **a smaller
`version_id` can carry a larger tx-time**. Any early-stop on the ascending `version_id`
key silently drops or misorders rows. This is guarded by the existing
`resume_across_version_id_inversion` test and must remain guarded.

## Chosen approach — Approach B: in-memory tx-time-ordered cold directory

Maintain, inside `TieredStorage`, an in-memory **directory** of the cold-resident
versions, ordered by `ChangeCursor` (i.e. by transaction-time, then the cursor
tie-breakers). Because `ChangeCursor` already carries `kind_ord` and `version_id`, each
directory entry is a self-sufficient pointer to the exact cold row: no per-entry payload
is needed beyond the cursor itself.

### Data structure

- `ColdChangeDirectory`: a `parking_lot::RwLock<Inner>` where `Inner` holds a
  `BTreeSet<ChangeCursor>` (tx-time-ordered membership of cold-resident versions), a
  `complete: bool` authority flag, a `coverage_watermark: Option<ChangeCursor>` (the
  largest cursor ever evicted for budget), and a `max_entries` cap.
- The set stores only `ChangeCursor` (≈ 5 machine words). No decoded payload — the
  materialization still decodes the *selected* cold rows via the existing point-read
  path, so parity is byte-identical **by construction** (same decode + `consider_version`).
- It lives *inside* `TieredStorage`, reached only through the `historical` field's
  `Option<Arc<TieredStorage>>`. Its `RwLock` is a **leaf**: it is acquired after
  `historical` (the query path already drops the `historical` lock before touching cold),
  and it never calls back into `historical` / `wal` / `current_timestamp`. This preserves
  the CLAUDE.md lock order with no new constraint.

### Maintenance

- **On migration** (`historical::migrate_to_cold` → migration service → cold batch store):
  each migrated node/edge version's `ChangeCursor` is inserted into the directory **as part
  of the cold store step, before the version is removed from the hot maps**. This ordering
  is the crux of concurrency correctness: during the migration window a version is present
  in *both* hot and directory (deduped by `(kind_ord, version_id)` exactly as today), and
  after hot removal it is present in the directory. It is never absent from both — so no
  concurrent `list_changes` scan can miss it.
- **On startup / cold attach**: rebuild the directory by scanning cold once
  (`scan_versions_into`, computing each version's cursor). This is a bounded, one-time cost
  measured and reported in the PR. If the rebuild would exceed `max_entries`, the directory
  retains the newest cursors and sets `complete = false` with the watermark (see budget).

### Query

Given the (tx_window, valid_window, label_filter, resume_after, bound) already computed by
`list_changes`:

1. If the directory is **eligible** (see budget) — range the `BTreeSet` over
   `[lower, upper]` where `lower` is a sentinel cursor just above `max(resume_after,
   tx_window.start)` and `upper` is a sentinel cursor at `tx_window.end` — ascending.
2. For each candidate cursor in range, point-decode the cold row it names
   (`get_node_version` / `get_edge_version` by `version_id`) and feed the decoded version
   through the **unchanged** shared `consider_version(acc, resume_after, ...)` — so the
   valid-window / label / strict-`> cursor` / bound decisions are byte-identical to the
   full-scan path.
3. Because candidates arrive in ascending cursor order and `consider_version` keeps the
   bound-smallest, once the accumulator holds `bound` survivors no later candidate can
   displace one → **early-stop**. (Unbounded `bound == usize::MAX` walks the whole window.)
4. If the directory is **not eligible**, fall back to the existing
   `collect_changes_filtered` full cold scan. Correctness never depends on the directory.

The result feeds the identical merge / cross-tier dedup / `select_nth` / sort /
`next_cursor` pagination in `list_changes`. Public API unchanged; no on-disk format change.

### Memory budget + eviction / degrade-to-scan

- `max_entries` (configurable via `TieredStorageConfig`, sensible default) caps directory
  size. When an insert would exceed the cap, evict the smallest cursors (`pop_first`,
  oldest tx-time) and advance `coverage_watermark` to the largest evicted cursor; set
  `complete = false`.
- **Eligibility rule (safe partial coverage):** a query may use the directory iff its scan
  lower bound is strictly greater than `coverage_watermark` (i.e. the whole window lies in
  the retained, newest region). Otherwise degrade to the full cold scan. Recent-window
  changefeed queries (the common case) stay on the fast path even for a huge cold history;
  old-window queries remain correct via scan. Correctness is independent of the directory
  being complete.

## Approaches considered (brainstorm / reverse-brainstorm / six-hats)

**Brainstorm — candidate designs:**
- (A) On-disk tx-time secondary index in redb. Rejected: on-disk format change, needs
  sign-off, explicitly out of scope for #3677.
- (B) In-memory ChangeCursor-ordered directory in TieredStorage (chosen).
- (C) Store the full materialized `RawChange` in each directory entry (zero cold decode).
  Rejected for v1: larger per-entry memory (fewer entries fit the budget → more
  degradation) and a second construction path that must be proven byte-identical to
  decode+`build_raw_change`. The chosen design point-decodes selected rows through the
  *same* builder, making parity free. (C) is a possible future optimization.
- (D) Re-key cold storage by a composite (tx_time, version_id). Rejected: format change +
  breaks `get_*_version` point reads.

**Reverse-brainstorm — how could this go wrong, and the guard:**
- Drop a row during migration → insert into directory *before* hot removal; dedup covers
  the overlap window (stress test AC4).
- Misorder under version_id inversion → directory is keyed by `ChangeCursor` (tx-time),
  never by `version_id` (inversion parity test).
- Silent wrong answer when over budget → eligibility watermark forces degrade-to-scan;
  never serve a partially-covered window from the directory (budget test).
- Directory/reality divergence → cold versions are immutable once stored, and the
  directory is rebuilt from cold on startup; membership only grows on migration and shrinks
  on eviction (never on read).
- Parity drift → materialization reuses `consider_version` verbatim; an independent
  hand-rolled reference oracle differentially validates output.
- Lock-order violation → directory RwLock is a leaf, acquired after `historical`, never
  re-entering upward.

**Six hats:**
- White (facts): cold I/O is O(N) today; version_id ≠ tx-time; redb keyed by version_id.
- Red (intuition): the directory "feels" like an index but must never be trusted as
  authoritative — hence the degrade path.
- Black (caution): startup rebuild cost, memory growth, migration race — each has a named
  test/measurement below.
- Yellow (benefit): bounded recent-window queries drop from O(N_cold) decodes to
  O(window) decodes; large memory/CPU/IO win on big histories.
- Green (creativity): watermark-based partial coverage keeps the win under a tight budget
  instead of all-or-nothing.
- Blue (process): design-first → red tests → green → 4-lens review → AC evidence.

## Risks / edge cases as test cases (red phase)

1. `cold_directory_parity_hot_only` / `_cold_only` / `_mixed_tiers` — byte-identical
   `changes` + `next_cursor` vs the reference oracle, through the directory path.
2. `cold_directory_version_id_inversion_parity` — smaller version_id carries larger
   tx-time; directory orders by cursor; paging follows tx-time.
3. `cold_directory_decodes_only_window` — instrument a cold-decode counter; assert decoded
   count ≪ total cold versions for a bounded recent-window query (AC1 measurement).
4. `cold_directory_early_stop_bound` — bounded query stops after `bound` survivors.
5. `cold_directory_degrade_over_budget_parity` — tiny `max_entries`, force eviction, query
   an old window below the watermark → correct via scan (parity) and confirmed to take the
   degrade path.
6. `cold_directory_partial_coverage_recent_window` — under a tight budget a *recent* window
   still uses the directory (eligible) and is byte-identical.
7. `cold_directory_startup_rebuild_parity` — build directory from cold on attach; parity.
8. `cold_directory_concurrent_migration_stress` — drive writes + hot→cold migration while
   paging `list_changes`; union of all pages == ground truth; no dup, no gap (AC4; also
   discharges #3685's deferred live-migration stress follow-up).
9. `cold_directory_empty_window` / `_limit_over_bound` / `_midstream_cursor` — reuse
   #3685's edge cases through the directory path.
10. Independent parity oracle: a hand-rolled reference `list_changes` (naive full scan, no
    shared `collect_changes`/directory) used only by tests — discharges #3685's deferred
    "fully-independent parity oracle" follow-up.
11. All existing `changefeed_tests` (lib) + `changefeed_subscribe` (integration) remain
    green untouched (AC3 — cursor / at-least-once / dedup unchanged).

## Benchmark plan

Criterion bench (`benches/changefeed.rs`): a bounded (`limit = 10`) recent-window
`list_changes` over a large cold history (e.g. 100k cold versions), before (full cold
scan) vs after (directory pushdown). Report wall time **and** cold versions decoded per
call. Honest framing: the headline is decodes-per-query dropping from `O(N_cold)` to
`O(window)`; also report the one-time startup rebuild cost.

## Acceptance criteria (from #3677 / coordinator)

1. Windowed/limited queries no longer decode every cold version (measured before/after).
2. Byte-identical parity across hot-only / cold-only / mixed / empty-window /
   limit-over-bound / mid-stream-cursor (reuse #3685 parity style + BoundedChanges seam).
3. Cursor / at-least-once / dedup semantics unchanged (all changefeed suites green).
4. Directory correctness under concurrent hot→cold migration (stress test).
5. Memory budget respected with degrade-to-scan fallback tested.
6. No on-disk format change, no lock-order violation (directory is a leaf), no public API
   break.
7. Criterion bench: bounded query over a large cold history, before/after, honest framing.
