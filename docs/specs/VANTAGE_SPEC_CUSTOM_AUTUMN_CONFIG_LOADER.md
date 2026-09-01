# 🔭 Vantage: Spec for Custom Autumn Config Loader

## 👤 User Story
**As a** Systems Engineer deploying AletheiaDB,
**I want** the HTTP server to load its configuration cleanly via code rather than relying on global environment variable mutations,
**so that** I can avoid unsafe concurrent behavior, simplify deployment, and ensure predictable startup regardless of the host environment state.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, AletheiaDB bridges its internal `ServerConfig` into the `autumn` web framework by manually setting `AUTUMN_*__*` environment variables. Because mutating the environment in Rust via `std::env::set_var` is fundamentally unsafe in concurrent contexts (and is explicitly marked as `unsafe` in Edition 2024), this approach carries an inherent risk of Undefined Behavior (UB) if any background threads happen to read the environment concurrently. By replacing this hack with a custom `ConfigLoader` natively supported by `autumn` 0.3+, we eliminate `unsafe` blocks, prevent subtle initialization crashes in multi-tenant environments, and improve the overall reliability of the database server.

**Metric Definition:**
- **Code Quality/Safety:** Zero uses of `unsafe` for environment manipulation (`std::env::set_var`) in `src/http/server.rs`.
- **Initialization Reliability:** 100% of concurrent startup tests pass without UB or environment-related panics.

**Gap Analysis:**
- *Current State:* `src/http/server.rs` uses an `unsafe` block to call `apply_autumn_env`, mutating the process environment as a workaround for autumn 0.2.0 limitations.
- *Required State:* Implement a custom `ConfigLoader` trait (or equivalent mechanism in autumn 0.3) to pass configuration structures directly without touching the OS environment variables.

## ✅ Acceptance Criteria
- Must remove the `unsafe` block and calls to `std::env::set_var` in `src/http/server.rs`.
- Must construct an `autumn` configuration object directly from `AletheiaDB`'s `ServerConfig`.
- Must successfully configure the `autumn` server (including host, port, and CORS settings) without relying on `AUTUMN_` prefixed environment variables.
- Must ensure that all existing HTTP configuration tests pass.

## 🚫 Out of Scope (Phase 1)
- Dynamic configuration reloading without restarting the HTTP server.
- Full migration off the `autumn` framework.
