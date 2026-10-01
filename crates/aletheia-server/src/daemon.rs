//! The AletheiaDB **daemon**: one process that owns the database files and
//! serves every local consumer (Issue #2905).
//!
//! # Why a daemon
//!
//! AletheiaDB is an embedded, single-writer store. Command-based MCP clients,
//! however, spawn *one server process per client session*. Without a daemon
//! that leaves exactly two bad options: every session opens its own ephemeral
//! database (agents silently disagree about reality), or every session opens the
//! **same** data directory (unsupported multi-process writers). On Windows it
//! also pins the executable, so `cargo install --force` fails at the final
//! rename with `Access is denied. (os error 5)`.
//!
//! This module makes the ownership boundary explicit: **one** `aletheia-daemon`
//! process opens the WAL, indexes, and cold storage, and everything else — the
//! CLI, HTTP clients, MCP agents, SDK callers — talks to it over HTTP.
//!
//! # What it serves
//!
//! [`run_server`] assembles the same routes the parity-tested proving ground
//! assembles ([`crate::app::all_routes`]) onto autumn's production `AppBuilder`,
//! and projects them three ways from the one set of handlers:
//!
//! | Surface | Path | Consumer |
//! |---|---|---|
//! | REST/JSON | `/nodes`, `/edges`, `/traverse`, … | CLI, SDKs, scripts |
//! | MCP (Streamable HTTP) | `/mcp` | agents — directly, or via the `aletheia-mcp` proxy |
//! | OpenAPI | `/openapi.json` | tooling |
//! | Metrics | `/metrics`, `/status` | operators |
//!
//! # Single ownership is enforced, not assumed
//!
//! [`DaemonLock`] holds `{data_dir}/daemon.lock` containing the owning pid. A
//! second daemon started against the same directory refuses to boot rather than
//! becoming an unsupported second writer. A lock left behind by a crashed
//! daemon is reclaimed (liveness is checked, not mere file existence).

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore, Role, SecretString, auth_mode_from_env};
pub use aletheiadb::daemon_lock::DaemonLock;
use autumn_web::config::{AutumnConfig, ConfigError, ConfigLoader, TomlEnvConfigLoader};
use autumn_web::openapi::OpenApiConfig;

use crate::app::all_routes;
use crate::security::{self, SecurityConfig};
use crate::state::ServerState;

/// Default daemon port (1963 — the year of the first Doctor Who broadcast, kept
/// consistent with the legacy HTTP server).
pub const DEFAULT_PORT: u16 = 1963;

/// Default bind address.
///
/// **Loopback**, deliberately. The daemon is a *local* database owner holding
/// unencrypted graph state; the legacy `aletheia-server` binary defaults to
/// `0.0.0.0` for containerized deployments, but a developer running
/// `aletheia daemon start` on a laptop should not thereby publish their database
/// to the local network. Set `ALETHEIADB_HOST=0.0.0.0` to opt in explicitly.
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// Name of the single-owner lock file inside the data directory.
///
/// Re-exported from [`aletheiadb::daemon_lock`], which owns the claim protocol
/// so the daemon, the CLI, and the embedded servers cannot drift apart about
/// who owns a data directory.
pub use aletheiadb::daemon_lock::LOCK_FILE_NAME;

/// Resolved daemon configuration.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    host: String,
    port: u16,
    data_dir: Option<PathBuf>,
    auth_mode: AuthMode,
    bootstrap_admin_key: Option<SecretString>,
}

