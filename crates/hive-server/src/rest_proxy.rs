//! REST proxy — forwards API requests to the co-located room daemon.
//!
//! Implements BE-004 through BE-007: room list, room info, messages, send.
//! Forwards Authorization headers from the client to the daemon.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde_json::Value;

use crate::AppState;

/// Extract the daemon REST base URL from config.
fn daemon_base(state: &AppState) -> String {
    state
        .config
        .daemon
        .ws_url
        .replace("ws://", "http://")
        .replace("wss://", "https://")
}

/// Build a reqwest client with optional auth header forwarding.
fn build_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    let mut req = client.request(method, url);
    // Forward Authorization header to room daemon
    if let Some(auth) = headers.get("authorization") {
        if let Ok(val) = auth.to_str() {
            req = req.header("Authorization", val);
        }
    }
    req
}

/// GET /api/rooms — list available rooms from the daemon.
pub async fn list_rooms(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let base = daemon_base(&state);
    let client = reqwest::Client::new();

    // Try daemon health endpoint to discover rooms
    let health_url = format!("{base}/api/health");
    match build_request(&client, reqwest::Method::GET, &health_url, &headers)
        .send()
        .await
    {
        Ok(resp) => {
            let body: Value = resp.json().await.unwrap_or_default();
            let room = body
                .get("room")
                .and_then(|r| r.as_str())
                .unwrap_or("default");
            let users = body.get("users").and_then(|u| u.as_u64()).unwrap_or(0);
            Ok(Json(serde_json::json!({
                "rooms": [{ "id": room, "name": room, "users": users }],
                "daemon_connected": true
            })))
        }
        Err(_) => Ok(Json(serde_json::json!({
            "rooms": [],
            "daemon_connected": false,
            "error": "daemon unavailable"
        }))),
    }
}

/// GET /api/rooms/:room_id — get room info.
pub async fn get_room(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let base = daemon_base(&state);
    let url = format!("{base}/api/{room_id}/poll");

    let client = reqwest::Client::new();
    match build_request(&client, reqwest::Method::GET, &url, &headers)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => Ok(Json(serde_json::json!({
            "id": room_id,
            "name": room_id,
            "status": "active"
        }))),
        Ok(resp) => Ok(Json(serde_json::json!({
            "id": room_id,
            "name": room_id,
            "status": "error",
            "daemon_status": resp.status().as_u16()
        }))),
        Err(_) => Ok(Json(serde_json::json!({
            "id": room_id,
            "name": room_id,
            "status": "daemon_unavailable"
        }))),
    }
}

/// GET /api/rooms/:room_id/messages — poll messages from a room.
pub async fn get_messages(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let base = daemon_base(&state);
    let url = format!("{base}/api/{room_id}/poll");

    let client = reqwest::Client::new();
    match build_request(&client, reqwest::Method::GET, &url, &headers)
        .send()
        .await
    {
        Ok(resp) => {
            let body: Value = resp
                .json()
                .await
                .unwrap_or(serde_json::json!({"messages": []}));
            Ok(Json(body))
        }
        Err(e) => Ok(Json(serde_json::json!({
            "messages": [],
            "error": format!("daemon unavailable: {e}")
        }))),
    }
}

/// POST /api/rooms/:room_id/send — send a message to a room.
pub async fn send_message(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let base = daemon_base(&state);
    let url = format!("{base}/api/{room_id}/send");

    let client = reqwest::Client::new();
    match build_request(&client, reqwest::Method::POST, &url, &headers)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let result: Value = resp.json().await.unwrap_or_default();
            Ok(Json(result))
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            let _error: Value = resp.json().await.unwrap_or_default();
            Err(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY))
        }
        Err(_) => Err(StatusCode::BAD_GATEWAY),
    }
}
