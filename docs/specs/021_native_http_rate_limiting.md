# 🔭 Vantage Spec: Native HTTP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/http/server.rs` |

## 1. 👤 User Stories

> **As a** Database Administrator,
> **I want to** enforce per-IP rate limits on the HTTP API natively,
> **So that** I can prevent denial-of-service (DoS) attacks and abusive traffic without needing to configure and maintain a separate reverse proxy (like Nginx or Envoy).

> **As a** Managed Service Provider,
> **I want to** apply custom rate limiting policies based on API keys or tenant IDs,
> **So that** I can enforce billing tiers and ensure fair resource allocation among different users.

## 2. 🧐 The "So What?" (Business Value)

AletheiaDB currently lacks native HTTP rate limiting due to limitations in the `autumn` middleware stack (waiting for `autumn-0.3`). Operators are forced to rely on external reverse proxies.

**The Gap:**
- **Deployment Friction:** Users must deploy and configure an extra layer of infrastructure (Nginx, Caddy) just to get basic protection against API abuse.
- **Complexity:** Managing rate limits externally makes it harder to align rate-limiting logic with database-specific concepts (e.g., query complexity, tenant ID).

**ROI:**
- **Security & Reliability:** Out-of-the-box protection ensures that test deployments and simple self-hosted setups are robust against traffic spikes.
- **Operational Simplicity:** A self-contained binary that handles its own traffic shaping is easier to deploy, especially in containerized or edge environments.

**Metric Definition:** Success = A single IP making > 100 requests/second receives a 429 Too Many Requests response within 2ms, without crashing the server.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Per-IP Rate Limiting:**
    - Must enforce a configurable request limit per IP address over a defined time window (e.g., 100 requests per minute).
    - Must return a standard HTTP `429 Too Many Requests` status code when the limit is exceeded.
    - Must include standard `RateLimit-*` headers in the response (e.g., `RateLimit-Limit`, `RateLimit-Remaining`, `RateLimit-Reset`).

2.  **Configuration:**
    - Rate limiting parameters (limit, window) must be configurable via environment variables (`ALETHEIADB_RATE_LIMIT`) or a configuration file.
    - Must support an option to disable rate limiting entirely for internal/trusted networks.

### Non-Functional Requirements

-   **Performance:** The rate-limiting middleware must add negligible overhead (< 1ms) to valid requests.
-   **Memory Efficiency:** The IP tracking state must bounded to prevent memory exhaustion attacks (e.g., using an LRU cache).

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Rate Limiting:** Synchronizing rate limits across multiple AletheiaDB nodes in a cluster (Phase 2).
-   **GraphQL Query Cost Limiting:** Limiting requests based on the computational complexity of a GraphQL or Cypher query (Phase 2).
-   **User/Tenant Based Limiting:** Rate limiting based on authenticated user IDs or API keys, assuming Phase 1 focuses purely on IP-based limits.
