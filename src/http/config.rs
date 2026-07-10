//! HTTP server configuration.

/// Default maximum accepted request body size for the JSON API, in bytes (2 MiB).
///
/// The `/query` endpoint buffers and deserializes the entire request body into
/// a `serde_json` tree before any handler logic runs, so an unbounded body is a
/// memory-amplification / denial-of-service vector (a small compressed or
/// terse payload can expand into a very large in-memory structure). Capping the
/// body size bounds that allocation up front and returns `413 Payload Too Large`
/// for anything larger.
///
/// This value matches axum's historical implicit `DefaultBodyLimit` default, so
/// making it explicit changes no existing client behavior — it only makes the
/// bound intentional, operator-configurable, and covered by a regression test
/// (see Issue #3108 / this endpoint is otherwise protected only by an implicit
/// framework default that a middleware refactor could silently remove).
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;

use crate::auth::{AuthMode, SecretString};
use serde::Deserialize;

// ============================================================================
// Per-query resource limits (Issue #3368)
// ============================================================================

/// Default per-query wall-clock timeout, in milliseconds (30 s).
///
/// Bounds how long the HTTP `/query` handler waits for the underlying
/// computation before returning a prompt `429` to the client. See
/// [`QueryLimitsConfig`].
pub const DEFAULT_QUERY_TIMEOUT_MS: u64 = 30_000;

/// Operator hard ceiling for a per-call timeout override, in milliseconds
/// (5 min). A per-call `timeout_ms` above this is rejected with `422`.
pub const DEFAULT_MAX_QUERY_TIMEOUT_MS: u64 = 300_000;

/// Default cap on the number of rows/entities returned by a single query.
///
/// Deliberately aligned with the pre-existing hardcoded result caps in the
/// `/query` handlers (e.g. `MAX_EXEC_RESULTS`) so enabling limits by default
/// changes no existing behavior.
pub const DEFAULT_MAX_RESULT_ROWS: usize = 10_000;

/// Operator hard ceiling for a per-call `max_result_rows` override.
pub const DEFAULT_MAX_RESULT_ROWS_CEILING: usize = 100_000;

/// Default cap on the serialized response size of a single query, in bytes
/// (8 MiB). Generous: bounds pathological large responses without affecting
/// ordinary reads.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Operator hard ceiling for a per-call `max_response_bytes` override (64 MiB).
pub const DEFAULT_MAX_RESPONSE_BYTES_CEILING: usize = 64 * 1024 * 1024;

/// Which per-query resource-limit dimension a value applies to (Issue #3368).
///
/// The [`as_str`](Self::as_str) token is the stable `details.dimension` value
/// in the structured error body, so callers can branch on it without string
/// matching the human message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitDimension {
    /// The wall-clock time budget for producing the HTTP response.
    WallClockTimeout,
    /// The number of rows/entities in the result.
    ResultRows,
    /// The serialized byte size of the result.
    ResultBytes,
}

impl LimitDimension {
    /// Stable snake_case token used in `error.details.dimension`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WallClockTimeout => "wall_clock_timeout",
            Self::ResultRows => "result_rows",
            Self::ResultBytes => "result_bytes",
        }
    }
}

/// Behavior when a result exceeds the effective row cap (Issue #3368).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowOverflowPolicy {
    /// Return the first `max_result_rows` rows and flag the response
    /// `truncated: true`. Safe default for list-like reads.
    #[default]
    Truncate,
    /// Reject the whole response with a structured `413` error.
    Reject,
}

/// Per-call limit override carried on a `/query` request under `"limits"`.
///
/// Every field is optional and additive (`#[serde(default)]`), so existing
/// request bodies that omit `"limits"` parse and behave exactly as before.
/// An override present but *larger* than the operator ceiling is rejected
/// (`422`); an override *smaller* than the server default is honored (a caller
/// self-limiting tighter). A value of `0` means "unlimited" for that dimension
/// and is only accepted when the operator ceiling is itself unbounded.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct QueryLimitsOverride {
    /// Per-call wall-clock timeout in milliseconds (`0` = unlimited).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Per-call maximum result rows (`0` = unlimited).
    #[serde(default)]
    pub max_result_rows: Option<usize>,
    /// Per-call maximum serialized response bytes (`0` = unlimited).
    #[serde(default)]
    pub max_response_bytes: Option<usize>,
}

impl QueryLimitsOverride {
    /// True when no override fields are set (equivalent to no `"limits"` key).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.timeout_ms.is_none()
            && self.max_result_rows.is_none()
            && self.max_response_bytes.is_none()
    }
}

