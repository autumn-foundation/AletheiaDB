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

> **As a** Database Operator running AletheiaDB in production,
> **I want to** configure per-IP rate limiting natively within the database server,
> **So that** I can protect my cluster from brute-force API abuse and Denial-of-Service (DoS) attacks without relying on an external reverse proxy like Nginx or Envoy.

## 2. 🧐 The "So What?" (Business Value)

In ADR-0055, the HTTP server was migrated to `autumn-web`. Due to library constraints with the autumn layer, native per-IP rate limiting was deferred and left as a `TODO(autumn-0.3)` marker in the codebase. Currently, users *must* place a reverse proxy in front of AletheiaDB if they expose the HTTP API publicly.

**The Gap:**
- **Operational Complexity**: Forces users to maintain a separate load balancer / proxy component even for simple deployments.
- **Lost Parity**: We regressed on this feature compared to the old stack.
- **Resource Exhaustion**: Direct API exposure can easily exhaust connection pools or memory via abusive clients without any throttling mechanism.

**ROI:**
- **Self-Contained Security**: Makes AletheiaDB safe to expose "naked" to internal networks or public testing without proxy boilerplate.
- **Developer Experience (DX)**: Zero-configuration setups remain safe by default.
- **Performance Protection**: Safeguards the embedded `autumn-web` runtime from being overwhelmed.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Native Per-IP Rate Limiting**:
    - The server MUST implement an HTTP middleware layer that tracks request rates by client IP address.
    - It MUST reject requests exceeding the limit with a `429 Too Many Requests` status code.

2.  **Configuration Support**:
    - Must allow configuring the limit (e.g., requests per second) and burst capacity via standard environment variables or TOML config.

### Non-Functional Requirements

-   **Performance Overhead**: The rate-limiting middleware must add `< 1ms` latency per request. It should ideally use an efficient, memory-bounded tracking mechanism rather than unbounded memory growth per IP.
-   **Metric Definition**: Success = Simulating a 1,000 req/s flood from a single IP results in 429s after the burst limit is hit, while the server maintains `200 OK` responses for other legitimate IPs with no noticeable latency degradation.

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Rate Limiting**: Rate limiting across multiple AletheiaDB shards or cluster nodes. Phase 1 is strictly single-node, in-memory rate limiting.
-   **Per-Endpoint / Per-Token Limits**: MVP is strictly per-IP global limits. API-key or role-based limits are deferred.
