use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Unified error type for the Hive server.
///
/// All handler errors should return `HiveError` — axum's `IntoResponse`
/// impl converts it to a consistent JSON error response with the
/// appropriate HTTP status code.
#[derive(Debug)]
pub enum HiveError {
    /// 404 — requested resource does not exist.
    NotFound(String),
    /// 400 — client sent an invalid request.
    BadRequest(String),
    /// 401 — missing or invalid authentication.
    Unauthorized(String),
    /// 403 — authenticated but not permitted.
    Forbidden(String),
    /// 409 — conflict (e.g. duplicate agent name).
    Conflict(String),
    /// 502 — room daemon is unreachable or returned an error.
    DaemonUnavailable(String),
    /// 500 — unexpected internal error.
    Internal(String),
}

/// JSON body returned for all error responses.
#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
}

impl HiveError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::DaemonUnavailable(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::BadRequest(_) => "bad_request",
            Self::Unauthorized(_) => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::Conflict(_) => "conflict",
            Self::DaemonUnavailable(_) => "daemon_unavailable",
            Self::Internal(_) => "internal_error",
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::NotFound(m)
            | Self::BadRequest(m)
            | Self::Unauthorized(m)
            | Self::Forbidden(m)
            | Self::Conflict(m)
            | Self::DaemonUnavailable(m)
            | Self::Internal(m) => m,
        }
    }
}

impl IntoResponse for HiveError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.error_code(),
                message: self.message().to_owned(),
            },
        };
        (status, axum::Json(body)).into_response()
    }
}

impl std::fmt::Display for HiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.error_code(), self.message())
    }
}

impl std::error::Error for HiveError {}

/// Convenience type for handler return values.
pub type HiveResult<T> = Result<T, HiveError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn not_found_returns_404() {
        let err = HiveError::NotFound("workspace not found".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "not_found");
        assert_eq!(json["error"]["message"], "workspace not found");
    }

    #[tokio::test]
    async fn bad_request_returns_400() {
        let err = HiveError::BadRequest("missing field".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unauthorized_returns_401() {
        let err = HiveError::Unauthorized("invalid token".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn forbidden_returns_403() {
        let err = HiveError::Forbidden("not permitted".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn conflict_returns_409() {
        let err = HiveError::Conflict("agent already exists".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn daemon_unavailable_returns_502() {
        let err = HiveError::DaemonUnavailable("connection refused".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn internal_returns_500() {
        let err = HiveError::Internal("unexpected panic".into());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn error_body_is_json() {
        let err = HiveError::NotFound("test".into());
        let resp = err.into_response();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("application/json"),
            "expected JSON content-type, got: {content_type}"
        );
    }

    #[test]
    fn display_includes_code_and_message() {
        let err = HiveError::BadRequest("oops".into());
        assert_eq!(err.to_string(), "bad_request: oops");
    }

    #[test]
    fn hive_result_ok() {
        let result: HiveResult<i32> = Ok(42);
        assert!(result.is_ok());
    }

    #[test]
    fn hive_result_err() {
        let result: HiveResult<i32> = Err(HiveError::Internal("fail".into()));
        assert!(result.is_err());
    }
}
