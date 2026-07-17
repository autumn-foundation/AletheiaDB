# Reacting to change: the push changefeed (Issue #3375)

AletheiaDB exposes two complementary ways to observe *what changed*:

| Feed | API | Shape | Durability |
|------|-----|-------|------------|
| **Pull** (#3216) | [`AletheiaDB::list_changes`] | "what changed between T1 and T2?" — you poll | **Durable ground truth** (rebuilt by WAL recovery) |
| **Push** (#3375) | [`AletheiaDB::subscribe_changes`] | events arrive as transactions commit — no polling | **Best-effort at-least-once** over the pull feed |

This guide covers the push feed: subscribing, filtering, the catch-up/reconnect loop, and
its at-least-once + crash-recovery contract.

> **Scope note.** This is the **Rust-API-only** slice. An HTTP Server-Sent-Events surface and
> an MCP `await_changes` tool that wrap the blocking [`Subscription::recv_timeout`] long-poll
> are a coordinated follow-up (Lane 1); this guide describes the embedded Rust API only.

## Subscribing

```rust
use aletheiadb::{AletheiaDB, ChangeFilter};

let db = AletheiaDB::new()?;

// Receive every committed change.
let sub = db.subscribe_changes(ChangeFilter::all())?;

// ... commits happen elsewhere ...

// Drain what has buffered (non-blocking):
for change in sub.poll() {
    println!("{:?} {} v{}", change.change_type, change.entity_id, change.version_id);
}
```

Dropping the `Subscription` deregisters it — there is no leak and no need to unsubscribe
explicitly.

### Blocking long-poll

`recv_timeout` blocks up to a deadline, waking the instant an event is emitted. It is the
primitive an SSE / `await_changes` layer wraps:

```rust
use std::time::Duration;

match sub.recv_timeout(Duration::from_secs(5)) {
    Ok(events) if events.is_empty() => { /* timed out, nothing new */ }
    Ok(events) => { /* one or more changes */ }
    Err(err) => { /* lagged — see "Slow consumers" below */ }
}
```

## Filtering

A [`ChangeFilter`] narrows the stream. Each dimension is optional; an unconstrained filter
receives everything.

```rust
use aletheiadb::ChangeFilter;
use aletheiadb::core::changefeed::ChangeType;

// Only Person node changes:
let f = ChangeFilter::all().with_node_labels(["Person"]);

// Only KNOWS / FOLLOWS edge changes:
let f = ChangeFilter::all().with_edge_types(["KNOWS", "FOLLOWS"]);

// Only deletions, across both nodes and edges:
let f = ChangeFilter::all().with_change_types([ChangeType::Deleted]);

// AND-composed: only newly-created Person nodes:
let f = ChangeFilter::all()
    .with_node_labels(["Person"])
    .with_change_types([ChangeType::Created]);
```

**Semantics** (chosen so each dimension is meaningful):

- A filter-less filter (all dimensions unset) matches **everything**.
- Setting **only** `node_labels` yields **only** matching node changes (edges excluded);
  setting **only** `edge_types` yields **only** matching edge changes (nodes excluded). Set
  both to receive both kinds.
- `change_types`, if set, is an independent AND applied regardless of kind.
- Label / edge-type matching is **exact string** match (mirroring the #3216 `label` filter).

There is no way in v1 to say "all node changes regardless of label" without an edge
constraint present; use a filter-less subscription and discard edges, or list the labels.

## The at-least-once contract and the catch-up loop

The push feed is **best-effort at-least-once**. The *durable* record of history is
`list_changes` (backed by historical storage, which WAL recovery rebuilds). The broadcaster
never blocks the writer and persists nothing. Two situations interrupt live delivery:

1. **Lag** — a slow consumer's bounded buffer overflows; it is disconnected (Lagged).
2. **Crash / gap** — the process restarts, or a commit fsynced but the in-memory broadcast
   never ran (crash between commit and notify), so the event was never pushed.

In **both** cases you recover with **zero loss** by resuming a `list_changes` pull from your
last **resume token** — the encoded cursor of the last event you drained:

```rust
use aletheiadb::core::changefeed::ChangeFeedQuery;
use aletheiadb::core::temporal::{TIMESTAMP_MAX, Timestamp};

// 1. Remember where you were:
let resume = sub.resume_token(); // Option<String>

// 2. On reconnect / catch-up, pull everything after that cursor:
let query = ChangeFeedQuery {
    tx_from: Timestamp::from(0),
    tx_to: TIMESTAMP_MAX,
    valid_from: None,
    valid_to: None,
    label: None,
    limit: 10_000,
    cursor: resume,                 // resume exactly after the last event seen
};
let missed = db.list_changes(&query)?.changes;
// ... apply `missed`, then re-subscribe and continue live ...
```

The union of **(live-delivered ∪ resume-pull)** equals exactly the #3216 pull over the whole
window — **no event is ever missed.**

### Duplicates on resume, and the dedup key

Because a resume re-pulls from a cursor at or before some already-seen events, **duplicate
delivery is possible**. Deduplicate by the stable change **cursor**, equivalently the tuple

```
(transaction_time, kind, entity_id, version_id)
```

which is exactly `transaction_time_range.start()` + `kind` + `entity_id` + `version_id` on a
[`ChangeRecord`]. A consumer that has already applied a given cursor ignores it on
re-delivery. This tuple is stable across restarts and identical between the live feed and the
pull feed.

### Crash recovery

If the process crashes after a commit was made durable (WAL fsynced) but **before** the
in-memory broadcast ran, no subscriber ever saw the event live — yet it is fully present in
historical storage after WAL replay. A consumer that persisted its last resume token recovers
those commits on restart via the same `list_changes` catch-up above, before going live again.
Persist your resume token alongside whatever state you derive from the feed (e.g. a cache) and
the derived state can never silently drift from the database.

> **Tiered-storage note (hot → cold migration window).** The live broadcast builds a commit's
> records from a short read of just that transaction's hot-tier versions. In the rare case a
> just-committed version is migrated from the hot tier to the cold (disk) tier *before* that
> read runs, it is skipped live — but it remains fully present in the durable `list_changes`
> ground truth (whose scan spans both tiers), so a resume pull still recovers it. At-least-once
> therefore holds across the migration window exactly as it does across a crash.

> **Authentication / authorization dependency (Issue #3350).** This Rust-API slice performs no
> access-control filtering: an embedded caller with a `subscribe_changes` handle sees every
> committed change matching its `ChangeFilter`. The role-gated MCP / HTTP surface that wraps
> this feed is a coordinated Lane 1 follow-up; once #3350's RBAC composes over it, a subscriber
> will see only the changes its role is permitted to read (reader-class), and the catch-up
> `list_changes` pull will be gated identically. Do not expose the raw embedded subscription
> across a trust boundary until that authorization layer is in place.

### The agent cache-invalidation pattern

A common use is keeping an LLM/agent's working cache in sync with the graph:

1. Subscribe with a filter matching the entities the agent cares about.
2. On each delivered change, invalidate/refresh the affected `entity_id` in the cache.
3. Persist `sub.resume_token()` periodically (and the derived cache).
4. On restart or after a lag/`Err(Lagged)`, run the catch-up pull from the persisted token,
   invalidate everything it returns (deduped by cursor), then resume live delivery.

Because the durable ground truth backs every gap, the cache is **eventually exact** even
across crashes and slow-consumer episodes.

## Slow consumers and bounded resources

Two caps protect memory (both configurable via [`AletheiaDB::set_changefeed_config`]):

| Cap | Default | Meaning |
|-----|---------|---------|
| `max_subscriptions` | 128 | Maximum concurrently-live subscriptions. Exceeding it fails `subscribe_changes` with `CapacityExceeded` (a non-retriable resource error). |
| `buffer_capacity` | 1024 | Per-subscription buffer, in events. A consumer that falls this far behind is **disconnected (Lagged)**, not awaited. |

A slow consumer can **never** back-pressure the writer or another subscriber: when its buffer
would overflow, it is marked Lagged and its events are dropped (recoverable via the resume
token). Other subscribers and the committing writer are unaffected.

Once Lagged, `recv_timeout` returns `Err(RecvError::Lagged { resume_token })`. Feed that
token to `list_changes` to recover the gap losslessly, then re-subscribe:

```rust
use aletheiadb::core::changefeed_subscription::RecvError;
use std::time::Duration;

match sub.recv_timeout(Duration::from_millis(100)) {
    Ok(events) => { /* apply events */ }
    Err(RecvError::Lagged { resume_token }) => {
        // Catch up from `resume_token` via list_changes, then subscribe again.
    }
}
```

`set_changefeed_config` applies the subscription cap immediately; a changed `buffer_capacity`
applies to **future** subscriptions (existing ones keep the capacity they were created with).

## Ordering guarantees

Per-subscriber delivery is **strictly cursor-ascending** — the `(tx_time, kind, entity_id,
version_id)` total order of the #3216 pull feed — **within and across commits, including under
concurrent writers**. A commit reserves its release slot under the commit-timestamp lock and an
ordered-emit sequencer releases each commit's records into subscriber buffers only once every
earlier-committed commit has been released, so a later-committing transaction can never appear
before an earlier one even when the two finish their post-commit record build out of order. Each
commit's records stay contiguous (never torn).

This ordering is exactly the precondition that makes the resume token a zero-loss anchor:
because delivery is cursor-ascending, `resume_token()` (the last delivered cursor) is a true
**high-water-mark** — every change with a smaller-or-equal cursor has already been delivered,
and `list_changes(cursor = resume_token)` recovers precisely the rest. **No record is ever
lost**, given the two-part precondition: (1) ordered (cursor-ascending) live delivery, and
(2) recovery via a `list_changes` resume from `last_delivered` (the `resume_token`) on every
lag/reconnect/restart. Duplicates arise only from an overlapping resume pull and are deduped by
the stable cursor.

## Per-principal subscription quota (Issue #3678)

Two caps protect the changefeed. `max_subscriptions` (default **128**) bounds the *total*
concurrently-live subscriptions across the broadcaster. `max_subscriptions_per_principal`
(default **16**, `DEFAULT_MAX_PER_PRINCIPAL_SUBSCRIPTIONS`) bounds how many any one
*authenticated principal* may hold, so a single principal cannot exhaust the global cap and
starve others. Both are enforced atomically under one lock; whichever binds first wins. A
principal can be given a different limit via `per_principal_overrides` (keyed by principal id),
and the shared `"anonymous"` bucket — used by every caller in anonymous mode, so the quota is
never a no-op locally — is tunable via the `"anonymous"` key.

Configure both through the unified config:

```rust
use aletheiadb::{AletheiaDB, config::AletheiaDBConfig};
use aletheiadb::core::changefeed_subscription::ChangefeedConfig;
use std::collections::HashMap;

let mut overrides = HashMap::new();
overrides.insert("ingest-service".to_string(), 64); // a trusted high-fan-out principal

let config = AletheiaDBConfig::builder()
    .changefeed(ChangefeedConfig {
        max_subscriptions: 256,
        buffer_capacity: 1024,
        max_subscriptions_per_principal: 16,
        per_principal_overrides: overrides,
    })
    .build();
let db = AletheiaDB::with_unified_config(config)?;
# Ok::<(), aletheiadb::Error>(())
```

Enforcement is identical across every subscription-creating surface — the MCP `await_changes`
long-poll and the HTTP `POST /changes/await` + `GET /changes/stream` (SSE) all funnel through
`AletheiaDB::subscribe_changes_for_principal`. A breach returns the structured
`RESOURCE_EXHAUSTED` envelope with `retriable: true` and `details {principal, current, limit}`
(it is a *fairness* limit — another of the principal's subscriptions may drop, so a backed-off
retry can succeed). Slots release promptly on unsubscribe, client disconnect, long-poll return,
and expiry, because the decrement lives only in the `Subscription` drop → `deregister` hook.

The bare embedded `AletheiaDB::subscribe_changes` path carries no principal and is unaffected —
omitting the principal reproduces the pre-#3678 behavior exactly.

**Observability.** `/metrics` exposes only *bounded aggregates* — a
`aletheiadb_changefeed_subscriptions_active` gauge and a
`aletheiadb_changefeed_quota_rejections_total` counter (never a per-principal label, preserving
the info-disclosure invariant). The per-principal breakdown is surfaced on the authenticated
`database_stats` JSON under `changefeed.per_principal`.

## Performance

The broadcast runs **after** the commit is durable, applied, and visible, and outside every
write-path lock — the broadcaster's locks are leaves. With no subscribers the write path pays
a single atomic load. The records for a commit are built by a targeted O(transaction-size)
read of just that transaction's versions (never a full history rescan), so they are
byte-identical to what `list_changes` would return for that window. See
`benches/changefeed_subscribe.rs` for the idle-subscriber throughput and per-commit emit-cost
benchmarks (acceptance target: ≤5% throughput reduction with 100 idle subscribers).

[`AletheiaDB::list_changes`]: ../../src/db/temporal.rs
[`AletheiaDB::subscribe_changes`]: ../../src/db/changefeed_sub.rs
[`AletheiaDB::set_changefeed_config`]: ../../src/db/changefeed_sub.rs
[`ChangeFilter`]: ../../src/core/changefeed_subscription.rs
[`ChangeRecord`]: ../../src/core/changefeed.rs
[`Subscription::recv_timeout`]: ../../src/core/changefeed_subscription.rs