impl DaemonConfig {
    /// A daemon configuration with defaults (loopback, port 1963, required
    /// auth, no data directory).
    #[must_use]
    pub fn new() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            data_dir: None,
            auth_mode: AuthMode::Required,
            bootstrap_admin_key: None,
        }
    }

    /// Resolve the configuration from the environment, using exactly the same
    /// variables the other AletheiaDB binaries read so a single exported
    /// environment configures the CLI, the server, and the daemon identically.
    ///
    /// | Variable | Meaning |
    /// |---|---|
    /// | `ALETHEIADB_HOST` | Bind address (default `127.0.0.1`) |
    /// | `ALETHEIADB_PORT` | Port (default `1963`) |
    /// | `ALETHEIADB_CONFIG` / `ALETHEIADB_DATA_DIR` | Database location |
    /// | `ALETHEIADB_AUTH_MODE` | `required` (default) or `anonymous` |
    /// | `ALETHEIADB_BOOTSTRAP_ADMIN_KEY` | Memory-only bootstrap admin key |
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::new();
        if let Ok(host) = std::env::var("ALETHEIADB_HOST")
            && !host.trim().is_empty()
        {
            config.host = host.trim().to_string();
        }
        if let Ok(raw) = std::env::var("ALETHEIADB_PORT") {
            match raw.trim().parse::<u16>() {
                Ok(port) => config.port = port,
                Err(e) => eprintln!(
                    "WARNING: invalid ALETHEIADB_PORT '{raw}': {e}. Using default {DEFAULT_PORT}."
                ),
            }
        }
        config.data_dir = aletheiadb::config::data_dir_from_env();
        config.auth_mode = auth_mode_from_env();
        config.bootstrap_admin_key = trimmed_secret_env("ALETHEIADB_BOOTSTRAP_ADMIN_KEY");
        config
    }

    /// Override the bind address.
    #[must_use]
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Override the port.
    #[must_use]
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Override the data directory (the database the daemon owns).
    #[must_use]
    pub fn with_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }

    /// Override the authentication mode.
    #[must_use]
    pub fn with_auth_mode(mut self, mode: AuthMode) -> Self {
        self.auth_mode = mode;
        self
    }

    /// Install a memory-only bootstrap admin key.
    #[must_use]
    pub fn with_bootstrap_admin_key(mut self, key: SecretString) -> Self {
        self.bootstrap_admin_key = Some(key);
        self
    }

    /// The bind address, as `host:port`.
    #[must_use]
    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// The base URL clients should use.
    #[must_use]
    pub fn base_url(&self) -> String {
        // An unspecified bind address (`0.0.0.0` / `::`) is not a usable client
        // target; advertise loopback instead of an address nothing can dial.
        let host = match self.host.parse::<IpAddr>() {
            Ok(ip) if ip.is_unspecified() => "127.0.0.1".to_string(),
            _ if self.host.contains(':') => format!("[{}]", self.host),
            _ => self.host.clone(),
        };
        format!("http://{host}:{}", self.port)
    }

    /// The MCP endpoint URL agents connect to.
    #[must_use]
    pub fn mcp_endpoint(&self) -> String {
        format!("{}/mcp", self.base_url())
    }

    /// The configured data directory, if any.
    #[must_use]
    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    /// The configured authentication mode.
    #[must_use]
    pub fn auth_mode(&self) -> AuthMode {
        self.auth_mode
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Operator-facing label for an authentication mode.
fn auth_mode_label(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::Required => "required",
        AuthMode::Anonymous => "ANONYMOUS (no authentication)",
    }
}

/// Read a secret-bearing environment variable into a redacted [`SecretString`].
///
/// Trimmed: presented credentials are trimmed at the transport layer, so an env
/// value with stray whitespace could otherwise never verify.
fn trimmed_secret_env(name: &str) -> Option<SecretString> {
    match std::env::var(name) {
        Ok(raw) if !raw.trim().is_empty() => Some(SecretString::new(raw.trim())),
        _ => None,
    }
}

// ────────────────────────────── config loader ───────────────────────────────

/// autumn [`ConfigLoader`] that runs the default five-layer load and then pins
/// the settings the daemon owns.
///
/// Three overrides, each for a reason:
///
/// 1. **host/port** come from `ALETHEIADB_*`, so operators configure the daemon
///    with AletheiaDB's own variables rather than autumn's `AUTUMN_SERVER__*`.
///    Doing it here (instead of `std::env::set_var`) means the production
///    daemon needs no `unsafe` env mutation at startup.
/// 2. **`actuator.sensitive = false`** in every profile: autumn mounts actuator
///    routes *outside* AletheiaDB's auth extractor, and its `dev` profile would
///    otherwise expose `/actuator/env`, `/actuator/configprops`, and an
///    unauthenticated `PUT /actuator/loggers/{name}`. Same hardening the legacy
///    server applies.
/// 3. **upload cap**, the only non-overridden knob for the app-wide body limit.
#[derive(Debug, Clone)]
struct DaemonConfigLoader {
    host: String,
    port: u16,
    max_request_body_bytes: usize,
}