/// A per-call override that exceeded the operator hard ceiling (Issue #3368).
///
/// Maps to `422 INVALID_ARGUMENT` with
/// `details: {dimension, requested, ceiling}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitOverrideError {
    /// The dimension whose override was rejected.
    pub dimension: LimitDimension,
    /// The value the caller requested.
    pub requested: u64,
    /// The operator ceiling it exceeded.
    pub ceiling: u64,
}

/// The concrete limits in force for a single query, after folding the
/// server defaults, the per-call override, and the operator ceilings
/// (Issue #3368). A value of `0` on any dimension means "unlimited".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveQueryLimits {
    /// Wall-clock timeout in milliseconds (`0` = unlimited).
    pub timeout_ms: u64,
    /// Maximum result rows (`0` = unlimited).
    pub max_result_rows: usize,
    /// Maximum serialized response bytes (`0` = unlimited).
    pub max_response_bytes: usize,
    /// What to do when the row cap is exceeded.
    pub row_overflow: RowOverflowPolicy,
}

impl EffectiveQueryLimits {
    /// No enforcement on any dimension.
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            timeout_ms: 0,
            max_result_rows: 0,
            max_response_bytes: 0,
            row_overflow: RowOverflowPolicy::Truncate,
        }
    }
}

/// Fold one dimension's server default, operator ceiling, and per-call
/// override into a single effective value.
///
/// Semantics (all in the dimension's native unit; `0` = unlimited, a ceiling
/// of `0` = no ceiling):
///
/// - **Override present**: rejected when a ceiling exists and the override
///   either requests unlimited (`0`) or exceeds the ceiling; otherwise the
///   override wins (it may be *below* the default — a tighter self-limit).
/// - **Override absent**: the server default, silently clamped down to the
///   ceiling if the default is unlimited or above it.
fn merge_dimension(
    default_v: u64,
    ceiling: u64,
    override_v: Option<u64>,
    dimension: LimitDimension,
) -> Result<u64, LimitOverrideError> {
    match override_v {
        Some(requested) => {
            if ceiling != 0 && (requested == 0 || requested > ceiling) {
                return Err(LimitOverrideError {
                    dimension,
                    requested,
                    ceiling,
                });
            }
            Ok(requested)
        }
        None => {
            let mut effective = default_v;
            if ceiling != 0 && (effective == 0 || effective > ceiling) {
                effective = ceiling;
            }
            Ok(effective)
        }
    }
}

/// Server-level configuration for per-query resource limits (Issue #3368).
///
/// Each dimension has a **default** (applied when the request supplies no
/// override), a **ceiling** (the largest value a per-call override may
/// request; `0` = no ceiling), and — for rows — an overflow **policy**. A
/// value of `0` on a default/override means "unlimited" for that dimension.
///
/// The master [`enabled`](Self::enabled) switch disables all enforcement when
/// `false` (see [`disabled`](Self::disabled)): the effective limits are then
/// unconditionally unlimited and per-call overrides are ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryLimitsConfig {
    /// Master switch. When `false`, no limit is enforced.
    pub enabled: bool,
    /// Default wall-clock timeout in milliseconds (`0` = unlimited).
    pub default_timeout_ms: u64,
    /// Operator ceiling for a per-call timeout override (`0` = no ceiling).
    pub max_timeout_ms: u64,
    /// Default maximum result rows (`0` = unlimited).
    pub default_max_result_rows: usize,
    /// Operator ceiling for a per-call row override (`0` = no ceiling).
    pub max_result_rows: usize,
    /// Default maximum serialized response bytes (`0` = unlimited).
    pub default_max_response_bytes: usize,
    /// Operator ceiling for a per-call byte override (`0` = no ceiling).
    pub max_response_bytes: usize,
    /// What to do when a result exceeds the effective row cap.
    pub row_overflow: RowOverflowPolicy,
}

