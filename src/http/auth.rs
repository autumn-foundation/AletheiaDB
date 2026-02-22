//! Authentication middleware.

use std::future::{Ready, ready};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::{Error, HttpResponse, http::header};

use super::handlers::ApiResponse;

/// Middleware for API Key authentication.
pub struct ApiKeyMiddleware {
    key: Rc<String>,
}

impl ApiKeyMiddleware {
    /// Create a new API Key middleware.
    pub fn new(key: String) -> Self {
        Self { key: Rc::new(key) }
    }
}

impl<S, B> Transform<S, ServiceRequest> for ApiKeyMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = ApiKeyMiddlewareService<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(ApiKeyMiddlewareService {
            service,
            key: self.key.clone(),
        }))
    }
}

pub struct ApiKeyMiddlewareService<S> {
    service: S,
    key: Rc<String>,
}

impl<S, B> Service<ServiceRequest> for ApiKeyMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&self, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(ctx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let auth_header = req.headers().get(header::AUTHORIZATION);
        let key = self.key.clone();

        let is_valid = if let Some(auth_val) = auth_header {
            if let Ok(auth_str) = auth_val.to_str() {
                if auth_str.starts_with("Bearer ") {
                    let token = &auth_str[7..];
                    token == key.as_str()
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        if !is_valid {
            return Box::pin(async move {
                let json_error = ApiResponse::error("Unauthorized".to_string());
                let resp = HttpResponse::Unauthorized().json(json_error);
                // Convert HttpResponse to ServiceResponse
                // We need to map the body to B, but we don't know B.
                // However, ServiceResponse::new takes BoxBody usually?
                // req.into_response(resp) returns ServiceResponse<BoxBody> usually?
                // Wait, if B is not BoxBody, we have a type mismatch.
                // But most Actix apps use ServiceResponse<BoxBody> or similar.
                // Transform trait says Response = ServiceResponse<B>.
                // So we MUST return ServiceResponse<B>.

                // If B is MessageBody, we can use map_into_boxed_body() or similar?
                // Or maybe we can't easily return a response here if B is generic.

                // If I use Err(InternalError...), Actix handles it.
                // In tests, I should handle the error.

                // But wait, if I use Err, the middleware returns Err.
                // App services return Result<ServiceResponse, Error>.
                // So the test sees Err.
                // I should update the test to expect Err or use try_call_service.

                // But wait, I want to verify the JSON content of the error response.
                // If I use Err, I can't easily check the body unless I convert Error to Response.

                // Let's try to return ServiceResponse using into_response.
                // But I need to cast the body to B.
                // Since I don't know B, this is hard.

                // Actually, standard practice is to use Err and let the framework handle it.
                // In tests, I can use test::try_call_service which returns Result<ServiceResponse, Error>.
                // Then I can assert it is Err.
                // And I can check if the error is the one I expect.
                // But checking JSON body of an Error is hard.

                // Alternative:
                // Use ? No.

                // If I look at how  middleware works, it usually bounds B.
                //  middleware handles body types.

                // I will stick to returning Err, and update the TEST to handle it.
                // Because that's how Actix middleware usually rejects requests.
                // The issue is just the test helper  panicking.

                use actix_web::error::InternalError;
                Err(InternalError::from_response("", resp).into())
            });
        }

        let fut = self.service.call(req);
        Box::pin(async move {
            let res = fut.await?;
            Ok(res)
        })
    }
}