impl ConfigLoader for DaemonConfigLoader {
    async fn load(&self) -> Result<AutumnConfig, ConfigError> {
        let mut config = TomlEnvConfigLoader::new().load().await?;
        config.server.host = self.host.clone();
        config.server.port = self.port;
        config.actuator.sensitive = false;
        config.security.upload.max_request_size_bytes = self.max_request_body_bytes;
        Ok(config)
    }
}

// ──────────────────────────────── the server ────────────────────────────────

/// The directory the daemon will **own**, resolved from every configuration
/// path — not just `ALETHEIADB_DATA_DIR`.
///
/// `ALETHEIADB_CONFIG` selects an equally durable database through a TOML file,
/// so deriving ownership from the env var alone would leave that deployment with
/// no lock, an ephemeral credential store, and no shutdown index flush — while
/// telling the operator it was running in-memory. Everything that depends on
/// "which directory do we own" reads this one answer.
fn owned_data_dir(config: &DaemonConfig) -> Option<PathBuf> {
    if let Some(dir) = config.data_dir() {
        return Some(dir.to_path_buf());
    }
    let path = aletheiadb::config::config_path_from_env()?;
    let toml = aletheiadb::config::AletheiaDBConfig::from_toml_file(&path).ok()?;
    let data_dir = toml.persistence.data_dir.clone();
    // The persistence layer's directory is `{root}/indexes` under the canonical
    // durable layout; the owned root is its parent when that holds.
    if data_dir.file_name().is_some_and(|name| name == "indexes") {
        data_dir.parent().map(Path::to_path_buf)
    } else {
        Some(data_dir)
    }
}

/// Open the database the daemon will own.
fn build_database(config: &DaemonConfig) -> Result<AletheiaDB, String> {
    match config.data_dir() {
        Some(dir) => AletheiaDB::open(dir)
            .map_err(|e| format!("failed to open database at '{}': {e}", dir.display())),
        None if aletheiadb::config::config_path_from_env().is_some() => {
            AletheiaDB::open_from_env().map_err(|e| format!("failed to open database: {e}"))
        }
        None => AletheiaDB::new().map_err(|e| format!("failed to create database: {e}")),
    }
}

/// Assemble the credential store: persisted under `{owned_dir}/auth/keys.json`
/// when the daemon owns a directory (the same path the other surfaces use, so
/// keys minted anywhere work everywhere), plus the optional memory-only
/// bootstrap admin key.
fn build_auth_store(
    config: &DaemonConfig,
    owned_dir: Option<&Path>,
) -> Result<Arc<AuthStore>, String> {
    let store = match owned_dir {
        Some(dir) => AuthStore::with_persistence(dir.join("auth").join("keys.json"))
            .map_err(|e| format!("failed to open auth key store: {e}"))?,
        None => AuthStore::new(),
    };
    if let Some(key) = &config.bootstrap_admin_key {
        store
            .insert_bootstrap_key("bootstrap-admin", Role::Admin, key.expose())
            .map_err(|e| format!("failed to install bootstrap admin key: {e}"))?;
    }
    Ok(Arc::new(store))
}

/// Whether a bind host accepts connections from beyond this machine.
fn is_public_bind(host: &str) -> bool {
    match host.parse::<IpAddr>() {
        Ok(ip) => !ip.is_loopback(),
        // A hostname resolves wherever DNS says; treat it as public rather than
        // guessing, so the anonymous-mode refusal below fails safe.
        Err(_) => !matches!(host, "localhost"),
    }
}

/// Environment variable that lets an operator deliberately serve an
/// **unauthenticated** daemon on a non-loopback address.
pub const ALLOW_ANONYMOUS_NETWORK_ENV: &str = "ALETHEIADB_ALLOW_ANONYMOUS_NETWORK";

