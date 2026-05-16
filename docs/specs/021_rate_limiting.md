# 🔭 Vantage Spec: HTTP API Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/http/server.rs`, `src/http/config.rs` |

## 1. 👤 User Stories

> **As a** Database Operator,
> **I want to** enforce a maximum number of requests per second per IP address on the HTTP API,
> **So that** I can protect the database from denial-of-service (DoS) attacks, brute-force scraping, and poorly written client scripts that could degrade performance for other tenants.

> **As an** API Consumer,
> **I want to** receive a clear HTTP 429 Too Many Requests response with a `Retry-After` header when I exceed my quota,
> **So that** my client applications can implement exponential backoff gracefully instead of blindly hammering a failing server.

## 2. 🧐 The "So What?" (Business Value)

During the migration of the HTTP server to `autumn-web` (ADR-0055), native per-IP rate limiting was temporarily deferred because `autumn-web 0.2` did not expose an ergonomic way to attach tower-side limiters like `tower-governor`. The functionality is currently mocked via `RateLimitConfig` in the API schema, but the enforcement layer is missing (`TODO(autumn-0.3)` in `src/http/server.rs`).

**The Gap:**
- **Security & Stability Risk:** A single misconfigured script or malicious actor can exhaust server resources (CPU, connection pool, memory) by flooding the `/query` endpoint, degrading performance for all other users.
- **Operator Burden:** Operators are forced to rely on external reverse proxies (like NGINX or Envoy) to implement basic rate limiting, increasing infrastructure complexity.

**ROI:**
- **Resilience:** Built-in protection against the noisy-neighbor problem and simple DoS vectors.
- **Reduced Infrastructure Overhead:** Smaller deployments can safely expose the database directly to internal networks or edges without deploying an API gateway just for throttling.
- **Improved DX:** Predictable failure modes (429s) encourage clients to build robust retry mechanisms.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1. **Per-IP Enforcement:**
   - The server MUST enforce the `requests_per_second` and `burst_size` defined in the existing `RateLimitConfig` on a per-IP basis.
2. **Rejection Semantics:**
   - When a client exceeds its quota, the server MUST return an HTTP 429 Too Many Requests status.
   - The response MUST include a standard `Retry-After` header indicating how long the client should wait before retrying.
3. **Configuration Wiring:**
   - The feature MUST properly consume the existing `ALETHEIADB_RATE_LIMIT__REQUESTS_PER_SECOND` and `ALETHEIADB_RATE_LIMIT__BURST_SIZE` environment variables (or their TOML equivalents) to initialize the limiter.

### Non-Functional Requirements

- **Performance:** Tracking request rates MUST NOT add more than 1ms of latency per request under normal load.
- **Metric Definition:** Success = A load test simulating 100 requests per second from a single IP against a 10 req/s limit results in exactly 10 successful requests and 90 HTTP 429 responses per second, while a concurrent test from a *different* IP experiences 0% throttling.

## 4. 🚫 Out of Scope (Phase 1)

- **Distributed Rate Limiting:** Synchronizing rate limits across multiple AletheiaDB nodes (e.g., via Redis) is out of scope. Rate limiting is enforced per-process.
- **Authentication-Based Throttling:** Rate limiting by API key or user token is deferred. Phase 1 strictly uses client IP addresses.
- **Dynamic Quota Updates:** Changing rate limits without restarting the HTTP server process.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Config Schema** | `RateLimitConfig` exists and validates | No change | None |
| **Enforcement Layer** | None (deferred via TODO) | `tower-governor` or `autumn 0.3` native middleware | Wire middleware in `src/http/server.rs` |
| **HTTP Responses** | Always processes request | Returns 429 with `Retry-After` | Verify middleware headers |
| **Testing** | Fails to test rate limit behavior | Integration tests for 429 responses | Add tests in `src/http/server.rs` |
