# 🔭 Vantage: Spec for Native HTTP Rate Limiting

## 👤 User Story
**As a** System Administrator managing AletheiaDB in production,
**I want** the database to natively enforce rate limiting on incoming HTTP and MCP requests,
**so that** I can protect the database from denial-of-service attacks or aggressive agent loops without needing to deploy and configure a separate reverse proxy.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, AletheiaDB's HTTP surface dropped in-process rate limiting. Without native rate limiting, the database is vulnerable to request floods, forcing users to manage external infrastructure (API gateways, reverse proxies) to protect the database. Restoring native rate limiting fulfills the "database-in-a-box" promise, making deployments simpler and safer by default.

**Metric Definition:**
- **Protection:** Requests exceeding the configured limit (e.g., > 100 req/sec per IP) are rejected with HTTP 429 Too Many Requests.
- **Performance:** Enforcing the rate limit adds <1ms overhead to the request processing time.
- **Completeness:** Both HTTP API routes and MCP endpoints are protected by the unified rate limit layer.

## ✅ Acceptance Criteria
- Must implement native HTTP rate limiting.
- Must read configuration limits (e.g., `requests_per_second`, `burst_size`) from the standard configuration source.
- Must correctly identify the client IP address (respecting `X-Forwarded-For` if configured behind a trusted proxy).
- Must return a standard HTTP 429 response when limits are exceeded, containing appropriate headers (e.g., `Retry-After`).

## 🚫 Out of Scope
- Distributed rate limiting across a sharded cluster (Phase 2). MVP is single-node, in-memory rate limiting.
- Granular, per-endpoint rate limits (Phase 2). MVP applies a global limit per IP address across all endpoints.
- User-level rate limiting based on authentication tokens. MVP focuses on IP-based limiting.
