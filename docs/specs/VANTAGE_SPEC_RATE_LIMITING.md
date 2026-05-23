# 🔭 Vantage Spec: Per-IP Rate Limiting

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Component** | HTTP API |

## 1. 👤 User Stories

> **As a** Database Operator,
> **I want to** configure per-IP rate limiting on the AletheiaDB HTTP API,
> **So that** I can protect my instance from abuse, denial-of-service (DoS) attacks, or buggy client applications that submit excessive queries.

> **As an** Application Developer exposing the AletheiaDB HTTP API directly or via a thin proxy,
> **I want** built-in rate limiting handling (like `429 Too Many Requests` responses with `Retry-After` headers),
> **So that** my clients can gracefully back off and retry without overwhelming the database.

## 2. 🧐 The "So What?" (Business Value)

Currently, the HTTP server component of AletheiaDB lacks out-of-the-box rate limiting. This leaves instances exposed to being overwhelmed by sheer request volume.

**The Gap:**
- **Security / Stability:** A single misconfigured client or malicious actor could degrade performance for all users by spamming the query endpoint.
- **Operational Complexity:** Operators are forced to configure rate limiting at an external reverse-proxy layer, which requires extra infrastructure and deployment steps.

**ROI:**
- **Reliability:** Prevents resource starvation and improves database uptime.
- **Developer Experience (DX):** Allows users to securely run the HTTP server "bare-metal" without requiring a complex multi-tier deployment just for basic traffic shaping.

## 3. ✅ Acceptance Criteria

### Functional Requirements
1.  **Per-IP Rate Limiting:**
    - The server MUST track request rates based on the client IP address.
    - If a client exceeds the configured request limit within the configured time window, the server MUST reject subsequent requests.
2.  **Configuration:**
    - The existing rate limit configuration must be actively wired up to enforce limits.
    - Operators must be able to define the maximum number of requests per time window.
3.  **HTTP Response:**
    - Rejected requests MUST return an HTTP status code `429 Too Many Requests`.
    - The response MUST include standard rate limit headers, notably `Retry-After`, to inform the client when they can resume sending requests.

### Non-Functional Requirements
-   **Performance Metrics:**
    - The rate limiting check should add `< 1ms` overhead per request on average.

## 4. 🚫 Out of Scope
-   **Distributed Rate Limiting:** Sharing rate limit state across multiple AletheiaDB instances in a sharded/clustered setup. Phase 1 is strictly in-memory per instance.
-   **Per-User / Token Rate Limiting:** Limiting based on authenticated API keys or users. Phase 1 focuses solely on the network IP layer.
-   **Different Limits per Endpoint:** Applying a higher limit to status endpoints and a lower limit to query endpoints. Phase 1 applies a global limit across all HTTP endpoints.
