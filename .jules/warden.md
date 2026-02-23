# Warden's Journal

**2025-05-23 - Hardened HTTP Rate Limiting and Input Validation**
**Threat:**
1.  **Panic on Invalid Config:** `RateLimitConfig` allowed zero values for `requests_per_second` and `burst_size`, which caused `actix-governor` to panic at runtime (or startup) when building the limiter. This is a DoS vector if configuration is reloadable or controlled by external inputs.
2.  **Panic on Malformed JSON:** Initially suspected that `unwrap()` calls in `src/http/handlers.rs` would panic on invalid user input. Investigation revealed these were mostly in tests or safely handled by `actix-web` extractors, but explicit validation was reinforced.

**Defense:**
1.  **Strict Validation:** Added `RateLimitConfig::validate()` to enforce strict positivity (> 0) for rate limit parameters.
2.  **Error Propagation:** Refactored `build_rate_limit` in `src/http/server.rs` to return `Result` instead of panicking with `expect`. Errors are now propagated up to `create_server` and `run_server`, allowing graceful failure.
3.  **Verification:** Added `tests/warden_http_panic.rs` to verify that invalid JSON payloads (syntax errors, missing fields, wrong types) return `400 Bad Request` instead of crashing the server.
