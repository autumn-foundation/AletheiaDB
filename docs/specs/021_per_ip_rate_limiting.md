# 🔭 Vantage Spec: Per-IP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/http/server.rs` |

## 1. 👤 User Stories

> **As an** Application Administrator running AletheiaDB in production,
> **I want to** apply per-IP rate limiting directly within the database's HTTP server,
> **So that** I can protect my graph from denial-of-service (DoS) attacks, abusive bots, and poorly-written automated clients without requiring an external proxy.

> **As a** Managed Service Provider (MSP) offering AletheiaDB hosting,
> **I want to** define and enforce API request quotas per IP address,
> **So that** I can ensure fair usage across all tenants and prevent a single heavy user from degrading the database performance for others.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB's HTTP layer (migrated to `autumn-web` 0.2.0) lacks built-in per-IP rate limiting. The previous `actix-web` stack supported this via `actix-governor`, but it was temporarily removed during the migration (as noted by a `TODO(autumn-0.3)` in `src/http/server.rs`).

**The Gap:**
- **Security & Stability:** Without rate limiting, the HTTP API is vulnerable to floods of requests that can exhaust connection pools, memory, or CPU resources, bringing down the database.
- **Operational Complexity:** Operators are currently forced to deploy and configure a reverse proxy (like Nginx, Envoy, or Caddy) in front of AletheiaDB just to achieve basic rate limiting.

**ROI:**
- **Simplified Deployment:** A "batteries-included" database that is safe to expose to semi-trusted networks out of the box, reducing the total cost of ownership (TCO) and infrastructure overhead.
- **Reliability:** Guarantees database availability even under sudden spikes in traffic or abusive behavior.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1. **Native Per-IP Rate Limiting Integration:**
   - The system MUST integrate the native per-IP rate limiting functionality expected in `autumn-web` 0.3.0.
   - The rate limiter MUST accurately track and throttle incoming HTTP requests based on the client's IP address.

2. **Configuration Surface:**
   - The existing `RateLimitConfig` structure MUST be wired to the new implementation.
   - Operators MUST be able to configure:
     - `enabled`: Toggle rate limiting on/off.
     - `requests_per_second`: The maximum allowed requests per second per IP.
     - `burst_size`: The maximum allowed burst of requests.

3. **HTTP Responses:**
   - When a client exceeds their rate limit, the server MUST immediately return a `429 Too Many Requests` HTTP status code.
   - The response SHOULD include standard rate-limiting headers (e.g., `Retry-After`, `X-RateLimit-Limit`, `X-RateLimit-Remaining`) to communicate the throttle state to the client.

### Non-Functional Requirements

- **Performance:** Tracking IP requests MUST introduce negligible overhead (< 1ms per request). It should not block the main async event loop.
- **Metric Definition:** Success = A load test sending 2x the configured `requests_per_second` results in exactly 50% `200 OK` and 50% `429 Too Many Requests`, with database CPU utilization remaining stable.

## 4. 🚫 Out of Scope (Phase 1)

- **Distributed Rate Limiting:** Synchronizing rate limits across multiple AletheiaDB shards or instances (e.g., via Redis). Rate limiting will be strictly local to the individual node's memory.
- **Per-User/Token Rate Limiting:** Throttling based on API keys, JWTs, or user IDs. Phase 1 is strictly IP-based.
- **Dynamic Quotas:** Adjusting rate limits on the fly without a service restart or config reload.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Per-IP Limiting** | Deferred (`TODO(autumn-0.3)`) | Natively integrated | Wire `RateLimitConfig` to autumn 0.3 limiter |
| **HTTP 429 Responses** | Not generated natively | Generated on limit exceeded | Ensure proper middleware chaining |
| **Configuration** | `RateLimitConfig` exists but is ignored | Actively enforced | Plumb config values into the middleware |
