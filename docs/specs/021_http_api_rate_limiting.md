# 🔭 Vantage Spec: HTTP API Rate Limiting

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
> **I want to** enforce per-IP rate limits on the Universal HTTP API,
> **So that** I can protect my AletheiaDB instance from denial-of-service (DoS) attacks and abusive scrapers running complex queries.

> **As a** Platform Engineer,
> **I want to** configure the rate limiting parameters (e.g., requests per second) natively via AletheiaDB's configuration,
> **So that** I don't necessarily have to deploy and manage an external reverse proxy (like Nginx or Envoy) just for basic API protection.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB's Universal HTTP API lacks built-in rate limiting due to limitations in the underlying web framework migration.

**The Gap:**
- **Security & Reliability:** A single abusive client sending rapid, complex analytical queries can starve the database of resources, causing a denial of service for all other users.
- **Operational Overhead:** While users can deploy a reverse proxy to handle rate limiting, this adds architectural complexity and defeats the purpose of an easy-to-run, standalone database executable.

**ROI:**
- **Out-of-the-Box Security:** Makes the HTTP API production-ready for public or semi-public exposure without requiring additional infrastructure.
- **Improved DX for Operators:** A simple configuration block is much easier to manage than external proxy configurations.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Per-IP Rate Limiting:**
    - The HTTP server MUST enforce a configurable limit on the number of requests per IP address over a specific time window.
    - If a client exceeds the limit, the server MUST return an HTTP `429 Too Many Requests` status code.

2.  **Configuration:**
    - The system MUST accept configuration to enable and tune rate limiting.
    - Operators MUST be able to define the maximum requests per second and burst size.

3.  **Metrics Integration:**
    - The system SHOULD expose a metric tracking the number of requests rejected due to rate limiting.

### Non-Functional Requirements

-   **Performance:** The rate-limiting middleware MUST add negligible overhead (< 1ms) to valid requests.
-   **Metric Definition:** Success = A load test sending 2x the configured limit of requests per second sees exactly the excess requests rejected with 429, while maintaining low latency for accepted requests.

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Rate Limiting:** Synchronizing rate limits across multiple AletheiaDB nodes in a cluster (Phase 2).
-   **Authenticated User Limits:** Rate limiting based on API keys, users, or roles. Phase 1 is strictly per-IP.
-   **Endpoint-Specific Limits:** Configuring different limits for different endpoints (e.g., `/query` vs `/status`). Phase 1 applies a global per-IP limit.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Framework Support** | Awaiting upstream support | Native integration | Integrate rate limiter |
| **Configuration** | Exists but ignored | Fully applied | Apply configuration to the HTTP server |
| **Observability** | No 429 metrics | Exposed counter | Add rate limit rejection metric |
