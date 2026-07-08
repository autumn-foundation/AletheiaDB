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

- **Unauthenticated** (missing/unknown/revoked credential, `required` mode):
  - HTTP: `401`, body `{"success":false,"error":"authentication required","code":"UNAUTHENTICATED"}` — byte-identical regardless of why authentication failed (no key-existence oracle).
  - MCP: structured error `{"error":{"code":"UNAUTHENTICATED","message":"authentication required","retriable":false}}` — uniform for **every** tool, including unknown tool names (the tool inventory is not revealed to unauthenticated callers). Never echoes the presented credential.
- **Permission denied** (authenticated, role does not allow the class):
  - HTTP: `403`, body `{"success":false,"error":"role '<role>' does not permit <class> access","code":"PERMISSION_DENIED"}`.
  - MCP: `{"error":{"code":"PERMISSION_DENIED","message":"role '<role>' does not permit <class> access","retriable":false,"details":{"required_class":"<class>","principal_role":"<role>"}}}`.

Both codes are additive to the #3234 enum and are never retriable: obtain a
valid credential / a sufficient role, then re-issue.

## MCP tools

The MCP transport is stdio, so the credential is session-scoped (supplied at
process start via `ALETHEIADB_MCP_API_KEY`) and re-verified on every tool
call — revoking the key takes effect on the next call. Enforcement happens at
the single dispatch point before any tool executes.

There are no `admin`-class MCP tools yet: key lifecycle (create/list/revoke)
is served by the HTTP admin endpoints over the shared persisted store
(`{data_dir}/auth/keys.json`).

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
| `list_vector_indexes` | read |
| `list_unique_constraints` | read |
| `get_node_at_time` | read |
| `get_edge_at_time` | read |
| `find_nodes_at_time` | read |
| `list_changes` | read |
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
| `enable_vector_index` | write |
| `enable_unique_constraint` | write |

<!-- mcp-tool-matrix:end -->

Note: `query` is classified `read` because the tool is read-only **by
contract** — mutating clauses (CREATE/MERGE/SET/DELETE/…) are rejected by the
shared guard (`src/query/read_only.rs`) before execution and never write.

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
