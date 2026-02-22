//! HTTP server creation and management.

use actix_cors::Cors;
use actix_web::{
    App, HttpServer,
    dev::Server,
    middleware::{Condition, DefaultHeaders, Logger},
    web,
};
use tokio::sync::oneshot;

use super::auth::ApiKeyMiddleware;
use super::config::{CorsConfig, ServerConfig};
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
/// Middleware (CORS, logging) is configured in [`create_app`] or [`create_app_with_config`].
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

/// Create a configured Actix-web application factory with all middleware.
///
/// This function is used by [`create_server`] and [`run_server`] to ensure consistent configuration.
///
/// # Arguments
///
/// * `config` - Server configuration including port, host, CORS, and API key settings.
pub fn create_app_with_config(
    config: ServerConfig,
) -> App<
    impl actix_web::dev::ServiceFactory<
        actix_web::dev::ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    let cors_config = config.cors();
    let api_key = config.get_api_key().map(|k| k.to_string());
    let has_key = api_key.is_some();

    App::new()
        .wrap(Logger::default())
        .wrap(build_security_headers())
        .wrap(build_cors(cors_config))
        .wrap(Condition::new(
            has_key,
            ApiKeyMiddleware::new(api_key.unwrap_or_default()),
        ))
        .configure(configure_app)
}

/// Create a configured Actix-web application factory with all middleware.
///
/// This creates an app with **permissive CORS** suitable for development/testing.
/// For production use, prefer [`create_app_with_config`] with proper CORS settings.
///
/// This includes:
/// - Request logging middleware
/// - Permissive CORS support (allows any origin)
/// - All configured routes
///
/// # Security Warning
///
/// This function uses permissive CORS settings and NO authentication.
/// For production deployments, use [`create_app_with_config`] with a properly configured [`ServerConfig`].
pub fn create_app() -> App<
    impl actix_web::dev::ServiceFactory<
        actix_web::dev::ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    // Create a default config which has permissive CORS for backward compatibility
    let config = ServerConfig::builder()
        .cors(CorsConfig::permissive())
        .build();

    create_app_with_config(config)
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
    // We need to clone config for the factory closure
    let config = std::sync::Arc::new(config);

    let server = HttpServer::new(move || create_app_with_config((*config).clone()))
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

    eprintln!("Starting AletheiaDB HTTP server on {}", bind_address);
    if config.cors().is_permissive() {
        eprintln!(
            "WARNING: CORS is configured in permissive mode (any origin allowed).              This is not recommended for production."
        );
    }
    if config.get_api_key().is_none() {
        eprintln!("WARNING: No API key configured. The server is open to public access.");
    }

    // Clone config for factory
    let config = std::sync::Arc::new(config);

    HttpServer::new(move || create_app_with_config((*config).clone()))
        .bind(&bind_address)?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{ResponseError, test};

    #[actix_rt::test]
    async fn test_configure_app_has_health_endpoint() {
        let app = test::init_service(App::new().configure(configure_app)).await;

        let req = test::TestRequest::get().uri("/status").to_request();
        let resp = test::call_service(&app, req).await;

        assert!(resp.status().is_success());
    }

    #[actix_rt::test]
    async fn test_create_app_with_cors() {
        let app = test::init_service(create_app()).await;

        let req = test::TestRequest::default()
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

        let req = test::TestRequest::get().uri("/nonexistent").to_request();
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

        let req = test::TestRequest::get().uri("/status").to_request();
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
    async fn test_auth_middleware_blocks_request_without_key() {
        let config = ServerConfig::builder().api_key("secret").build();
        let app = test::init_service(create_app_with_config(config)).await;

        let req = test::TestRequest::get().uri("/status").to_request();

        // Use match instead of unwrap_err because Debug is missing
        match test::try_call_service(&app, req).await {
            Ok(_) => panic!("Should have failed with 401"),
            Err(err) => {
                let resp = err.error_response();
                assert_eq!(resp.status().as_u16(), 401);
            }
        }
    }

    #[actix_rt::test]
    async fn test_auth_middleware_blocks_request_with_wrong_key() {
        let config = ServerConfig::builder().api_key("secret").build();
        let app = test::init_service(create_app_with_config(config)).await;

        let req = test::TestRequest::get()
            .uri("/status")
            .insert_header(("Authorization", "Bearer wrong"))
            .to_request();

        match test::try_call_service(&app, req).await {
            Ok(_) => panic!("Should have failed with 401"),
            Err(err) => {
                let resp = err.error_response();
                assert_eq!(resp.status().as_u16(), 401);
            }
        }
    }

    #[actix_rt::test]
    async fn test_auth_middleware_allows_request_with_correct_key() {
        let config = ServerConfig::builder().api_key("secret").build();
        let app = test::init_service(create_app_with_config(config)).await;

        let req = test::TestRequest::get()
            .uri("/status")
            .insert_header(("Authorization", "Bearer secret"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert!(resp.status().is_success());
    }

    #[actix_rt::test]
    async fn test_no_auth_by_default() {
        let config = ServerConfig::default();
        let app = test::init_service(create_app_with_config(config)).await;

        let req = test::TestRequest::get().uri("/status").to_request();
        let resp = test::call_service(&app, req).await;

        assert!(resp.status().is_success());
    }
}