impl QueryLimitsConfig {
    /// A config that enforces nothing: every dimension unlimited, overrides
    /// ignored. Useful for trusted embedded deployments and tests.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            default_timeout_ms: 0,
            max_timeout_ms: 0,
            default_max_result_rows: 0,
            max_result_rows: 0,
            default_max_response_bytes: 0,
            max_response_bytes: 0,
            row_overflow: RowOverflowPolicy::Truncate,
        }
    }

    /// Fold this config and an optional per-call override into the concrete
    /// [`EffectiveQueryLimits`] for one query.
    ///
    /// Returns the effective limits, or a [`LimitOverrideError`] when a
    /// supplied override exceeds an operator ceiling. When
    /// [`enabled`](Self::enabled) is `false`, this always returns
    /// [`EffectiveQueryLimits::unlimited`] and never errors (overrides are
    /// ignored).
    ///
    /// # Errors
    ///
    /// Returns [`LimitOverrideError`] if `override_` requests a value above the
    /// corresponding ceiling (or requests unlimited under a finite ceiling).
    pub fn effective(
        &self,
        override_: Option<&QueryLimitsOverride>,
    ) -> Result<EffectiveQueryLimits, LimitOverrideError> {
        if !self.enabled {
            return Ok(EffectiveQueryLimits::unlimited());
        }
        let ov = override_.filter(|o| !o.is_empty());
        let timeout_ms = merge_dimension(
            self.default_timeout_ms,
            self.max_timeout_ms,
            ov.and_then(|o| o.timeout_ms),
            LimitDimension::WallClockTimeout,
        )?;
        let max_result_rows = merge_dimension(
            self.default_max_result_rows as u64,
            self.max_result_rows as u64,
            ov.and_then(|o| o.max_result_rows).map(|v| v as u64),
            LimitDimension::ResultRows,
        )? as usize;
        let max_response_bytes = merge_dimension(
            self.default_max_response_bytes as u64,
            self.max_response_bytes as u64,
            ov.and_then(|o| o.max_response_bytes).map(|v| v as u64),
            LimitDimension::ResultBytes,
        )? as usize;
        Ok(EffectiveQueryLimits {
            timeout_ms,
            max_result_rows,
            max_response_bytes,
            row_overflow: self.row_overflow,
        })
    }
}

impl Default for QueryLimitsConfig {
    /// Enabled by default, but with generous limits chosen so no existing
    /// request behavior changes (Issue #3368).
    fn default() -> Self {
        Self {
            enabled: true,
            default_timeout_ms: DEFAULT_QUERY_TIMEOUT_MS,
            max_timeout_ms: DEFAULT_MAX_QUERY_TIMEOUT_MS,
            default_max_result_rows: DEFAULT_MAX_RESULT_ROWS,
            max_result_rows: DEFAULT_MAX_RESULT_ROWS_CEILING,
            default_max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES_CEILING,
            row_overflow: RowOverflowPolicy::Truncate,
        }
    }
}

/// CORS (Cross-Origin Resource Sharing) configuration.
#[derive(Debug, Clone)]
pub struct CorsConfig {
    /// Allowed origins. Empty means allow any origin (development mode only).
    allowed_origins: Vec<String>,
    /// Allowed HTTP methods.
    allowed_methods: Vec<String>,
    /// Allowed HTTP headers.
    allowed_headers: Vec<String>,
    /// Max age for preflight cache in seconds.
    max_age: u32,
}

impl CorsConfig {
    /// Create a permissive CORS config for development.
    ///
    /// # Security Warning
    ///
    /// This allows any origin and should NOT be used in production.
    /// Use [`CorsConfig::restrictive`] or configure specific origins instead.
    pub fn permissive() -> Self {
        Self {
            allowed_origins: vec![],
            allowed_methods: vec![
                "GET".to_string(),
                "POST".to_string(),
                "PUT".to_string(),
                "DELETE".to_string(),
                "OPTIONS".to_string(),
            ],
            allowed_headers: vec!["Content-Type".to_string(), "Authorization".to_string()],
            max_age: 3600,
        }
    }

    /// Create a restrictive CORS config with no allowed origins.
    ///
    /// You must explicitly add allowed origins using [`CorsConfig::allow_origin`].
    pub fn restrictive() -> Self {
        Self {
            allowed_origins: vec!["http://localhost:3000".to_string()],
            allowed_methods: vec!["GET".to_string(), "POST".to_string(), "OPTIONS".to_string()],
            allowed_headers: vec!["Content-Type".to_string()],
            max_age: 3600,
        }
    }

    /// Add an allowed origin.
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.allowed_origins.push(origin.into());
        self
    }

    /// Set allowed HTTP methods.
    pub fn allowed_methods(mut self, methods: Vec<String>) -> Self {
        self.allowed_methods = methods;
        self
    }

    /// Set allowed HTTP headers.
    pub fn allowed_headers(mut self, headers: Vec<String>) -> Self {
        self.allowed_headers = headers;
        self
    }

    /// Set max age for preflight cache.
    pub fn max_age(mut self, seconds: u32) -> Self {
        self.max_age = seconds;
        self
    }

    /// Check if any origin is allowed (permissive mode).
    pub fn is_permissive(&self) -> bool {
        self.allowed_origins.is_empty()
    }

    /// Get allowed origins.
    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed_origins
    }

    /// Get allowed methods.
    pub fn get_allowed_methods(&self) -> &[String] {
        &self.allowed_methods
    }

    /// Get allowed headers.
    pub fn get_allowed_headers(&self) -> &[String] {
        &self.allowed_headers
    }

    /// Get max age in seconds.
    pub fn get_max_age(&self) -> u32 {
        self.max_age
    }
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self::restrictive()
    }
}

/// Rate limiting configuration.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Number of requests allowed per second per IP.
    requests_per_second: u32,
    /// Maximum burst size (concurrent requests) per IP.
    burst_size: u32,
}

