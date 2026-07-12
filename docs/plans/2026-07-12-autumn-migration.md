# Autumn Migration & HTTP/MCP Surface Unification — Design & Evaluation

**Date:** 2026-07-12
**Status:** Draft — decision-grade, awaiting go/no-go
**Type:** Design / Evaluation (no code in this PR)
**Related:** ADR 0055 (migrate HTTP to autumn), Issues #465, #475, #2905, #3350, #3234, #3368, #3376, #3426, #3353, #3360, #3231, #3209

> **Update 2026-07-12 — P0 spike confirmed.** The one P0 assumption this evaluation flagged as unverified — *does autumn-web 0.5.0 relax the `IntoAppLayer` middleware bound?* — is **CONFIRMED**. A P0 spike compiled `tower-governor`, `ConcurrencyLimit`, `SetResponseHeader`, `RequireApiToken`, and custom layers against 0.5.0: `IntoAppLayer` is a blanket impl over any `tower::Layer<Route>` (`app.rs:438-440`); its "sealed" wording is only a diagnostic wrapper (`app.rs:408`), not a real restriction. Consequences folded in below: rate limiting via `tower-governor` supersedes/reopens ADR 0055 (§2.5, §8), and the remaining bespoke security work narrows to the single token→role RBAC layer (§6).
>
> **Update 2026-07-12 — structural constraint (separate axis).** The same spike proved empirically that **autumn 0.4 and 0.5 cannot coexist in one crate**: 0.5's macros expand to hardcoded `::autumn_web::…` paths with no `proc-macro-crate` rename support, so a combined build fails with 11 type-mismatch errors. This **rules out the original "in-crate feature-flag, route-by-route" staging** and forces **crate-boundary staging** — a separate 0.5 workspace member (§5.0). The strangler recommendation stands; its unit of incrementality is now the workspace member, not a main-crate flag. Upstream `proc-macro-crate` ask filed (§7.1).

---

## 1. Executive summary + recommendation

**Verdict: GO — but as an *idiomatic-adoption + MCP-unification* project, not a "port from axum," and staged as a strangler (Approach B) whose unit of incrementality is a *separate workspace member on autumn 0.5* (a crate-boundary, not a per-route feature flag inside the main crate — see §5.0), so the embedded-library use case stays first-class and untouched.**

The premise that started this evaluation ("the HTTP layer is basically straight axum, let's move it to autumn") is **factually wrong and worth correcting up front**: AletheiaDB's HTTP surface is **already on `autumn-web`** (pinned at **0.4** in `Cargo.toml:70`), and has been since ADR 0055 (2026-04-19). What we actually have is autumn used *non-idiomatically* — for app lifecycle and baseline middleware only — with auth and tracing shoved into `FromRequestParts` extractors because the **0.2-era `IntoAppLayer` bound blocked tower middleware**, and with rate limiting dropped entirely (ADR 0055, the `TODO(autumn-0.3)` gap).

So the real decision is not "adopt autumn." It is:

1. **Upgrade `autumn-web` 0.4 → 0.5.0**, which — **confirmed by the P0 spike (2026-07-12)** — relaxes `IntoAppLayer` to accept raw tower/axum layers and adds escape hatches, letting us move auth/tracing back to normal layers and **restore `tower-governor` rate limiting, superseding/reopening the ADR 0055 gap**. The spike found 0.5.0's `IntoAppLayer` is a **blanket impl over any `tower::Layer<Route>`** (`app.rs:438-440`); the "sealed" appearance is purely a diagnostic wrapper for cleaner error messages (`app.rs:408`, `:881-883`), not a real restriction — `tower-governor`, `ConcurrencyLimit`, `SetResponseHeader`, `RequireApiToken`, and custom layers all compile-mount.
2. **Adopt `#[api_doc(mcp)]` + `.mount_mcp("/mcp")` + `.openapi(...)`**, so one handler definition becomes an HTTP route **and** an MCP tool **and** an OpenAPI operation, with a guarantee they cannot drift.
3. **Unify the two divergent error models** (`AletheiaHttpError` vs `McpError`) and the two RBAC classification registries behind one source of truth.

The prize is real: today we maintain **5 HTTP routes + 44 MCP tools as two entirely separate stacks** (a 6.4k-line string-match dispatcher on one side, a polymorphic `/query` enum on the other), with two error shapes and two auth-classification tables kept in sync only by conformance tests. Unification collapses that duplication and gives us OpenAPI + an HTTP MCP transport **for near-free**.

The risk is also real: `autumn-web` is **pre-1.0** (no SemVer guarantee, ~123 open issues), and autumn's MCP transport is **HTTP-only (no stdio)** while we ship a stdio `aletheia-mcp` binary today. On security, the P0 spike narrowed what autumn does *not* give us: **auth-on-by-default** (`RequireApiToken` + `secure_mcp`) is confirmed available, and **constant-time key verify** via a custom `ApiTokenStore` is confirmed to compile and be accepted — leaving essentially **one genuinely-bespoke security component: token→role `AccessClass` RBAC**, which autumn does not do natively (`verify` returns only `Option<principal>`, and `#[secured]` role gating is session-based). The structured-error `retriable` flag remains an error-model adapter (§6(f)). These will silently regress if we are not deliberate, so they are written as acceptance tests first.

