# 🔭 Vantage Spec: HTTP Per-IP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |

## 1. 👤 User Stories

> **As a** Database Administrator offering AletheiaDB as a public or semi-public service,
> **I want to** restrict the number of HTTP requests a single IP address can make within a given time window,
> **So that** a single malicious or poorly-written client cannot overwhelm the database and degrade performance for other users.

> **As an** Application Developer building an API directly on top of AletheiaDB,
> **I want** the database to automatically handle rate limiting and return a standard `429 Too Many Requests` response when limits are exceeded,
> **So that** I do not have to build and maintain a separate rate-limiting proxy or middleware layer just to protect the database.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB's HTTP server does not enforce per-IP rate limiting, leaving it vulnerable to noisy neighbor problems and simple Denial of Service (DoS) attacks.

**The Gap:**
- **Resource Protection:** Without rate limits, a single client can consume all available connection threads or CPU resources.
- **Developer Experience (DX):** Users expect modern databases exposed via HTTP to have built-in safeguards. Requiring an external proxy (like Nginx or HAProxy) just for basic rate limiting increases deployment complexity.

**ROI:**
- **Reliability:** Ensures predictable performance and uptime even under adverse conditions or misconfigured clients.
- **Reduced Deployment Cost:** Simplifies the architecture for users who want to expose AletheiaDB directly to internal networks or light public traffic without an intermediate reverse proxy.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Configurable Limits:**
    - The system MUST allow administrators to configure the maximum number of requests allowed per IP address within a specific time window (e.g., 100 requests per second).
2.  **Standard HTTP Response:**
    - When a client exceeds the limit, the server MUST return an HTTP `429 Too Many Requests` status code.
    - The response MUST include a `Retry-After` header indicating when the client can make requests again.
3.  **Global vs. Endpoint Granularity:**
    - The rate limit MUST apply globally across all HTTP endpoints for a given IP address.

### Non-Functional Requirements

-   **Performance:**
    - Evaluating the rate limit for an incoming request MUST add negligible latency (e.g., < 1ms overhead).
-   **Metric Definition:**
    - Success = A single IP sending 200 requests/sec with a configured limit of 100 requests/sec successfully receives 100 `200 OK` responses and 100 `429 Too Many Requests` responses, with server CPU usage increasing by less than 5% compared to no rate limiting.

## 4. 🚫 Out of Scope (Phase 1)

-   **User-Based Rate Limiting:** Limiting requests based on an authenticated user ID or API key (Phase 1 is strictly IP-based).
-   **Distributed Rate Limiting:** Synchronizing rate limit counters across multiple AletheiaDB nodes in a cluster. Phase 1 applies local limits per node.
-   **Endpoint-Specific Limits:** Configuring different limits for different API routes (e.g., strict limits on writes, loose limits on reads).

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Request Throttling** | None | Throttles requests exceeding a threshold | Introduce IP-based tracking and request rejection |
| **HTTP Responses** | Always processes request | Returns `429 Too Many Requests` | Add middleware to intercept and short-circuit requests |
| **Configuration** | N/A | Env var/config file driven limits | Expose rate limiting settings to the operator |
