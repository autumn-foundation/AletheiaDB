# Promotion Runbook: Manual Failover

Step-by-step operator runbook for promoting a replica to primary after the
original primary is lost or unreachable. Read the
[Replication Guide](replication-guide.md#consistency-contract) first,
especially the consistency contract and the "No automatic fencing" limitation
— **promotion never fences the old primary for you.**

## Step 0: Preconditions

Before you start, make sure you have:

- **Access to promote**: either an HTTP API key with the `admin` role (for
  `POST /admin/promote`) or Rust/embedded access to the replica process (for
  `AletheiaDB::promote_to_primary()`). Per the
  [access control matrix](access-control-matrix.md), promotion is an
  `Admin`-class operation on the HTTP surface.
- **A way to isolate the old primary** from the network or from write
  traffic — the ability to stop its process, block its port, pull its
  network interface, or otherwise ensure it cannot accept further writes.
  You need this capability *before* you promote, not after.
- **Access to the replica's progress/stats** (`replication_progress()` /
  `database_stats`) to make an informed choice among candidates, if more
  than one replica exists.

## Step 1: Confirm the primary is actually dead — and fence it

Do not promote on suspicion alone. Confirm the primary is unreachable through
more than one signal if possible (health check timeout, no recent commits
observed by any replica's `primary_flushed_lsn`, infrastructure-level alert).

**Then, before promoting anything, fence the old primary**: stop its process,
or block network access to it (firewall rule, security group change, remove
it from a load balancer), so it categorically cannot accept a write again
while still believing it is the primary.

> **Why this matters — split-brain.** AletheiaDB has no automatic
> fencing/quorum in v1 ([ADR-0059](../adr/0059-asynchronous-wal-shipping-replication.md)).
> Promoting a replica does not stop the old primary from accepting writes if
> it is still running (e.g., a network partition rather than a true crash,
> or a "primary" that looks dead but is actually just slow/unresponsive). If
> both nodes accept writes after this point, you now have two divergent
> histories with no reconciliation path. **Fencing is the operator's
> responsibility and must happen before or as part of promotion, not after.**

## Step 2: Inspect replica state before choosing a promotion target

If you have more than one replica, check each candidate's progress before
picking one:

- **Rust API**: `replica.replication_progress()` → `Option<ReplicaProgressStats>`
- **MCP/HTTP**: the `database_stats` tool/route → `replication` block

Look at:

| Field | What it tells you |
|---|---|
| `state` | `"streaming"` is healthy; `"resync_required"` means this replica is missing history and cannot safely resume — see below; `"connecting"`/`"stopped"` mean it isn't actively caught up |
| `last_applied_lsn` | The replica's last fully-applied primary LSN |
| `entries_behind` | How many entries behind the primary's last-known flushed LSN this replica was, as of its last successful fetch |
| `lag_ms` | Estimated replication lag in milliseconds |
| `last_error` | The most recent applier error, if any |

**`entries_behind`/`lag_ms` at the moment connectivity to the old primary was
lost is your data-loss window (RPO).** Any primary write committed after that
point and not yet applied to the replica you promote is gone. If you have
multiple candidates, prefer the one with the lowest `entries_behind`/`lag_ms`
(most caught up).

**If `state == "resync_required"`**: this replica's applier detected that the
primary had already truncated WAL history it still needed (see the
[Replication Guide](replication-guide.md#bootstrap--resume)) and stopped
applying — it may be missing an unknown amount of recent history. **Prefer a
different, still-`"streaming"` replica if one exists.** If this is your only
replica, understand that promoting it accepts whatever state it was frozen
at; there is no way to recover the gap from this node alone.

## Step 3: Promote

Choose one of two equivalent surfaces.

### HTTP (server deployments)

```bash
curl -X POST https://replica-host:PORT/admin/promote \
  -H "Authorization: Bearer $ADMIN_API_KEY"
```

Requires an `admin`-role key (`AccessClass::Admin`). Response body:

```json
{
  "success": true,
  "data": {
    "role": "primary",
    "applier_stopped": true,
    "last_applied_lsn": 128734
  }
}
```

- `role`: always `"primary"` on success.
- `applier_stopped`: `true` if a running replication applier was stopped as
  part of this call; `false` if the node was already `Primary`, or was a
  replica on which `start_replication`/`bootstrap_replica` was never called
  (nothing to stop).
- `last_applied_lsn`: the replica's last fully-applied primary LSN at the
  moment of promotion, when an applier was running; `null` otherwise. This is
  the coordinate the new primary's local WAL now allocates *past* — it is
  also your reference point for confirming no data was lost relative to
  Step 2's observed lag.

This is the **one** write-class admin operation a replica is allowed to
accept (`src/http/admin.rs`'s `promote` handler deliberately does not apply
the read-only-replica refusal every other write/admin route does) — refusing
it would make it impossible to ever promote a replica over this surface.

### Rust API (embedded use)

```rust
// Illustrative — see src/db/replication_role.rs for the exact signature.
let report: aletheiadb::PromotionReport = replica.promote_to_primary()?;
println!("applier_stopped={} last_applied_lsn={:?}",
    report.applier_stopped, report.last_applied_lsn);
```

`PromotionReport` has the same two fields as the HTTP response
(`applier_stopped: bool`, `last_applied_lsn: Option<u64>`).

**What promotion does, in order** (from `AletheiaDB::promote_to_primary`,
`src/db/replication_role.rs`):

1. Stops and joins the background replication applier thread (if one was
   running) — no more replicated writes can land after this point.
2. Seeds the local WAL's next LSN to `applied_lsn + 1` (never backwards), so
   a newly-accepted write's LSN can never collide with history this node
   already applied as a replica.
3. Persists indexes at the promotion point, if index persistence is
   configured, so a subsequent restart resumes from here rather than
   needing to re-replicate from the (now former) primary.
4. Flips the role atomic to `Primary`.

**Idempotent**: promoting an already-primary node succeeds as a no-op
(`applier_stopped: false`, `last_applied_lsn: None`).

## Step 4: Verify

- **Role flipped**: re-check `database_stats`/`replication_progress` —
  `replication.role` should now read `"primary"`, and (Rust API)
  `db.is_replica()` should be `false`.
- **Writes accepted**: perform a small, low-risk write (e.g., create a
  throwaway test node, or issue whatever your application's health-check
  write is) and confirm it succeeds — no more `read_only_replica` rejection.
- **Spot-check recent data**: compare a handful of recently-written entities
  against what you expect from the old primary (application-level records,
  logs, or another replica) to build confidence nothing looks obviously
  wrong at the promotion boundary.

## Step 5: Repoint clients/writers

Update your application's write path (connection string, load balancer
target, DNS record, service discovery entry — whatever your deployment uses)
to point at the newly-promoted primary. Nothing in AletheiaDB does this
automatically; there is no built-in proxy or client-side failover routing in
v1.

If other replicas exist, repoint their `primary_addr` at the new primary and
restart their replication (or reconfigure and restart the process) so they
resume streaming from the new source. (A stale replica still pointed at the
now-fenced old primary will simply fail to connect — the applier's poll loop
retries and reports `state: "connecting"`, harmlessly, until repointed.)

## Step 6: Aftermath — the old primary

**Never let the old primary rejoin as primary.** Once you have promoted a
replica, the old primary's history may have diverged (any writes it accepted
after the replica's last-applied position, but that never reached any
promoted node, are now orphaned relative to the new primary's timeline).

When the old primary comes back online (network partition healed, process
restarted, etc.):

1. Treat it as **stale**, not as a live participant, until you decide what
   to do with it.
2. Do **not** simply restart replication pointing it at the new primary in
   place — its own local WAL/history may conflict with the new primary's
   LSN space.
3. **Wipe its data directory and re-bootstrap it as a fresh replica** of the
   new primary (`AletheiaDB::bootstrap_replica` against an empty data
   directory, or the config-driven `primary_addr` auto-wiring against a
   fresh directory) — this is the supported path back into the topology.
4. If the old primary accepted any writes after the failover point that you
   need to recover (because fencing came too late), that data must be
   recovered manually (e.g., from its `.albk` backup or WAL archive) and
   reconciled with the new primary out-of-band — AletheiaDB has no automatic
   merge/reconciliation for diverged histories.

## Step 7: Rollback / abort paths

- **Before Step 3 (promote)**: entirely safe to abort — nothing has changed
  yet. Re-run Step 1/2 with different information, or wait.
- **After Step 3 (promote), before Step 5 (repoint clients)**: the promoted
  node is now a writable primary, but nothing outside AletheiaDB has been
  told yet. If you change your mind about which node to promote, you can
  call `enter_replica_mode()` on the promoted node (Rust API) to flip it
  back to `Replica`, though it will not automatically resume streaming
  unless you call `start_replication`/`bootstrap_replica` again — there is
  no single "demote" that also re-attaches it to a source. In practice, once
  you have promoted and verified (Step 4), proceed forward rather than
  reverting.
- **After Step 5 (repoint clients)**: you have committed to the new primary
  as the source of truth. Reverting now means the new primary has already
  accepted writes that the old primary (or another candidate) does not have
  — do not try to "undo" this by pointing clients back; instead, treat any
  further correction as a forward operation (fix data, or promote yet
  another node if the current primary also fails) and follow Step 6's
  wipe-and-re-bootstrap procedure for whatever you're retiring.

## Expected RTO drivers

Promotion itself is **local, in-process work** with no network round-trip
to the (now unreachable) old primary: stopping the applier thread, seeding
the local WAL's next LSN, and (if configured) one index-persistence flush.
The dominant costs are:

- Stopping/joining the background applier thread (bounded by its poll
  interval).
- The index-persistence flush, if configured (proportional to current-state
  size, not full history).
- Everything *outside* AletheiaDB: detection/decision time (Steps 1-2),
  fencing the old primary, and repointing clients (Step 5) — these are
  usually the larger share of real-world RTO and are entirely
  operator/infrastructure-dependent.

For a reproducible, scripted measurement of promotion latency on a reference
fixture, see `tests/replication_slo_harness.rs`.

<!-- SLO-NUMBERS: filled after harness run -->

## See also

- [Replication Guide](replication-guide.md) — architecture, consistency
  contract, monitoring, security, limitations.
- [ADR-0059: Asynchronous WAL-Shipping Replication](../adr/0059-asynchronous-wal-shipping-replication.md)
- [Crash scenarios index](../testing/crash-scenarios.md)
- `tests/replication_chaos.rs` — chaos coverage for primary-loss-then-promote scenarios.
