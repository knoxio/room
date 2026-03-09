use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use crate::{
    message::{make_dm, make_message, Message},
    query::{has_narrowing_filter, QueryFilter},
};

use super::super::{
    auth::validate_token,
    service::{DispatchResult, RoomService},
};
use super::{DaemonWsState, WsAppState};

// ── Shared helpers ──────────────────────────────────────────────────────

pub(super) fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
}

/// Convert a `route_and_dispatch` result into an axum HTTP response.
fn dispatch_to_response(result: anyhow::Result<DispatchResult>) -> axum::response::Response {
    match result {
        Ok(DispatchResult::Handled) => {
            (StatusCode::OK, Json(serde_json::json!({"type":"ok"}))).into_response()
        }
        Ok(DispatchResult::Reply(json)) => {
            let v: serde_json::Value =
                serde_json::from_str(&json).unwrap_or(serde_json::json!({"reply": json}));
            (StatusCode::OK, Json(v)).into_response()
        }
        Ok(DispatchResult::Shutdown) => {
            (StatusCode::OK, Json(serde_json::json!({"type":"shutdown"}))).into_response()
        }
        Ok(DispatchResult::Sent(seq_msg)) => {
            let json = serde_json::to_value(&seq_msg).unwrap_or_default();
            (StatusCode::OK, Json(json)).into_response()
        }
        Ok(DispatchResult::SendDenied(reason)) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"type":"error","code":"send_denied","message": reason})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"type":"error","code":"route_error","message": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Request/query structs ───────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct JoinRequest {
    pub(super) username: String,
}

#[derive(Deserialize)]
pub(super) struct SendRequest {
    pub(super) content: String,
    pub(super) to: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct PollQuery {
    pub(super) since: Option<String>,
}

/// Query parameters for `GET /api/{room_id}/query`.
///
/// All fields are optional. Absent fields impose no constraint on that dimension.
#[derive(Deserialize)]
pub(super) struct QueryParams {
    /// Filter to messages from this user.
    pub(super) user: Option<String>,
    /// Maximum number of messages to return.
    pub(super) n: Option<usize>,
    /// Return only messages with seq strictly greater than this value.
    pub(super) since: Option<u64>,
    /// Return only messages with seq strictly less than this value.
    pub(super) before: Option<u64>,
    /// Substring search on message content (case-sensitive).
    pub(super) content: Option<String>,
    /// Regex search on message content.
    pub(super) regex: Option<String>,
    /// Only include messages that @mention this username.
    pub(super) mention: Option<String>,
    /// If `true`, exclude DirectMessage variants.
    pub(super) public: Option<bool>,
    /// If `true`, return messages oldest-first; otherwise newest-first.
    pub(super) asc: Option<bool>,
    /// ISO-8601 lower bound on message timestamp (exclusive).
    pub(super) after_ts: Option<String>,
    /// ISO-8601 upper bound on message timestamp (exclusive).
    pub(super) before_ts: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CreateRoomRequest {
    pub(super) room_id: String,
    #[serde(default)]
    pub(super) visibility: Option<String>,
    #[serde(default)]
    pub(super) invite: Option<Vec<String>>,
}

// ── Query helpers ───────────────────────────────────────────────────────

/// Build a [`QueryFilter`] from REST query params, scoped to `room_id`.
pub(super) fn build_query_filter(params: &QueryParams, room_id: &str) -> QueryFilter {
    QueryFilter {
        users: params
            .user
            .as_ref()
            .map(|u| vec![u.clone()])
            .unwrap_or_default(),
        limit: params.n,
        after_seq: params.since.map(|seq| (room_id.to_owned(), seq)),
        before_seq: params.before.map(|seq| (room_id.to_owned(), seq)),
        content_search: params.content.clone(),
        content_regex: params.regex.clone(),
        mention_user: params.mention.clone(),
        public_only: params.public.unwrap_or(false),
        ascending: params.asc.unwrap_or(false),
        after_ts: params.after_ts.as_deref().and_then(|s| s.parse().ok()),
        before_ts: params.before_ts.as_deref().and_then(|s| s.parse().ok()),
        ..QueryFilter::default()
    }
}

/// Apply a [`QueryFilter`] to a history, enforcing DM privacy.
///
/// Returns JSON-serialisable values, limited and ordered per filter settings.
pub(super) fn apply_query_filter(
    history: Vec<Message>,
    filter: &QueryFilter,
    room_id: &str,
    username: &str,
    host: Option<&str>,
) -> Vec<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = history
        .into_iter()
        .filter(|msg| {
            // Enforce DM privacy before running QueryFilter.
            if !msg.is_visible_to(username, host) {
                return false;
            }
            filter.matches(msg, room_id)
        })
        .filter_map(|msg| serde_json::to_value(&msg).ok())
        .collect();

    if !filter.ascending {
        messages.reverse();
    }

    if let Some(limit) = filter.limit {
        messages.truncate(limit);
    }

