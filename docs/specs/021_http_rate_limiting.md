# 🔭 Vantage Spec: HTTP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/http/server.rs` |

## 1. 👤 User Stories

> **As a** DevOps Engineer,
> **I want to** apply rate limits per-IP to the HTTP API,
> **So that** a single rogue client cannot overwhelm the database and cause an outage for other users.

> **As a** Platform Operator,
> **I want to** configure rate limits seamlessly through the existing `ALETHEIADB_*` environment variables,
> **So that** I do not need to deploy a separate reverse proxy just for basic rate limiting.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB's `http-server` feature uses `autumn-web` but lacks built-in per-IP rate limiting (deferred in ADR 0055).

**The Gap:**
- **Vulnerability**: The `/query` endpoint is susceptible to abuse, potentially allowing a single IP to exhaust server resources.
- **Operational Burden**: Users are forced to set up and configure external reverse proxies (e.g., Nginx, Envoy) for basic rate limiting, increasing operational complexity.

**ROI:**
- **Reliability**: Protects the database from noisy neighbors and simple DoS attacks.
- **Simplicity**: Provides a battery-included experience for operators.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Per-IP Rate Limiting**:
    - The server must track requests per client IP address.
    - If a client exceeds the configured rate limit (e.g., requests per second), the server must reject subsequent requests with a `429 Too Many Requests` status code.
    - The rate limiter must respect the configured limit settings (e.g., requests per second, burst size).

2.  **Configuration Integration**:
    - The rate limits must be configurable via existing mechanisms (e.g., server configuration files, environment variables).

### Non-Functional Requirements
-   **Performance**: The overhead of rate limiting per request should be negligible (< 1ms).
-   **Metric Definition**: Success = A client exceeding the limit receives a 429 response, and legitimate traffic from other IPs is unaffected.

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Rate Limiting**: Synchronizing rate limit counters across multiple instances of AletheiaDB (Phase 2).
-   **User-Based Rate Limiting**: Rate limiting based on API keys or authenticated user IDs (Phase 2).
-   **Dynamic Rate Limiting**: Automatically adjusting limits based on server load.