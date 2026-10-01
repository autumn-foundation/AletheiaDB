# Multi-Tenant Isolation (Issue #3365)

Serve many isolated logical databases — **tenants** — from one AletheiaDB
process. Each tenant is a *fully separate* database with its own graph, history,
vector indexes, constraints, schema, WAL, and (for a durable deployment) its own
on-disk directory, plus enforceable per-tenant resource quotas. This is the
foundation a hosted / SaaS / platform deployment needs: onboard a new tenant
without provisioning a new process, and guarantee that no tenant can read,
corrupt, or starve another.

> **Tenants vs namespaces.** Agent-scoped namespaces (Issue #3349) are a
> *cooperative* data-scoping convenience **within one trust domain** — explicitly
> *not* a security or resource boundary. Tenants (this feature) are the **hard
> isolation boundary**: separate databases, separate IDs, enforced quotas. Use
> namespaces to organize one tenant's agents; use tenants to separate customers.

## Why instance-per-tenant

A [`TenantManager`] owns one `Arc<AletheiaDB>` per tenant. Because the instances
share no storage, no locks, and no ID space, the isolation guarantees fall out
**by construction** rather than from per-query filtering (which is leak-prone):

| Guarantee | How it is achieved |
|-----------|--------------------|
| **Hard data isolation** | A tenant handle only ever touches its own `AletheiaDB`. Traversal, k-NN, `AS OF` reads, changefeed, and error messages are all scoped to one instance — there is no shared table to filter, so there is nothing to leak. |
| **Temporal invariants compose** | Each tenant *is* a single-tenant database, so its valid/tx-time semantics, WAL durability, and crash recovery are byte-for-byte the single-tenant behavior. |
| **Independent backup/restore** | Each tenant has its own `.albk` backup/restore (Issue #3217), taken and restored without touching any other tenant. |
| **Blast-radius containment** | One tenant hitting a quota, erroring, or being dropped touches only its own map entry and directory; every other tenant is a separate `Arc<AletheiaDB>` with a separate lock graph. |
| **ID / business-key collision is harmless** | Two tenants can both have node `1` named "Alice" with zero interference. |

## Quick start

```rust
use aletheiadb::tenant::TenantManager;
use aletheiadb::core::tenant::TenantQuota;
use aletheiadb::PropertyMapBuilder;

// Ephemeral (tests / scratch); use `TenantManager::open(path)` for durable.
let mgr = TenantManager::new_ephemeral();

// Create a tenant with a resource quota. Provisioning is process-restart-free.
let acme = mgr.create_tenant(
    "acme",
    TenantQuota::unlimited()
        .with_max_nodes(1_000_000)
        .with_max_edges(5_000_000)
        .with_max_storage_bytes(8 * 1024 * 1024 * 1024),
)?;

// The handle is bound to exactly this tenant. Writes are quota-enforced.
let alice = acme.create_node("Person", PropertyMapBuilder::new().insert("name", "Alice").build())?;

// Reads/queries go through the underlying isolated database.
let node = acme.db().get_node(alice)?;

// Metering: O(1) usage counters.
let usage = acme.usage();
println!("{} nodes, ~{} bytes", usage.node_count, usage.storage_bytes);
# Ok::<(), Box<dyn std::error::Error>>(())
```

> **Tenant ids are lowercase-only** (`[a-z0-9._-]`, ≤128 bytes, no leading/trailing
> `.`, not a Windows reserved device name). A tenant id doubles as an on-disk
> directory name, and the lowercase rule makes two ids *impossible* to collide on
> a case-insensitive filesystem (macOS APFS, Windows NTFS) — `Acme` and `acme`
> cannot both exist because the uppercase form is rejected outright. Lifecycle:
> `create_tenant` / `get_tenant` / `get_tenant_info` / `list_tenants` /
> `tenant_usage` / `set_tenant_quota` / `delete_tenant` / `restore_tenant`.

## The write path is the handle

Quota enforcement and usage accounting live on [`TenantHandle`]'s
`create_*` / `delete_*` methods. `handle.db()` exposes the underlying
`AletheiaDB` for **reads, queries, and index management** (always fully
isolated). Writes issued directly through `db()` are isolated but bypass quota
accounting — the sanctioned, enforced write path is the handle.

## Resource quotas

A [`TenantQuota`] bounds four dimensions; any dimension left `None` is unlimited:

| Dimension | Enforcement | Notes |
|-----------|-------------|-------|
| `max_nodes` | **Precise** | Atomic reservation taken before the write, released on failure — concurrent writers can never race past the cap. |
| `max_edges` | **Precise** | Same reservation mechanism. |
| `max_vector_index_bytes` | **Best-effort (v1)** | Checked against an O(1) estimator (`Σ dims × vector_count × 4`). |
| `max_storage_bytes` | **Best-effort (v1)** | Checked against an O(1) estimator over current entities + retained versions. |

A write that would exceed a quota is rejected with a structured
`TenantError::QuotaExceeded` **before** it is applied — **never a partial write,
never a process-wide failure**. Quotas are adjustable after creation
(`set_tenant_quota`); lowering a limit never retroactively deletes data, it only
rejects further growth until usage drops back under the new limit.

### Structured errors (Issue #3234 contract)

Tenant errors classify to the uniform MCP/HTTP error envelope:

| Error | Code | Retriable | `details` |
|-------|------|-----------|-----------|
| `InvalidId` | `INVALID_ARGUMENT` | false | — |
| `NotFound` | `NOT_FOUND` | false | `{tenant}` (HTTP) |
| `AlreadyExists` | `CONFLICT` | false | `{tenant}` (HTTP) |
| `QuotaExceeded` | `RESOURCE_EXHAUSTED` | **false** | `{tenant, dimension, current, limit}` |

A quota breach is deliberately **non-retriable** (unlike the transient
per-principal changefeed quota of #3678): a capacity limit heals only by freeing
data or raising the quota, never by retrying the same call.

## Usage accounting

`handle.usage()` (and `mgr.tenant_usage(id)`) return a [`TenantUsage`] snapshot of
`{node_count, edge_count, vector_index_bytes, storage_bytes}`. Every field is an
O(1) counter read — never a version scan — consistent with the `database_stats`
approach (Issue #3222), suitable for metering. Node/edge counts are exact; the
byte fields are best-effort estimates in v1.

## Durable deployments and restart

`TenantManager::open(root)` persists tenants under `{root}`:

```
{root}/
├── tenants.json            # registry sidecar: ids + creation time + quotas
└── tenants/
    ├── acme/               # a full durable AletheiaDB (WAL + indexes + cold)
    └── globex/
```

On open, every registered tenant's database is reopened from its directory, so a
restart restores **every tenant to its last consistent state** — each tenant's
crash recovery is exactly a single-tenant database's. The registry sidecar uses
the same atomic temp→fsync→rename write and quarantine-on-corrupt load as the
namespace/snapshot registries: a corrupt sidecar is moved aside (`*.corrupt`) and
startup proceeds rather than bricking.

## Binding a connection/session to one tenant

A connection binds to exactly one tenant by holding one [`TenantHandle`]. The
tenant claim a future authenticated identity (Issue #3350) carries maps 1:1 onto
which handle it is issued — identity owns *who you are*, this feature owns *which
isolated database you touch*.

### Tenant-scoped MCP server

Because a tenant is a normal `AletheiaDB`, an MCP server can be started
tenant-scoped so existing agent tooling works unchanged:

```rust
# #[cfg(feature = "mcp-server")]
# fn demo(acme: &aletheiadb::tenant::TenantHandle) {
let server = acme.mcp_server(); // every tool operates only on this tenant
# let _ = server;
# }
```

## Independent per-tenant backup/restore

```rust
# #[cfg(not(target_arch = "wasm32"))]
# fn demo(mgr: &aletheiadb::tenant::TenantManager, acme: &aletheiadb::tenant::TenantHandle) -> Result<(), Box<dyn std::error::Error>> {
# use aletheiadb::core::tenant::TenantQuota;
acme.backup(std::path::Path::new("/backups/acme.albk"))?;                  // one tenant only
mgr.restore_tenant("acme-copy", std::path::Path::new("/backups/acme.albk"), TenantQuota::unlimited())?;
# Ok(())
# }
```

## Out of scope in v1

- Exact byte accounting for the two byte quotas (best-effort estimators today).
- CPU/IOPS scheduling fairness beyond the blast-radius bound.
- Cross-tenant queries / union scope — impossible by design (that is what
  intra-tenant namespaces, #3349, are for).
- Admin *tool* wiring on the MCP/HTTP/CLI surfaces (lifecycle over the Rust API
  today) and identity→tenant binding (Issue #3350) are coordinated follow-ups
  that compose directly on this isolation core.

## Success metrics (Issue #3365)

- Tenant-scoped current-state single-hop reads: p99 ≤ 1.2× the single-tenant
  target — each tenant read is a normal single-tenant read with no added
  indirection on the hot path.
- Isolation soak: **0** cross-tenant leaks and **0** quota-bypass writes across
  concurrent interleaved operations from many tenants
  (`tests/multi_tenant_isolation.rs`).
- Noisy-neighbor: a saturating writer does not break another tenant's reads
  (separate lock graphs; `noisy_writer_does_not_break_reader`).
- New-tenant provisioning completes in well under a second with no process
  restart (a single database open).

[`TenantManager`]: https://docs.rs/aletheiadb/latest/aletheiadb/tenant/struct.TenantManager.html
[`TenantHandle`]: https://docs.rs/aletheiadb/latest/aletheiadb/tenant/struct.TenantHandle.html
[`TenantQuota`]: https://docs.rs/aletheiadb/latest/aletheiadb/core/tenant/struct.TenantQuota.html
[`TenantUsage`]: https://docs.rs/aletheiadb/latest/aletheiadb/core/tenant/struct.TenantUsage.html
