# ADR 0055: Migrate HTTP server from actix-web to autumn-web

## Status

Accepted — 2026-04-19. **Partially superseded — 2026-07-13** (see
[Supersession: rate limiting returns under autumn 0.5](#supersession-rate-limiting-returns-under-autumn-05) below).

## Supersession: rate limiting returns under autumn 0.5

The migration landed on autumn-web **0.5** (not the 0.2/0.3 line this ADR was
drafted against), and the production server now lives in the dedicated
`aletheia-server` crate (Issue #3524). Two of the deferrals recorded under
[Decision](#decision) are lifted by that move:

-   **Rate limiting is no longer deferred.** This ADR judged a custom
    tower/middleware rate limiter impractical because autumn 0.4's sealed
    `IntoAppLayer` rejected arbitrary `tower::Layer`s, and it bet on autumn
    0.3 shipping native per-IP rate limiting. Neither held: autumn **0.5**
    relaxes that bound so an arbitrary `tower::Layer` — including
    `tower_governor::GovernorLayer` — mounts through `.layer()`, and no native
    limiter shipped. Rate limiting therefore returns as a **default-off**
    `tower-governor` layer built by
    `aletheia_server::security::rate_limit::governor_layer`, which yields
    `None` unless the operator opts in via `SecurityConfig::rate_limit`. Default
    behavior is byte-for-byte unchanged (no layer, no `429`s), so this
    supersedes the deferral without changing the shipped default. The
    "[Retire `tower-governor`](#follow-up-work)" follow-up is likewise moot —
    `tower-governor` is retained, not retired.
-   **The `ConnectInfo` test-harness gap is unchanged.** The negative
    consequence that `PeerIpKeyExtractor` needs `ConnectInfo<SocketAddr>` (only
    populated by `axum::serve()` at the TCP layer) still stands; the new crate's
    `security::rate_limit` acceptance tests insert `ConnectInfo` by hand when
    driving the layer via `tower`'s `oneshot`, exactly as noted here.

The rate limiter arrives alongside the other per-query security **primitives**
in the `aletheia-server::security` module (per-query timeout / row / byte caps
and a bounded in-flight guard, Issues #3542 / #3550; signed opaque cursor
tokens, Issue #3360). These are the building blocks; the wiring PR that mounts
them via `apply_security` (and wraps governor's raw `429` into the Issue #3234
`{code, retriable, ...}` envelope) is tracked under the autumn migration §8 /
Issue #3561 plan. This note records the reversal; it does not re-open the ADR's
other decisions.

## Context

AletheiaDB's `http-server` feature shipped an actix-web 4 stack (actix-cors,
actix-rt, actix-governor) exposing two JSON endpoints: `GET /status` and
`POST /query`. The surface was small (~2,500 LOC of handlers + middleware,
~790 LOC of tests) but the framework footprint — actix-web's actor runtime,
its own async worker pool, four crates of middleware — was disproportionate
for what amounts to a thin JSON API on top of an embedded database.

Two forces pushed a change *now* rather than later:

1.  **Ecosystem alignment.** Other in-house Rust projects are converging on
    `autumn-web` — the author's own Spring Boot-style framework, built on
    Axum. Running the same HTTP story across projects pays dividends in
    shared skills, shared middleware, and shared operational patterns.
2.  **Near-term management dashboard.** A dashboard (query runner, node/edge
    browser, metrics inspection) is on the roadmap. autumn ships Maud +
    htmx + static-file serving + hybrid rendering as first-class features,
    so the dashboard becomes "add `/admin` routes" rather than
    "spin up a separate SPA project." This payoff does not exist for a
    straight Axum migration, and certainly not for staying on actix-web.

AletheiaDB is also approaching its 0.1.0 release. This is the right pre-1.0
window to break and reset the HTTP layer; after 0.1.0 we owe downstream
consumers migration paths.

## Decision

Replace actix-web with `autumn-web` 0.2.0 for the `http-server` feature.

-   Keep the `http-server` feature flag name so downstream consumers' Cargo
    feature wiring is unchanged.
-   Keep all `ALETHEIADB_*` environment variables, the `ServerConfig` /
    `CorsConfig` / `RateLimitConfig` types, and the JSON request/response
    shape unchanged. The migration is *plumbing-only*; downstream clients
    see identical wire behavior with one exception noted below.
-   **Rate limiting is deferred.** The pre-migration stack used
    `actix-governor` for per-IP rate limiting. The tower-side equivalent
    (`tower-governor` 0.8) does not satisfy autumn's sealed `IntoAppLayer`
    bound in the obvious form, and writing a short-lived wrapper is a poor
    use of effort given autumn 0.3 will ship native per-IP rate limiting.
    `RateLimitConfig` is preserved in the public API; the HTTP layer
    simply does not attach a limiter. Operators can enforce rate limits at
    the reverse-proxy layer in the interim (nginx, Caddy, Envoy). See the
    `TODO(autumn-0.3)` marker in `src/http/server.rs`.
-   **Custom middleware layers are deferred.** Similarly, the explicit
    security-header / CORS / tracing layers that the actix stack applied
    directly do not chain onto autumn's `AppBuilder` as freely as they
    chained onto `actix_web::App`. autumn provides equivalent capabilities
    as framework-level middleware (tracing, request IDs, security headers
    are all part of its default stack; CORS can be configured through
    `autumn.toml` / `AUTUMN_CORS__*` env vars). `build_test_router` still
    applies our original tower-http layers so the integration tests
    exercise the exact header/CORS behavior we care about, just not in
    production until autumn's layer ergonomics catch up.

### Scope in this PR

-   Dependency swap in `Cargo.toml`.
-   Full rewrite of `src/http/server.rs`, `src/http/state.rs`,
    `src/http/handlers.rs`, `src/http/mod.rs`, plus a new
    `src/http/error.rs` for the unified `AletheiaHttpError` type.
-   `#[actix_web::main]` → `#[autumn_web::main]` in `src/bin/server.rs`.
-   All three integration test files ported from `actix_web::test` to
    `autumn_web::test::TestApp` / `TestClient`.
-   Docs updated; this ADR.

### Not in scope

-   The management dashboard itself (separate follow-up PR).
-   Any API additions or breaking changes beyond the status-code note below.
-   Changes to the MCP server (`aletheia-mcp`), which remains untouched.

## Consequences

### Positive

-   **One HTTP framework across the ecosystem.** Future projects and the
    dashboard plug into the same stack.
-   **Tokio-native.** Handlers already offloaded blocking work via
    `actix_web::web::block`; that becomes `tokio::task::spawn_blocking`,
    the standard tokio primitive. No more actix-rt.
-   **First-class testing harness.** `autumn_web::test::TestApp` +
    `TestClient` gives a fluent request builder and assertion helpers
    roughly modeled on Spring Boot's `MockMvc`. The actix test harness
    used a lower-level builder/dispatcher pattern; the new one is easier
    to read.
-   **Dashboard groundwork.** Maud + htmx + hybrid rendering are one
    `cargo feature` away when the dashboard lands.

### Neutral

-   **Status-code semantics for malformed payloads.**
    actix-web collapsed all `Json<T>` failures into `400 Bad Request`.
    axum distinguishes:
    -   Unparseable JSON bytes → `400 Bad Request`
    -   Parseable JSON, schema mismatch → `422 Unprocessable Entity`

    axum's split is RFC 9110-correct. Clients that relied on "any broken
    request → 400" will now see `422` for missing/wrong-typed fields.
    `warden_http_panic.rs` tests have been updated to assert the two
    codes independently.

### Negative / accepted tradeoffs

-   **Opinionated default features.** `autumn-web`'s default feature set
    pulls in Postgres+Diesel, Moka, Maud, htmx, Tailwind — none of which
    AletheiaDB uses. We depend with `default-features = false` and enable
    only `maud` (which autumn 0.2.0 requires unconditionally due to a
    compile-gate bug in its `error_pages` module; this bug is worth
    reporting upstream).
-   **No programmatic shutdown handle for tests.** autumn's `.run()` owns
    signal handling and doesn't return a shutdown channel. The actix-era
    `create_server` + `ShutdownHandle` pair has been removed. Integration
    tests no longer boot a real server; they exercise the router in-process
    via `tower::ServiceExt::oneshot` (the `TestApp::from_router` path).
    This is strictly more hermetic — no TCP listener, no port-bind race —
    at the cost of not testing the graceful-shutdown path end-to-end.
    autumn's own tests cover that path.
-   **Test harness cannot populate `ConnectInfo`.** `tower-governor`'s
    `PeerIpKeyExtractor` requires `ConnectInfo<SocketAddr>` on each
    request, which is only populated by `axum::serve()` at the TCP layer.
    The in-process `TestApp` path therefore skips the rate-limit layer
    entirely. Rate limiting is exercised in production and can be
    smoke-tested with a real `curl` flood against `cargo run
    --bin aletheia-server`.
-   **Environment-variable plumbing relies on `unsafe std::env::set_var`.**
    `run_server` translates our `ALETHEIADB_HOST` / `ALETHEIADB_PORT` into
    `AUTUMN_SERVER__HOST` / `AUTUMN_SERVER__PORT` before handing control
    to autumn. In Rust 2024 `set_var` is `unsafe` because concurrent
    reads from other threads are UB. We document and minimise the window
    (the writes happen before autumn spawns any tasks) but a cleaner
    path is a custom `ConfigLoader` impl; that's deferred.

## Follow-up work

-   **Dashboard PR** — add `maud` + `htmx` features, add `/admin` routes,
    first real Maud page (status overview).
-   **Retire `tower-governor`** when autumn 0.3 ships per-IP rate
    limiting OOTB. The `TODO(autumn-0.3):` marker in
    `src/http/server.rs` is the breadcrumb.
-   **Custom `ConfigLoader`** replacing the `env::set_var` bridge in
    `run_server`, so ops-facing config stays `ALETHEIADB_*`-only without
    touching autumn's env-var conventions.
-   **Upstream autumn bug**: `error_pages` module imports `maud`
    unconditionally — breaks `default-features = false` consumers that
    don't enable `maud`. Tracked at time of writing only in this ADR.

## References

-   Plan: `C:\Users\markm\.claude\plans\swift-percolating-fiddle.md` (session
    plan that drove this PR).
-   [`autumn-web` 0.2.0 on crates.io](https://crates.io/crates/autumn-web/0.2.0)
-   [`autumn` repo](https://github.com/madmax983/autumn)
-   `src/http/server.rs` — `run_server`, `build_test_router`, layer wiring.
-   `docs/guides/http-state-management.md` — rewritten for the autumn model.
