# 🔭 Vantage: Spec for HTTP Rate Limiting

## 👤 User Story
**As a** SaaS provider hosting AletheiaDB as a managed service,
**I want to** apply rate limiting per-IP or per-API key on the HTTP endpoints,
**so that** I can prevent noisy-neighbor problems, protect the database from denial-of-service (DoS) attacks, and enforce tiered API usage limits for my customers.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, AletheiaDB relies entirely on the upstream web framework (autumn) or external reverse proxies (like NGINX/Envoy) for rate limiting. As AletheiaDB matures into a production-ready system capable of being exposed directly to untrusted networks (via the Universal HTTP API), lacking native rate limiting makes it vulnerable to abuse. A single bad actor executing complex queries can saturate CPU and memory, degrading performance for all other tenants. By adding native rate limiting, we improve the resilience of the platform and enable commercial monetization strategies (e.g., free tier vs. paid tier limits).

**Metric Definition:**
- **Protection Efficiency:** 100% of requests exceeding the configured rate limit are rejected with HTTP 429 Too Many Requests.
- **Latency Overhead:** Rate limiting checks add < 1ms to the overall request latency.

**Gap Analysis:**
- *Current State:* `src/http/server.rs` explicitly documents a `TODO` to wire up per-IP rate limiting once the `autumn` framework supports it natively in version 0.3.
- *Required State:* Implement sliding-window or token-bucket rate limiting at the HTTP middleware layer.

## ✅ Acceptance Criteria
- Must introduce an HTTP middleware layer capable of tracking request counts per IP address.
- Must reject requests that exceed the limit with an HTTP 429 status code and an appropriate `Retry-After` header.
- Must be configurable via environment variables (e.g., `ALETHEIADB_RATE_LIMIT_REQS_PER_SEC`, `ALETHEIADB_RATE_LIMIT_BURST`).
- Must operate with minimal lock contention (e.g., using atomic counters or a lock-free cache) to prevent the rate limiter itself from becoming a bottleneck.

## 🚫 Out of Scope (Phase 1)
- Complex distributed rate limiting (e.g., synchronizing rate limits across a cluster using Redis). Phase 1 is in-memory per node.
- Deep API-key based rate limiting with distinct quotas per key (requires a full auth/tenant system first). Phase 1 focuses on IP-based limiting.
