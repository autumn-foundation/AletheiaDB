# 🔭 Vantage Spec: HTTP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/http/server.rs` |

## 1. 👤 User Stories

> **As a** Database Administrator,
> **I want to** configure per-IP rate limits on the HTTP API,
> **So that** I can prevent malicious actors or runaway scripts from overwhelming the database with requests.

> **As a** Managed Service Provider,
> **I want to** enforce tier-based rate limits on different API endpoints,
> **So that** I can offer different service level agreements (SLAs) to my customers.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB does not enforce rate limiting at the HTTP layer because the underlying `autumn` framework does not yet support it natively. This leaves the database vulnerable to DoS attacks and resource exhaustion unless operators configure a reverse proxy.

**The Gap:**
- **Security:** The database is exposed to brute-force and DoS attacks if exposed directly to the internet.
- **Resource Management:** Runaway queries can consume all available connections or CPU, impacting other legitimate workloads.
- **Developer Experience (DX):** Operators have to configure external tools (like Nginx or Caddy) to protect the database, increasing deployment complexity.

**ROI:**
- **Resilience:** Protects the database from abusive traffic, ensuring consistent performance for legitimate users.
- **Simplicity:** Offers a "batteries-included" experience where users don't need additional infrastructure for basic security.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Per-IP Rate Limiting:**
    - Must enforce a configurable maximum number of requests per second (RPS) or requests per minute (RPM) per IP address.
    - Must return a standard HTTP `429 Too Many Requests` status code when the limit is exceeded.
    - Must include standard rate limit headers (e.g., `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset`) in the response.

2.  **Configuration:**
    - The rate limit must be configurable via environment variables or a configuration file (e.g., `RateLimitConfig`).
    - The rate limit must be able to be disabled entirely (e.g., for internal deployments).

### Non-Functional Requirements

-   **Performance:** The overhead of checking the rate limit should be minimal (< 1ms).
-   **Metric Definition:** Success = A sustained load of 10,000 req/s from a single IP is capped exactly at the configured limit (e.g., 100 req/s), with subsequent requests receiving a 429 response in under 5ms.

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Rate Limiting:** Synchronizing rate limits across a cluster of AletheiaDB nodes is deferred to Phase 2. Phase 1 limits are per-node.
-   **User/Token-based Rate Limiting:** Rate limiting based on authentication tokens or user IDs instead of IP addresses.
-   **Dynamic Rate Limiting:** Automatically adjusting limits based on system load or other heuristics.