    messages
}

// ── Single-room REST endpoints ──────────────────────────────────────────

pub(super) async fn api_join(
    Path(room_id): Path<String>,
    State(state): State<WsAppState>,
    Json(body): Json<JoinRequest>,
) -> impl IntoResponse {
    let rs = &*state.room_state;
    if room_id != rs.room_id() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"type":"error","code":"room_not_found"})),
        )
            .into_response();
    }
    if let Err(reason) = rs.check_join(&body.username) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"type":"error","code":"join_denied","message": reason})),
        )
            .into_response();
    }
    match rs.issue_token(&body.username).await {
        Ok(token) => {
            let resp = serde_json::json!({
                "type":"token","token": token, "username": body.username
            });
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(_) => {
            let err = serde_json::json!({
                "type":"error","code":"username_taken","username": body.username
            });
            (StatusCode::CONFLICT, Json(err)).into_response()
        }
    }
}

pub(super) async fn api_send(
    Path(room_id): Path<String>,
    State(state): State<WsAppState>,
    headers: HeaderMap,
    Json(body): Json<SendRequest>,
) -> impl IntoResponse {
    let rs = &*state.room_state;
    if room_id != rs.room_id() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"type":"error","code":"room_not_found"})),
        )
            .into_response();
    }

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match rs.validate_token(token).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let msg = if let Some(ref to) = body.to {
        make_dm(rs.room_id(), &username, to, &body.content)
    } else {
        make_message(rs.room_id(), &username, &body.content)
    };

    dispatch_to_response(rs.route_and_dispatch(msg, &username).await)
}

pub(super) async fn api_poll(
    Path(room_id): Path<String>,
    State(state): State<WsAppState>,
    headers: HeaderMap,
    Query(query): Query<PollQuery>,
) -> impl IntoResponse {
    let rs = &*state.room_state;
    if room_id != rs.room_id() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"type":"error","code":"room_not_found"})),
        )
            .into_response();
    }

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match rs.validate_token(token).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let history = rs.load_history().await;
    let host_name = rs.host_name().await;

    // Filter messages after the `since` ID (stateless — no server-side cursor).
    let mut found_since = query.since.is_none();
    let messages: Vec<serde_json::Value> = history
        .into_iter()
        .filter(|msg| {
            if !found_since {
                if msg.id() == query.since.as_deref().unwrap_or_default() {
                    found_since = true;
                }
                return false;
            }
            msg.is_visible_to(&username, host_name.as_deref())
        })
        .filter_map(|msg| serde_json::to_value(&msg).ok())
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({ "messages": messages })),
    )
        .into_response()
}

pub(super) async fn api_query(
    Path(room_id): Path<String>,
    State(state): State<WsAppState>,
    headers: HeaderMap,
    Query(params): Query<QueryParams>,
) -> impl IntoResponse {
    let rs = &*state.room_state;
    if room_id != rs.room_id() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"type":"error","code":"room_not_found"})),
        )
            .into_response();
    }

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match rs.validate_token(token).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let filter = build_query_filter(&params, &room_id);

    // `public=true` alone is not a valid query — require at least one narrowing param.
    if filter.public_only && !has_narrowing_filter(&filter, false) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "type": "error",
                "code": "public_requires_filter",
                "message": "public=true requires at least one narrowing filter (user, content, regex, mention, since, before, n)"
            })),
        )
            .into_response();
    }

    let history = rs.load_history().await;
    let host_name = rs.host_name().await;
    let messages = apply_query_filter(history, &filter, &room_id, &username, host_name.as_deref());

    (
        StatusCode::OK,
        Json(serde_json::json!({ "messages": messages })),
    )
        .into_response()
}

pub(super) async fn api_health(State(state): State<WsAppState>) -> impl IntoResponse {
    let rs = &*state.room_state;
    let users = RoomService::status_count(rs).await;
    Json(serde_json::json!({
        "status": "ok",
        "room": rs.room_id(),
        "users": users
    }))
}

// ── Daemon REST endpoints ────────────────────────────────────────────────

pub(super) async fn daemon_api_join(
    Path(room_id): Path<String>,
    State(state): State<DaemonWsState>,
    Json(body): Json<JoinRequest>,
) -> impl IntoResponse {
    let room = match state.get_room(&room_id).await {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"type":"error","code":"room_not_found"})),
            )
                .into_response();
        }
    };
    if let Err(reason) = room.check_join(&body.username) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"type":"error","code":"join_denied","message": reason})),
        )
            .into_response();
    }
    match room.issue_token(&body.username).await {
        Ok(token) => {
            let resp = serde_json::json!({
                "type":"token","token": token, "username": body.username
            });
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(_) => {
            let err = serde_json::json!({
                "type":"error","code":"username_taken","username": body.username
            });
            (StatusCode::CONFLICT, Json(err)).into_response()
        }
    }
}

pub(super) async fn daemon_api_send(
    Path(room_id): Path<String>,
    State(state): State<DaemonWsState>,
    headers: HeaderMap,
    Json(body): Json<SendRequest>,
) -> impl IntoResponse {
    let room = match state.get_room(&room_id).await {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"type":"error","code":"room_not_found"})),
            )
                .into_response();
        }
    };

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match room.validate_token(token).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let msg = if let Some(ref to) = body.to {
        make_dm(room.room_id(), &username, to, &body.content)
    } else {
        make_message(room.room_id(), &username, &body.content)
    };

    dispatch_to_response(room.route_and_dispatch(msg, &username).await)
}

