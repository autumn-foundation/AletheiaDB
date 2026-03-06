//! HTTP server creation and management.

use actix_cors::Cors;
use actix_governor::{Governor, GovernorConfigBuilder};
use actix_web::{
    App, HttpServer,
    dev::Server,
    middleware::{DefaultHeaders, Logger},
    web,
};
use tokio::sync::oneshot;

use super::config::{CorsConfig, RateLimitConfig, ServerConfig};
use super::handlers::{configure_health_routes, handle_query};

/// Handle for gracefully shutting down the server.
#[derive(Debug)]
pub struct ShutdownHandle {
    sender: Option<oneshot::Sender<()>>,
}

impl ShutdownHandle {
    /// Create a new shutdown handle with the given sender.
    fn new(sender: oneshot::Sender<()>) -> Self {
        Self {
            sender: Some(sender),
        }
    }

    /// Trigger graceful shutdown of the server.
    ///
    /// This will signal the server to stop accepting new connections
    /// and wait for existing connections to complete.
    pub async fn shutdown(mut self) {
        if let Some(sender) = self.sender.take() {
            // Ignore error if receiver is already dropped
            let _ = sender.send(());
        }
    }
}

/// Configure the Actix-web application routes.
///
/// This function sets up the HTTP routes:
/// - Health check endpoint at `/status`
///
/// Note: This function only configures routes, not middleware.
/// Middleware (CORS, logging) is configured in [`create_app`] or [`create_server`].
///
/// # Example
///
/// ```ignore
/// use actix_web::App;
/// use aletheiadb::http::configure_app;
///
/// let app = App::new().configure(configure_app);
/// ```
pub fn configure_app(cfg: &mut web::ServiceConfig) {
    configure_health_routes(cfg);
    cfg.route("/query", web::post().to(handle_query));
}

/// Build security headers middleware.
fn build_security_headers() -> DefaultHeaders {
    DefaultHeaders::new()
        .add(("X-Content-Type-Options", "nosniff"))
        .add(("X-Frame-Options", "DENY"))
        .add((
            "Content-Security-Policy",
            "default-src 'none'; frame-ancestors 'none'",
        ))
}

/// Build CORS middleware from configuration.
fn build_cors(cors_config: &CorsConfig) -> Cors {
    let mut cors = Cors::default();

    if cors_config.is_permissive() {
        // Development mode: allow any origin
        cors = cors.allow_any_origin();
    } else {
        // Production mode: allow only configured origins
        for origin in cors_config.allowed_origins() {
            cors = cors.allowed_origin(origin);
        }
    }

    // Configure allowed methods
    let methods: Vec<actix_web::http::Method> = cors_config
        .get_allowed_methods()
        .iter()
        .filter_map(|m| m.parse().ok())
        .collect();
    cors = cors.allowed_methods(methods);

    // Configure allowed headers
    let headers: Vec<actix_web::http::header::HeaderName> = cors_config
        .get_allowed_headers()
        .iter()
        .filter_map(|h| h.parse().ok())
        .collect();
    cors = cors.allowed_headers(headers);

    cors.max_age(cors_config.get_max_age() as usize)
}

/// Build rate limiting middleware from configuration.
fn build_rate_limit(
    rate_limit_config: &RateLimitConfig,
) -> Result<
    Governor<
        actix_governor::PeerIpKeyExtractor,
        actix_governor::governor::middleware::NoOpMiddleware,
    >,
    String,
> {
    rate_limit_config.validate()?;

    let governor_conf = GovernorConfigBuilder::default()
        .requests_per_second(u64::from(rate_limit_config.requests_per_second()))
        .burst_size(rate_limit_config.burst_size())
        .finish()
        .ok_or_else(|| "Failed to build rate limit configuration".to_string())?;
    Ok(Governor::new(&governor_conf))
}

/// Create a configured Actix-web application factory with all middleware.
///
/// This creates an app with **permissive CORS** suitable for development/testing.
/// For production use, prefer [`create_server`] with proper CORS settings.
///
/// This includes:
/// - Request logging middleware
/// - Permissive CORS support (allows any origin)
/// - All configured routes
///
/// # Security Warning
///
/// This function uses permissive CORS settings. For production deployments,
/// use [`create_server`] with a properly configured [`ServerConfig`].
pub fn create_app() -> App<
    impl actix_web::dev::ServiceFactory<
        actix_web::dev::ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    let cors_config = CorsConfig::permissive();
    let rate_limit_config = RateLimitConfig::default();
    App::new()
        .wrap(Logger::default())
        .wrap(build_security_headers())
        .wrap(build_cors(&cors_config))
        .wrap(
            build_rate_limit(&rate_limit_config)
                .expect("Default rate limit config should be valid"),
        )
        .configure(configure_app)
}

