# 🔭 Vantage Spec: HTTP API Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/http/server.rs` |

## 1. 👤 User Stories

> **As a** Database Administrator,
> **I want to** configure per-IP rate limiting on the AletheiaDB HTTP API,
> **So that** I can protect the database from denial-of-service attacks, brute-force attempts, and overly aggressive client applications.

> **As an** Application Developer consuming the HTTP API,
> **I want to** receive standard `429 Too Many Requests` responses with `Retry-After` headers when I exceed the rate limit,
> **So that** my application can gracefully back off and retry without overwhelming the database.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB's HTTP API (migrated to `autumn-web` in 0.2.0) lacks built-in rate limiting. While users can configure external reverse proxies, this places an additional operational burden on deployments that desire a simple, self-contained database node.

**The Gap:**
- **Security:** Without rate limiting, the database is vulnerable to layer 7 DDoS attacks and resource exhaustion from misconfigured clients.
- **Developer Experience (DX):** `RateLimitConfig` is exposed in the API but silently ignored by the server, leading to operator confusion.
- **Operational Simplicity:** A self-contained, production-ready embedded database should provide basic protections out-of-the-box.

**ROI:**
- **Reliability:** Prevents single noisy neighbors or malicious actors from degrading performance for other users.
- **Trust:** Aligns system behavior with the configuration API, removing "gotchas" for operators.
- **Reduced Infra Complexity:** Eliminates the strict necessity for an external API gateway/proxy for basic deployments.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Native Integration:**
    - The rate limiter MUST be natively integrated into the HTTP server using `autumn` 0.3's built-in per-IP rate limiting capabilities.
    - The `tower-governor` shim workaround MUST NOT be used.

2.  **Configuration Wiring:**
    - The existing `RateLimitConfig` settings (e.g., requests per second, burst capacity) MUST be properly wired into the `autumn` configuration loader.
    - Rate limiting MUST be toggleable via environment variables (e.g., `AUTUMN_RATELIMIT__ENABLED`).

3.  **Client Feedback:**
    - When a client exceeds the limit, the server MUST return an HTTP `429 Too Many Requests` status code.
    - The response MUST include standard rate limit headers (e.g., `Retry-After`, `X-RateLimit-Limit`, `X-RateLimit-Remaining`).

### Non-Functional Requirements

-   **Performance:**
    - The rate limiting middleware MUST introduce minimal latency overhead (< 1ms per request) under normal load.
    - Success = Throughput and latency metrics remain within 5% of the baseline without rate limiting.
-   **Resource Utilization:**
    - The rate limiter state MUST be bounded to prevent memory exhaustion (e.g., dropping stale IP tracking records).

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Rate Limiting:** Synchronizing rate limit counters across multiple database nodes in a clustered deployment. Phase 1 focuses purely on single-node protection.
-   **User/Token-Based Limits:** Rate limiting based on authenticated user IDs or API tokens. Phase 1 is strictly per-IP based.
-   **Dynamic Limit Adjustment:** APIs to change rate limits at runtime without restarting the server.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Rate Limiter Engine** | None (deferred to proxy) | `autumn` 0.3 native limiter | Upgrade `autumn-web` and wire middleware |
| **Config Plumbed** | Ignored | Active | Plumb `RateLimitConfig` into `autumn` |
| **HTTP 429 Responses** | Not generated | Generated natively | Handled by `autumn` middleware |