Hence: **strangler at the crate boundary (a separate 0.5 workspace member), security invariants written as acceptance tests first.** One structural constraint the spike proved empirically shapes this: **autumn 0.4 and 0.5 cannot coexist in a single crate** (0.5's macros expand to hardcoded `::autumn_web::…` paths with no `proc-macro-crate` rename support — combined build fails with 11 type-mismatch errors), so the staging is a workspace member, not an in-crate feature flag (§5.0). Detail below.

---

## 2. Current state (verified against the repo)

### 2.1 The reframe: already on autumn, non-idiomatically

| Claim | Verdict | Evidence |
|-------|---------|----------|
| "HTTP is basically straight axum" | **False** | `src/http/**` imports `autumn_web::*` throughout; routes declared with autumn's `#[get]`/`#[post]` + `routes![]` macros (`src/http/handlers.rs:36-37`, `src/http/admin.rs:16-17`) |
| Which autumn version? | **0.4** (code comments still say 0.2.0) | `Cargo.toml:70` `autumn-web = { version = "0.4", ... }` |
| Uses `#[api_doc]` / `mount_mcp` / `.openapi()`? | **No — none present** | `grep api_doc\|mount_mcp\|\.openapi(` over `src/` → 0 hits |
| MCP shares the HTTP surface? | **No** | MCP is a separate `rmcp` 1.7 stdio server (`Cargo.toml`, `src/mcp/server.rs`) |

**Conclusion:** this is an *idiomatic-adoption + upgrade + MCP-unification* project, not a framework port. The framework is already chosen and shipping.

### 2.2 HTTP surface — 5 routes

| Method | Path | Handler | file:line | Class |
|--------|------|---------|-----------|-------|
| GET | `/status` | `health_check` | `handlers.rs:66` | Metrics |
| POST | `/query` | `handle_query` (polymorphic over **10 ops**) | `handlers.rs:839` | per-op Read/Write via `query_access_class` (`:674`) |
| POST | `/admin/keys` | `create_key` | `admin.rs:41` | Admin |
| GET | `/admin/keys` | `list_keys` | `admin.rs:70` | Admin |
| POST | `/admin/keys/revoke` | `revoke_key` | `admin.rs:94` | Admin |

`/query` dispatches 10 operations (`find_node`, `get_node`, `bulk_get_nodes`, `find_neighbors`, `execute_query`, `bulk_execute_query`, `create_node`, `bulk_create_nodes`, `bulk_update_nodes`, `bulk_delete_nodes`) via an exhaustive `query_access_class` match with no wildcard, so classification is compile-time-total.

### 2.3 MCP surface — 44 tools, one string match

Built on `rmcp` (`ServerHandler`). `src/mcp/server.rs` is **6,443 lines**. **44 tools** advertised, dispatched by a literal 44-arm `match name { ... }` in `dispatch_tool` (`server.rs:5340` → `dispatch_read_tool:5416`), kept in lockstep with `TOOL_ACCESS_CLASSES` (`src/mcp/auth.rs:68`) and the access-control matrix doc by conformance tests. Split: **31 Read / 1 Metrics / 12 Write / 0 Admin** (key lifecycle is HTTP-only). Transport: `server.serve(stdio())` — **stdio, single client, process-lifetime**.

### 2.4 Two error models (a unification target)

| | HTTP | MCP |
|-|------|-----|
| Type | `AletheiaHttpError` (`src/http/error.rs:80`) | `McpError` (`src/mcp/error.rs:115`) |
| Wire shape | `{success:false, error, code?, retriable?, details?, trace_id?}` | `{error:{code,message,retriable,details?}}` |
| Code vocab | #3234 (`code_str`) | #3234 (`McpErrorCode`, `:42`) |
| `retriable` | only on auth + limit variants | on every error |

Codes overlap conceptually (`INVALID_ARGUMENT`, `NOT_FOUND`, `PERMISSION_DENIED`, `UNAUTHENTICATED`) but the enums and serialization are **separate implementations**.

### 2.5 Middleware-as-extractors and the rate-limiting gap (ADR 0055)

autumn 0.2's **sealed `IntoAppLayer` bound blocked arbitrary tower/axum middleware** on the production path. Consequence, load-bearing across the codebase:

- **Auth/RBAC lives in a `FromRequestParts` extractor** (`AuthContext`, `src/http/auth.rs:138`) — not a tower layer — *because* layers were blocked (`auth.rs:3` says so verbatim).
- **OTel tracing lives in a `FromRequestParts` extractor** (`HttpTrace`, `src/http/trace.rs:161`) — same reason (`trace.rs:3`).
- **Rate limiting is not wired at all.** `actix-governor` → `tower-governor` 0.8 failed the **0.4-era** `IntoAppLayer` bound; ADR 0055 punted it to the reverse proxy ("autumn 0.3 native rate limiting"), preserving `RateLimitConfig` in the public API. `TODO(autumn-0.3)` marker at `src/http/server.rs:17-26`. **The P0 spike confirms 0.5.0 accepts `tower-governor`, so upgrading supersedes/reopens ADR 0055's punt:** in-process rate limiting via `tower-governor` (plus `ConcurrencyLimit` for backpressure) becomes available again as a normal layer — see §8 (inbound HTTP/MCP concurrency control).
- Only two things attach as real layers in prod: autumn's baseline stack, and `DefaultBodyLimit` (`server.rs:230`, chosen specifically because it satisfies `IntoAppLayer`).
- The **test router is a plain `axum::Router`** (`server.rs:396`) precisely so it can attach the full tower-http stack (CORS, security headers, `TraceLayer`) that prod can't.

**This is the crux of the reconciliation (flagged item #2):** the surface audit ("`IntoAppLayer` blocks middleware") and the API deep-dive ("0.5.0 supports `.layer` + escape hatches") are **both right, about different versions**. The repo is on **0.4**, written against the **0.2** limitation. autumn **0.5.0's** `.layer<L: IntoAppLayer>` accepts raw tower/axum layers, and `.merge`/`.nest` drop to raw axum on the same `AppState` (API research §6, §8). **This is now a confirmed, direct reason to upgrade:** it lets auth/tracing move back to normal layers *and* restores `tower-governor` rate limiting, superseding/reopening the ADR 0055 gap as a side effect of this project rather than as separate work.

> **CONFIRMED (P0 spike, 2026-07-12):** autumn-web 0.5.0 relaxes `IntoAppLayer` enough for `tower-governor` and our security-header/CORS layers. The spike compiled these against 0.5.0 and found `IntoAppLayer` is a **blanket impl over any `tower::Layer<Route>`** meeting axum's service bounds (`impl<L> IntoAppLayer for L where L: tower::Layer<axum::routing::Route> + Clone + Send + Sync + 'static`, `app.rs:438-440`; docstring `app.rs:881-883`). The "sealed" wording is **only a diagnostic wrapper** to surface a clean `IntoAppLayer is not implemented for YourType` message instead of a 40-line associated-type wall (`app.rs:408-416`) — not a real restriction. `tower-governor`, `ConcurrencyLimit`, `SetResponseHeaderLayer`, custom layers, and `RequireApiToken` all compile-mount on 0.5.0. **Caveat (not a blocker):** body-*wrapping* layers (`TraceLayer`, `RequestBodyLimit`) still need axum's usual body-map treatment — but that is axum's own `Router::layer` rule, **not** an autumn seal, so it is a known axum pattern rather than a porting obstacle.

### 2.6 No streaming anywhere

No SSE / websockets / multipart / streaming in either surface — plain buffered JSON. This materially de-risks the port: autumn's MCP eligibility rule is "JSON in, JSON out," which our handlers already satisfy structurally.

---

## 3. The unification opportunity

### 3.1 The mechanism

Tag a handler `#[api_doc(mcp, summary = "…")]` (snake_case macro, `mcp` is a **bare flag**; the `#[apiDoc]`/`description=` spellings from early notes are wrong) and call `.mount_mcp("/mcp")` **once**. Then:

- the same `async fn` is **an HTTP route** (from `#[get]`/`#[post]`) **and** an MCP tool;
- `.openapi(OpenApiConfig::new(...))` serves `openapi.json` + Swagger UI from the *same* `ApiDoc` metadata;
- tool `name`/`description`/`inputSchema` derive from that one metadata source, so **they cannot drift from the handler**;
- `inputSchema` is built from typed extractors: `Path` → required string props, `Query<T>` → a `query` object, `Json<T>` → a required `body` prop;
- `tools/call` **replays the real handler pipeline** through a pre-merge router snapshot — so auth, validation, and error mapping apply identically to HTTP and MCP.

One definition → **HTTP endpoint + MCP tool + OpenAPI operation.** OpenAPI + an HTTP MCP transport are **new deliverables we gain**, not costs.

### 3.2 Before / after — one endpoint

Representative: today's `/query` `get_node` op vs a unified typed handler.

**Before (today).** The read lives in *two unrelated places*:

```rust
// HTTP: src/http/handlers.rs — buried in the 10-arm QueryRequest enum
#[serde(tag = "operation", rename_all = "snake_case")]
enum QueryRequest { GetNode { id: u64 }, /* 9 more */ }
async fn handle_get_node(state, req) -> Result<Json<Value>, AletheiaHttpError> { ... }

// MCP: src/mcp/server.rs — arm 1 of a 44-arm string match, different types,
// different error model, different budget/vector-elision plumbing
"get_node" => self.handle_get_node(args)   // -> CallToolResult / McpError
```

**After (unified).** One typed handler is route + tool + spec:

```rust
#[get("/nodes/{id}")]
#[api_doc(mcp, summary = "Fetch a node by id, with bi-temporal bounds")]
async fn get_node(
    state: AppState,                 // custom extractor over Arc<AletheiaDB>
    _auth: Authorized<Read>,         // RBAC gate (see §6)
    Path(id): Path<u64>,
    Query(opts): Query<GetNodeOpts>, // include_vectors, max_response_tokens, ...
) -> AletheiaResult<Json<NodeResponse>> { ... }
// mounted once:
autumn_web::app()
    .routes(routes![get_node, /* ... */])
    .openapi(OpenApiConfig::new("AletheiaDB", env!("CARGO_PKG_VERSION")))
    .mount_mcp("/mcp")
    .secure_mcp(require_api_token())   // catalog auth — see §6(e)
    .run().await;
```

`GET /nodes/1` and MCP `tools/call get_node {"id":1}` now run the **same code, same auth, same error shape**, and `openapi.json` documents it automatically.

### 3.3 "JSON-in / JSON-out eligibility" — which of our 44 tools map cleanly

Eligibility = handler takes its body via `Json<T>` (or none) and returns `Json<T>`. Our surface is buffered JSON throughout, so the *structural* bar is met everywhere. The real gradient is **response-shaping features** that today live inside the rmcp dispatcher and would need to become handler-level concerns:

| Bucket | Tools (examples) | Maps cleanly? | Note |
|--------|------------------|---------------|------|
| **Clean reads** | `get_node`, `get_edge`, `list_nodes`, `count_nodes`, `get_schema`, `temporal_extent`, `get_*_at_time`, `diff_*` | **Yes** | Path/Query/Json in, Json out. Direct `#[api_doc(mcp)]` candidates. |
| **Clean writes** | `create_node`, `update_node`, `delete_node`, `create_edge`, … | **Yes** | `Json<T>` in, `Json<T>` out; DELETE verb sets `destructiveHint`. |
| **Budget-shaped reads** (#3353) | 13 `BUDGETABLE_READ_TOOLS` (`server.rs:5474`) | **Yes, with adapter** | `max_response_tokens`/`max_response_bytes`/`priority_properties` become `Query`/`Json` fields → they land in the OpenAPI+MCP schema automatically (a *bonus*: discoverability we hand-inject today). The budget-shaping ladder must move into a shared response wrapper so it runs for HTTP too. |
| **Vector-elision reads** (#3220) | `find_similar`, `hybrid_query`, `traverse`, … | **Yes, with adapter** | `include_vectors` becomes a typed field; elision logic moves to the shared serializer. |
| **Cursor reads** (#3360) | `list_nodes`, `find_nodes_at_time`, `get_*_edges`, `traverse` | **Care** | opaque `cursor`/`use_cursor` are just fields, but the **per-process signing secret + TTL/cap registry** is stateful; must be carried in `AppState`, and cursor+budget composition (already live) must be preserved. |
| **Polymorphic / batch** | `apply_batch` (#3231) | **Yes but bespoke schema** | Ordered heterogeneous `Vec<BatchOperation>` with `$alias`/`$index` local refs and per-op `failed_op_index`. `Json<ApplyBatchRequest>` works; the JSON-Schema autumn derives will be large/nested — verify Swagger renders it and that static pre-commit validation still runs before any txn opens. |
| **Metrics/admin** | `database_stats`, `/admin/keys*` | **Yes** | `database_stats` is a clean read. Admin key routes stay HTTP-only, **not** `#[api_doc(mcp)]` (no admin MCP tools today — preserve that). |

Nothing in our surface is structurally *ineligible* (no streaming, no multipart). The work is **re-homing the cross-cutting response shapers** (budget, vector-elision, temporal-bounds stamping, structured errors) from the rmcp dispatcher into a **shared layer both surfaces call** — which is exactly the unification win.

---

## 4. Ideation (shown, not hidden)

### 4.1 Brainstorming — options generated

- Big-bang rewrite of both surfaces onto `#[api_doc(mcp)]`.
- Strangler: convert route-by-route / tool-by-tool. *(Note: the spike later proved this must be at the **crate boundary** — a separate 0.5 workspace member — because in-crate 0.4↔0.5 coexistence is impossible, §5.0. An in-main-crate feature-flag variant of this option is ruled out.)*
- HTTP-only adoption (api_doc + OpenAPI), defer MCP unification.
- Keep two surfaces, only **share the response-shaper + error + RBAC libraries** (no api_doc at all).
- Upgrade to 0.5.0 first as a standalone PR (restore rate limiting + move auth to layers), *then* decide on api_doc separately.
- Generate typed handlers from the existing rmcp request structs via a macro (mechanical de-dup of the 44-arm match).
- Dual MCP: keep stdio (`rmcp`) **and** add HTTP `/mcp` (autumn), sharing one handler core.

### 4.2 Reverse-brainstorming — "how would we guarantee this fails / regresses security?" → inverted safeguards

| Failure recipe | Inverted safeguard (becomes an AC in §6) |
|----------------|------------------------------------------|
| Let autumn default to anonymous and forget to enforce on-by-default | Startup refuses with zero credentials (#3350); test asserts it |
| Expose `/mcp` catalog (`tools/list`) without wrapping `.secure_mcp` | AC(e): unauthenticated `tools/list` → 401; conformance test |
| Use autumn's built-in token store (SHA-256 → hashmap lookup, **not** constant-time) | AC(c): custom `ApiTokenStore` with `subtle::ConstantTimeEq`; timing test |
| Map bearer tokens to a principal with no role (`#[secured]` reads session role, not token role) | AC(b): custom policy layer maps our 4 roles onto token principals; RBAC matrix test extended to `/mcp` |
| Adopt autumn's RFC-7807 shape and silently drop `retriable` | AC(f): custom error `IntoResponse` preserves `{code,message,retriable,details}` on both surfaces |
| Big-bang the 6.4k-line match and lose a tool's classification | Strangler + registry-sweep conformance tests (must stay green each step) |
| Log the bearer token in a trace span | AC(d): credentials never logged; log-scrub test |
| Ship without a rollback | Feature flag / route-by-route, rollback = revert the converted route |

### 4.3 Six hats

- **White (facts):** Already on autumn 0.4. 5 routes + 44 tools. Two error models. Auth+tracing are extractors; rate limiting absent (ADR 0055). No streaming. autumn 0.5.0 adds api_doc→MCP+OpenAPI, HTTP-only MCP, and (**P0-spike-confirmed**) a relaxed `IntoAppLayer` that is a blanket impl over any `tower::Layer<Route>` (`app.rs:438-440`). Pre-1.0, ~123 open issues.
- **Red (gut):** The unification is genuinely attractive — one definition, three surfaces, no drift. Nervousness centers on pre-1.0 churn and the security invariants that autumn does *not* give us for free.
- **Black (risks):** pre-1.0 SemVer breakage; `IntoAppLayer`-relaxation is **now P0-confirmed** (no longer a risk); **token→role RBAC is the one bespoke security piece left on us** (auth-on-by-default and constant-time verify are P0-confirmed available/compiling); error-shape drift (the `retriable` adapter); the batch schema; OpenAPI correctness for temporal/vector types; the embedded-first constraint must never regress.
- **Yellow (value):** kills duplication across 5+44 handlers, two error models, two RBAC tables; **free** OpenAPI + Swagger; an HTTP MCP transport (multi-agent-ready); restored rate limiting; positions us for the 0.6.0 daemon (#475/#2905).
- **Green (alternatives):** Approach C (HTTP-only) as a de-risked first cut; share libraries without api_doc; upgrade-to-0.5.0 as its own PR before committing to unification.
- **Blue (process):** Spike 0.5.0 in an isolated crate → write security ACs as failing tests → strangle route-by-route **in the separate 0.5 workspace member** (crate-boundary, §5.0 — not an in-crate flag) → add HTTP `/mcp` beside stdio → cut over → deprecate stdio only if/when the daemon lands. Each step independently revertible.

---

## 5. Implementation approaches & recommendation

### 5.0 Structural constraint: autumn 0.4 and 0.5 **cannot coexist in one crate** (P0 spike, empirical)

Before choosing an approach, a hard mechanism the L3 spike proved empirically **rules out the original staging plan**. **autumn-macros 0.5.0 expands `#[get]`/`#[post]`/`#[api_doc(mcp)]` to HARDCODED absolute paths `::autumn_web::…` (14+ occurrences in `route.rs`), with NO `proc-macro-crate` rename support.** Because the extern prelude binds `autumn_web` to exactly **one** version, **a single crate cannot host autumn 0.4 and 0.5 simultaneously.** Proven: the standalone 0.5 spike compiles, but a combined `--features http-server,autumn-server` build **fails with 11 type-mismatch errors** (0.5 macro output vs the 0.4 `Route`/`ApiDoc` structs).

**Consequence:** the originally-sketched staging — *"a parallel unified module behind a default-off feature flag **inside the main crate**, converted route-by-route"* — **is impossible while the repo pins autumn 0.4.** Per-route feature-flag coexistence in one crate would require both autumn versions linked at once, which the hardcoded macro paths forbid. The two *real* staging options are:

- **(a) Atomic 0.4→0.5 repo bump.** The whole `src/http` moves to 0.5 in one shot — no in-crate coexistence, one version at all times. Simple, but the cutover is all-at-once for the HTTP surface.
- **(b) Crate-boundary isolation (RECOMMENDED unit of incrementality).** A **separate workspace member** depends on autumn-web 0.5 **plus a path-dep on aletheiadb's public API**; the main crate's features stay untouched and the change is strictly *additive*. The old 0.4 surface and the new 0.5 surface coexist **at the workspace level** (two crates, each pinning one autumn version) — never inside one crate. This is exactly what the L3 spike is building.

> This constraint is a **separate axis** from the P0 `IntoAppLayer` relaxation (§2.5): the layer/`RequireApiToken`/`ApiTokenStore` verdict stands (all compile on 0.5). This is about the **0.4↔0.5 coexistence mechanism**, not about whether 0.5 accepts our middleware.

### A) Big-bang
Rewrite both surfaces onto the unified `#[api_doc(mcp)]` surface at once; retire the rmcp dispatcher and the `/query` enum in one PR. Equivalent to staging option (a) taken to its limit — one version, one cutover, no coexistence.

### B) Strangler (crate-boundary, route-by-route) — **RECOMMENDED**
Stand up the unified surface as a **separate workspace member** on autumn 0.5 (staging option (b)) with a path-dep on aletheiadb's public API; introduce the unified handler pattern for one endpoint **in that new crate**; convert routes/tools incrementally **by adding them to the new crate** (not by feature-flagging inside the main crate — impossible per §5.0); **add HTTP `/mcp` alongside the stdio binary** (coexistence, not replacement); cut over tool-by-tool; rollback = stop routing to / drop the new crate (registry-sweep tests stay green throughout). The unit of incrementality is the **workspace member**, not a per-route flag in the main crate.

### C) Minimal
Adopt `#[api_doc]` + OpenAPI on the **HTTP surface only**; leave the rmcp/stdio MCP untouched; defer unification.

### Comparison

| Criterion | A Big-bang | B Strangler | C Minimal |
|-----------|-----------|-------------|-----------|
| Risk to security invariants | **High** (all at once) | **Low** (per-route ACs gate each step) | Low |
| Diff size / blast radius | **Very large** (6.4k-line rewrite) | Medium, incremental | Small |
| Rollback story | Poor (all-or-nothing) | **Excellent** (revert per route *in the isolated crate*; stop routing to / drop the new workspace member) | Excellent |
| Coexistence mechanism | option (a) atomic bump — one version | **crate-boundary (b)** — 0.4 & 0.5 in *separate* workspace members (in-crate coexistence impossible, §5.0) | atomic bump of the HTTP crate |
| Value delivered | Full, but late | Full, incremental | Partial (no MCP unification, no HTTP MCP) |
| Coexistence with stdio MCP | Forces a stdio decision now | **Keeps stdio; adds HTTP `/mcp`** | stdio untouched |
| OpenAPI gained | Yes | Yes | Yes |

### Recommendation — **B, Strangler.**

The 6.4k-line dispatcher and the security invariants that autumn does *not* hand us for free make a big-bang (A) reckless: a single missed classification or a swap to autumn's non-constant-time store is a security regression, and A has no incremental safety net. C is a legitimate *first cut* and is in fact **Phase 1 of B** — but stopping at C forfeits the headline prize (one definition → HTTP + MCP + OpenAPI, no drift) and the HTTP MCP transport we need for the 0.6.0 daemon. B gets C's safety while still reaching full unification, keeps the embedded library and the stdio MCP working the entire time, and makes every step revertible. **Crucially, B's incrementality lives at the *crate boundary* (staging option (b), §5.0), not at a per-route feature flag inside the main crate — which the hardcoded-macro-path constraint forbids.** The new unified surface is a separate workspace member on autumn 0.5 with a path-dep on aletheiadb; the main crate (and its 0.4 HTTP surface, until retired) is untouched. Bias toward strangler confirmed by the evidence.

---

## 6. Security invariants as acceptance criteria

Each invariant → an explicit AC + the test that proves it. **These are written as failing tests before any handler is converted.**

**What the P0 spike changed here:** most of these ACs are now "wire up a *confirmed* API" rather than "unknown." The spike confirmed against 0.5.0 that (a) **auth-on-by-default** — `RequireApiToken` (a real `tower::Layer`) + `.secure_mcp` gating `/mcp` — is available; and (c) a **custom constant-time `ApiTokenStore` compiles and is accepted** (the trait's `verify`/`issue`/`revoke` are the documented extension point). That leaves exactly **one genuinely-bespoke security component: the token→role `AccessClass` mapping (AC(b))** — autumn's `verify` returns only `Option<principal>` and `#[secured]` role gating reads the *session* role, not a bearer-token role, so per-token RBAC needs a **thin custom authorization layer** (or role encoded in the principal). That layer is what the RBAC conformance tests must cover on the `/mcp` surface.

| # | Acceptance criterion | Evidence / test |
|---|----------------------|-----------------|
| (a) | Server **refuses to start with zero credentials** (Required mode); anonymous is explicit opt-in — **auth-on-by-default P0-confirmed available** (`RequireApiToken` + `.secure_mcp`) | Startup-refusal test mirroring `validate_mcp_auth_startup` (`src/mcp/auth.rs:299`) + HTTP equivalent; extend to the autumn `on_startup` path |
| (b) | **Every route and every MCP tool gated by the correct role** (admin/writer/reader/metrics) — **the single bespoke security component** (see §6 intro) | Extend the RBAC conformance sweep (`auth_tests.rs:190`, `documented_matrix_matches_code:137`) to cover `/mcp` tool calls; a **thin custom authorization layer** maps our 4 `AccessClass` roles onto API-token principals (autumn's `verify` yields only `Option<principal>` and `#[secured]` alone reads *session* role, not token role — insufficient) |
| (c) | **Constant-time key verification** — custom `ApiTokenStore` **P0-confirmed to compile & be accepted** | Custom `ApiTokenStore` impl using `subtle::ConstantTimeEq` (2.6.1 already a dep); a timing/`ConstantTimeEq`-usage test. **Do NOT** use autumn's built-in stores (SHA-256-then-hashmap-lookup) |
| (d) | **Credentials never logged** | Log-scrub test asserting no bearer/key material in trace spans or error bodies; SHA-256-hashed keys in `{data_dir}/auth/keys.json` (0600) |
| (e) | **MCP catalog (`tools/list`) not reachable unauthenticated** | Wrap `.secure_mcp(RequireApiToken layer)` — `RequireApiToken` P0-confirmed to be a real `tower::Layer` and `secure_mcp` gates `/mcp`; test asserts `initialize`/`tools/list`/`tools/call` all 401 without a valid credential (autumn leaves the catalog open unless wrapped) |
| (f) | **Structured error contract with `retriable` preserved on both surfaces** | Custom error type + `IntoResponse` emitting `{code,message,retriable,details}`; contract test on HTTP and `/mcp`. autumn's RFC-7807 has `code` but **no `retriable`** and a different shape (#3234) |
| (g) | **Revocation is immediate** (re-verify per call) | Revoke-then-call test returns 401 on the next request; no caching of verify results across calls |

### 6.1 The authoritative migration checklist — the parity harness

The ACs above are the *security* contract; the *whole-surface* completion contract lives in the parity-harness lane (branch `feature/parity-harness`, draft PR #3527). It lands `tests/parity/inventory.json` (with human summary `tests/parity/README.md`) — the **machine-readable inventory of all 5 HTTP routes + 44 MCP tools**, each row carrying its access class and the pinning test that fixes its observable behavior. The completion criterion is plain: **the port is done when every inventory row's pinning test passes unchanged against the autumn implementation.** The harness pins the surfaces a port must preserve with **23 HTTP + 7 MCP black-box golden tests plus one in-crate registry-sweep test** (the latter guarding the classification lockstep §2.3 relies on).

Some behaviors are **not black-box reachable** and so cannot be golden-pinned; the harness records them in a `coverage_gaps` array in `inventory.json`, and they carry over as **in-crate test obligations** the port must satisfy directly (they overlap the §6 ACs — RBAC is AC(b), the error envelope is AC(f)):

- **MCP RBAC / token-budget (#3353) / cursor (#3360) per-behavior semantics** — role gating and the response-shaper ladders exercised at the handler level, not just the wire.
- **HTTP 429 (rate limiting)** — absent today (§2.5); restored as a `tower-governor` layer under 0.5, so its behavior is a new in-crate obligation.
- **Feature-gated OTel `trace_id`** — only present under the tracing feature, so not exercisable from a default black-box build.
- **The exact HTTP 500 error envelope** — the internal-error shape (AC(f)'s `{code,message,retriable,details}`) asserted in-crate rather than provoked over the wire.

---

## 7. Risks & edge cases (as test cases)

| Risk | Test / mitigation |
|------|-------------------|
| **autumn 0.4↔0.5 cannot coexist in one crate** — **CONFIRMED (P0 spike)** | 0.5's macros expand to hardcoded `::autumn_web::…` paths (14+ in `route.rs`), no `proc-macro-crate` rename support; combined `--features http-server,autumn-server` build fails with **11 type-mismatch errors**. **Mitigation = the strategy itself:** stage at the crate boundary (separate 0.5 workspace member, §5.0), never an in-crate feature flag. See "Upstream asks" below |
| **axum 0.8 version alignment** | autumn 0.5.0 pins axum ^0.8, edition 2024, MSRV 1.88; our `axum`/`tower` deps must match. Build matrix + `cargo tree -d` for duplicate axum |
| **Pre-1.0 autumn (no SemVer, ~123 open issues)** | Pin an exact `=0.5.0`; supply-chain review; contract tests catch behavior drift on upgrade; treat autumn upgrades as breaking until they hit 1.0 |
| **`IntoAppLayer` relaxation** — **CONFIRMED (P0 spike)**, no longer a risk | Spike compiled `tower-governor` + `ConcurrencyLimit` + security-header + `RequireApiToken` layers under 0.5.0: all mount (blanket impl `app.rs:438-440`; "sealed" is diagnostic-only, `app.rs:408`). Residual note: body-*wrapping* layers (`TraceLayer`, `RequestBodyLimit`) use axum's normal body-map treatment — an axum pattern, not an autumn seal |
| **Token-budget (#3353) & cursor (#3360) interacting with api_doc schema** | These become typed `Query`/`Json` fields → assert they appear in `openapi.json` and `tools/list` inputSchema; conformance sweep (`ac2_conformance_sweep_never_overruns`, `tests.rs:13627`) must stay green; cursor signing secret + TTL/cap registry carried in `AppState`, not per-request |
| **6.4k-line `server.rs` match refactor** | Strangle one arm at a time; keep the registry-sweep tests (`auth_tests.rs`, `tests.rs:9258`) green each step; a name→handler table can replace the match mechanically |
| **Error-shape drift between surfaces** | Single shared error type + `IntoResponse`; golden-file contract test on both surfaces (AC(f)) |
| **OpenAPI schema correctness for temporal/vector types** | Snapshot-test `openapi.json`; verify RFC-3339 temporal bounds, elided-vector descriptors, and the `apply_batch` nested schema render and validate |
| **`mount_mcp` path collision** | axum panics at startup on a duplicate `/mcp`; startup smoke test |
| **`unsafe set_var` autumn env bridge** (edition-2024) | `apply_autumn_env` (`server.rs:328`) already uses `unsafe set_var`; keep it isolated to startup, single-threaded, pre-serve |

### 7.1 Upstream asks (filed with autumn)

- **autumn-macros should adopt `proc-macro-crate` (or `$crate`-style path resolution)** so its `#[get]`/`#[post]`/`#[api_doc]` expansions resolve `autumn_web` through the caller's dependency name instead of the hardcoded absolute path `::autumn_web::…`. Until then, **renamed or parallel-version deps are impossible**, which is precisely why in-crate 0.4↔0.5 coexistence fails (§5.0) and the migration must be staged at the crate boundary. **L3 is filing this upstream issue.** If/when it lands, per-crate feature-flag coexistence (and cleaner multi-version testing) becomes possible and could simplify a future staging.

---

## 8. Connection lifecycle / pooling (design, not implementation)

**Critical distinction:** AletheiaDB is **embedded** — a single process holds one `Arc<AletheiaDB>`. There is **no outbound DB connection pool** (autumn's diesel `Db`/`#[repository]` pool layer is explicitly *not* pulled in — `default-features = false`). The only "pool" concern is **inbound HTTP/MCP client connections**.

| Surface | Lifecycle today | Concern | Recommendation |
|---------|-----------------|---------|----------------|
| **HTTP** | axum/hyper keep-alive; `Arc<AletheiaDB>` shared via extractor | Concurrency limit, backpressure, body-size (`DefaultBodyLimit`, #3108), per-query timeout→429 (#3368) | Under 0.5.0, restore a `tower` `ConcurrencyLimit` (backpressure) + `tower-governor` rate-limit **layer** — **P0-confirmed to compile-mount**, so this **supersedes/reopens ADR 0055's** reverse-proxy punt and brings rate limiting back in-process |
| **MCP stdio** | one client, process-lifetime, serialized | trivial — single session | Keep as-is for the embedded/CLI case |
| **MCP over HTTP (new)** | **many concurrent agent sessions** over `POST /mcp` | session limits, backpressure, cursor TTL/cap (#3360) interplay, per-session auth | Needs a **connection/session budget**: max concurrent MCP sessions, per-session cursor cap (already have TTL default 5 min / cap 128), and the same rate-limit layer. This is where multi-agent load actually lands |

**Recommended follow-up issue scope:** *"Inbound connection & MCP-session lifecycle policy for the unified surface"* — `ConcurrencyLimit` + `tower-governor` rate-limit layer (**supersedes/reopens ADR 0055**, now P0-confirmed available on 0.5.0), MCP-over-HTTP session cap + backpressure, and cursor registry sizing under many sessions. Explicitly out of scope: any outbound DB pool (none exists; embedded).

---

## 9. Trajectory / phasing

| Phase | autumn ver | Deliverable | Constraint |
|-------|-----------|-------------|------------|
| **0.5.0 (this work)** | new **0.5 workspace member** (crate-boundary, §5.0); main crate stays 0.4 until retired | Unified HTTP+MCP surface in the new crate: `#[api_doc(mcp)]` handlers, `.mount_mcp("/mcp")` beside stdio, `.openapi(...)`, one error model, one RBAC registry, restored rate-limit layer | Structure it so it **becomes the daemon with zero rearchitecting**; **no in-crate 0.4↔0.5 coexistence** — the new surface is a separate crate with a path-dep on aletheiadb |
| **0.6.0 (next)** | autumn daemon serve mode | autumn's forthcoming **embed-assets → one self-contained `aletheia serve` binary**, built with Aletheia in mind (epic **#475**, backlog **#2905** "daemon-owned AletheiaDB for MCP clients") | The 0.5.0 unified surface *is* the daemon's core; the daemon adds process lifetime + asset embedding around it |
| **Longer term** | autumn plugin | An **`aletheiadb` plugin for autumn** | — |

**Non-negotiable constraint (stated once, loudly):** AletheiaDB is embedded in a **private AI-first CRM today**. The **embedded-library use case stays first-class**. The autumn layer must remain an **optional, feature-gated shell** (`http-server`) around the library — **never** a requirement to use AletheiaDB. Every phase preserves `AletheiaDB::new()` / `open()` with zero autumn in the dependency graph when `http-server` is off.

---

## 10. Effort estimate (complexity / blast-radius / diff-size, not time)

Expressed per strangler phase as touch-points / new adapters / files. **The unit of work is the new 0.5 workspace member (§5.0), not the main crate's `src/http` — the two autumn versions live in separate crates.**

| Phase | Blast radius | New adapters / files | Complexity |
|-------|-------------|----------------------|------------|
| **P0 — 0.5.0 spike (isolated crate)** | new workspace member `Cargo.toml`; spike `main.rs` | none (attach `tower-governor`) | **Low-Med** — **`IntoAppLayer` relaxation now P0-confirmed** *and* the **0.4↔0.5 in-crate-coexistence constraint confirmed** (§5.0); the spike compiled `tower-governor`/`ConcurrencyLimit`/`RequireApiToken` against a standalone 0.5 crate, so this phase is de-risked from "unknown" to "wire up a confirmed API in an isolated crate" |
| **P1 — shared foundation** | **new workspace member** (e.g. `aletheia-server`), path-dep on `aletheiadb` | 1 unified error type + `IntoResponse`; 1 custom `ApiTokenStore` (constant-time); 1 RBAC policy layer; 1 shared response-shaper (budget/vector-elision/temporal-bounds) | **High** — this is where the real design lives; everything downstream reuses it. Lives in the new crate, not `src/` of the main crate |
| **P2 — HTTP routes onto api_doc** | new crate's handler modules (mirroring `src/http/handlers.rs`, `admin.rs`) | convert 5 routes / 10 ops to typed `#[api_doc]` handlers in the new crate; add `.openapi(...)` | **Med** — mechanical once P1 exists; `/query` polymorphism → typed handlers is the fiddly part |
| **P3 — MCP over HTTP** | `.mount_mcp` + `.secure_mcp`; `src/mcp/**` | tag reads/writes `#[api_doc(mcp)]`; keep stdio binary | **Med** — coexistence, not replacement |
| **P4 — strangle the 44-arm match** | `src/mcp/server.rs` (6.4k lines) | replace match with name→handler table sharing P1 shapers; retire duplicated dispatch | **High** — largest single-file churn; done tool-by-tool with registry sweeps green |
| **P5 — cutover & cleanup** | remove `/query` enum duplication; docs, ADR update | — | **Low-Med** |

Rollback at every phase = stop routing to / revert the phase's routes in the isolated crate (or drop the new workspace member entirely); conformance sweeps are the ratchet. Because the new surface is a separate crate, the main crate's 0.4 HTTP surface keeps working untouched throughout — the two never link the same autumn version.

---

## 11. Open questions for go/no-go

1. **Upgrade to 0.5.0 now?** P0 is a prerequisite for everything; its central unknown — the `IntoAppLayer` relaxation — is now **P0-spike-confirmed** (blanket impl, `app.rs:438-440`). Commit to the whole strangler, or still stage the upgrade as its own PR first?
2. **stdio MCP: keep indefinitely, or deprecate once HTTP `/mcp` + the 0.6.0 daemon land?** Recommendation: keep for the embedded/CLI case; revisit at 0.6.0.
3. **Pre-1.0 autumn tolerance.** Are we comfortable pinning `=0.5.0` and treating every autumn bump as breaking until 1.0, given ~123 open issues?
4. **`/query` polymorphic endpoint:** convert the 10 ops to 10 typed routes (idiomatic, more OpenAPI ops), or keep one polymorphic route with a hand-written schema? Recommendation: typed routes.
5. **`apply_batch` over MCP-HTTP:** accept the large nested JSON-Schema autumn derives, or keep batch HTTP-only initially?
6. **Admin key routes as MCP tools?** Today there are **zero** admin MCP tools. Keep it that way (recommended), or expose key lifecycle over authenticated `/mcp`?
7. **Rate limiting:** restore as an in-process `tower-governor` layer under 0.5.0 (**supersedes/reopens ADR 0055** — P0-confirmed the layer compile-mounts), or keep deferring to the reverse proxy?
8. **Staging mechanism (forced by §5.0 — 0.4↔0.5 can't coexist in one crate):** option **(a) atomic 0.4→0.5 repo bump** of `src/http`, or option **(b) crate-boundary isolation** (separate 0.5 workspace member with a path-dep on aletheiadb)? Recommendation: **(b)** — it is the strangler's unit of incrementality, strictly additive, and leaves the main crate's gates untouched. (An in-main-crate feature-flag staging is **not** an option — the hardcoded macro paths forbid it.)

---

## Appendix

### P0 spike — CONFIRMED (2026-07-12)

- **autumn-web 0.5.0 relaxes `IntoAppLayer`** enough for `tower-governor` and our security-header/CORS layers. The spike compiled these against 0.5.0: `IntoAppLayer` is a **blanket impl over any `tower::Layer<Route>`** meeting axum's service bounds (`app.rs:438-440`); the "sealed" appearance is only a diagnostic wrapper for a cleaner error message, not a restriction (`app.rs:408-416`, docstring `:881-883`). `tower-governor`, `ConcurrencyLimit`, `SetResponseHeader`, custom layers, and `RequireApiToken` all compile-mount. Body-*wrapping* layers (`TraceLayer`, `RequestBodyLimit`) still need axum's normal body-map treatment — axum's own rule, not an autumn seal.
- **`RequireApiToken` is a real `tower::Layer`** and `.secure_mcp` gates `/mcp`; a **custom constant-time `ApiTokenStore` compiles and is accepted** (the trait's `verify`/`issue`/`revoke` extension point). The one security piece autumn does **not** give us is **token→role `AccessClass` RBAC** (`verify` returns only `Option<principal>`; `#[secured]` gates on *session* role) — a thin custom authorization layer (§6(b)).
- **autumn 0.4 and 0.5 cannot coexist in one crate** (structural, separate axis from the layer verdict). autumn-macros 0.5.0 expands `#[get]`/`#[post]`/`#[api_doc(mcp)]` to **hardcoded absolute paths `::autumn_web::…`** (14+ occurrences in `route.rs`) with **no `proc-macro-crate` rename support**; the extern prelude binds `autumn_web` to exactly one version. The standalone 0.5 spike compiles, but a combined `--features http-server,autumn-server` build **fails with 11 type-mismatch errors** (0.5 macro output vs 0.4 `Route`/`ApiDoc` structs). This forces crate-boundary staging (§5.0) and motivates the upstream `proc-macro-crate` ask (§7.1).

### Items still NOT verified in this pass

- The exact **wire shape of autumn 0.5.0's RFC-7807** vs our error model, byte-for-byte (research describes it; not observed in a running 0.5.0).
- Whether the **`maud` workaround** (kept because 0.2.0's `error_pages` imported maud unconditionally, `Cargo.toml:68-70`) is still needed under 0.5.0 with `default-features = false, features = ["mcp"]`.
- That `#[secured]`/session module **compiles cleanly with `default-features = false`** (research expects it to; unverified).