impl RateLimitConfig {
    /// Create a new rate limit configuration.
    pub fn new(requests_per_second: u32, burst_size: u32) -> Self {
        Self {
            requests_per_second,
            burst_size,
        }
    }

    /// Get requests per second.
    pub fn requests_per_second(&self) -> u32 {
        self.requests_per_second
    }

    /// Get burst size.
    pub fn burst_size(&self) -> u32 {
        self.burst_size
    }

    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.requests_per_second == 0 {
            return Err("requests_per_second must be > 0".to_string());
        }
        if self.burst_size == 0 {
            return Err("burst_size must be > 0".to_string());
        }
        Ok(())
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_second: 10,
            burst_size: 20,
        }
    }
}

/// Configuration for the HTTP server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    port: u16,
    host: String,
    cors: CorsConfig,
    rate_limit: RateLimitConfig,
    /// When `Some`, WAL + index persistence are written under this directory
    /// so restarts preserve state. When `None`, the server runs on an in-memory
    /// `AletheiaDB::new()` (useful for tests and ephemeral demos).
    data_dir: Option<std::path::PathBuf>,
    /// Maximum accepted request body size in bytes. Requests with a larger body
    /// are rejected with `413 Payload Too Large` before deserialization. See
    /// [`DEFAULT_MAX_REQUEST_BODY_BYTES`].
    max_request_body_bytes: usize,
    /// Authentication mode. Defaults to [`AuthMode::Required`]: the server
    /// refuses to start without at least one credential unless the operator
    /// explicitly opts into anonymous mode.
    auth_mode: AuthMode,
    /// Bootstrap admin key installed at startup (from env/config), if any.
    /// Wrapped in [`SecretString`] so it never appears in `Debug` output.
    bootstrap_admin_key: Option<SecretString>,
    /// Explicit path for the persisted auth key store. When unset and
    /// [`data_dir`](Self::data_dir) is set, defaults to
    /// `{data_dir}/auth/keys.json`.
    auth_persist_path: Option<std::path::PathBuf>,
    /// Per-query resource limits (timeout / result rows / response bytes)
    /// enforced by the `/query` handler (Issue #3368).
    query_limits: QueryLimitsConfig,
}

impl ServerConfig {
    /// Create a new server config with the specified port.
    ///
    /// Uses default host of "0.0.0.0" (all interfaces), restrictive CORS,
    /// default rate limiting (10 req/s, 20 burst), and **no** data directory
    /// (database is in-memory).
    pub fn new(port: u16) -> Self {
        Self {
            port,
            host: "0.0.0.0".to_string(),
            cors: CorsConfig::default(),
            rate_limit: RateLimitConfig::default(),
            data_dir: None,
            max_request_body_bytes: DEFAULT_MAX_REQUEST_BODY_BYTES,
            auth_mode: AuthMode::default(),
            bootstrap_admin_key: None,
            auth_persist_path: None,
            query_limits: QueryLimitsConfig::default(),
        }
    }

    /// Get the configured port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get the configured host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Get the CORS configuration.
    pub fn cors(&self) -> &CorsConfig {
        &self.cors
    }

    /// Get the rate limit configuration.
    pub fn rate_limit(&self) -> &RateLimitConfig {
        &self.rate_limit
    }

    /// Get the maximum accepted request body size in bytes.
    ///
    /// Requests whose body exceeds this are rejected with `413 Payload Too
    /// Large` before the JSON payload is buffered or deserialized.
    pub fn max_request_body_bytes(&self) -> usize {
        self.max_request_body_bytes
    }

    /// Get the per-query resource limits configuration (Issue #3368).
    pub fn query_limits(&self) -> &QueryLimitsConfig {
        &self.query_limits
    }

    /// Get the configured data directory, if any.
    ///
    /// When `Some(path)`, the server will create `{path}/wal` and
    /// `{path}/indexes` for durable storage. When `None`, the server runs
    /// on an in-memory database.
    pub fn data_dir(&self) -> Option<&std::path::Path> {
        self.data_dir.as_deref()
    }

    /// Get the configured authentication mode (default: [`AuthMode::Required`]).
    pub fn auth_mode(&self) -> AuthMode {
        self.auth_mode
    }

    /// Get the bootstrap admin key, if configured.
    pub fn bootstrap_admin_key(&self) -> Option<&SecretString> {
        self.bootstrap_admin_key.as_ref()
    }

    /// Get the explicitly configured auth persist path, if any.
    ///
    /// Most callers want [`resolved_auth_persist_path`](Self::resolved_auth_persist_path),
    /// which also applies the `data_dir` derivation.
    pub fn auth_persist_path(&self) -> Option<&std::path::Path> {
        self.auth_persist_path.as_deref()
    }