/// Create an HTTP server with the given configuration.
///
/// Returns the server instance and a shutdown handle that can be used
/// to gracefully terminate the server.
///
/// # Arguments
///
/// * `config` - Server configuration including port, host, and CORS settings
///
/// # Returns
///
/// A tuple of `(Server, ShutdownHandle)` where:
/// - `Server` is the Actix-web server that can be awaited
/// - `ShutdownHandle` can be used to trigger graceful shutdown
///
/// # Example
///
/// ```ignore
/// use aletheiadb::http::{ServerConfig, CorsConfig, create_server};
///
/// let config = ServerConfig::builder()
///     .port(8080)
///     .cors(CorsConfig::restrictive().allow_origin("https://myapp.com"))
///     .build();
/// let (server, shutdown) = create_server(config).await?;
///
/// // Later, to shut down:
/// shutdown.shutdown().await;
/// ```
pub async fn create_server(config: ServerConfig) -> std::io::Result<(Server, ShutdownHandle)> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let shutdown_handle = ShutdownHandle::new(shutdown_tx);

    let bind_address = config.bind_address();
    let cors_config = config.cors().clone();
    let rate_limit_config = config.rate_limit().clone();

    // Validate rate limit config early
    if let Err(e) = rate_limit_config.validate() {
        return Err(std::io::Error::other(e));
    }

    let server = HttpServer::new(move || {
        // We can unwrap here because we validated above, but to be 100% safe
        // against race conditions (though config is cloned), we handle it.
        // Actually, inside the closure, we can't easily return errors to the caller of `new`.
        // But since we validated `rate_limit_config` before creating the server,
        // `build_rate_limit` should succeed unless `GovernorConfigBuilder` fails for other reasons.
        // If it fails, we panic, which is acceptable inside the factory closure if invariants are violated.
        let rate_limit = build_rate_limit(&rate_limit_config)
            .expect("Rate limit configuration should be valid after pre-validation");

        App::new()
            .wrap(Logger::default())
            .wrap(build_security_headers())
            .wrap(build_cors(&cors_config))
            .wrap(rate_limit)
            .configure(configure_app)
    })
    .bind(&bind_address)?
    .disable_signals() // We handle signals ourselves
    .run();

    // Spawn a task that waits for shutdown signal using actix runtime
    let server_handle = server.handle();
    actix_rt::spawn(async move {
        let _ = shutdown_rx.await;
        server_handle.stop(true).await;
    });

    Ok((server, shutdown_handle))
}

