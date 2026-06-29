# 🔭 Vantage: Spec for HTTP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/http/server.rs` |

## 1. 👤 User Stories

> **As a** Database Operator,
> **I want** the HTTP API to enforce per-IP rate limits natively,
> **So that** a single rogue client cannot overwhelm the database and degrade performance for everyone else.

> **As an** Application Developer,
> **I want** the database to reject excessive requests with a standard HTTP 429 status code,
> **So that** my client application can easily detect throttling and back off appropriately using standard retry mechanisms.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB relies on external reverse-proxies (like Nginx or Envoy) to enforce rate limiting because the migration to the `autumn-web` framework temporarily dropped native support.

**The Gap:**
- **Operational Complexity:** Users deploying AletheiaDB directly must now run and configure an additional proxy just for basic protection.
- **Developer Experience (DX):** `RateLimitConfig` exists in the public API but silently does nothing. This is confusing and violates the principle of least astonishment.

**ROI:**
- **Reliability:** Built-in protection against unintentional DoS.
- **Simplicity:** One less moving part for users to manage in production deployments.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Per-IP Limiting:**
    - The server MUST limit incoming HTTP requests based on the client's IP address.
2.  **Configuration Wiring:**
    - The existing `RateLimitConfig` (which dictates requests per second/bursts) MUST be wired into the `autumn-web` 0.3 native rate limiter.
3.  **HTTP 429 Responses:**
    - When a limit is exceeded, the server MUST return an HTTP 429 (Too Many Requests) response.
4.  **Header Support:**
    - The response SHOULD include standard rate-limiting headers (e.g., `Retry-After`) to allow well-behaved clients to back off.

### Non-Functional Requirements

-   **Metric Definition:** Success = Rate limiting overhead adds < 1ms to the P99 latency of successful API requests under load.

## 4. 🚫 Out of Scope (Phase 1)

-   **User-based Limiting:** Rate limiting based on API keys, users, or JWT tokens. This spec focuses solely on per-IP limits.
-   **Distributed Rate Limiting:** Synchronizing rate limit counters across a cluster of AletheiaDB nodes.
-   **Custom Shims:** Writing a custom HTTP shim layer. This feature MUST wait for native support in `autumn` 0.3 as decided in ADR 0055.