    /// Resolve where the auth key store should be persisted:
    ///
    /// 1. The explicit [`auth_persist_path`](Self::auth_persist_path) when set.
    /// 2. Otherwise `{data_dir}/auth/keys.json` when a data dir is set.
    /// 3. Otherwise `None` (memory-only auth store).
    #[must_use]
    pub fn resolved_auth_persist_path(&self) -> Option<std::path::PathBuf> {
        if let Some(explicit) = &self.auth_persist_path {
            return Some(explicit.clone());
        }
        self.data_dir
            .as_deref()
            .map(|d| d.join("auth").join("keys.json"))
    }

    /// Materialize the [`AletheiaDBConfig`] this server config implies.
    ///
    /// Returns `None` when no [`data_dir`](Self::data_dir) is set — that
    /// signals in-memory mode, and the caller should construct the DB via
    /// [`AletheiaDB::new`](crate::AletheiaDB::new) instead.
    ///
    /// When set, delegates to [`crate::config::durable_config_for_data_dir`]
    /// so every exposed binary (HTTP server, MCP server, CLI, Python SDK)
    /// uses the same persistence shape.
    #[must_use]
    pub fn to_unified_config(&self) -> Option<crate::AletheiaDBConfig> {
        self.data_dir
            .as_deref()
            .map(crate::config::durable_config_for_data_dir)
    }

    /// Get the bind address as "host:port".
    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Create a builder for more complex configuration.
    pub fn builder() -> ServerConfigBuilder {
        ServerConfigBuilder::default()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 1963,
            host: "0.0.0.0".to_string(),
            cors: CorsConfig::default(),
            rate_limit: RateLimitConfig::default(),
            data_dir: None,
            max_request_body_bytes: DEFAULT_MAX_REQUEST_BODY_BYTES,
            auth_mode: AuthMode::default(),
            bootstrap_admin_key: None,
            auth_persist_path: None,
            query_limits: QueryLimitsConfig::default(),
        }
    }
}

/// Builder for [`ServerConfig`].
#[derive(Debug, Clone, Default)]
pub struct ServerConfigBuilder {
    port: Option<u16>,
    host: Option<String>,
    cors: Option<CorsConfig>,
    rate_limit: Option<RateLimitConfig>,
    data_dir: Option<std::path::PathBuf>,
    max_request_body_bytes: Option<usize>,
    auth_mode: Option<AuthMode>,
    bootstrap_admin_key: Option<SecretString>,
    auth_persist_path: Option<std::path::PathBuf>,
    query_limits: Option<QueryLimitsConfig>,
}

impl ServerConfigBuilder {
    /// Set the port to bind to.
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set the host to bind to.
    ///
    /// # Valid Values
    ///
    /// - IPv4 addresses: "0.0.0.0", "127.0.0.1", etc.
    /// - IPv6 addresses: "::", "::1", etc.
    /// - Hostnames: "localhost", etc.
    ///
    /// The host is validated at bind time by the underlying server.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Set the CORS configuration.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::http::{ServerConfig, CorsConfig};
    ///
    /// let config = ServerConfig::builder()
    ///     .port(1963)
    ///     .cors(CorsConfig::restrictive().allow_origin("https://myapp.com"))
    ///     .build();
    /// ```
    pub fn cors(mut self, cors: CorsConfig) -> Self {
        self.cors = Some(cors);
        self
    }

    /// Set the rate limit configuration.
    pub fn rate_limit(mut self, rate_limit: RateLimitConfig) -> Self {
        self.rate_limit = Some(rate_limit);
        self
    }

    /// Set a data directory for durable WAL + index persistence.
    ///
    /// The server will create `{path}/wal` and `{path}/indexes` on startup.
    /// If `None` (the default), the database is in-memory and everything is
    /// lost on shutdown.
    pub fn data_dir(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.data_dir = Some(path.into());
        self
    }

    /// Set the maximum accepted request body size in bytes.
    ///
    /// Requests with a larger body are rejected with `413 Payload Too Large`
    /// before the JSON payload is buffered or deserialized, bounding the
    /// memory a single request can force the server to allocate. Defaults to
    /// [`DEFAULT_MAX_REQUEST_BODY_BYTES`] (2 MiB).
    pub fn max_request_body_bytes(mut self, bytes: usize) -> Self {
        self.max_request_body_bytes = Some(bytes);
        self
    }

    /// Set the per-query resource limits (Issue #3368).
    ///
    /// Controls the `/query` handler's wall-clock timeout, result-row cap, and
    /// response-byte cap, plus their per-call override ceilings. Defaults to
    /// [`QueryLimitsConfig::default`] (enabled, generous). Pass
    /// [`QueryLimitsConfig::disabled`] to turn all enforcement off.
    pub fn query_limits(mut self, limits: QueryLimitsConfig) -> Self {
        self.query_limits = Some(limits);
        self
    }

