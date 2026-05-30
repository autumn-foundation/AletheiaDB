# 🔭 Vantage: Spec for HTTP Rate Limiting

## 👤 User Story
**As a** System Administrator operating a public-facing AletheiaDB instance,
**I want** to enforce per-IP rate limiting natively within the HTTP API,
**so that** I can protect the database from denial-of-service attacks, brute force abuse, and accidental high-frequency scraping without relying on complex external reverse proxies.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, AletheiaDB's HTTP server (`autumn-web`) lacks built-in rate limiting, as documented in `src/http/server.rs` (`TODO(autumn-0.3)`). Leaving the database exposed without rate limiting allows a single abusive client or compromised script to overwhelm the connection pool, exhaust server resources, and degrade latency for legitimate users. By integrating a native rate limiter, we ensure reliable Quality of Service (QoS), enhance baseline security against volumetric attacks, and simplify operational deployment since small to medium projects won't be forced to configure an external Nginx or Envoy proxy just for rate limiting.

**Success Metric Definition:**
- **Protection:** Requests exceeding the configured limit (e.g., 100 req/sec per IP) are immediately rejected with an HTTP 429 Too Many Requests response.
- **Overhead:** The rate limiting middleware introduces less than 1ms of latency per legitimate request.

## ✅ Acceptance Criteria
- Must implement native per-IP rate limiting middleware in `src/http/server.rs` using `autumn_web` or a compatible tower middleware.
- Must support configurable limits via standard configuration (e.g., `max_requests_per_second` and `burst_capacity`).
- Must return a standard HTTP 429 response along with appropriate `Retry-After` headers when limits are exceeded.
- Must correctly extract client IP addresses even when deployed behind reverse proxies, utilizing standard headers like `X-Forwarded-For`.

## 🚫 Out of Scope
- Distributed rate limiting across a sharded cluster (Phase 2).
- Advanced rate limiting strategies (e.g., token bucket per API key or authenticated user) – Phase 1 focuses exclusively on per-IP limits.