pub(super) async fn daemon_api_poll(
    Path(room_id): Path<String>,
    State(state): State<DaemonWsState>,
    headers: HeaderMap,
    Query(query): Query<PollQuery>,
) -> impl IntoResponse {
    let room = match state.get_room(&room_id).await {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"type":"error","code":"room_not_found"})),
            )
                .into_response();
        }
    };

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match room.validate_token(token).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let history = room.load_history().await;
    let host_name = room.host_name().await;

    let mut found_since = query.since.is_none();
    let messages: Vec<serde_json::Value> = history
        .into_iter()
        .filter(|msg| {
            if !found_since {
                if msg.id() == query.since.as_deref().unwrap_or_default() {
                    found_since = true;
                }
                return false;
            }
            msg.is_visible_to(&username, host_name.as_deref())
        })
        .filter_map(|msg| serde_json::to_value(&msg).ok())
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({ "messages": messages })),
    )
        .into_response()
}

pub(super) async fn daemon_api_health(State(state): State<DaemonWsState>) -> impl IntoResponse {
    let rooms = state.rooms.lock().await;
    let mut room_info = Vec::new();
    for (id, rs) in rooms.iter() {
        let users = RoomService::status_count(&**rs).await;
        room_info.push(serde_json::json!({
            "room": id,
            "users": users,
        }));
    }
    Json(serde_json::json!({
        "status": "ok",
        "rooms": room_info,
    }))
}

pub(super) async fn daemon_api_rooms(State(state): State<DaemonWsState>) -> impl IntoResponse {
    let rooms = state.rooms.lock().await;
    let ids: Vec<&String> = rooms.keys().collect();
    Json(serde_json::json!({
        "rooms": ids,
    }))
}

pub(super) async fn daemon_api_create_room(
    State(state): State<DaemonWsState>,
    headers: HeaderMap,
    Json(body): Json<CreateRoomRequest>,
) -> impl IntoResponse {
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    // Validate token against the shared token map.
    if validate_token(token, &state.system_token_map)
        .await
        .is_none()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"type":"error","code":"invalid_token"})),
        )
            .into_response();
    }

    let visibility_str = body.visibility.as_deref().unwrap_or("public");
    let invite = body.invite.unwrap_or_default();

    let room_config = match visibility_str {
        "public" => room_protocol::RoomConfig {
            visibility: room_protocol::RoomVisibility::Public,
            max_members: None,
            invite_list: invite.into_iter().collect(),
            created_by: "system".to_owned(),
            created_at: chrono::Utc::now().to_rfc3339(),
        },
        "private" => room_protocol::RoomConfig {
            visibility: room_protocol::RoomVisibility::Private,
            max_members: None,
            invite_list: invite.into_iter().collect(),
            created_by: "system".to_owned(),
            created_at: chrono::Utc::now().to_rfc3339(),
        },
        "dm" => {
            if invite.len() != 2 {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "type": "error",
                        "code": "invalid_config",
                        "message": "dm visibility requires exactly 2 users in invite list"
                    })),
                )
                    .into_response();
            }
            room_protocol::RoomConfig::dm(&invite[0], &invite[1])
        }
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "type": "error",
                    "code": "invalid_config",
                    "message": format!("unknown visibility: {other}")
                })),
            )
                .into_response();
        }
    };

    match super::super::daemon::create_room_entry(
        &body.room_id,
        Some(room_config),
        &state.rooms,
        &state.config,
        &state.system_token_map,
        Some(state.user_registry.clone()),
    )
    .await
    {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"type":"room_created","room": body.room_id})),
        )
            .into_response(),
        Err(e) if e.contains("room already exists") => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"type":"error","code":"room_exists","message": e})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"type":"error","code":"invalid_room_id","message": e})),
        )
            .into_response(),
    }
}

pub(super) async fn daemon_api_query(
    Path(room_id): Path<String>,
    State(state): State<DaemonWsState>,
    headers: HeaderMap,
    Query(params): Query<QueryParams>,
) -> impl IntoResponse {
    let room = match state.get_room(&room_id).await {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"type":"error","code":"room_not_found"})),
            )
                .into_response();
        }
    };

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match room.validate_token(token).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let filter = build_query_filter(&params, &room_id);

    // `public=true` alone is not a valid query — require at least one narrowing param.
    if filter.public_only && !has_narrowing_filter(&filter, false) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "type": "error",
                "code": "public_requires_filter",
                "message": "public=true requires at least one narrowing filter (user, content, regex, mention, since, before, n)"
            })),
        )
            .into_response();
    }

    let history = room.load_history().await;
    let host_name = room.host_name().await;
    let messages = apply_query_filter(history, &filter, &room_id, &username, host_name.as_deref());

    (
        StatusCode::OK,
        Json(serde_json::json!({ "messages": messages })),
    )
        .into_response()
}
