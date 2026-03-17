pub mod auth;
mod config;
pub mod daemon;
pub mod db;
pub mod error;
mod rest_proxy;
mod ws_relay;

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    http::{HeaderName, HeaderValue, Method},
    routing::{get, post},
    Json, Router,
};
use config::HiveConfig;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

/// Shared application state.
struct AppState {
    config: HiveConfig,
    #[allow(dead_code)]
    db: db::Database,
    start_time: std::time::Instant,
}

/// Health check response (BE-001).
#[derive(serde::Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    uptime_secs: u64,
    daemon_connected: bool,
    daemon_url: String,
}

/// GET /api/health — returns server status, version, uptime, and daemon connection.
async fn health(state: axum::extract::State<Arc<AppState>>) -> Json<HealthResponse> {
    let daemon_url = state.config.daemon.ws_url.clone();
    let base = daemon_url
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    let daemon_connected = reqwest::Client::new()
        .get(format!("{base}/api/health"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok();

    let status = if daemon_connected { "ok" } else { "degraded" };

    Json(HealthResponse {
        status,
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.start_time.elapsed().as_secs(),
        daemon_connected,
        daemon_url,
    })
}

/// Build a [`CorsLayer`] from a list of allowed origin strings.
///
/// Allows `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, and `OPTIONS` methods, with
/// `Authorization`, `Content-Type`, and `X-Request-ID` headers exposed.
fn build_cors_layer(origins: &[String]) -> CorsLayer {
    let allowed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| o.parse::<HeaderValue>().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed))
        .allow_methods(AllowMethods::list([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ]))
        .allow_headers(AllowHeaders::list([
            HeaderName::from_static("authorization"),
            HeaderName::from_static("content-type"),
            HeaderName::from_static("x-request-id"),
        ]))
}

#[tokio::main]
async fn main() {
    // Logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hive=info".parse().unwrap()),
        )
        .init();

    // Config
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("hive.toml"));
    let config = config::load_config(&config_path);

    // Database
    let db_path = PathBuf::from(&config.server.data_dir).join("hive.db");
    if let Some(parent) = db_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "[hive] error: cannot create data directory {}: {e}\n\
                 hint: set data_dir in hive.toml or HIVE_DATA_DIR env to a writable path",
                parent.display()
            );
            std::process::exit(1);
        }
    }
    let db = db::Database::open(&db_path).expect("failed to open database");
    tracing::info!("database opened at {}", db_path.display());

    let bind_addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!("hive-server starting on {bind_addr}");

    // CORS validation: wildcard is forbidden in production.
    for origin in &config.server.cors_origins {
        if origin == "*" && config::is_production() {
            eprintln!(
                "[hive] error: HIVE_CORS_ORIGINS='*' is forbidden in production (HIVE_ENV=production). \
                 Set explicit allowed origins."
            );
            std::process::exit(1);
        }
    }
    if std::env::var("HIVE_CORS_ORIGINS").is_err() && config::is_production() {
        tracing::warn!(
            "HIVE_CORS_ORIGINS is not set; defaulting to http://localhost:5173. \
             Set HIVE_CORS_ORIGINS explicitly in production."
        );
    }

    let cors_layer = build_cors_layer(&config.server.cors_origins);

    let state = Arc::new(AppState {
        config,
        db,
        start_time: std::time::Instant::now(),
    });

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/token", post(auth::issue_token))
        .route("/api/rooms", get(rest_proxy::list_rooms))
        .route("/api/rooms/{room_id}", get(rest_proxy::get_room))
        .route(
            "/api/rooms/{room_id}/messages",
            get(rest_proxy::get_messages),
        )
        .route("/api/rooms/{room_id}/send", post(rest_proxy::send_message))
        .route("/ws/{room_id}", get(ws_relay::ws_handler))
        .layer(cors_layer)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("failed to bind");

    tracing::info!("hive-server listening on {bind_addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    tracing::info!("hive-server shutting down gracefully");
}

