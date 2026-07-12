# Spike: Migrating AletheiaDB HTTP+MCP to autumn-web 0.5.0

Issue: #3524 · Branch: `feature/autumn-web-spike` · Status: **spike complete, green**

## Goal

Prove/disprove porting one authenticated endpoint (`GET /nodes/{id}`) to
autumn-web 0.5.0 idioms (`#[get]` + `#[api_doc(mcp)]`), projecting the same
handler definition to **HTTP**, **MCP-over-HTTP** (`/mcp`), and **OpenAPI**
(`/openapi.json`) from a single annotation, while preserving bearer auth
(constant-time, on-by-default), RBAC, structured errors, body limits, and OTel
spans.

## Outcome

**Feasible — via an isolated crate, not an in-place slice.** The slice is
implemented in a new workspace member `crates/autumn-spike`
(`aletheia-autumn-spike`) that depends on autumn-web 0.5 **unrenamed** and reuses
AletheiaDB's shared public core (constant-time `AuthStore`, `AccessClass`/`Role`
RBAC, the `POST /query` GetNode node serializer, the `{success,data}` envelope,
`get_node`). One `#[get("/nodes/{id}")] #[api_doc(mcp)]` handler drives all three
surfaces; 10 integration tests (cases a–i + a tower-layer demo) pass.

## Approaches considered

1. **Renamed-dep parallel module, same crate** — add autumn-web 0.5 under a
   renamed key (`autumn-web-05`) behind a feature, parallel `src/autumn_spike/`.
   **REJECTED — disproven, does not compile** (see Findings §1): autumn-macros
   0.5 hard-codes `::autumn_web`, which cannot resolve to the 0.5 crate while the
   existing `src/http` binds `::autumn_web` to 0.4.
2. **Isolated workspace crate (CHOSEN)** — `crates/autumn-spike` depends on
   autumn-web 0.5 unrenamed (so `::autumn_web` = 0.5 there) plus `aletheiadb`
   (path dep) for the shared public API. Additive, zero risk to `src/http`; the
   root crate's gates are untouched (`default-members = ["."]`).
3. **Whole-repo 0.4→0.5 bump** — REJECTED for a spike: modifies `src/http`,
   large blast radius. (It IS one of the two viable shapes for the *real*
   migration — see Findings §1.)
4. **Stay on 0.4** — REJECTED: doesn't evaluate 0.5's MCP/OpenAPI projection.

## Design of the chosen slice

```
crates/autumn-spike/
├── Cargo.toml         # autumn-web 0.5 (maud,mcp; no diesel) + aletheiadb path dep
└── src/
    ├── handler.rs     # #[get("/nodes/{id}")] #[api_doc(mcp)] → HTTP+MCP+OpenAPI
    ├── auth.rs        # SpikeAuth extractor (RBAC) + AuthStoreTokenAdapter (native ApiTokenStore)
    ├── state.rs       # SpikeState extractor (Arc<AletheiaDB> via AppState extension)
    ├── error.rs       # SpikeError → flat {success,error,code,retriable,details}
    └── app.rs         # TestApp: routes + .openapi + .mount_mcp + .layer(...)
```

- **One definition → three surfaces.** `#[get] + #[api_doc(mcp)]` emits an
  `ApiDoc` consumed by both the OpenAPI generator and the MCP tool derivation;
  the MCP `inputSchema` is derived from the typed signature (the `id` path
  param), so it cannot drift.
- **Auth reuse.** `SpikeAuth` is a `FromRequestParts` extractor (autumn 0.5's
  sealed-`IntoAppLayer` no longer forces this, but it's the cleanest way to
  carry a *role*) that verifies the bearer/`x-api-key` credential against the
  shared `AuthStore` (constant-time) and enforces `AccessClass::Read`. MCP
  `tools/call` replays through the same router forwarding the `Authorization`
  header, so auth applies identically to HTTP and MCP.