/// Run the HTTP server with the given configuration.
///
/// This function blocks until the server is shut down via SIGTERM or SIGINT.
///
/// # Arguments
///
/// * `config` - Server configuration including port, host, and CORS settings
///
/// # Example
///
/// ```ignore
/// use aletheiadb::http::{ServerConfig, CorsConfig, run_server};
///
/// #[actix_web::main]
/// async fn main() -> std::io::Result<()> {
///     let config = ServerConfig::builder()
///         .port(8080)
///         .cors(CorsConfig::restrictive().allow_origin("https://myapp.com"))
///         .build();
///     run_server(config).await
/// }
/// ```
pub async fn run_server(config: ServerConfig) -> std::io::Result<()> {
    let bind_address = config.bind_address();
    let cors_config = config.cors().clone();
    let rate_limit_config = config.rate_limit().clone();

    eprintln!("Starting AletheiaDB HTTP server on {}", bind_address);
    if cors_config.is_permissive() {
        eprintln!(
            "WARNING: CORS is configured in permissive mode (any origin allowed). \
             This is not recommended for production."
        );
    }

    // Validate rate limit config early
    if let Err(e) = rate_limit_config.validate() {
        return Err(std::io::Error::other(e));
    }

    HttpServer::new(move || {
        let rate_limit = build_rate_limit(&rate_limit_config)
            .expect("Rate limit configuration should be valid after pre-validation");

        App::new()
            .wrap(Logger::default())
            .wrap(build_security_headers())
            .wrap(build_cors(&cors_config))
            .wrap(rate_limit)
            .configure(configure_app)
    })
    .bind(&bind_address)?
    .run()
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test;

    #[actix_rt::test]
    async fn test_configure_app_has_health_endpoint() {
        let app = test::init_service(App::new().configure(configure_app)).await;

        let req = test::TestRequest::get()
            .peer_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 12345)))
            .uri("/status")
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert!(resp.status().is_success());
    }

    #[actix_rt::test]
    async fn test_create_app_with_cors() {
        let app = test::init_service(create_app()).await;

        let req = test::TestRequest::default()
            .peer_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 12345)))
            .method(actix_web::http::Method::OPTIONS)
            .uri("/status")
            .insert_header(("Origin", "http://example.com"))
            .insert_header(("Access-Control-Request-Method", "GET"))
            .to_request();

        let resp = test::call_service(&app, req).await;

        // CORS preflight should succeed
        assert!(resp.status().is_success() || resp.status().as_u16() == 204);
    }

    #[actix_rt::test]
    async fn test_unknown_route_returns_404() {
        let app = test::init_service(create_app()).await;

        let req = test::TestRequest::get()
            .peer_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 12345)))
            .uri("/nonexistent")
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status().as_u16(), 404);
    }

    #[actix_rt::test]
    async fn test_build_cors_permissive() {
        let cors_config = CorsConfig::permissive();
        let _cors = build_cors(&cors_config);
        // If we get here without panic, CORS middleware was created successfully
    }

    #[actix_rt::test]
    async fn test_build_cors_restrictive() {
        let cors_config = CorsConfig::restrictive();
        let _cors = build_cors(&cors_config);
        // If we get here without panic, CORS middleware was created successfully
    }

    #[actix_rt::test]
    async fn test_security_headers() {
        let app = test::init_service(create_app()).await;

        let req = test::TestRequest::get()
            .peer_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 12345)))
            .uri("/status")
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert!(resp.status().is_success());
        let headers = resp.headers();

        assert_eq!(headers.get("X-Content-Type-Options").unwrap(), "nosniff");
        assert_eq!(headers.get("X-Frame-Options").unwrap(), "DENY");
        assert_eq!(
            headers.get("Content-Security-Policy").unwrap(),
            "default-src 'none'; frame-ancestors 'none'"
        );
    }

    #[actix_rt::test]
    async fn test_rate_limiting() {
        // Configure a very strict rate limit: 1 request per second, burst 1
        let rate_limit_config = RateLimitConfig::new(1, 1);

        let app = test::init_service(
            App::new()
                .wrap(build_rate_limit(&rate_limit_config).unwrap())
                .configure(configure_app),
        )
        .await;

        // First request should succeed
        let req1 = test::TestRequest::get()
            .peer_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 12345)))
            .uri("/status")
            .to_request();
        let resp1 = test::call_service(&app, req1).await;
        assert!(resp1.status().is_success());

        // Second request immediately after should fail with 429 Too Many Requests
        let req2 = test::TestRequest::get()
            .peer_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 12345)))
            .uri("/status")
            .to_request();
        let resp2 = test::call_service(&app, req2).await;
        assert_eq!(resp2.status().as_u16(), 429);
    }

    #[actix_rt::test]
    async fn test_rate_limit_replenishment() {
        // 2 requests per second, burst 1
        // If per_second(2) means 2 req/s, then after 500ms we should have 1 token.
        // If per_second(2) means 1 req per 2s, then after 500ms we have 0.25 tokens.
        let rate_limit_config = RateLimitConfig::new(2, 1);

        let app = test::init_service(
            App::new()
                .wrap(build_rate_limit(&rate_limit_config).unwrap())
                .configure(configure_app),
        )
        .await;

        // First request
        let req1 = test::TestRequest::get()
            .peer_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 12345)))
            .uri("/status")
            .to_request();
        let resp1 = test::call_service(&app, req1).await;
        assert!(resp1.status().is_success());

        // Wait 600ms
        actix_rt::time::sleep(std::time::Duration::from_millis(600)).await;

        // Second request
        let req2 = test::TestRequest::get()
            .peer_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 12345)))
            .uri("/status")
            .to_request();
        let resp2 = test::call_service(&app, req2).await;

        // If this succeeds, it confirms per_second(2) means 2 req/s
        assert!(
            resp2.status().is_success(),
            "Rate limit did not replenish in time"
        );
    }
}
