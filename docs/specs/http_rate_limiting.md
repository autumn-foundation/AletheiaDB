# 🔭 Vantage Spec: HTTP Per-IP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |

## 1. 👤 User Stories

> **As a** Database Operator,
> **I want to** configure per-IP rate limits directly on the database's HTTP server,
> **So that** I can protect my database from abusive clients, poorly written scripts, or denial-of-service (DoS) attacks without requiring a separate reverse proxy layer.

> **As an** Application Developer,
> **I want to** rely on built-in API limits that return standardized status codes when exceeded,
> **So that** my client applications can smoothly implement backoff-and-retry logic when hitting the database directly.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB's HTTP server defers rate limiting to external reverse proxies. While this is an acceptable workaround, many users run AletheiaDB in environments where deploying an additional load balancer or proxy introduces unnecessary operational overhead (e.g., local development, embedded deployments, or lightweight edge installations).

**The Gap:**
- **Security Vulnerability:** Exposing the HTTP API directly to clients leaves the database vulnerable to resource exhaustion from sudden traffic spikes or malicious actors.
- **Developer Experience (DX):** Users expect modern databases to handle basic traffic shaping out-of-the-box. Requiring them to configure Nginx or HAProxy just to prevent API flooding is high friction.

**ROI:**
- **Reliability:** Prevents the database from crashing under excessive load, ensuring stable performance for legitimate requests.
- **Operational Simplicity:** Lowers the barrier to entry by removing the need for third-party infrastructure components.
- **Predictability:** Guarantees that runaway queries or infinite loops in client code will fail fast rather than dragging down the entire system.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Configurable Limits:**
    - The system must allow administrators to define a maximum number of allowed requests per second (RPS) per individual IP address.
    - The system must support defining a "burst" capacity, allowing short spikes in traffic above the baseline RPS, provided the average stays within limits over time.
2.  **Enforcement & Rejection:**
    - When a client IP exceeds its allotted rate limit, the server must immediately reject subsequent requests from that IP.
    - Rejected requests must return an HTTP 429 status code ("Too Many Requests").
3.  **Client Feedback:**
    - Responses (both successful and rejected) should ideally include standard rate-limiting headers (e.g., indicating the limit, remaining quota, and time until reset) to aid client-side throttling.

### Non-Functional Requirements

-   **Performance:** Evaluating the rate limit for an incoming request must add negligible latency (e.g., < 1ms overhead).
-   **Resource Usage:** Tracking IP limits must not lead to unbounded memory growth (e.g., old IP records should expire or be garbage-collected).
-   **Metric Definition:** Success = A sustained load test exceeding the configured RPS by 50% from a single IP results in 100% of excess requests receiving an HTTP 429, while memory usage remains stable.

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Rate Limiting:** Synchronizing rate limit counters across multiple database instances or shards.
-   **User-Based Rate Limiting:** Limiting traffic based on API keys, authentication tokens, or specific users (Phase 1 focuses exclusively on IP addresses).
-   **Endpoint-Specific Limits:** Configuring different limits for different HTTP routes (e.g., allowing more reads than writes). Phase 1 applies a single global limit per IP across all routes.
-   **Dynamic Limit Adjustment:** Changing rate limits at runtime without restarting the server or reloading the configuration.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **HTTP Rate Limiting** | Deferred to reverse proxies (no internal limit) | Enforced per-IP directly in the HTTP server | Integrate the rate limiting middleware when available. |
| **Resource Exhaustion Protection** | Vulnerable if directly exposed | Protected from runaway IP traffic | Reject excessive traffic with HTTP 429. |
| **Configuration** | Configuration objects exist but are inactive | Active configuration affecting server behavior | Wire the active configuration into the HTTP listener. |