/// Refuse the one combination that turns a local database into an open one.
///
/// `anonymous` alone is a supported local-development choice, and a non-loopback
/// bind alone is a supported deployment. Together they publish full read, write,
/// **and admin** access — including irreversible crypto-shred erasure — to
/// anyone who can reach the port. That deserves more than a warning line, which
/// `aletheia daemon start` redirects into a log file the operator may never open.
fn validate_exposure(config: &DaemonConfig) -> Result<(), String> {
    if config.auth_mode != AuthMode::Anonymous || !is_public_bind(&config.host) {
        return Ok(());
    }
    if std::env::var(ALLOW_ANONYMOUS_NETWORK_ENV).is_ok_and(|v| v == "1" || v == "true") {
        eprintln!(
            "WARNING: serving an UNAUTHENTICATED daemon on {} because \
             {ALLOW_ANONYMOUS_NETWORK_ENV} is set. Anyone who can reach this address \
             has full read, write, and admin access to the database.",
            config.bind_address()
        );
        return Ok(());
    }
    Err(format!(
        "refusing to serve an unauthenticated daemon on the non-loopback address '{}': \
         anonymous mode grants every caller full read, write, and admin access. \
         Configure authentication (unset ALETHEIADB_AUTH_MODE and set \
         ALETHEIADB_BOOTSTRAP_ADMIN_KEY), bind loopback (ALETHEIADB_HOST=127.0.0.1), \
         or set {ALLOW_ANONYMOUS_NETWORK_ENV}=1 to accept the risk deliberately.",
        config.host
    ))
}

