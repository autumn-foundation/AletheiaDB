# Namespaces: a shared knowledge base with private agent scratch (Issue #3349)

A **namespace** is a stable string label on the *ownership / visibility* axis of
every node and edge — orthogonal to the type `label` axis. A node's `label` says
*what kind of thing* it is (`Person`); its `namespace` says *whose scope it lives
in* (`agent:planner`). Every entity belongs to exactly one namespace, fixed at
creation and **immutable for the life of the entity**.

Namespaces exist so several agents (or sessions, or tenants) can share one
database while each keeps a private working set:

- **Isolated by default** — a read that names no scope sees only the `default`
  namespace. An agent scoped to its own namespace cannot even tell that another
  agent's entities exist (out-of-scope entities read back as `NOT_FOUND`,
  indistinguishable from missing).
- **Shared by explicit query** — a read can widen its scope to a *union* of
  namespaces (e.g. "my scratch **plus** the shared KB") or to `all`.

> **Namespaces are a data-scoping axis, not a security boundary.** They partition
> *visibility* for well-behaved callers; they do **not** authenticate or authorize.
> For access control use the RBAC roles in
> [security-quickstart.md](security-quickstart.md). A caller that passes
> `"all"` (or is allowed to choose its own scope) sees everything.

This guide is the worked "shared knowledge base + private agent scratch" pattern.
It gives the MCP-tool, Rust-API, and AQL forms side by side; all three are
accurate against the merged implementation.

> **AQL availability.** The in-statement `USE NAMESPACE` clause (shown below)
> lands with Issue #3736. The MCP `query` tool's `namespace` scope parameter and
> every Rust/MCP API in this guide are already merged.

---

## The naming rules in one place

| Rule | Detail |
|------|--------|
| Charset | `[A-Za-z0-9._:/-]`, case-sensitive, non-empty, at most 128 bytes |
| Implicit namespace | `default` — every legacy/omitted-namespace entity lives here and carries no marker (byte-identical to pre-namespace data) |
| Reserved selector | `all` is a **read-scope selector**, not a creatable name |
| Reserved prefixes | `__aletheia_*` and `__shred_*` are engine-owned; rejected as namespace names *and* as user property keys (`INVALID_ARGUMENT`) |
| Immutability | A namespace is set at creation and can never change; supplying one to an update/delete is `INVALID_ARGUMENT`, never a silent no-op |
| Registration | Writing to an unknown namespace **auto-registers** it; `create_namespace` up front is optional (it just makes an empty namespace listable) |

---

## The walkthrough

Scenario: a planner agent and a researcher agent collaborate through a shared
knowledge base. Each agent has a private scratch namespace; facts everyone can
rely on live in `shared`.

- `shared` — the common knowledge base
- `agent:planner` — the planner's private scratch
- `agent:researcher` — the researcher's private scratch

### 1. Create the namespaces

Creation is optional (any write auto-registers its namespace), but doing it up
front gives each one a description and makes an empty namespace immediately
listable — which helps catch a typo like `agnet:planner`.

**MCP** (`create_namespace`, `writer`-class):

```json
{ "name": "shared", "description": "Common knowledge base" }
```

Response data:

```json
{ "name": "shared", "description": "Common knowledge base", "created_at": "2026-07-19T12:00:00Z" }
```

Repeat for `agent:planner` and `agent:researcher`.

**Rust:**

```rust
use aletheiadb::AletheiaDB;

let db = AletheiaDB::new()?;
db.create_namespace("shared", Some("Common knowledge base".into()))?;
db.create_namespace("agent:planner", Some("Planner scratch".into()))?;
db.create_namespace("agent:researcher", Some("Researcher scratch".into()))?;
```

### 2. Write, stamping the namespace

The namespace is stamped onto the entity at creation. It is surfaced back as a
first-class `namespace` field on reads (`None`/absent means `default`).

**MCP** (`create_node`, `namespace` write parameter):

