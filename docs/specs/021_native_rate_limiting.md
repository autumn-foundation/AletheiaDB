# 🔭 Vantage Spec: Native Per-IP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Topic** | HTTP Server Security |

## 1. 👤 User Stories

> **As a** Database Operator,
> **I want to** configure native per-IP rate limiting directly in AletheiaDB's HTTP server configuration,
> **So that** I can protect my database from abusive clients, Denial of Service (DoS) attacks, and brute-force attempts without requiring an external reverse proxy (like Nginx or HAProxy).

> **As an** Application Developer,
> **I want to** rely on a built-in rate limiter that gracefully returns HTTP 429 Too Many Requests,
> **So that** I can easily test my application's backoff and retry logic in a local development environment.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB does not enforce per-IP rate limiting natively on its HTTP server. The current fallback relies on users configuring reverse proxies. Now that the underlying web framework supports it natively, we can remove this limitation.

**The Gap:**
- **Security:** Exposed database endpoints are vulnerable to simple layer 7 DoS attacks or accidental traffic bursts from misconfigured clients.
- **Developer Experience (DX):** Users must set up external infrastructure (e.g., Nginx, Envoy) just to get basic rate limiting, increasing the barrier to entry for production deployments.

**ROI:**
- **Reliability:** Protects the database's available connections and compute resources.
- **Simplicity:** Enables a "batteries-included" production deployment model.
- **Lower TCO (Total Cost of Ownership):** Reduces the need for additional proxy layers for simple deployments.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Configuration:**
    - The server must allow configuration of the rate limiter specifying the allowed requests per second and burst limit.
    - These configurations must be exposed through the standard mechanisms (e.g., environment variables, config files).

2.  **Enforcement:**
    - The rate limiter MUST restrict incoming HTTP requests on a per-IP address basis.
    - If a single IP exceeds the configured requests per second (allowing for the burst limit), the server MUST reject the request.

3.  **Response:**
    - Rejected requests MUST return an HTTP `429 Too Many Requests` status code.
    - The response should ideally include standard `Retry-After` headers if supported by the underlying implementation.

### Non-Functional Requirements

-   **Performance:** Evaluating the rate limit for a request must take < 1ms of overhead.
-   **Metric Definition:** Success = A load test sending 200 requests per second from a single IP to a server configured with 10 req/sec limits results in exactly 10 successful `200 OK` responses and 190 `429 Too Many Requests` responses within a 1-second window.

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Rate Limiting:** Synchronizing rate limits across multiple AletheiaDB nodes (e.g., using Redis). Phase 1 is strictly in-memory and local to the current node.
-   **Endpoint-Specific Limits:** Configuring different rate limits for different endpoints (e.g., strict limits for `/auth`, loose limits for `/query`). Phase 1 applies a global per-IP limit across all routes.
-   **Client-ID/Token Based Limiting:** Rate limiting based on authenticated user IDs or API tokens. Phase 1 only uses the client's IP address.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Native Rate Limiting** | Not implemented | Fully integrated | Wire the rate limit layer into the web framework |
| **Config Validation** | Config values exist but are unused | Used in server setup | Pass configuration values to the rate limiter |
| **429 Responses** | Missing | Standardized | Ensure the middleware returns the correct HTTP status |
