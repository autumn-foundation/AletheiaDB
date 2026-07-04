# 🔭 Vantage Spec: HTTP API Rate Limiting

## 1. 👤 User Story
> **As a** Database Operator,
> **I want** to enforce per-IP rate limits on the HTTP API,
> **So that** I can prevent denial-of-service (DoS) attacks and ensure fair resource allocation among different clients accessing AletheiaDB.

## 2. 🧐 The "So What?" (Business Value)
Currently, the HTTP server (`src/http/server.rs`) lacks native rate limiting. A single misconfigured or malicious client can overwhelm the database with requests, leading to increased latency, memory exhaustion, or complete downtime for all users.
**Metric Definition:** Success = The database can limit an IP to `N` requests per second and return a `429 Too Many Requests` response for subsequent requests, keeping overall query latency < 10ms for 99% of valid requests.

## 3. ✅ Acceptance Criteria
- Must allow configuring a default global rate limit (e.g., requests per second).
- Must identify clients by IP address.
- Must return HTTP 429 status code when the limit is exceeded.
- Must include standard `RateLimit-*` or `Retry-After` HTTP headers.
- Must have negligible performance overhead for valid requests.

## 4. 🚫 Out of Scope (Phase 1)
- Advanced rate limiting based on API keys, user roles, or tokens (only IP-based for now).
- Distributed rate limiting across multiple AletheiaDB nodes (single-node only for now).