```json
{ "label": "Fact", "properties": { "text": "Rust has no GC" }, "namespace": "shared" }
```

```json
{ "label": "Note", "properties": { "text": "draft plan v1" }, "namespace": "agent:planner" }
```

**Rust** (`create_node_in_namespace` / `create_edge_in_namespace`):

```rust
use aletheiadb::PropertyMapBuilder;

let shared_fact = db.create_node_in_namespace(
    "Fact",
    PropertyMapBuilder::new().insert("text", "Rust has no GC").build(),
    "shared",
)?;

let planner_note = db.create_node_in_namespace(
    "Note",
    PropertyMapBuilder::new().insert("text", "draft plan v1").build(),
    "agent:planner",
)?;

// A cross-namespace edge is legal — the edge's OWN namespace governs whether a
// scoped traversal may cross it. Here the planner records, in its own scratch,
// that its note derives from a shared fact.
db.create_edge_in_namespace(
    planner_note,
    shared_fact,
    "DERIVED_FROM",
    PropertyMapBuilder::new().build(),
    "agent:planner",
)?;
```

### 3. A scoped read sees only its own scratch (isolated by default)

The planner reads its own scratch. An omitted scope resolves to the `default`
namespace only, so the planner passes its own namespace explicitly.

**MCP** (`list_nodes` with a single-namespace scope — the `namespace` parameter
accepts a string, an array, or `"all"`):

```json
{ "label": "Note", "namespace": "agent:planner" }
```

Only the planner's notes come back. The researcher's scratch is invisible.

**Rust** (`list_nodes_scoped` / `get_node_scoped` take a `&NamespaceScope`):

```rust
use aletheiadb::core::namespace::{Namespace, NamespaceScope};

let planner = NamespaceScope::single(Namespace::new("agent:planner")?);

// Sees only agent:planner entities.
let planner_notes = db.list_nodes_scoped(Some("Note"), &planner)?;

// The planner cannot fetch a researcher-owned node: out of scope ⇒ NOT_FOUND
// (indistinguishable from missing — the planner never learns it exists).
assert!(db.get_node_scoped(researcher_note, &planner).is_err());
```

### 4. An explicit UNION read pulls private scratch + shared KB together

To reason over its own scratch *and* the shared KB, the planner widens its scope
to the union `["agent:planner", "shared"]`.

**MCP** (array scope = union):

```json
{ "label": "Fact", "namespace": ["agent:planner", "shared"] }
```

**Rust** (`NamespaceScope::list`):

```rust
let planner_plus_shared = NamespaceScope::list(vec![
    Namespace::new("agent:planner")?,
    Namespace::new("shared")?,
])?;

// Traversal honors the same union: from the planner's note, follow DERIVED_FROM
// into the shared fact. The hop is crossed only because BOTH the edge's own
// namespace (agent:planner) AND the target node's namespace (shared) are in scope.
let reachable = db.traverse_scoped(
    planner_note,
    Some("DERIVED_FROM"),
    /* max_depth */ 2,
    &planner_plus_shared,
)?;
assert!(reachable.contains(&shared_fact));
```

