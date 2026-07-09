# HTTP Per-Query Resource Limits (Issue #3368)

The HTTP `/query` surface enforces per-query resource limits so a single
request can neither hold a connection open indefinitely nor force the server to
materialize an unbounded response. Three dimensions are enforceable at the HTTP
layer, each with a **server-level default**, an optional **per-call override**,
and an **operator hard ceiling**.

> Scope note. This guide documents the **HTTP lane** of #3368. Engine-level
> cancellation of in-flight CPU work and a query **memory budget** are separate
> dimensions owned by the query-executor lane; the MCP surface is a separate
> lane. See the [coverage matrix](#coverage-matrix) below for an honest
> breakdown of what is and is not covered here.

## The three dimensions

| Dimension | What it bounds | Over-limit result |
|-----------|----------------|-------------------|
| **Wall-clock timeout** | How long the handler waits for the query before returning to the client | `429 RESOURCE_EXHAUSTED`, `retriable: true` |
| **Result-row cap** | Number of rows/entities in the result | `Truncate` → `200` + `truncated: true`; `Reject` → `413 RESOURCE_EXHAUSTED` |
| **Result-byte cap** | Serialized byte size of the result | `413 RESOURCE_EXHAUSTED`, `retriable: false` |

### Wall-clock timeout — what it does and does not do

The timeout wraps query execution in `tokio::time::timeout`. It bounds the HTTP
**response deadline**: once the budget elapses the client promptly receives a
structured `429`. It does **not** cancel the underlying computation — AletheiaDB
runs synchronous query work on a blocking thread pool, and that work may run to
completion in the background even after the client has been answered. True
engine-level cancellation is tracked in the query-executor lane. In practice the
HTTP timeout protects the client and the connection; pair it with the
request-body-size limit (below) and a reverse-proxy connection cap for
end-to-end protection.

## Configuration

Limits are configured through `ServerConfig::builder().query_limits(...)` with a
`QueryLimitsConfig`:

```rust
use aletheiadb::http::{QueryLimitsConfig, RowOverflowPolicy, ServerConfig};

let config = ServerConfig::builder()
    .query_limits(QueryLimitsConfig {
        enabled: true,
        // Wall-clock timeout: default 30s, ceiling 5min.
        default_timeout_ms: 30_000,
        max_timeout_ms: 300_000,
        // Result rows: default 10k, ceiling 100k.
        default_max_result_rows: 10_000,
        max_result_rows: 100_000,
        // Response bytes: default 8 MiB, ceiling 64 MiB.
        default_max_response_bytes: 8 * 1024 * 1024,
        max_response_bytes: 64 * 1024 * 1024,
        row_overflow: RowOverflowPolicy::Truncate,
    })
    .build();
```

`QueryLimitsConfig::default()` is the above (enabled, generous — chosen so no
pre-#3368 request behavior changes). `QueryLimitsConfig::disabled()` turns off
all enforcement (unlimited on every dimension, per-call overrides ignored) for
trusted embedded deployments and tests.

### Field reference

| Field | Meaning | `0` means |
|-------|---------|-----------|
| `enabled` | Master switch; `false` ⇒ no enforcement | — |
| `default_timeout_ms` | Applied when the request supplies no `timeout_ms` | unlimited |
| `max_timeout_ms` | Ceiling for a per-call `timeout_ms` | no ceiling |
| `default_max_result_rows` | Applied when no `max_result_rows` override | unlimited |
| `max_result_rows` | Ceiling for a per-call row override | no ceiling |
| `default_max_response_bytes` | Applied when no `max_response_bytes` override | unlimited |
| `max_response_bytes` | Ceiling for a per-call byte override | no ceiling |
| `row_overflow` | `Truncate` (cap + flag) or `Reject` (413) when the row cap is hit | — |

## Per-call overrides

A request may carry an optional `limits` object. Every field is optional and
additive — a body with no `limits` key parses and behaves exactly as before:

```json
{
  "operation": "find_node",
  "label": "Person",
  "limits": {
    "timeout_ms": 5000,
    "max_result_rows": 50,
    "max_response_bytes": 262144
  }
}
```

Merge semantics (the effective limit for each dimension):

- **Override present, within ceiling** → the override wins. It may be *smaller*
  than the server default (a caller self-limiting tighter is always allowed).
- **Override present, above the ceiling** (or requesting unlimited `0` under a
  finite ceiling) → **rejected** `422 INVALID_ARGUMENT` before any DB work. An
  explicit over-ceiling request is never silently clamped.
- **Override absent** → the server default, silently clamped down to the ceiling
  if the default itself is unlimited or above the ceiling.

The merge is a single tested function
(`QueryLimitsConfig::effective`), covered per-branch by unit tests in
`src/http/config.rs`.

## Error contract

All limit errors keep the existing `{success:false, error:"…"}` body and add the
`code` / `retriable` / `details` fields (aligning the HTTP surface toward the MCP
`#3234` contract). Existing non-limit error bodies are unchanged.

### `429` — wall-clock timeout

```json
{
  "success": false,
  "error": "query exceeded the wall-clock timeout of 30000 ms",
  "code": "RESOURCE_EXHAUSTED",
  "retriable": true,
  "details": { "dimension": "wall_clock_timeout", "limit_ms": 30000 }
}
```

### `413` — result too large (row `Reject` policy, or byte cap)

```json
{
  "success": false,
  "error": "query response exceeded the byte limit of 1048576 (serialized 2400512)",
  "code": "RESOURCE_EXHAUSTED",
  "retriable": false,
  "details": { "dimension": "result_bytes", "limit": 1048576, "consumed": 2400512 }
}
```

For the row-cap `Reject` policy, `details.dimension` is `"result_rows"` and
`details` carries `limit` + `consumed` (the row count produced).

### `200` — result truncated (row `Truncate` policy, the default)

```json
{
  "success": true,
  "data": [ /* first max_result_rows rows */ ],
  "truncated": true
}
```

`truncated` is present and `true` only when rows were actually dropped; it is
omitted otherwise, so responses that fit under the cap are byte-identical to
pre-#3368 responses.

### `422` — per-call override exceeds the ceiling

```json
{
  "success": false,
  "error": "limit override for 'result_rows' (1000) exceeds the maximum allowed (100)",
  "code": "INVALID_ARGUMENT",
  "retriable": false,
  "details": { "dimension": "result_rows", "requested": 1000, "ceiling": 100 }
}
```

### Ordering: authentication first

Authentication and authorization run **before** any limit logic. An
unauthenticated request that also breaches a limit gets `401 UNAUTHENTICATED`
(never a `422`/`413`), and an authenticated-but-unauthorized request gets
`403 PERMISSION_DENIED`. Limits are then evaluated identically in anonymous and
required-auth modes.

## Related HTTP-layer guard: request body size

The already-merged request **body-size** limit (#3424,
`DefaultBodyLimit` / `ServerConfig::max_request_body_bytes`, default 2 MiB) is
the HTTP-layer *input* memory guard: it rejects an oversized request body with
`413` before it is buffered or deserialized. That bounds the memory a single
*request* can force the server to allocate; the per-query limits here bound the
*response* and the *time*. Together they cover the HTTP layer's input/output/time
axes.

## Coverage matrix

Honest breakdown of #3368 across lanes:

| Dimension | Default | Per-call override | Operator ceiling | Status |
|-----------|:-------:|:-----------------:|:----------------:|--------|
| Wall-clock timeout (HTTP response deadline) | ✅ | ✅ | ✅ | **Covered (this lane)** |
| Result-row cap | ✅ | ✅ | ✅ | **Covered (this lane)** |
| Result-byte cap | ✅ | ✅ | ✅ | **Covered (this lane)** |
| Request body-size (input memory) | ✅ | n/a | ✅ | Covered previously (#3424) |
| Engine-level cancellation of in-flight CPU work | — | — | — | **Deferred** → query-executor lane |
| Query **memory budget** | — | — | — | **Deferred** → query-executor lane |
| Same limits on the **MCP** surface | — | — | — | **Deferred** → MCP lane |
| Rust-API builder ergonomics for limits | partial | — | — | Config type is public; a fluent builder is a follow-up |

The HTTP timeout is a response-deadline bound, not a compute bound — see
[the timeout note](#wall-clock-timeout--what-it-does-and-does-not-do).

## Tests

- `src/http/config.rs` — unit tests for the default/override/ceiling merge
  (all branches), the `disabled()` escape hatch, and dimension tokens.
- `src/http/error.rs` — unit tests for the `429`/`413`/`422` status +
  `{code, retriable, details}` body mapping.
- `src/http/handlers.rs` — unit tests for the enforcement helper: deterministic
  timeout (a sleeping future), row truncate/reject, byte cap, and the
  `QueryEnvelope` flatten (operation + optional `limits`).
- `tests/http_query_limits.rs` — end-to-end integration: row cap
  truncate/reject, byte cap, override within/above ceiling, disabled config,
  concurrency isolation, and auth-before-limits ordering.
