# AletheiaDB Parity Harness

Black-box golden tests that pin the **current, observed** behavior of the two
network surfaces so a future port to another framework must pass them
**unchanged**. If a parity test fails after a port, the port changed observable
behavior — fix the port, not the assertion.

- HTTP suite: [`tests/parity_http.rs`](../parity_http.rs) — gate `--features http-server`
- MCP suite: [`tests/parity_mcp.rs`](../parity_mcp.rs) — gate `--features mcp-server`
- Machine-readable inventory: [`inventory.json`](./inventory.json)

Run both:

```bash
cargo test --features http-server,mcp-server --test parity_http --test parity_mcp
```

## HTTP surface — 5 routes

| Method | Path | Access | Success | Error statuses | Pinned by |
|--------|------|--------|---------|----------------|-----------|
| GET  | `/status` | metrics | 200 `{"status":"healthy"}` | 401 | `status_*` |
| POST | `/query` | per-operation | 200 `{success,data?,truncated?}` | 400/401/403/404/413/422/429/500 | `query_*`, `run_server_enforces_request_body_413` |
| POST | `/admin/keys` | admin | 200 `{data:{key,id,role}}` | 400/401/403 | `admin_create_key_*` |
| GET  | `/admin/keys` | admin | 200 masked list | 401/403 | `admin_list_keys_*` |
| POST | `/admin/keys/revoke` | admin | 200 `{data:{revoked,id}}` | 401/403/404 | `admin_revoke_*` |

**Error envelope:** `{success:false, error, code?, retriable?, details?, trace_id?}`.
`code`/`retriable`/`details` are additive — present only on auth (401/403) and
resource-limit (413/422/429) variants. Plain 400/404/500 keep the minimal
`{success,error}` shape. HTTP limit errors use `RESOURCE_EXHAUSTED`; the uniform
401 carries `WWW-Authenticate: Bearer` and a byte-identical body across every
route.

## MCP surface — 44 tools

Driven black-box through the public per-tool typed methods
(`server.get_node(GetNodeRequest{..}) -> String`) and the public `McpErrorCode`
enum. Access classes: 31 read, 12 write, 1 metrics (`database_stats`), 0 admin.

**Two kinds of MCP pin.** Four tools are **behavior-pinned** — `get_node`,
`create_node`, `create_edge`, `traverse` carry black-box *runtime* assertions
(envelopes, temporal block, vector elision, roundtrip reachability). The other
40 are **inventory-pinned** — only their name + access class are fixed via the
golden set. Live drift detection of the whole 44-name/class registry (a tool
added/removed/renamed/reclassified) is done in-crate by
`src/mcp/auth_tests.rs::live_tool_inventory_matches_golden`, which reads the
live registry and fails on drift. The external
`tool_inventory_golden_is_stable` is only a cross-crate mirror of that constant
(internal-consistency only — it cannot read the registry).

**Error envelope:** `{error:{code, message, retriable, details?}}` with `code` in
the 9-value `McpErrorCode` enum (`NOT_FOUND`, `INVALID_ARGUMENT`,
`CONSTRAINT_VIOLATION`, `FAILED_PRECONDITION`, `CONFLICT`, `UNAVAILABLE`,
`INTERNAL`, `UNAUTHENTICATED`, `PERMISSION_DENIED`); `retriable` is `true` only
for `CONFLICT`/`UNAVAILABLE`. Read responses carry a `temporal` block; vector
properties are elided to `{type,dim,elided:true}` unless `include_vectors:true`.

**13 budgetable read tools:** `get_node`, `list_nodes`, `get_edge`, `list_edges`,
`get_outgoing_edges`, `get_incoming_edges`, `traverse`, `find_similar`,
`hybrid_query`, `query`, `find_nodes_at_time`, `get_node_history`, `get_schema`.

## Key parity asymmetries (HTTP vs MCP)

1. **Code vocabulary differs:** HTTP limit errors are `RESOURCE_EXHAUSTED`; MCP
   has no such code. HTTP emits `code` only on auth+limit variants; MCP emits
   `code` on every error.
2. **Auth transport:** HTTP is per-request header (`Bearer`/`x-api-key`); MCP is
   a session-scoped credential re-verified per call.
3. **Admin surface:** HTTP has admin-class routes (`/admin/keys*`); MCP has no
   admin tools.
4. **Result model:** HTTP maps to status codes (200/4xx/5xx); MCP is a flat
   `is_error` + JSON `code`.
5. **Anonymous default:** `AletheiaMcpServer::new` is anonymous; HTTP derives the
   mode from config (default required, no startup refusal in `build_test_router`).

## Coverage gaps (honest, no silent skips)

Per-tool RUNTIME BEHAVIOR for the 40 non-behavior-tested tools, RBAC
enforcement, the token-budget ladder, and cursor paging are **not reachable**
through the public black-box API (`dispatch_tool` / `list_tools_for_test` /
`apply_budget` / `authorize_tool` are `pub(crate)`; the `rmcp::ServerHandler`
entry is not nameable from an external test crate, and the public per-tool
methods bypass the dispatch-path auth/budget/cursor logic). Per the harness
house rules we do not add `pub` to production code. These are pinned by the
existing in-crate suites and only referenced here:

- Registry name/class **drift** → **now closed in-crate** by
  `src/mcp/auth_tests.rs::live_tool_inventory_matches_golden` (reads the live
  registry). What remains external-unreachable is per-tool runtime behavior for
  the 40 inventory-pinned tools.
- Registry dispatch sweep → `src/mcp/tests.rs::every_advertised_tool_returns_structured_error_on_invalid_arguments`
- RBAC matrix → `src/mcp/auth_tests.rs::every_advertised_tool_has_a_classification`
- Token budget → `src/mcp/tests.rs` budget suite
- HTTP 429 timeout → `src/http/handlers.rs` `enforce_query_limits` unit tests
- HTTP OTel `trace_id`/`x-trace-id` → `tests/http_otel_tracing.rs` (observability/otel features)
- HTTP **500 / INTERNAL envelope shape** → asserted only **indirectly** (as
  "not 500" on the malformed path, `query_malformed_payload_is_400_not_500`);
  the Internal-variant body shape is covered by the `src/http/error.rs`
  `IntoResponse` unit tests, not positively pinned over the wire.

The 74-tool inventory + access classes are mirrored as a golden constant
(`tool_inventory_golden_is_stable`, cross-crate mirror) and drift-checked live
in-crate (`live_tool_inventory_matches_golden`); a porter must keep both in
lockstep with the server. See `inventory.json` for the full per-route/per-tool
table and the `coverage_gaps` array.