**AQL** (lands with #3736) — the `USE NAMESPACE` prefix clause carries the union:

```sql
USE NAMESPACE 'agent:planner', 'shared'
MATCH (n:Note)-[:DERIVED_FROM]->(f:Fact)
RETURN f
```

The MCP `query` tool scopes the *same* statement today via its `namespace`
parameter (a narrowing scope, `"all"`, or omitted = `default`-only):

```json
{
  "language": "aql",
  "query": "MATCH (n:Note)-[:DERIVED_FROM]->(f:Fact) RETURN f",
  "namespace": ["agent:planner", "shared"]
}
```

### 5. The researcher cannot see the planner's scratch

The researcher scopes to its own namespace (plus, if it likes, `shared`). The
planner's `Note` and the planner-owned `DERIVED_FROM` edge are outside that scope
and never appear:

```rust
let researcher = NamespaceScope::single(Namespace::new("agent:researcher")?);

// The planner's note is not in the researcher's scope ⇒ NOT_FOUND.
assert!(db.get_node_scoped(planner_note, &researcher).is_err());

// A researcher union over its scratch + shared sees shared facts but still not
// the planner's private note.
let researcher_plus_shared = NamespaceScope::list(vec![
    Namespace::new("agent:researcher")?,
    Namespace::new("shared")?,
])?;
let visible = db.list_nodes_scoped(None, &researcher_plus_shared)?;
assert!(visible.contains(&shared_fact));
assert!(!visible.contains(&planner_note));
```

---

## Isolation guarantees

| Guarantee | Behavior |
|-----------|----------|
| **Default-only when omitted** | A read with no scope resolves to `Single(default)` — isolated-by-default. For a pre-namespace database (all data is `default`) this still returns everything. |
| **`all` for operators** | Scope `"all"` (Rust `NamespaceScope::all()`, AQL `USE ALL NAMESPACES`) imposes no filter — every namespace, identical to the unscoped fast path. |
| **Traversal boundary** | From an in-scope node, an edge is crossed only when the **edge's own namespace ∈ scope AND the far node's namespace ∈ scope**. A scope can never leak across the boundary or transit through an out-of-scope node — in any direction (`traverse_scoped_directed` with `Outgoing` / `Incoming` / `Both`). |
| **Vector filter-completeness** | `find_similar_scoped` / `find_similar_by_embedding_scoped` **over-fetch** until they have `k` genuinely in-scope results (never "take `k`, then drop out-of-scope"), with scores and ordering identical to an unscoped search. |
| **Changefeed scoping** | A subscription scoped to a namespace never receives another namespace's changes (see below). |
| **Unknown namespace** | A scope naming an unregistered namespace is `NOT_FOUND` (`details.namespace`). An **empty array** scope is `INVALID_ARGUMENT` (a scope that silently matches nothing is forbidden). |
| **Immutability** | A namespace cannot change after creation. Supplying one to an update/delete tool is `INVALID_ARGUMENT`. |
| **Reserved names** | `all`, and any `__aletheia_*` / `__shred_*` name, are rejected at construction (`INVALID_ARGUMENT`). |

### Changefeed scoping

The push changefeed ([reacting-to-change.md](reacting-to-change.md)) filters by
namespace exactly like a scoped read:

```rust
use aletheiadb::ChangeFilter;

let filter = ChangeFilter::all()
    .with_namespace_scope(NamespaceScope::single(Namespace::new("shared")?));
let sub = db.subscribe_changes(filter)?;
```

The MCP `await_changes` long-poll takes the same `namespace` scope parameter
(string | array | `"all"`; omitted = `default`-only):

```json
{ "namespace": "shared", "timeout_ms": 25000 }
```

---

## Per-namespace observability

Both `database_stats` and `get_schema` surface a `namespaces` array of
`{ name, node_count, edge_count }` entries (one per registered-or-populated
namespace, O(1) membership-index reads). `list_namespaces` / `describe_namespace`
add the registry metadata.

**MCP** `list_namespaces` (no arguments) response data:

```json
{
  "namespaces": [
    { "name": "default",          "description": "The implicit default namespace", "created_at": "1970-01-01T00:00:00Z", "node_count": 0, "edge_count": 0 },
    { "name": "shared",           "description": "Common knowledge base",          "created_at": "2026-07-19T12:00:00Z", "node_count": 1, "edge_count": 0 },
    { "name": "agent:planner",    "description": "Planner scratch",                "created_at": "2026-07-19T12:00:01Z", "node_count": 1, "edge_count": 1 }
  ],
  "count": 3
}
```

**MCP** `describe_namespace` (`{ "name": "agent:planner" }`) returns that single
entry with its `node_count` / `edge_count`.

**Rust** (`namespace_counts` returns `Vec<NamespaceCount>`; `list_namespaces`
returns `Vec<NamespaceInfo>`):

```rust
for c in db.namespace_counts() {
    println!("{}: {} nodes, {} edges", c.name, c.node_count, c.edge_count);
}
```

`list_namespaces` lists the implicit `default` first, then the rest in creation
order — use it to catch an accidentally auto-registered typo namespace.

---

## Migration and back-compat

There is **nothing to migrate**. A pre-namespace (single-agent) database is
already entirely in the `default` namespace: `default` entities carry no marker,
so they are byte-identical to legacy data through WAL replay, anchor/delta
reconstruction, cold-tier migration, and `.albk` backup.

- Existing code that never mentions a namespace keeps working unchanged — writes
  land in `default`, and reads that omit a scope see `default`.
- Adopt namespaces incrementally: start writing new agents' data into
  `agent:*` / `session:*` namespaces while old code continues to read `default`.

---

## API reference

**Namespace management (Rust `AletheiaDB`):** `create_namespace(name, Option<String>)`,
`list_namespaces()`, `get_namespace(name)` / `describe_namespace(name)`,
`delete_namespace(name)` (unregisters the name only; does not touch entities).

**Namespaced writes:** `create_node_in_namespace(label, PropertyMap, namespace)`,
`create_edge_in_namespace(source, target, label, PropertyMap, namespace)`.

**Scoped reads (all take `&NamespaceScope`):** `get_node_scoped`,
`get_edge_scoped`, `list_nodes_scoped(Option<&str>, ..)`,
`find_nodes_by_property_scoped`, `traverse_scoped(start, Option<&str>, max_depth, ..)`,
`traverse_scoped_directed(.., TraverseDirection, ..)`, `find_similar_scoped`,
`find_similar_by_embedding_scoped`, and the point-in-time variants
`get_node_at_time_scoped`, `get_edge_at_time_scoped`, `find_nodes_at_time_scoped`,
`find_nodes_by_property_at_scoped`, `traverse_scoped_as_of` /
`traverse_scoped_as_of_directed`. `namespace_counts()` returns per-namespace
counts.

**`NamespaceScope`:** `NamespaceScope::single(ns)`, `NamespaceScope::list(vec)`
(non-empty — empty is `INVALID_ARGUMENT`), `NamespaceScope::all()`;
`Default` is `Single(default)`.

**Query builder:** `QueryBuilder::in_namespace(ns)`, `in_namespaces(iter)`,
`in_all_namespaces()`.

**MCP tools** carrying a `namespace` write parameter: `create_node`,
`create_edge`. Carrying a `namespace` **read scope** (string | array | `"all"`;
omitted = `default`-only): `get_node`, `get_edge`, `list_nodes`, `traverse`,
`find_similar`, `find_nodes_at_time`, `query`, `hybrid_query`, `await_changes`.
Management tools: `create_namespace`, `list_namespaces`, `describe_namespace`.
Per-namespace counts appear in `database_stats` and `get_schema`. (`list_edges`
does not enumerate edges by namespace in v1; a narrowing scope there is
`INVALID_ARGUMENT` — use the scoped adjacency/traversal reads.)

**AQL** (lands with #3736): `USE NAMESPACE '<name>'` (single),
`USE NAMESPACE '<a>', '<b>'` (union), `USE ALL NAMESPACES` (no filter); `IN` is a
synonym for `USE`. The clause is a prefix and composes with `AS OF` in either
order.

## See also

- [reacting-to-change.md](reacting-to-change.md) — the changefeed the namespace scope filters
- [mcp-query-tool.md](mcp-query-tool.md) — the `query` tool and structured error codes
- [security-quickstart.md](security-quickstart.md) — RBAC (the real security boundary)
- [snapshot-pin.md](snapshot-pin.md) — reproducible reads, another read-scoping axis