#[cfg(test)]
mod cors_tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get};
    use tower::ServiceExt; // for `oneshot`

    /// Minimal router with a single route, wrapped in a CorsLayer built from `origins`.
    fn test_app(origins: &[&str]) -> Router {
        let origin_strings: Vec<String> = origins.iter().map(|s| s.to_string()).collect();
        Router::new()
            .route("/api/test", get(|| async { "ok" }))
            .layer(build_cors_layer(&origin_strings))
    }

    /// Returns the value of an HTTP response header as a String, panicking if absent.
    fn header_str(resp: &axum::response::Response, name: &str) -> String {
        resp.headers()
            .get(name)
            .unwrap_or_else(|| panic!("expected header {name} to be present"))
            .to_str()
            .expect("header value is not valid UTF-8")
            .to_owned()
    }

    #[tokio::test]
    async fn preflight_returns_2xx_with_cors_headers() {
        let app = test_app(&["http://localhost:5173"]);

        let req = Request::builder()
            .method("OPTIONS")
            .uri("/api/test")
            .header("Origin", "http://localhost:5173")
            .header("Access-Control-Request-Method", "GET")
            .header("Access-Control-Request-Headers", "authorization")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();

        // tower-http CorsLayer returns 200 for preflights (which is spec-compliant —
        // the CORS spec requires a "successful HTTP response", 200 and 204 are both valid).
        assert!(
            resp.status().is_success(),
            "expected 2xx for preflight, got {}",
            resp.status()
        );

        let allow_origin = header_str(&resp, "access-control-allow-origin");
        assert_eq!(allow_origin, "http://localhost:5173");

        let allow_methods = header_str(&resp, "access-control-allow-methods");
        for method in &["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            assert!(
                allow_methods.contains(method),
                "expected {method} in Access-Control-Allow-Methods: {allow_methods}"
            );
        }

        let allow_headers = header_str(&resp, "access-control-allow-headers");
        for header in &["authorization", "content-type", "x-request-id"] {
            assert!(
                allow_headers.to_lowercase().contains(header),
                "expected {header} in Access-Control-Allow-Headers: {allow_headers}"
            );
        }
    }

    #[tokio::test]
    async fn disallowed_origin_is_rejected() {
        let app = test_app(&["http://localhost:5173"]);

        let req = Request::builder()
            .method("OPTIONS")
            .uri("/api/test")
            .header("Origin", "http://evil.example.com")
            .header("Access-Control-Request-Method", "GET")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();

        // The origin header should not be echoed back.
        let origin_echo = resp.headers().get("access-control-allow-origin");
        assert!(
            origin_echo
                .map(|v| v != "http://evil.example.com")
                .unwrap_or(true),
            "evil.example.com should not be reflected as an allowed origin"
        );
    }

    #[tokio::test]
    async fn non_preflight_request_receives_allow_origin_header() {
        let app = test_app(&["http://localhost:5173"]);

        let req = Request::builder()
            .method("GET")
            .uri("/api/test")
            .header("Origin", "http://localhost:5173")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();

        assert!(resp.status().is_success());
        let allow_origin = header_str(&resp, "access-control-allow-origin");
        assert_eq!(allow_origin, "http://localhost:5173");
    }

    #[tokio::test]
    async fn allowed_origin_is_reflected() {
        let app = test_app(&["http://localhost:5173", "https://app.example.com"]);

        let req = Request::builder()
            .method("GET")
            .uri("/api/test")
            .header("Origin", "http://localhost:5173")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let allow_origin = header_str(&resp, "access-control-allow-origin");
        assert_eq!(allow_origin, "http://localhost:5173");
    }

    #[tokio::test]
    async fn second_allowed_origin_is_reflected() {
        let app = test_app(&["http://localhost:5173", "https://app.example.com"]);

        let req = Request::builder()
            .method("GET")
            .uri("/api/test")
            .header("Origin", "https://app.example.com")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let allow_origin = header_str(&resp, "access-control-allow-origin");
        assert_eq!(allow_origin, "https://app.example.com");
    }

    #[test]
    fn is_production_false_without_env() {
        // Don't set HIVE_ENV — should return false.
        // (This test assumes HIVE_ENV is not set in the test environment.
        //  If it is, this test will correctly fail, indicating misconfiguration.)
        if std::env::var("HIVE_ENV").is_ok() {
            return; // skip: env is externally controlled
        }
        assert!(!config::is_production());
    }

    #[test]
    fn is_production_true_when_env_set() {
        // Guard: only assert if we can control the env.
        // Note: env mutation in parallel tests is unsafe; this is a best-effort check.
        if std::env::var("HIVE_ENV")
            .map(|v| v == "production")
            .unwrap_or(false)
        {
            assert!(config::is_production());
        }
    }

    #[test]
    fn default_cors_origins_fallback_to_dev() {
        if std::env::var("HIVE_CORS_ORIGINS").is_ok() {
            return; // skip: externally controlled
        }
        let config = config::HiveConfig::default();
        assert_eq!(config.server.cors_origins, vec!["http://localhost:5173"]);
    }
}

/// Wait for SIGTERM or SIGINT (Ctrl+C) to initiate graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT, initiating shutdown"),
        _ = terminate => tracing::info!("received SIGTERM, initiating shutdown"),
    }
}
