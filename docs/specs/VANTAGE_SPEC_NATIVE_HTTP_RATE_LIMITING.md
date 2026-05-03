# 🔭 Vantage: Spec for Native HTTP Rate Limiting

## 👤 User Story
**As an** API Operator,
**I want** the database's HTTP server to natively enforce per-IP rate limits based on my configuration,
**so that** I can protect my database from abusive traffic, scraping, or accidental Denial of Service without needing to deploy and manage a separate reverse proxy.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, AletheiaDB exposes configuration parameters for HTTP rate limiting (`requests_per_second` and `burst_size`), but these are completely ignored by the underlying server. Operators who want to protect their database endpoints from abuse must introduce infrastructure complexity by deploying a reverse proxy (like NGINX or Envoy) in front of the database. By providing native rate limiting, we dramatically simplify the deployment architecture for small-to-medium teams, delivering a "secure by default" experience straight out of the box.

**Success Metric Definition:**
- **Protection:** Requests exceeding the configured rate limit are immediately rejected with an HTTP 429 Too Many Requests status.
- **Performance:** Tracking per-IP request rates adds negligible latency (less than 1ms overhead per request).
- **Usability:** The existing configuration parameters correctly map to and enforce the limits.

**Gap Analysis:**
The current `ServerConfig` accepts a `RateLimitConfig`, but the actual enforcement layer was removed during a recent framework migration. Standard database HTTP APIs (like Elasticsearch or Neo4j) often have built-in traffic shaping or expect it at the gateway, but for a tool aiming for low-friction setup, requiring a gateway for basic protection is a missing capability compared to previous versions.

## ✅ Acceptance Criteria
- Must natively enforce the configured requests-per-second and burst-size limits per client IP address.
- Must return a standard HTTP 429 status code when limits are exceeded.
- Must accurately respect the existing `RateLimitConfig` parameters provided during server startup.
- Must automatically track and expire IP rate histories to prevent memory bloat over time.

## 🚫 Out of Scope
- Distributed rate limiting across multiple database nodes.
- User-specific or token-based rate limits (IP-based only for Phase 1).
- Advanced traffic shaping or dynamic rate adjustment policies.