    /// Set the authentication mode.
    ///
    /// Defaults to [`AuthMode::Required`]. [`AuthMode::Anonymous`] disables
    /// authentication entirely and must be an explicit, deliberate opt-in.
    pub fn auth_mode(mut self, mode: AuthMode) -> Self {
        self.auth_mode = Some(mode);
        self
    }

    /// Set a bootstrap admin key installed at startup (principal
    /// `bootstrap-admin`, role `admin`). Memory-only; never persisted.
    pub fn bootstrap_admin_key(mut self, key: SecretString) -> Self {
        self.bootstrap_admin_key = Some(key);
        self
    }

    /// Set an explicit path for the persisted auth key store.
    ///
    /// When unset, `{data_dir}/auth/keys.json` is used if a data dir is
    /// configured; otherwise the store is memory-only.
    pub fn auth_persist_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.auth_persist_path = Some(path.into());
        self
    }

    /// Build the server configuration.
    pub fn build(self) -> ServerConfig {
        ServerConfig {
            port: self.port.unwrap_or(1963),
            host: self.host.unwrap_or_else(|| "0.0.0.0".to_string()),
            cors: self.cors.unwrap_or_default(),
            rate_limit: self.rate_limit.unwrap_or_default(),
            data_dir: self.data_dir,
            max_request_body_bytes: self
                .max_request_body_bytes
                .unwrap_or(DEFAULT_MAX_REQUEST_BODY_BYTES),
            auth_mode: self.auth_mode.unwrap_or_default(),
            bootstrap_admin_key: self.bootstrap_admin_key,
            auth_persist_path: self.auth_persist_path,
            query_limits: self.query_limits.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.port(), 1963);
        assert_eq!(config.host(), "0.0.0.0");
        assert!(!config.cors().is_permissive());
    }

    #[test]
    fn test_new_with_port() {
        let config = ServerConfig::new(3000);
        assert_eq!(config.port(), 3000);
        assert_eq!(config.host(), "0.0.0.0");
    }

    #[test]
    fn test_builder() {
        let config = ServerConfig::builder().port(9000).host("127.0.0.1").build();
        assert_eq!(config.port(), 9000);
        assert_eq!(config.host(), "127.0.0.1");
    }

    #[test]
    fn test_bind_address() {
        let config = ServerConfig::builder().port(1963).host("localhost").build();
        assert_eq!(config.bind_address(), "localhost:1963");
    }

    #[test]
    fn test_cors_permissive() {
        let cors = CorsConfig::permissive();
        assert!(cors.is_permissive());
        assert!(cors.allowed_origins().is_empty());
    }

    #[test]
    fn test_cors_restrictive() {
        let cors = CorsConfig::restrictive();
        assert!(!cors.is_permissive());
        assert!(!cors.allowed_origins().is_empty());
    }

    #[test]
    fn test_cors_allow_origin() {
        let cors = CorsConfig::restrictive().allow_origin("https://example.com");
        assert!(
            cors.allowed_origins()
                .contains(&"https://example.com".to_string())
        );
    }

    #[test]
    fn test_builder_with_cors() {
        let config = ServerConfig::builder()
            .port(1963)
            .cors(CorsConfig::permissive())
            .build();
        assert!(config.cors().is_permissive());
    }

    #[test]
    fn test_default_rate_limit() {
        let config = ServerConfig::default();
        assert_eq!(config.rate_limit().requests_per_second(), 10);
        assert_eq!(config.rate_limit().burst_size(), 20);
    }

    #[test]
    fn test_custom_rate_limit() {
        let rate_limit = RateLimitConfig::new(100, 200);
        let config = ServerConfig::builder().rate_limit(rate_limit).build();
        assert_eq!(config.rate_limit().requests_per_second(), 100);
        assert_eq!(config.rate_limit().burst_size(), 200);
    }

    /// Auth defaults are conservative: Required mode, no bootstrap key,
    /// no persist path (Issue #3350).
    #[test]
    fn test_default_auth_is_required() {
        let config = ServerConfig::default();
        assert_eq!(config.auth_mode(), AuthMode::Required);
        assert!(config.bootstrap_admin_key().is_none());
        assert!(config.auth_persist_path().is_none());
        assert!(config.resolved_auth_persist_path().is_none());

        // `new(port)` and the builder default the same way.
        assert_eq!(ServerConfig::new(3000).auth_mode(), AuthMode::Required);
        assert_eq!(
            ServerConfig::builder().build().auth_mode(),
            AuthMode::Required
        );
    }

    #[test]
    fn test_auth_builder_settings() {
        let config = ServerConfig::builder()
            .auth_mode(AuthMode::Anonymous)
            .bootstrap_admin_key(SecretString::new("boot-key"))
            .auth_persist_path("/tmp/keys.json")
            .build();
        assert_eq!(config.auth_mode(), AuthMode::Anonymous);
        assert_eq!(
            config.bootstrap_admin_key().map(SecretString::expose),
            Some("boot-key")
        );
        assert_eq!(
            config.resolved_auth_persist_path(),
            Some(std::path::PathBuf::from("/tmp/keys.json"))
        );
    }

    #[test]
    fn test_auth_persist_path_derives_from_data_dir() {
        let config = ServerConfig::builder().data_dir("/data/aletheia").build();
        assert_eq!(
            config.resolved_auth_persist_path(),
            Some(std::path::PathBuf::from("/data/aletheia/auth/keys.json"))
        );

        // Explicit path wins over the derivation.
        let config = ServerConfig::builder()
            .data_dir("/data/aletheia")
            .auth_persist_path("/elsewhere/keys.json")
            .build();
        assert_eq!(
            config.resolved_auth_persist_path(),
            Some(std::path::PathBuf::from("/elsewhere/keys.json"))
        );
    }

    #[test]
    fn test_config_debug_does_not_leak_bootstrap_key() {
        let config = ServerConfig::builder()
            .bootstrap_admin_key(SecretString::new("super-secret-bootstrap"))
            .build();
        let debug = format!("{config:?}");
        assert!(!debug.contains("super-secret-bootstrap"));
    }

    // ------------------------------------------------------------------
    // Per-query resource limits (Issue #3368)
    // ------------------------------------------------------------------

    #[test]
    fn query_limits_default_is_enabled_and_generous() {
        let limits = QueryLimitsConfig::default();
        assert!(limits.enabled);
        assert_eq!(limits.default_timeout_ms, DEFAULT_QUERY_TIMEOUT_MS);
        assert_eq!(limits.max_timeout_ms, DEFAULT_MAX_QUERY_TIMEOUT_MS);
        assert_eq!(limits.default_max_result_rows, DEFAULT_MAX_RESULT_ROWS);
        assert_eq!(limits.row_overflow, RowOverflowPolicy::Truncate);

        // ServerConfig wires the default through.
        let config = ServerConfig::default();
        assert_eq!(config.query_limits(), &QueryLimitsConfig::default());
    }

    #[test]
    fn query_limits_builder_setter_overrides_default() {
        let limits = QueryLimitsConfig::disabled();
        let config = ServerConfig::builder().query_limits(limits.clone()).build();
        assert_eq!(config.query_limits(), &limits);
        assert!(!config.query_limits().enabled);
    }

    #[test]
    fn disabled_limits_ignore_overrides_and_never_error() {
        let limits = QueryLimitsConfig::disabled();
        // Even a wildly-over-ceiling override yields unlimited, no error.
        let over = QueryLimitsOverride {
            timeout_ms: Some(u64::MAX),
            max_result_rows: Some(usize::MAX),
            max_response_bytes: Some(usize::MAX),
        };
        let eff = limits
            .effective(Some(&over))
            .expect("disabled never errors");
        assert_eq!(eff, EffectiveQueryLimits::unlimited());
    }

    #[test]
    fn effective_uses_defaults_when_no_override() {
        let limits = QueryLimitsConfig::default();
        let eff = limits.effective(None).expect("defaults are valid");
        assert_eq!(eff.timeout_ms, DEFAULT_QUERY_TIMEOUT_MS);
        assert_eq!(eff.max_result_rows, DEFAULT_MAX_RESULT_ROWS);
        assert_eq!(eff.max_response_bytes, DEFAULT_MAX_RESPONSE_BYTES);
    }

    #[test]
    fn effective_empty_override_is_treated_as_absent() {
        let limits = QueryLimitsConfig::default();
        let empty = QueryLimitsOverride::default();
        assert!(empty.is_empty());
        let eff = limits.effective(Some(&empty)).expect("empty override ok");
        assert_eq!(eff.timeout_ms, DEFAULT_QUERY_TIMEOUT_MS);
    }

    #[test]
    fn override_within_ceiling_is_honored() {
        let limits = QueryLimitsConfig::default();
        let over = QueryLimitsOverride {
            timeout_ms: Some(5_000),
            max_result_rows: Some(50),
            max_response_bytes: Some(1024),
        };
        let eff = limits.effective(Some(&over)).expect("within ceiling");
        assert_eq!(eff.timeout_ms, 5_000);
        assert_eq!(eff.max_result_rows, 50);
        assert_eq!(eff.max_response_bytes, 1024);
    }

    #[test]
    fn override_below_default_is_honored_tighter() {
        // Default rows is 10_000; a caller may self-limit to 5.
        let limits = QueryLimitsConfig::default();
        let over = QueryLimitsOverride {
            max_result_rows: Some(5),
            ..Default::default()
        };
        let eff = limits.effective(Some(&over)).expect("tighter self-limit");
        assert_eq!(eff.max_result_rows, 5);
        // Untouched dimensions keep their defaults.
        assert_eq!(eff.timeout_ms, DEFAULT_QUERY_TIMEOUT_MS);
    }

    #[test]
    fn override_above_ceiling_is_rejected_per_dimension() {
        let limits = QueryLimitsConfig::default();

        let too_slow = QueryLimitsOverride {
            timeout_ms: Some(DEFAULT_MAX_QUERY_TIMEOUT_MS + 1),
            ..Default::default()
        };
        let err = limits.effective(Some(&too_slow)).unwrap_err();
        assert_eq!(err.dimension, LimitDimension::WallClockTimeout);
        assert_eq!(err.requested, DEFAULT_MAX_QUERY_TIMEOUT_MS + 1);
        assert_eq!(err.ceiling, DEFAULT_MAX_QUERY_TIMEOUT_MS);

        let too_many_rows = QueryLimitsOverride {
            max_result_rows: Some(DEFAULT_MAX_RESULT_ROWS_CEILING + 1),
            ..Default::default()
        };
        let err = limits.effective(Some(&too_many_rows)).unwrap_err();
        assert_eq!(err.dimension, LimitDimension::ResultRows);

        let too_big = QueryLimitsOverride {
            max_response_bytes: Some(DEFAULT_MAX_RESPONSE_BYTES_CEILING + 1),
            ..Default::default()
        };
        let err = limits.effective(Some(&too_big)).unwrap_err();
        assert_eq!(err.dimension, LimitDimension::ResultBytes);
    }

    #[test]
    fn override_requesting_unlimited_under_finite_ceiling_is_rejected() {
        let limits = QueryLimitsConfig::default();
        let unlimited = QueryLimitsOverride {
            timeout_ms: Some(0), // 0 = unlimited, but a finite ceiling exists
            ..Default::default()
        };
        let err = limits.effective(Some(&unlimited)).unwrap_err();
        assert_eq!(err.dimension, LimitDimension::WallClockTimeout);
        assert_eq!(err.requested, 0);
    }

    #[test]
    fn absent_override_clamps_unlimited_default_to_ceiling() {
        // Default unlimited (0) but a finite ceiling → clamped down silently.
        let limits = QueryLimitsConfig {
            default_timeout_ms: 0,
            max_timeout_ms: 1_000,
            ..QueryLimitsConfig::default()
        };
        let eff = limits.effective(None).expect("clamped, not rejected");
        assert_eq!(eff.timeout_ms, 1_000);
    }

    #[test]
    fn absent_override_clamps_default_above_ceiling() {
        let limits = QueryLimitsConfig {
            default_timeout_ms: 90_000,
            max_timeout_ms: 60_000,
            ..QueryLimitsConfig::default()
        };
        let eff = limits.effective(None).expect("clamped");
        assert_eq!(eff.timeout_ms, 60_000);
    }

    #[test]
    fn no_ceiling_allows_any_override_including_unlimited() {
        let limits = QueryLimitsConfig {
            max_timeout_ms: 0, // no ceiling
            ..QueryLimitsConfig::default()
        };
        let over = QueryLimitsOverride {
            timeout_ms: Some(0), // unlimited, allowed when ceiling is 0
            ..Default::default()
        };
        let eff = limits.effective(Some(&over)).expect("no ceiling");
        assert_eq!(eff.timeout_ms, 0);
    }

    #[test]
    fn limit_dimension_tokens_are_stable() {
        assert_eq!(
            LimitDimension::WallClockTimeout.as_str(),
            "wall_clock_timeout"
        );
        assert_eq!(LimitDimension::ResultRows.as_str(), "result_rows");
        assert_eq!(LimitDimension::ResultBytes.as_str(), "result_bytes");
    }

    #[test]
    fn query_limits_override_deserializes_partial() {
        // Only one field present; the rest default to None.
        let ov: QueryLimitsOverride =
            serde_json::from_value(serde_json::json!({ "timeout_ms": 1234 })).unwrap();
        assert_eq!(ov.timeout_ms, Some(1234));
        assert_eq!(ov.max_result_rows, None);
        assert_eq!(ov.max_response_bytes, None);
    }

    #[test]
    fn test_rate_limit_validation() {
        let valid = RateLimitConfig::new(10, 20);
        assert!(valid.validate().is_ok());

        let invalid_rps = RateLimitConfig::new(0, 20);
        assert!(invalid_rps.validate().is_err());

        let invalid_burst = RateLimitConfig::new(10, 0);
        assert!(invalid_burst.validate().is_err());
    }
}
