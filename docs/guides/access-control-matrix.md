# Access Control Matrix

Roles, access classes, and the per-operation classification for AletheiaDB's
serving surfaces (Issue #3350). This file is the **canonical documented
matrix**: the conformance test
`src/mcp/auth_tests.rs::documented_matrix_matches_code` mechanically compares
the MCP tool table below against the code
(`src/mcp/auth.rs::TOOL_ACCESS_CLASSES`) — when adding, renaming, or
reclassifying a tool, update both together or CI fails. The HTTP
classification is enforced by the exhaustive match in
`src/http/handlers.rs::query_access_class` plus the route-inventory
conformance test in the same file.

The full security quickstart (key lifecycle, bootstrap flow, deployment
guidance) lives in
[docs/guides/security-quickstart.md](security-quickstart.md); this document
is the matrix it is built around.

## Roles × access classes

Defined by `Role::allows(AccessClass)` in `src/auth/role.rs`.

| Role      | read | write | metrics | admin |
|-----------|------|-------|---------|-------|
| `admin`   | ✅   | ✅    | ✅      | ✅    |
| `writer`  | ✅   | ✅    | ✅      | ❌    |
| `reader`  | ✅   | ❌    | ✅      | ❌    |
| `metrics` | ❌   | ❌    | ✅      | ❌    |

## Error contract

> **Breaking change (HTTP error-envelope unification, Issue #3234):** the HTTP
> error body is now the **same nested `{"error":{"code","message","retriable",
> "details"?}}` envelope** the MCP surface emits, with `trace_id` (when present)
> as a top-level sibling of `error`. The legacy flat body
> (`{"success":false,"error":"<msg>","code":…}`) — the top-level `success` and
> the flat `error` string — has been **removed**. Read `error.code` /
> `error.message` / `error.retriable` / `error.details` instead of the old
> top-level fields.

- **Unauthenticated** (missing/unknown/revoked credential, `required` mode):
  - HTTP: `401`, body `{"error":{"code":"UNAUTHENTICATED","message":"authentication required","retriable":false}}` — byte-identical regardless of why authentication failed (no key-existence oracle).
  - MCP: structured error `{"error":{"code":"UNAUTHENTICATED","message":"authentication required","retriable":false}}` — uniform for **every** tool, including unknown tool names (the tool inventory is not revealed to unauthenticated callers). Never echoes the presented credential.
- **Permission denied** (authenticated, role does not allow the class):
  - HTTP: `403`, body `{"error":{"code":"PERMISSION_DENIED","message":"role '<role>' does not permit <class> access","retriable":false,"details":{"required_class":"<class>","principal_role":"<role>"}}}`.
  - MCP: `{"error":{"code":"PERMISSION_DENIED","message":"role '<role>' does not permit <class> access","retriable":false,"details":{"required_class":"<class>","principal_role":"<role>"}}}` — now identical to the HTTP body.

Both codes are additive to the #3234 enum and are never retriable: obtain a
valid credential / a sufficient role, then re-issue.

- **Resource limit exceeded** (per-query limits, HTTP `/query`, Issue #3368):
  authentication and authorization run **first**, so these are only reachable
  by an already-authorized caller. All fields below live under the nested
  `error` object (e.g. `error.code`, `error.details.dimension`).
  - HTTP `429`, `error.code:"RESOURCE_EXHAUSTED"`, `error.retriable:true`,
    `error.details:{dimension:"wall_clock_timeout", limit_ms}` — wall-clock timeout.
  - HTTP `413`, `error.code:"RESOURCE_EXHAUSTED"`, `error.retriable:false`,
    `error.details:{dimension:"result_rows"|"result_bytes", limit, consumed}` — result
    too large (row `Reject` policy / byte cap).
  - HTTP `422`, `error.code:"INVALID_ARGUMENT"`, `error.retriable:false`,
    `error.details:{dimension, requested, ceiling}` — a per-call `limits` override
    exceeded the operator ceiling.
  - See [HTTP Per-Query Resource Limits](http-query-limits.md) for the full
    contract. (MCP parity is deferred to the MCP lane.)

## MCP tools

The MCP transport is stdio, so the credential is session-scoped (supplied at
process start via `ALETHEIADB_MCP_API_KEY`) and re-verified on every tool
call — revoking the key takes effect on the next call. Enforcement happens at
the single dispatch point before any tool executes.

The `admin`-class MCP tools are the GDPR crypto-shred surface
(`designate_subject` / `erase_subject`, Issue #3359) — erasure destroys
per-subject key material, an irreversible privileged operation. Key lifecycle
(create/list/revoke) is **not** an MCP tool: it is served by the HTTP admin
endpoints over the shared persisted store (`{data_dir}/auth/keys.json`).

<!-- mcp-tool-matrix:start -->

| Tool | Class |
|------|-------|
| `get_node` | read |
| `list_nodes` | read |
| `count_nodes` | read |
| `get_edge` | read |
| `list_edges` | read |
| `count_edges` | read |
| `get_outgoing_edges` | read |
| `get_incoming_edges` | read |
| `traverse` | read |
| `find_similar` | read |
| `embed_query` | read |
| `embed_text` | read |
| `semantic_search` | read |
| `semantic_path` | read |
| `concept_analogy` | read |
| `concept_mean` | read |
| `find_duplicate_candidates` | read |
| `semantic_horizon` | read |
| `context_aspects` | read |
| `list_vector_indexes` | read |
| `list_unique_constraints` | read |
| `get_node_at_time` | read |
| `get_edge_at_time` | read |
| `find_nodes_at_time` | read |
| `list_changes` | read |
| `await_changes` | read |
| `get_node_at_valid_time` | read |
| `get_node_at_transaction_time` | read |
| `get_node_history` | read |
| `diff_node_versions` | read |
| `get_edge_at_valid_time` | read |
| `get_edge_at_transaction_time` | read |
| `get_edge_history` | read |
| `diff_edge_versions` | read |
| `hybrid_query` | read |
| `query` | read |
| `get_schema` | read |
| `temporal_extent` | read |
| `lineage_upstream` | read |
| `lineage_downstream` | read |
| `audit_export` | read |
| `verify_chain` | read |
| `export_chain_head` | read |
| `list_namespaces` | read |
| `describe_namespace` | read |
| `database_stats` | metrics |
| `create_node` | write |
| `update_node` | write |
| `delete_node` | write |
| `delete_node_cascade` | write |
| `retract_node` | write |
| `create_edge` | write |
| `update_edge` | write |
| `delete_edge` | write |
| `retract_edge` | write |
| `apply_batch` | write |
| `enable_vector_index` | write |
| `enable_unique_constraint` | write |
| `create_node_with_embedding` | write |
| `update_node_embedding` | write |
| `create_namespace` | write |
| `designate_subject` | admin |
| `erase_subject` | admin |

<!-- mcp-tool-matrix:end -->

Note: `query` is classified `read` because the tool is read-only **by
contract** — mutating clauses (CREATE/MERGE/SET/DELETE/…) are rejected by the
shared guard (`src/query/read_only.rs`) before execution and never write.

Note: `database_stats` stays `metrics`-class (any monitoring credential may read
the holistic snapshot), but one **field is admin-gated** (Issue #3678): the
changefeed **`per_principal` identity breakdown** — the roster of *other*
principals' ids and live subscription counts — is included **only for an `admin`
caller**. A `metrics`/`reader`/`writer` caller receives every scalar aggregate
(including `changefeed.active_subscriptions`) but the `per_principal` key is
omitted entirely, so a low-privilege credential cannot enumerate who is
currently subscribed. Both surfaces enforce this: the MCP handler derives
admin-ness from the session principal, the HTTP `GET /database_stats` route from
its own authenticated principal. Conformance:
`crates/aletheia-server/tests/changefeed_principal_quota_surface.rs::database_stats_per_principal_breakdown_is_admin_gated`.

## HTTP surface

The HTTP credential is per-request (`Authorization: Bearer <key>` or
`x-api-key`).

| Route / operation | Class |
|-------------------|-------|
| `GET /status` | metrics |
| `POST /query` — `find_node`, `get_node`, `bulk_get_nodes`, `find_neighbors` | read |
| `POST /query` — `create_node`, `bulk_create_nodes`, `bulk_update_nodes`, `bulk_delete_nodes` | write |
| `POST /query` — `execute_query` | read if the statement passes the read-only guard, else write |
| `POST /query` — `bulk_execute_query` | read only if **every** statement passes the read-only guard, else write |
| `POST /admin/keys` (create key) | admin |
| `GET /admin/keys` (list keys, masked) | admin |
| `POST /admin/keys/revoke` | admin |
| `GET /changes/stream` (SSE changefeed stream) | read |
| `POST /changes/await` (`await_changes` long-poll) | read |

Note: `GET /changes/stream` is a **route-only** Server-Sent Events surface
(Issue #3375) — it is served over HTTP + OpenAPI but is deliberately **not** an
MCP tool (like `GET /metrics`). Its long-poll MCP projection is the
`await_changes` tool (also served at `POST /changes/await`). Both are `read`
class.

## Framework endpoints outside this matrix (HTTP)

autumn-web (the HTTP framework) mounts its own routes **outside**
AletheiaDB's authentication layer; they answer without any AletheiaDB
credential and are not part of the role matrix above:

| Framework route | Exposure |
|-----------------|----------|
| `GET /health`, `/live`, `/ready`, `/startup` | liveness/readiness probes |
| `GET /actuator/health`, `/actuator/info`, `/actuator/metrics`, `/actuator/a11y`, `/actuator/ui`, `/actuator/ui/metrics` | service health, framework version/profile, request metrics |

The framework's *sensitive* actuator group (`/actuator/env`,
`/actuator/configprops`, `PUT /actuator/loggers/{name}`,
`/actuator/tasks`, `/actuator/jobs`, `/actuator/prometheus`) is
force-disabled by AletheiaDB in every profile (`HardenedConfigLoader`
in `src/http/server.rs`). Block `/actuator` and the probe paths at your
reverse proxy if they must not be publicly reachable — see the
[security quickstart](security-quickstart.md#framework-endpoints-that-bypass-api-key-auth-http-server).