/// Run the daemon to completion.
///
/// Claims the data directory, opens the database, assembles the HTTP + MCP +
/// OpenAPI surface, and serves until the process is signalled. Index state is
/// flushed on graceful shutdown so a stop/start cycle is lossless.
///
/// # Errors
///
/// Returns a human-readable message when the data directory is already owned by
/// a live daemon, the exposure is refused (anonymous auth on a public address),
/// the database cannot be opened, or the security configuration is refused
/// (authentication required with zero credentials).
pub async fn run_server(config: DaemonConfig) -> Result<(), String> {
    validate_exposure(&config)?;

    // Resolve ownership once, from every config path, and drive the lock, the
    // credential store, and the shutdown flush from that single answer.
    let owned_dir = owned_data_dir(&config);

    // Claim the directory BEFORE opening any storage: refusing early is what
    // keeps a second daemon from becoming an unsupported second writer.
    let _lock = match owned_dir.as_deref() {
        Some(dir) => Some(DaemonLock::acquire(dir)?),
        None => None,
    };

    let store = build_auth_store(&config, owned_dir.as_deref())?;
    let security_config = SecurityConfig::new(store, config.auth_mode);
    // Refuse a configuration whose every request would be rejected.
    security::validate_startup(&security_config)?;
    if config.auth_mode == AuthMode::Anonymous {
        eprintln!(
            "WARNING: AUTHENTICATION IS DISABLED (auth mode: anonymous). Every request \
             — including MCP tool calls — has full, unauthenticated access to the \
             database."
        );
    }

    let db = Arc::new(build_database(&config)?);
    let server_state = ServerState::new(Arc::clone(&db));
    let init_config = security_config.clone();
    let persist_on_shutdown = owned_dir.is_some();

    match owned_dir.as_deref() {
        Some(dir) => eprintln!("AletheiaDB daemon owning data directory: {}", dir.display()),
        None => eprintln!(
            "WARNING: no data directory configured — the daemon is running in-memory; \
             state is lost on shutdown. Set ALETHEIADB_DATA_DIR to own durable storage."
        ),
    }
    eprintln!("AletheiaDB daemon listening on {}", config.bind_address());
    eprintln!("  auth    {}", auth_mode_label(config.auth_mode));
    eprintln!("  HTTP    {}", config.base_url());
    eprintln!("  MCP     {}", config.mcp_endpoint());
    eprintln!("  OpenAPI {}/openapi.json", config.base_url());

    let loader = DaemonConfigLoader {
        host: config.host.clone(),
        port: config.port,
        // Sourced from the SAME `SecurityConfig` the layer stack is built from.
        // `SecurableApp::with_body_limit` is a no-op on the production builder
        // (autumn's own global limit would override an outer layer), so reading
        // the value from anywhere else would let the two surfaces diverge
        // silently — the exact failure the shared stack exists to prevent.
        max_request_body_bytes: security_config.max_request_body_bytes,
    };

    let app = autumn_web::app()
        .with_config_loader(loader)
        .routes(all_routes())
        .openapi(OpenApiConfig::new("AletheiaDB", env!("CARGO_PKG_VERSION")))
        .mount_mcp("/mcp");

    // The identical security stack the parity-tested proving ground mounts.
    let app = security::apply_security(app, &security_config);

    app.state_initializer(move |app_state| {
        app_state.insert_extension(server_state.clone());
        security::init_state(app_state, &init_config);
    })
    .on_shutdown(move || {
        let db = Arc::clone(&db);
        async move {
            if !persist_on_shutdown {
                return;
            }
            match tokio::task::spawn_blocking(move || db.persist_indexes()).await {
                Ok(Ok(())) => eprintln!("Shutdown: indexes persisted."),
                Ok(Err(e)) => eprintln!("Shutdown: persist_indexes failed: {e}"),
                Err(e) => eprintln!("Shutdown: persist_indexes task panicked: {e}"),
            }
        }
    })
    .run()
    .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_advertises_loopback_for_an_unspecified_bind_address() {
        let config = DaemonConfig::new().with_host("0.0.0.0").with_port(1963);
        assert_eq!(config.base_url(), "http://127.0.0.1:1963");
        assert_eq!(config.mcp_endpoint(), "http://127.0.0.1:1963/mcp");
    }

    #[test]
    fn base_url_keeps_an_explicit_host() {
        let config = DaemonConfig::new().with_host("db.internal").with_port(9000);
        assert_eq!(config.base_url(), "http://db.internal:9000");
    }

    #[test]
    fn base_url_brackets_a_literal_ipv6_host() {
        let config = DaemonConfig::new().with_host("::1").with_port(1963);
        assert_eq!(config.base_url(), "http://[::1]:1963");
    }

    #[test]
    fn default_bind_is_loopback() {
        assert_eq!(DaemonConfig::new().bind_address(), "127.0.0.1:1963");
    }

    // The lock's own behavior (exclusive claim, stale reclaim, live-owner
    // refusal, ownership-checked release) is tested where it lives, in
    // `aletheiadb::daemon_lock` — duplicating it here would only let the two
    // copies drift.

    #[test]
    fn anonymous_auth_on_a_public_bind_is_refused() {
        let config = DaemonConfig::new()
            .with_host("0.0.0.0")
            .with_auth_mode(AuthMode::Anonymous);
        let refused = validate_exposure(&config)
            .expect_err("an unauthenticated daemon must not bind a public address");
        assert!(
            refused.contains(ALLOW_ANONYMOUS_NETWORK_ENV),
            "the refusal names the deliberate opt-out: {refused}"
        );
    }

    #[test]
    fn anonymous_auth_on_loopback_is_allowed() {
        let config = DaemonConfig::new()
            .with_host("127.0.0.1")
            .with_auth_mode(AuthMode::Anonymous);
        assert!(
            validate_exposure(&config).is_ok(),
            "anonymous local development stays supported"
        );
    }

    #[test]
    fn authenticated_public_binds_are_allowed() {
        let config = DaemonConfig::new()
            .with_host("0.0.0.0")
            .with_auth_mode(AuthMode::Required);
        assert!(validate_exposure(&config).is_ok());
    }

    #[test]
    fn a_hostname_bind_counts_as_public() {
        // DNS decides where a hostname resolves, so the exposure check must not
        // assume it is local.
        assert!(is_public_bind("db.internal"));
        assert!(!is_public_bind("localhost"));
        assert!(!is_public_bind("::1"));
    }

    #[test]
    fn the_owned_directory_comes_from_the_data_dir_when_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = DaemonConfig::new().with_data_dir(dir.path());
        assert_eq!(owned_data_dir(&config).as_deref(), Some(dir.path()));
    }
}
