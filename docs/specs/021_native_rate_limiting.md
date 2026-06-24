# 🔭 Vantage Spec: Native Per-IP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/http/server.rs` |

## 1. 👤 User Stories

> **As a** Database Administrator running a public-facing AletheiaDB instance,
> **I want to** automatically limit the number of HTTP requests a single IP address can make within a given time window,
> **So that** a single malicious or runaway client cannot exhaust database resources and degrade the performance for other users (preventing DoS attacks).

> **As a** SaaS Platform Architect utilizing AletheiaDB as a backend,
> **I want to** apply predictable burst and sustained request limits natively within the database,
> **So that** I don't have to manage complex external reverse proxies (like Nginx or Envoy) just for basic API protection, simplifying my deployment architecture.

## 2. 🧐 The "So What?" (Business Value)

During the migration from `actix-web` to `autumn-web` (ADR 0055), native per-IP rate limiting was explicitly deferred because `autumn-web` 0.2 did not support it. This left a gap where operators are forced to deploy reverse proxies to protect the database from being overwhelmed.

**The Gap:**
- **Security:** Without native rate limiting, the database is vulnerable to Denial of Service (DoS) attacks at the application layer.
- **Developer Experience (DX) & Deployment:** Forcing users to deploy and configure an external proxy (Nginx/Caddy) to get basic rate limiting violates our "batteries-included, zero-dependencies" philosophy.
- **Unfinished Business:** The `src/http/server.rs` contains a `TODO(autumn-0.3)` marker waiting for this feature to be implemented.

**ROI:**
- **Security Posture:** Immediate protection against abuse out-of-the-box.
- **Operational Simplicity:** A single configuration file (`RateLimitConfig`) governs the entire database's protection layer, rather than splitting configuration across the DB and a proxy.
- **Completeness:** Fulfills the promise made during the HTTP server migration, closing the feature parity gap with the old actix-based server.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Native Autumn Integration:**
    - The HTTP server MUST wire up native per-IP rate limiting using `autumn-web`'s rate limiting middleware (available in autumn 0.3+).
    - It MUST replace the existing `TODO(autumn-0.3)` marker in `src/http/server.rs`.
2.  **Configuration Driven:**
    - The rate limiter MUST correctly consume the existing `RateLimitConfig` struct (specifically `requests_per_second` and `burst_size`).
    - The rate limiter MUST be enabled or disabled based on the `enabled` boolean in `RateLimitConfig`.
3.  **HTTP Response Constraints:**
    - When a client exceeds the limit, the server MUST return an HTTP 429 Too Many Requests status code.
    - The response MUST NOT contain sensitive stack traces or internal state.

### Non-Functional Requirements

-   **Performance:** The overhead of the rate limiting middleware MUST be < 1ms per request under normal load. It MUST NOT block the async executor or cause thread starvation.
-   **Metric Definition:** Success = A load test simulating 100 req/s from a single IP against a configured 10 req/s limit correctly rejects exactly 90 requests per second with HTTP 429 without database memory bloat.

## 4. 🚫 Out of Scope (Phase 1)

-   **Per-User / Token Rate Limiting:** Phase 1 strictly implements IP-based rate limiting. Rate limiting based on authenticated API keys or GraphQL complexity scores is deferred.
-   **Distributed Rate Limiting:** Synchronizing rate limits across a clustered AletheiaDB deployment. Phase 1 relies on local, in-memory counters per node.
-   **Dynamic Limit Reloading:** Changing the rate limit configuration requires a server restart in Phase 1.
