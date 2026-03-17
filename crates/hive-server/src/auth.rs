use axum::extract::State;
use axum::http::{header, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::HiveError;
use crate::AppState;

// JWT_SECRET will be used when we switch to HMAC-SHA256 signed tokens.
// For MVP, tokens are base64-encoded claims (not cryptographically signed).

/// Token response returned by POST /api/auth/token.
#[derive(Serialize)]
pub struct TokenResponse {
    pub token: String,
    pub token_type: &'static str,
    pub expires_in: u64,
}

/// Login request body.
#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: Option<String>,
    pub api_key: Option<String>,
}

/// Claims encoded in the JWT.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: u64,
    pub iat: u64,
}

/// Issue a JWT token for a user.
///
/// POST /api/auth/token
///
/// For MVP: accepts any username (no password validation).
/// Production: validate against user database or OAuth provider.
pub(crate) async fn issue_token(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<TokenResponse>, HiveError> {
    if req.username.is_empty() {
        return Err(HiveError::BadRequest("username is required".into()));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let expires_in = 86400; // 24 hours

    let claims = Claims {
        sub: req.username,
        exp: now + expires_in,
        iat: now,
    };

    // Simple base64-encoded JSON token (not cryptographically secure — MVP only).
    // Production: use jsonwebtoken crate with HMAC-SHA256.
    let claims_json = serde_json::to_string(&claims)
        .map_err(|e| HiveError::Internal(format!("failed to serialize claims: {e}")))?;
    let token = base64_encode(&claims_json);

    Ok(Json(TokenResponse {
        token,
        token_type: "Bearer",
        expires_in,
    }))
}

/// Auth middleware — validates Bearer token on protected routes.
#[allow(dead_code)]
pub(crate) async fn auth_middleware(
    State(_state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let token = &header[7..];
            match validate_token(token) {
                Ok(_claims) => next.run(request).await,
                Err(e) => HiveError::Unauthorized(e).into_response(),
            }
        }
        Some(_) => {
            HiveError::Unauthorized("invalid authorization header format".into()).into_response()
        }
        None => {
            // For MVP: allow unauthenticated access (auth is optional)
            next.run(request).await
        }
    }
}

/// Validate a token and extract claims.
#[allow(dead_code)]
fn validate_token(token: &str) -> Result<Claims, String> {
    let decoded = base64_decode(token).map_err(|_| "invalid token encoding".to_string())?;
    let claims: Claims =
        serde_json::from_str(&decoded).map_err(|_| "invalid token format".to_string())?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if claims.exp < now {
        return Err("token expired".to_string());
    }

    Ok(claims)
}

/// Simple base64 encode (URL-safe, no padding).
fn base64_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input.as_bytes())
}

/// Simple base64 decode (URL-safe, no padding).
#[allow(dead_code)]
fn base64_decode(input: &str) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(input)
        .map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let input = r#"{"sub":"alice","exp":9999999999,"iat":1000000000}"#;
        let encoded = base64_encode(input);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(input, decoded);
    }

    #[test]
    fn validate_valid_token() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = Claims {
            sub: "test-user".into(),
            exp: now + 3600,
            iat: now,
        };
        let json = serde_json::to_string(&claims).unwrap();
        let token = base64_encode(&json);
        let result = validate_token(&token);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().sub, "test-user");
    }

    #[test]
    fn validate_expired_token() {
        let claims = Claims {
            sub: "test-user".into(),
            exp: 1000000000, // way in the past
            iat: 999999999,
        };
        let json = serde_json::to_string(&claims).unwrap();
        let token = base64_encode(&json);
        let result = validate_token(&token);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("expired"));
    }

    #[test]
    fn validate_invalid_token() {
        let result = validate_token("not-a-valid-token!!!");
        assert!(result.is_err());
    }
}
