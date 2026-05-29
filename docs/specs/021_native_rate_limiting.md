# 🔭 Vantage Spec: Native Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/http/server.rs` |

## 1. 👤 User Stories

> **As a** Database Operator,
> **I want to** configure per-IP rate limiting natively within AletheiaDB's HTTP server,
> **So that** I can protect my database from malicious or runaway clients without needing to deploy and manage a separate reverse proxy.

> **As an** Application Developer,
> **I want** the API to return standard `429 Too Many Requests` responses when limits are exceeded,
> **So that** my client applications can automatically back off and retry gracefully.

## 2. 🧐 The "So What?" (Business Value)

During the HTTP framework migration (ADR 0055), native per-IP rate limiting was deferred. The configuration struct (`RateLimitConfig`) exists in the public API but currently does nothing.

**The Gap:**
- **Security & Stability:** A raw AletheiaDB instance is vulnerable to resource exhaustion from abusive API clients.
- **Operational Complexity:** Forcing users to configure external reverse proxies just for basic rate limiting increases deployment friction.
- **Developer Experience:** Existing configuration options for rate limiting are non-functional, causing confusion.

**ROI:**
- **Out-of-the-box Security:** Ensures the database is safe to expose in simpler deployment topologies.
- **Operational Simplicity:** Lowers the barrier to entry by removing external dependencies.

## 3. ✅ Acceptance Criteria

### Functional Requirements

- The HTTP server MUST enforce per-IP rate limits as defined by the existing `RateLimitConfig`.
- When a client exceeds the limit, the server MUST reject the request with HTTP status `429 Too Many Requests`.
- The `429` response SHOULD include a `Retry-After` header.
- The implementation MUST integrate cleanly with the `autumn-web` middleware stack.

### Metric Definition

- **Success:** A client making 101 requests within a 100-request/minute limit receives exactly 100 `200 OK` responses and 1 `429 Too Many Requests` response. Overhead per request MUST be < 1ms.

## 4. 🚫 Out of Scope (Phase 1)

- **Distributed Rate Limiting:** Synchronizing rate limit counters across multiple AletheiaDB nodes. Counters are strictly local to the node process.
- **User/Token-based Limiting:** Complex application-level user rate limiting based on authentication tokens. Rate limiting is strictly IP-based for this phase.
