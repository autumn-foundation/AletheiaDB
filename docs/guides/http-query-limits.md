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
| **Wall-clock timeout** | How long the handler waits for the query before returning to the client | `429 RESOURCE_EXHAUSTED` + `Retry-After: 1`; `retriable: true` for reads, `retriable: false` for writes |
| **Result-row cap** | Number of rows/entities in the result (**reads only**) | `Truncate` → `200` + `truncated: true`; `Reject` → `413 RESOURCE_EXHAUSTED` |
| **Result-byte cap** | Serialized byte size of the response envelope (**reads only**) | `413 RESOURCE_EXHAUSTED`, `retriable: false` |

### Write operations are exempt from the result caps

The **result-row cap** and **result-byte cap** apply to **read-class**
operations only. A write (`create_node`, `bulk_create_nodes`,
`bulk_update_nodes`, `bulk_delete_nodes`, and mutating `execute_query` /
`bulk_execute_query` statements) has already committed by the time its
acknowledgement is shaped, so truncating or `413`-ing that acknowledgement would
misrepresent durability — the write took effect but the caller would read a
failure. Writes therefore always return their full acknowledgement (ids /
version ids), never flagged `truncated` and never `413`'d for size.

The **wall-clock timeout still applies to writes** (it bounds the client's
wait), but a write timeout is reported **`retriable: false`**. The synchronous
write may already have committed on the blocking pool even though the client's
response deadline elapsed; a naive retry would duplicate it. A read timeout, by
contrast, is `retriable: true` — re-running a read is safe. In both cases the
`429` carries a `Retry-After: 1` header; for the write case the flag tells a
disciplined client to reconcile (re-read) rather than blindly retry.

The byte cap is measured on the **fully-serialized response envelope** (the
`{success, data, truncated?}` wire body), not the inner `data`, so
`details.consumed` reports the real number of bytes that would have gone on the
wire. The response is serialized exactly once — there is no double-serialization
tax on the happy path.

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

Every timeout `429` carries a `Retry-After: 1` header (a conservative one-second
back-off hint). For a **read** the timeout is `retriable: true` and a client may
back off and retry directly. For a **write** it is `retriable: false` (the write
may already have committed — see [above](#write-operations-are-exempt-from-the-result-caps));
a client should reconcile by re-reading rather than blindly re-issuing the write.

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

> ⚠️ **`ceiling == 0` footgun.** A ceiling of `0` means **"no ceiling"**, not
> "zero". With a ceiling of `0`, a per-call override may set that dimension to
> **any** value — including `0` (unlimited) — overriding whatever finite default
> you configured. For example, `default_timeout_ms: 30_000` with
> `max_timeout_ms: 0` lets any caller send `"limits": {"timeout_ms": 0}` and run
> with **no** timeout at all. If you rely on a finite default as a real bound,
> set an explicit **non-zero** ceiling for that dimension so over-ceiling and
> unlimited (`0`) overrides are rejected `422`. Leave a ceiling at `0` only for
> dimensions you deliberately allow callers to uncap. `QueryLimitsConfig::default()`
> ships finite ceilings on every dimension for exactly this reason.

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

Carries a `Retry-After: 1` response header. `retriable` is `true` for a read
timeout and `false` for a write timeout (a committed write must not be
duplicated — see [Write operations are exempt](#write-operations-are-exempt-from-the-result-caps)).

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

### Structural parameter/embedding width cap

Complementing the body-size limit, the `/query` parameter path applies a
*structural* width cap (#3426): a numeric parameter array (an embedding) is
capped at `MAX_VECTOR_DIMENSIONS` (**100,000**) elements, and an over-cap array
is rejected with HTTP `400` / `INVALID_ARGUMENT` when the parameters are
converted (`json_to_parameter_value`). This mirrors the property path's
structural vector-dimension bound and bounds the converted embedding
allocation, so a future operator raising `max_request_body_bytes` cannot
silently reopen a structural amplification vector.

The two limits are complementary, not substitutes: `max_request_body_bytes`
remains the **aggregate** input-memory bound for a request — total across all
parameters, the number of parameters, and the length of any string parameter —
whereas this per-array cap is a structural bound on a single embedding's width.
Because the width check runs *after* serde has materialized the request body,
peak parse-time memory is still governed by the body-size limit, not by this
cap.

## Coverage matrix

Honest breakdown of #3368 across lanes:

| Dimension | Default | Per-call override | Operator ceiling | Status |
|-----------|:-------:|:-----------------:|:----------------:|--------|
| Wall-clock timeout (HTTP response deadline) | ✅ | ✅ | ✅ | **Covered (this lane)** — all ops; write timeout `retriable: false` |
| Result-row cap | ✅ | ✅ | ✅ | **Covered (this lane)** — reads only (writes exempt) |
| Result-byte cap (measured on the response envelope) | ✅ | ✅ | ✅ | **Covered (this lane)** — reads only (writes exempt) |
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
- `src/http/handlers.rs` — unit tests for the enforcement helper (deterministic
  timeout under paused tokio time; read timeout `retriable: true` vs write
  timeout `retriable: false`; row truncate/reject; write exemption from the row
  cap; the envelope-serialization byte-cap step; and the `QueryEnvelope` flatten
  of operation + optional `limits`).
- `src/http/error.rs` — includes `write_timeout_maps_to_429_not_retriable` and
  the `Retry-After` header on the timeout `429`.
- `tests/http_query_limits.rs` — end-to-end integration: row cap
  truncate/reject, byte cap, override within/above ceiling, disabled config,
  `"limits": null` and malformed-limits handling, write-op exemption (with
  persistence assertions), concurrency isolation across distinct per-call
  overrides, and auth-before-limits ordering (401/403 precede 413/422).
- `benches/http_query_limits.rs` — criterion benchmark of the under-limit
  enforcement overhead (limit resolution, end-to-end response path, and
  envelope-vs-request deserialization), enabled vs disabled.