- **Byte-parity — success AND error.** The handler calls the *same*
  `node_to_query_json` + `ApiResponse::success` the `POST /query` GetNode path
  uses, and returns the *same* `AletheiaHttpError` type on every failure — so
  success **and** error bodies are byte-identical, asserted against the live 0.4
  router (`trace_id` stripped). No invented richer error shape (an earlier draft
  added `code`/`retriable`/`details` to every error and a `required_class` on
  403; that diverged from the existing flat surface and was dropped — see
  Finding §3).
- **Native token layer, actually wired.** `AuthStoreTokenAdapter` implements
  autumn's `ApiTokenStore` (delegating `verify` to the shared `AuthStore`), and
  the app gates the whole `/mcp` endpoint with
  `.secure_mcp(RequireApiToken::new(...))` in `Required` mode — so the MCP
  catalog is **not** anonymously reachable (auth-on-by-default parity;
  `Anonymous` mode skips the gate deliberately).
- **OTel span, preserved.** Under the crate's `observability` feature the
  handler wraps its work in a `tracing` request root span (method + route +
  `operation`, recording `result_count`/`error.code`/`otel.status_code`),
  mirroring `src/http/trace.rs` (Issue #3376) and enabling `aletheiadb`'s own
  `observability` so DB child spans nest underneath. Off by default → compiled
  out to a no-op, so handler behavior and the 12 tests are identical either way
  (both feature states are gated and tested). A full port would layer the
  W3C-`traceparent` propagation (the `otel`-gated half of `trace.rs`) on top —
  the extractor pattern carries over unchanged.

## Risks → tests (all pass)

Every error case asserts the spike body is **byte-identical** to the same
request through the existing `POST /query` router (`build_test_router_with_auth`),
the per-request `trace_id` stripped before comparison — parity by construction,
since both reuse `AletheiaHttpError`.

| Case | Risk covered |
|------|--------------|
| a | Unauthenticated GET → 401 uniform `UNAUTHENTICATED` (+ `WWW-Authenticate`); **body == `/query`** |
| b | Reader token → 200, body **byte-identical** to `POST /query` GetNode (node carries a **vector** → raw, non-elided) |
| c | Metrics-only token (lacks Read) → 403 `PERMISSION_DENIED`; **body == `/query`** (flat, no `details`) |
| d | Missing node → `NOT_FOUND`; **body == `/query`** |
| e | id > `MAX_VALID_ID` → `INVALID_ARGUMENT`; **body == `/query`** |
| e2 | Non-numeric id `/nodes/abc` → structured `INVALID_ARGUMENT` envelope (not axum's plain-text 400) |
| f | `/mcp` `tools/list` (authenticated) advertises `get_node` with an `id` inputSchema; `jsonrpc`/`id` echoed |
| f-neg | `/mcp` catalog with **no** token → 401 (native `RequireApiToken` gate; auth-on-by-default parity) |
| g | `/mcp` `tools/call` payload equals the HTTP GET body incl. the raw vector (replay parity); `jsonrpc`/`id` echoed |
| h | `/mcp` `tools/call` wrong-role → tool error whose parsed body carries `code: PERMISSION_DENIED`; no-token → 401 envelope reject |
| i | `/openapi.json`: `/nodes/{id}` path, `id` param required, 200 → `$ref` `ApiResponse` |
| layer | An arbitrary `tower::Layer` mounts via `.layer()` (ADR 0055 sealed bound lifted) |

## Findings

### 1. The dual-version constraint (the central migration decision)

**A slice-by-slice port inside the existing crate is impossible while the repo
stays on autumn 0.4** — autumn-macros 0.5 hard-codes `::autumn_web`. A real
migration is therefore either **an atomic whole-repo 0.4→0.5 bump** OR **an
isolated crate boundary** (this spike). There is no incremental middle path that
lets 0.4 and 0.5 macros coexist in one crate.

Why: autumn-macros 0.5's route/`api_doc` macros expand to absolute paths
`::autumn_web::Route`, `::autumn_web::openapi::ApiDoc { … mcp_tool … }`,
`::autumn_web::RouteIdempotency`, etc., with **no `proc-macro-crate` support and
no `crate = "..."` override**. A single crate's extern prelude binds the name
`autumn_web` to exactly one crate version. Because `src/http` (0.4 macros) needs
`::autumn_web` = 0.4 and a spike module (0.5 macros) needs `::autumn_web` = 0.5,
and `--all-features` links both, the two are irreconcilable in one crate. Proven
empirically: with both features active the 0.5-macro output fails against 0.4
types with 11 errors (`struct autumn_web::Route has no field named api_version`,
`ApiDoc has no field named mcp_tool`, …). The renamed-dep alias trick fixes the
standalone build but cannot fix `--all-features`. The isolated crate sidesteps
this entirely (its extern prelude's `autumn_web` *is* 0.5).

### 2. Token→role gap in autumn's native auth

autumn 0.5's `ApiTokenStore::verify` returns only `Option<String>` (a principal
id) — **no role**. autumn's RBAC is session-based and orthogonal to the token
store. So `RequireApiToken` alone proves a token is *valid* but cannot gate by
`AccessClass`. A full port keeps AletheiaDB's role model: verify against the
shared `AuthStore` and enforce `Role::allows(class)` in a thin adapter
(`SpikeAuth` here). The spike ships both: the native `ApiTokenStore` adapter
(for `secure_mcp`) **and** the role-aware extractor.

### 3. HTTP vs MCP error-envelope divergence (real finding, NOT smuggled in)

The two existing surfaces disagree on error shape:

- **HTTP** `AletheiaHttpError` renders a **flat** body
  (`{success,error[,code]}`) and emits `code` *only* for the auth/limit
  variants — `NotFound`/`BadRequest` carry **no** `code`, and
  `PermissionDenied` carries **no** `details`.
- **MCP** `McpError` renders a **nested** `{"error":{code,message,retriable,
  details}}` body and its `PERMISSION_DENIED` carries
  `details.{required_class,principal_role}`.

The mission requires the spike **preserve** the existing HTTP envelope, so the
spike reuses `AletheiaHttpError` verbatim: its error bodies are byte-identical
to `POST /query` (asserted). It does **not** invent a richer shape. The
divergence itself — HTTP's flat envelope is *thinner* than MCP's #3234 nested
one — is the finding: a full port should **unify** both surfaces, bringing HTTP
up to the #3234 `{code,retriable,details}` contract (including
`details.required_class`/`principal_role` on 403). That is a deliberate
full-port enhancement, cheap during the bump, and is the *only* place
`required_class` belongs — not on the parity path.

### 4. Arbitrary tower layers now mount (ADR 0055 sealed bound lifted)

0.5's `IntoAppLayer` accepts **any** `tower::Layer<Route>` with an `Infallible`
service. The `layer` case mounts a *response-only* `tower-http`
`SetResponseHeaderLayer` via `.layer()` and asserts its header on the response —
proving 0.5 accepts a general `tower::Layer`, which is the real point (0.4's
sealed bound rejected them). The demo does **not** itself rate-limit; but
tower_governor's per-IP `GovernorLayer` has exactly the same
`tower::Layer<Route>`/`Infallible` shape, so HTTP-layer rate limiting can return
during the port. `.secure_mcp(layer)` accepts the same layer shape and is used
here (Finding §2/§3 wiring) to gate `/mcp`. (A genuine request-short-circuiting
429 layer + assertion is a trivial add if the full port wants it in-tree.)

### 5. Diesel is fully excludable; baseline is heavier

`default-features=false, features=["maud","mcp"]` pulls **0** diesel deps
(verified). `mcp` ⇒ `openapi` ⇒ `serde_yaml`. 0.5's always-on baseline is larger
than 0.4+maud (jsonwebtoken, validator, bcrypt, tokio-cron-scheduler, aes-gcm,
uuid, …) but carries no DB pool.

### 6. Vector properties are NOT elided on this HTTP path (parity confirmed)

This `POST /query` / REST path serializes node properties via
`property_value_to_json`, which emits vector/embedding properties as **raw
float arrays** — it does **not** apply the #3220 `{type,dim,elided}` elision.
That elision lives only on the MCP-**stdio** node serializer
(`node_to_response`, `src/mcp/server.rs`). The fixture node carries an
`embedding` vector and cases b/g assert the raw array survives byte-parity
across GET / `POST /query` / `/mcp` tools/call. A full port must keep this
distinction in mind: the autumn `/mcp` tool here replays the REST body, so it
inherits *no* elision — if elision is wanted on the HTTP/MCP-over-HTTP surface,
it is new work, not inherited from the stdio server.

### 7. Inherited latent behavior: 404 masks backend errors

The reused `get_node` path maps *any* `db.get_node` error to `NOT_FOUND` (a real
backend/IO error is reported as "node not found"). The spike preserves this
verbatim for parity; it is a pre-existing latent issue in `handle_get_node`, and
distinguishing genuine-missing from backend-failure is a full-port follow-up
(not introduced here).

### 8. What autumn-web 0.5.0 is missing for a full port

- **Rename/multi-version support in the macros** (Finding §1) — the single
  biggest constraint; forces the whole-repo-bump-or-isolated-crate choice.
- **Role-aware token auth** (Finding §2) — a custom adapter is required.
- **A unified structured-error contract** matching #3234 across HTTP+MCP
  (Finding §3) — must be hand-built.
- Everything else the current `src/http` relies on (extractor-delivered state,
  W3C-trace root span as an extractor, `DefaultBodyLimit`) carries forward
  unchanged; OTel spans remain feature-gated no-ops when `observability` is off.

### Effort estimate for the full port

Assuming the **whole-repo atomic 0.4→0.5 bump** (the pragmatic choice — one
crate, no permanent two-version boundary):

- **Dependency + build surface**: ~0.5 day. Bump the pin, drop the renamed-dep
  workaround, re-audit features (serde_yaml, heavier baseline), add `http-server`
  (and any new MCP feature) to the CI matrix.
- **Route migration** (`/status`, `/query`, `/admin/keys*`): ~2–3 days. The
  `POST /query` polymorphic handler and the admin key-lifecycle handlers are the
  bulk; extractors (state/auth/trace) port near-verbatim.
- **Auth/RBAC adapter + unified error envelope** (Findings §2/§3): ~1–1.5 days.
- **MCP-over-HTTP surface**: ~1–2 days to decide scope — either expose existing
  read tools via `#[api_doc(mcp)]` on new REST routes, or keep the stdio `rmcp`
  server and treat autumn `/mcp` as an additional transport. (These are
  *different* MCP stacks; reconciling them is a design decision, not just code.)
- **Rate limiting re-enable + tests + docs** (Finding §4): ~1 day.

**Total ≈ 6–9 engineer-days** for a faithful port of the current HTTP surface,
excluding the MCP-stack reconciliation decision (which could add materially if
the stdio `rmcp` server is folded into autumn's `/mcp`).

## Upstream issue (madmax983/autumn)

**Filed: [madmax983/autumn#1828](https://github.com/madmax983/autumn/issues/1828)**
— "autumn-macros: macro expansion hardcodes `::autumn_web` paths — breaks
renamed/parallel-version deps (proc-macro-crate support)". The drafted text is
kept below for the record.

> **Title:** autumn-macros: proc-macro output hardcodes `::autumn_web`, breaks
> renamed / multi-version deps
>
> **Body:**
> The route macros (`#[get]`/`#[post]`/…) and `#[api_doc]` in `autumn-macros`
> expand to **absolute** paths rooted at `::autumn_web` (e.g.
> `::autumn_web::Route`, `::autumn_web::openapi::ApiDoc`,
> `::autumn_web::RouteIdempotency`, `::autumn_web::reexports::…`). There is no
> `proc-macro-crate` resolution and no `crate = "..."` attribute override, so:
>
> 1. A crate that depends on `autumn-web` under a **renamed** Cargo key (e.g.
>    `autumn-web-05 = { package = "autumn-web" }`) cannot use the macros — the
>    generated `::autumn_web` path does not resolve
>    (`error[E0433]: could not find 'autumn_web' in the list of imported
>    crates`).
> 2. A crate that must host **two** autumn-web versions (e.g. during an
>    incremental framework upgrade) cannot use both sets of macros — only one
>    crate may own the extern-prelude name `autumn_web`.
>
> **Repro:** depend on autumn-web 0.4 (unrenamed) and 0.5 (renamed) in one crate;
> using the 0.5 `#[get]`/`#[api_doc(mcp)]` yields ~11 type-mismatch errors
> because the emitted `::autumn_web::{Route,openapi::ApiDoc}` resolve to the 0.4
> types (`struct autumn_web::Route has no field named api_version`;
> `ApiDoc has no field named mcp_tool/mcp_exclude/mcp_stream`; etc.).
>
> **Impact:** forces framework upgrades to be an all-or-nothing whole-crate bump;
> a gradual, slice-by-slice migration inside one crate is impossible.
>
> **Suggested fix:** adopt the `proc-macro-crate` crate to resolve the actual
> (possibly renamed) dependency name in macro output, and/or accept a
> `#[api_doc(crate = "...")]` / route-attr `crate = "..."` override (as serde,
> sqlx, and similar do). This would let downstreams rename the dep and host two
> versions during a migration.

*(Filed upstream as madmax983/autumn#1828.)*

## Gates (isolated `CARGO_TARGET_DIR`)

| Gate | Result |
|------|--------|
| `cargo fmt --all --check` | pass |
| `cargo clippy -p aletheia-autumn-spike --all-targets -- -D warnings` | pass |
| `cargo test -p aletheia-autumn-spike` | 12 passed |
| `cargo clippy/test -p aletheia-autumn-spike --features observability` | pass · 12 passed (OTel span compiles + no-ops correctly) |
| `cargo check -p aletheiadb --no-default-features --tests --features http-server` | pass (widening didn't break standalone) |
| `cargo clippy -p aletheiadb --features "config-toml,mcp-server,sharding-rpc,simulation" --all-targets -- -D warnings` | pass (root CI set unaffected) |
| `cd python && cargo metadata` · `cd fuzz && cargo metadata` | pass (workspace `exclude` keeps them independent) |

`just spike-check` runs the spike's own clippy + tests. **CI wiring for the new
crate is a follow-up** (add `-p aletheia-autumn-spike` clippy/test to
`.github/workflows/ci.yml`, and consider adding `http-server` to the CI matrix
per recon caveat #4).

## Touch summary (additive only)

- **New:** `crates/autumn-spike/**`, this doc.
- **Root `Cargo.toml`:** added `[workspace]` — `members = ["crates/autumn-spike"]`,
  `default-members = ["."]` (bare cargo only touches the root), and
  `exclude = ["python", "fuzz"]` (those independent crates keep their own
  manifests / CI).
- **`justfile`:** added `spike-check`.
- **`src/http` (minimal, additive, zero behavior change):**
  - new `pub fn converters::node_to_query_json` (extracted from the inline
    `handle_get_node` body it now calls — byte-identical), re-exported as
    `pub use converters::node_to_query_json` (the module stays `pub(crate)` —
    only the one fn is exposed, not the whole module);
  - `ApiResponse::success` `pub(crate)`→`pub`.

  Each carries an `// exposed for autumn-web migration spike` marker. The error
  envelope needed **no** widening: `AletheiaHttpError` and its variants are
  already `pub`, so the spike reuses it directly (byte-identical error bodies).
