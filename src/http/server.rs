//! HTTP server creation and management.

use actix_cors::Cors;
use actix_web::{App, HttpServer, dev::Server, middleware::Logger, web};
use tokio::sync::oneshot;

use super::config::ServerConfig;
use super::handlers::configure_health_routes;

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

/// Configure the Actix-web application with all routes and middleware.
///
/// This function sets up:
/// - Health check endpoint at `/status`
/// - CORS middleware for cross-origin requests
/// - Request logging middleware
///
/// # Example
///
/// ```ignore
/// use actix_web::App;
/// use gallifreydb::http::configure_app;
///
/// let app = App::new().configure(configure_app);
/// ```
pub fn configure_app(cfg: &mut web::ServiceConfig) {
    configure_health_routes(cfg);
}

/// Create a configured Actix-web application factory with all middleware.
///
/// This includes:
/// - Request logging
/// - CORS support
/// - All configured routes
pub fn create_app() -> App<
    impl actix_web::dev::ServiceFactory<
        actix_web::dev::ServiceRequest,
        Config = (),
        Response = actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    App::new()
        // Enable request logging
        .wrap(Logger::default())
        // Enable CORS
        .wrap(
            Cors::default()
                .allow_any_origin()
                .allow_any_method()
                .allow_any_header()
                .max_age(3600),
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
/// * `config` - Server configuration including port and host
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
/// use gallifreydb::http::{ServerConfig, create_server};
///
/// let config = ServerConfig::new(8080);
/// let (server, shutdown) = create_server(config).await?;
///
/// // Later, to shut down:
/// shutdown.shutdown().await;
/// ```
pub async fn create_server(config: ServerConfig) -> std::io::Result<(Server, ShutdownHandle)> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let shutdown_handle = ShutdownHandle::new(shutdown_tx);

    let bind_address = config.bind_address();

    let server = HttpServer::new(create_app)
        .bind(&bind_address)?
        .disable_signals() // We handle signals ourselves
        .run();

    // Spawn a task that waits for shutdown signal
    let server_handle = server.handle();
    tokio::spawn(async move {
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
/// * `config` - Server configuration including port and host
///
/// # Example
///
/// ```ignore
/// use gallifreydb::http::{ServerConfig, run_server};
///
/// #[actix_web::main]
/// async fn main() -> std::io::Result<()> {
///     let config = ServerConfig::new(8080);
///     run_server(config).await
/// }
/// ```
pub async fn run_server(config: ServerConfig) -> std::io::Result<()> {
    let bind_address = config.bind_address();

    eprintln!("Starting GallifreyDB HTTP server on {}", bind_address);

    HttpServer::new(create_app).bind(&bind_address)?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test;

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
}
