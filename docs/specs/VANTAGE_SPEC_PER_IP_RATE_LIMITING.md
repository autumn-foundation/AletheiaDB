# 🔭 Vantage: Spec for Native Per-IP Rate Limiting

## 1. 👤 User Story

> **As a** Platform Engineer managing public-facing deployments of AletheiaDB,
> **I want** the database to enforce per-IP rate limits natively via its HTTP server,
> **So that** I can protect my graph infrastructure from excessive crawling, DoS attacks, or runaway client loops without needing to configure and maintain a separate reverse proxy layer.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB lacks native rate limiting for the HTTP API layer, leaving deployments vulnerable to abuse unless they are deployed behind a separate reverse proxy like NGINX or Envoy.

**The Gap:**
- **Security & Stability**: Unmetered API access allows aggressive clients to consume server resources unchecked, leading to service degradation.
- **Operational Complexity**: Forcing users to configure and maintain external reverse proxies adds significant operational overhead, especially for smaller deployments or edge use cases.

**ROI:**
- **Reliability**: Guarantees that AletheiaDB remains responsive and stable under abusive or high-traffic conditions out-of-the-box.
- **Simplified Deployment**: Reduces the infrastructure required to run a secure AletheiaDB instance, lowering the barrier to entry and improving time-to-value for new adopters.
- **Metric Definition**: Success = Rate limiting rejects requests exceeding configured limits with HTTP 429 Too Many Requests, maintaining API responsiveness (< 10ms latency) for legitimate traffic.

## 3. ✅ Acceptance Criteria

### Functional Requirements

- The HTTP server must natively support tracking and rate limiting requests on a per-IP basis.
- Rate limits must correctly reject excessive requests by returning HTTP 429 Too Many Requests, and should provide appropriate rate limit headers (e.g., `RateLimit-Limit`, `RateLimit-Remaining`, `RateLimit-Reset`).
- The rate limiter must respect the configurable limits to allow operators fine-grained control over allowed request frequencies.

### Non-Functional Requirements

- **Performance**: The overhead of IP tracking and rate limit checks should be negligible (< 1ms per request) to prevent the limiter itself from becoming a bottleneck.

## 4. 🚫 Out of Scope (Phase 1)

- Distributed rate limiting across multiple sharded nodes (Phase 1 assumes a single-node context).
- Complex heuristic-based blocking or throttling (e.g., dynamically adjusting limits based on system load or semantic payload).
