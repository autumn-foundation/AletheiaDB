# 🔭 Vantage Spec: Native Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/http/server.rs` |

## 1. 👤 User Stories

> **As a** Database Operator,
> **I want to** configure per-IP rate limiting natively within AletheiaDB's HTTP server,
> **So that** I can protect my database from abusive clients, misconfigured scripts, or denial-of-service attempts without requiring a separate reverse proxy (like Nginx or Envoy).

> **As an** Application Developer using AletheiaDB in an embedded or edge context,
> **I want to** set sensible request limits out-of-the-box,
> **So that** my application doesn't accidentally exhaust system resources if my own code has a runaway query loop.

## 2. 🧐 The "So What?" (Business Value)

During the migration to the new HTTP stack (ADR-0055), native rate limiting was deferred because the framework didn't yet support the necessary ergonomics natively. Currently, users are forced to set up an external reverse proxy to protect the database HTTP endpoints.

**The Gap:**
- **Operational Complexity:** Small-to-medium deployments must manage a separate piece of infrastructure (Nginx/Caddy) just to enforce basic API limits.
- **Developer Experience:** Setting up a quick prototype or edge deployment is harder than it should be.

**ROI:**
- **Simplicity:** AletheiaDB becomes "batteries-included" for basic production deployments, lowering the operational burden for users.
- **Reliability:** Built-in protection against the most common class of accidental overload (the "runaway script").

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Configuration:**
    - The system MUST be fully wired up to respect rate limiting configurations.
    - Users MUST be able to configure requests per second and burst sizes via the standard configuration file or environment variables.

2.  **Enforcement:**
    - The server MUST track request rates on a per-IP basis.
    - If a client exceeds the configured limit, the server MUST return an HTTP `429 Too Many Requests` response.
    - The response SHOULD include standard rate-limiting headers (e.g., `Retry-After`, `X-RateLimit-Remaining`).

3.  **Observability:**
    - The system SHOULD log when an IP is throttled (at a sensible log level/rate to avoid log spam).
    - Rate limit rejections SHOULD be recorded in the server's metrics.

### Non-Functional Requirements

-   **Performance:** The rate-limiting logic must add negligible latency (< 1ms) to the request path.
-   **Metric Definition:** Success = Under load test, valid requests complete with < 5% overhead compared to the non-rate-limited baseline, and excess requests correctly receive 429s.

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Rate Limiting:** Synchronizing rate limit counters across multiple AletheiaDB instances in a sharded cluster. Rate limiting is strictly per-node for now.
-   **Per-User Limiting:** Limiting based on authenticated user IDs or API keys. Phase 1 focuses strictly on IP-based limiting.
-   **Dynamic Limit Updates:** Changing rate limits without restarting the server.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Rate Limit Config** | Present but ignored | Actively enforced | Wire up native rate limiting |
| **429 Responses** | Never returned natively | Returned when limit exceeded | Implement rate limiting rules |
| **Testing** | Manual/external only | Integration tests | Add comprehensive rate-limit tests |
