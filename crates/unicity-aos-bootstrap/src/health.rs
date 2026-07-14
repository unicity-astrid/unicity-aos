//! Product-owned loopback runtime health endpoint.
//!
//! This deliberately consumes Astrid Runtime's existing authenticated
//! agent-readiness operation and projects it to one public fact: whether the
//! runtime is ready. It is not a proxy for the runtime control protocol.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use astrid_core::PrincipalId;
use astrid_core::kernel_api::{KernelRequest, KernelResponse};
use astrid_uplink::KernelClient;
use axum::extract::{OriginalUri, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, on};
use axum::{Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;

/// Fixed product bind address. The health service never listens off-host.
pub const LOOPBACK_ADDR: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// Fixed local port for the AOS health service.
pub const HEALTH_PORT: u16 = 8765;

/// A source of the one runtime fact this HTTP edge is allowed to expose.
pub trait RuntimeReadiness: Send + Sync + 'static {
    /// Return whether the product runtime is currently ready.
    fn ready(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

/// Readiness client using Astrid Runtime's existing authenticated local IPC.
#[derive(Debug, Default, Clone, Copy)]
pub struct AstridRuntimeReadiness;

impl RuntimeReadiness for AstridRuntimeReadiness {
    fn ready(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async {
            let Ok(Ok(mut client)) = tokio::time::timeout(
                Duration::from_secs(5),
                KernelClient::connect(PrincipalId::default()),
            )
            .await
            else {
                return false;
            };

            matches!(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    client.request(KernelRequest::GetAgentReadiness),
                )
                .await,
                Ok(Ok(KernelResponse::AgentReadiness(report))) if report.ready
            )
        })
    }
}

#[derive(Debug, Serialize)]
struct HealthBody {
    ready: bool,
}

/// Build the health router. It has exactly one GET-only, query-free endpoint.
pub fn router<R>(readiness: R) -> Router
where
    R: RuntimeReadiness,
{
    Router::new()
        .route("/v1/runtime/health", on(MethodFilter::GET, health::<R>))
        .with_state(Arc::new(readiness))
}

async fn health<R>(
    State(readiness): State<Arc<R>>,
    OriginalUri(uri): OriginalUri,
    method: Method,
) -> Response
where
    R: RuntimeReadiness,
{
    // Axum routes HEAD through GET by default. This endpoint is intentionally
    // GET-only, including no implicit HEAD variant.
    if method != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    // Query parameters would create a caller-controlled health surface. This
    // endpoint intentionally has none.
    if uri.query().is_some() {
        return StatusCode::NOT_FOUND.into_response();
    }

    if readiness.ready().await {
        (StatusCode::OK, Json(HealthBody { ready: true })).into_response()
    } else {
        // The same body covers unavailable, denied, malformed, timed-out, and
        // genuinely unready runtime states. No runtime diagnostic crosses HTTP.
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthBody { ready: false }),
        )
            .into_response()
    }
}

/// Reject every address except literal IPv4 loopback.
pub fn validate_bind_address(address: SocketAddr) -> std::io::Result<()> {
    if address.ip() == IpAddr::V4(LOOPBACK_ADDR) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Unicity AOS health service must bind to 127.0.0.1",
        ))
    }
}

/// Run the product health service on the fixed loopback endpoint.
///
/// # Errors
/// Returns an error when the loopback listener cannot be created or the server
/// cannot run.
pub async fn serve_default() -> std::io::Result<()> {
    let address = SocketAddr::from((LOOPBACK_ADDR, HEALTH_PORT));
    validate_bind_address(address)?;
    let listener = TcpListener::bind(address).await?;
    axum::serve(listener, router(AstridRuntimeReadiness)).await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;

    #[derive(Default)]
    struct StubReadiness(AtomicBool);

    impl StubReadiness {
        fn with_ready(ready: bool) -> Self {
            Self(AtomicBool::new(ready))
        }
    }

    impl RuntimeReadiness for StubReadiness {
        fn ready(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
            let ready = self.0.load(Ordering::Relaxed);
            Box::pin(async move { ready })
        }
    }

    #[tokio::test]
    async fn ready_runtime_returns_only_the_ready_boolean() {
        let response = router(StubReadiness::with_ready(true))
            .oneshot(
                Request::get("/v1/runtime/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), br#"{"ready":true}"#);
    }

    #[tokio::test]
    async fn every_non_ready_state_has_the_same_generic_response() {
        let response = router(StubReadiness::with_ready(false))
            .oneshot(
                Request::get("/v1/runtime/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), br#"{"ready":false}"#);
    }

    #[tokio::test]
    async fn route_rejects_other_methods_paths_and_query_controls() {
        let app = router(StubReadiness::with_ready(true));
        let post = app
            .clone()
            .oneshot(
                Request::post("/v1/runtime/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);

        let head = app
            .clone()
            .oneshot(
                Request::head("/v1/runtime/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::METHOD_NOT_ALLOWED);

        let query = app
            .clone()
            .oneshot(
                Request::get("/v1/runtime/health?principal=other")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(query.status(), StatusCode::NOT_FOUND);

        let other = app
            .oneshot(
                Request::get("/v1/runtime/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(other.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn bind_validation_allows_only_literal_ipv4_loopback() {
        assert!(validate_bind_address(SocketAddr::from((LOOPBACK_ADDR, 0))).is_ok());
        assert!(validate_bind_address("0.0.0.0:8765".parse().unwrap()).is_err());
        assert!(validate_bind_address("[::1]:8765".parse().unwrap()).is_err());
    }
}
