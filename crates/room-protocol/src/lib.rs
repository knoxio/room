use std::collections::{BTreeSet, HashSet};
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Error returned when constructing a DM room ID with invalid inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmRoomError {
    /// Both usernames are the same — a DM requires two distinct users.
    SameUser(String),
}

impl fmt::Display for DmRoomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DmRoomError::SameUser(user) => {
                write!(f, "cannot create DM room: both users are '{user}'")
            }
        }
    }
}

impl std::error::Error for DmRoomError {}

/// Visibility level for a room, controlling who can discover and join it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomVisibility {
    /// Anyone can discover and join.
    Public,
    /// Discoverable in listings but requires invite to join.
    Private,
    /// Not discoverable; join requires knowing room ID + invite.
    Unlisted,
    /// Private 2-person room, auto-created by `/dm` command.
    Dm,
}

/// Configuration for a room's access controls and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomConfig {
    pub visibility: RoomVisibility,
    /// Maximum number of members. `None` = unlimited.
    pub max_members: Option<usize>,
    /// Usernames allowed to join (for private/unlisted/dm rooms).
    pub invite_list: HashSet<String>,
    /// Username of the room creator.
    pub created_by: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

impl RoomConfig {
    /// Create a default public room config.
    pub fn public(created_by: &str) -> Self {
        Self {
            visibility: RoomVisibility::Public,
            max_members: None,
            invite_list: HashSet::new(),
            created_by: created_by.to_owned(),
            created_at: Utc::now().to_rfc3339(),
        }
    }

    /// Create a DM room config for two users.
    pub fn dm(user_a: &str, user_b: &str) -> Self {
        let mut invite_list = HashSet::new();
        invite_list.insert(user_a.to_owned());
        invite_list.insert(user_b.to_owned());
        Self {
            visibility: RoomVisibility::Dm,
            max_members: Some(2),
            invite_list,
            created_by: user_a.to_owned(),
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

/// Compute the deterministic room ID for a DM between two users.
///
/// Sorts usernames alphabetically so `/dm alice` from bob and `/dm bob` from
/// alice both resolve to the same room.
///
/// # Errors
///
/// Returns [`DmRoomError::SameUser`] if both usernames are identical.
pub fn dm_room_id(user_a: &str, user_b: &str) -> Result<String, DmRoomError> {
    if user_a == user_b {
        return Err(DmRoomError::SameUser(user_a.to_owned()));
    }
    let (first, second) = if user_a < user_b {
        (user_a, user_b)
    } else {
        (user_b, user_a)
    };
    Ok(format!("dm-{first}-{second}"))
}

/// Check whether a room ID represents a DM room.
///
/// DM room IDs follow the pattern `dm-<user_a>-<user_b>` where usernames are
/// sorted alphabetically.
pub fn is_dm_room(room_id: &str) -> bool {
    room_id.starts_with("dm-") && room_id.matches('-').count() >= 2
}

/// Subscription tier for a user's relationship with a room.
///
/// Controls what messages appear in the user's default stream:
/// - `Full` — all messages from the room
/// - `MentionsOnly` — only messages that @mention the user
/// - `Unsubscribed` — excluded from the default stream (still queryable with `--public`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionTier {
    Full,
    MentionsOnly,
    Unsubscribed,
}

impl std::fmt::Display for SubscriptionTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "full"),
            Self::MentionsOnly => write!(f, "mentions_only"),
            Self::Unsubscribed => write!(f, "unsubscribed"),
        }
    }
}

impl std::str::FromStr for SubscriptionTier {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "full" => Ok(Self::Full),
            "mentions_only" | "mentions-only" | "mentions" => Ok(Self::MentionsOnly),
            "unsubscribed" | "none" => Ok(Self::Unsubscribed),
            other => Err(format!(
                "unknown subscription tier '{other}'; expected full, mentions_only, or unsubscribed"
            )),
        }
    }
}

/// Typed event categories for structured event filtering.
///
/// Used with the [`Message::Event`] variant. Marked `#[non_exhaustive]` so new
/// event types can be added without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventType {
    TaskPosted,
    TaskAssigned,
    TaskClaimed,
    TaskPlanned,
    TaskApproved,
    TaskUpdated,
    TaskReleased,
    TaskFinished,
    TaskCancelled,
    StatusChanged,
    ReviewRequested,
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskPosted => write!(f, "task_posted"),
            Self::TaskAssigned => write!(f, "task_assigned"),
            Self::TaskClaimed => write!(f, "task_claimed"),
            Self::TaskPlanned => write!(f, "task_planned"),
            Self::TaskApproved => write!(f, "task_approved"),
            Self::TaskUpdated => write!(f, "task_updated"),
            Self::TaskReleased => write!(f, "task_released"),
            Self::TaskFinished => write!(f, "task_finished"),
            Self::TaskCancelled => write!(f, "task_cancelled"),
            Self::StatusChanged => write!(f, "status_changed"),
            Self::ReviewRequested => write!(f, "review_requested"),
        }
    }
}

impl std::str::FromStr for EventType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "task_posted" => Ok(Self::TaskPosted),
            "task_assigned" => Ok(Self::TaskAssigned),
            "task_claimed" => Ok(Self::TaskClaimed),
            "task_planned" => Ok(Self::TaskPlanned),
            "task_approved" => Ok(Self::TaskApproved),
            "task_updated" => Ok(Self::TaskUpdated),
            "task_released" => Ok(Self::TaskReleased),
            "task_finished" => Ok(Self::TaskFinished),
            "task_cancelled" => Ok(Self::TaskCancelled),
            "status_changed" => Ok(Self::StatusChanged),
            "review_requested" => Ok(Self::ReviewRequested),
            other => Err(format!(
                "unknown event type '{other}'; expected one of: task_posted, task_assigned, \
                 task_claimed, task_planned, task_approved, task_updated, task_released, \
                 task_finished, task_cancelled, status_changed, review_requested"
            )),
        }
    }
}

impl Ord for EventType {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.to_string().cmp(&other.to_string())
    }
}

impl PartialOrd for EventType {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Filter controlling which event types a user receives during poll.
///
/// Used alongside [`SubscriptionTier`] to give fine-grained control over the
/// event stream. Tier controls message-level filtering (all, mentions, none);
/// `EventFilter` controls which [`EventType`] values pass through for
/// [`Message::Event`] messages specifically.
///
/// Non-Event messages are never affected by the event filter.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "filter")]
pub enum EventFilter {
    /// Receive all event types (the default).
    #[default]
    All,
    /// Receive no events.
    None,
    /// Receive only the listed event types.
    Only {
        #[serde(default)]
        types: BTreeSet<EventType>,
    },
}

impl fmt::Display for EventFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::None => write!(f, "none"),
            Self::Only { types } => {
                let names: Vec<String> = types.iter().map(|t| t.to_string()).collect();
                write!(f, "{}", names.join(","))
            }
        }
    }
}

impl std::str::FromStr for EventFilter {
    type Err = String;

    /// Parse an event filter from a string.
    ///
    /// Accepted formats:
    /// - `"all"` → [`EventFilter::All`]
    /// - `"none"` → [`EventFilter::None`]
    /// - `"task_posted,task_finished"` → [`EventFilter::Only`] with those types
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "all" => Ok(Self::All),
            "none" => Ok(Self::None),
            "" => Ok(Self::All),
            csv => {
                let mut types = BTreeSet::new();
                for part in csv.split(',') {
                    let trimmed = part.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let et: EventType = trimmed.parse()?;
                    types.insert(et);
                }
                if types.is_empty() {
                    Ok(Self::All)
                } else {
                    Ok(Self::Only { types })
                }
            }
        }
    }
}

impl EventFilter {
    /// Check whether a given event type passes this filter.
    pub fn allows(&self, event_type: &EventType) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Only { types } => types.contains(event_type),
        }
    }
}

/// Entry returned by room listing (discovery).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomListEntry {
    pub room_id: String,
    pub visibility: RoomVisibility,
    pub member_count: usize,
    pub created_by: String,
}

/// Wire format for all messages stored in the chat file and sent over the socket.
///
/// Uses `#[serde(tag = "type")]` internally-tagged enum **without** `#[serde(flatten)]`
/// to avoid the serde flatten + internally-tagged footgun that breaks deserialization.
/// Every variant carries its own id/room/user/ts fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Join {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    Leave {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    Message {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        content: String,
    },
    Reply {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        reply_to: String,
        content: String,
    },
    Command {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        cmd: String,
        params: Vec<String>,
    },
    System {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        content: String,
    },
    /// A private direct message. Delivered only to sender, recipient, and the
    /// broker host. Always written to the chat history file.
    #[serde(rename = "dm")]
    DirectMessage {
        id: String,
        room: String,
        /// Sender username (set by the broker).
        user: String,
        ts: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        /// Recipient username.
        to: String,
        content: String,
    },
    /// A typed event for structured filtering. Carries an [`EventType`] alongside
    /// human-readable content and optional machine-readable params.
    Event {
        id: String,
        room: String,
        user: String,
        ts: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        event_type: EventType,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<serde_json::Value>,
    },
}

impl Message {
    pub fn id(&self) -> &str {
        match self {
            Self::Join { id, .. }
            | Self::Leave { id, .. }
            | Self::Message { id, .. }
            | Self::Reply { id, .. }
            | Self::Command { id, .. }
            | Self::System { id, .. }
            | Self::DirectMessage { id, .. }
            | Self::Event { id, .. } => id,
        }
    }

    pub fn room(&self) -> &str {
        match self {
            Self::Join { room, .. }
            | Self::Leave { room, .. }
            | Self::Message { room, .. }
            | Self::Reply { room, .. }
            | Self::Command { room, .. }
            | Self::System { room, .. }
            | Self::DirectMessage { room, .. }
            | Self::Event { room, .. } => room,
        }
    }

    pub fn user(&self) -> &str {
        match self {
            Self::Join { user, .. }
            | Self::Leave { user, .. }
            | Self::Message { user, .. }
            | Self::Reply { user, .. }
            | Self::Command { user, .. }
            | Self::System { user, .. }
            | Self::DirectMessage { user, .. }
            | Self::Event { user, .. } => user,
        }
    }

    pub fn ts(&self) -> &DateTime<Utc> {
        match self {
            Self::Join { ts, .. }
            | Self::Leave { ts, .. }
            | Self::Message { ts, .. }
            | Self::Reply { ts, .. }
            | Self::Command { ts, .. }
            | Self::System { ts, .. }
            | Self::DirectMessage { ts, .. }
            | Self::Event { ts, .. } => ts,
        }
    }

    /// Returns the sequence number assigned by the broker, or `None` for
    /// messages loaded from history files that predate this feature.
    pub fn seq(&self) -> Option<u64> {
        match self {
            Self::Join { seq, .. }
            | Self::Leave { seq, .. }
            | Self::Message { seq, .. }
            | Self::Reply { seq, .. }
            | Self::Command { seq, .. }
            | Self::System { seq, .. }
            | Self::DirectMessage { seq, .. }
            | Self::Event { seq, .. } => *seq,
        }
    }

    /// Returns the text content of this message, or `None` for variants without content
    /// (Join, Leave, Command).
    pub fn content(&self) -> Option<&str> {
        match self {
            Self::Message { content, .. }
            | Self::Reply { content, .. }
            | Self::System { content, .. }
            | Self::DirectMessage { content, .. }
            | Self::Event { content, .. } => Some(content),
            Self::Join { .. } | Self::Leave { .. } | Self::Command { .. } => None,
        }
    }

    /// Extract @mentions from this message's content.
    ///
    /// Returns an empty vec for variants without content (Join, Leave, Command)
    /// or content with no @mentions.
    pub fn mentions(&self) -> Vec<String> {
        match self.content() {
            Some(content) => parse_mentions(content),
            None => Vec::new(),
        }
    }

    /// Returns `true` if `viewer` is allowed to see this message.
    ///
    /// All non-DM variants are visible to everyone. A [`Message::DirectMessage`]
    /// is visible only to the sender (`user`), the recipient (`to`), and the
    /// room host (when `host == Some(viewer)`).
    pub fn is_visible_to(&self, viewer: &str, host: Option<&str>) -> bool {
        match self {
            Self::DirectMessage { user, to, .. } => {
                viewer == user || viewer == to.as_str() || host == Some(viewer)
            }
            _ => true,
        }
    }

    /// Assign a broker-issued sequence number to this message.
    pub fn set_seq(&mut self, seq: u64) {
        let n = Some(seq);
        match self {
            Self::Join { seq, .. } => *seq = n,
            Self::Leave { seq, .. } => *seq = n,
            Self::Message { seq, .. } => *seq = n,
            Self::Reply { seq, .. } => *seq = n,
            Self::Command { seq, .. } => *seq = n,
            Self::System { seq, .. } => *seq = n,
            Self::DirectMessage { seq, .. } => *seq = n,
            Self::Event { seq, .. } => *seq = n,
        }
    }
}

// ── Constructors ─────────────────────────────────────────────────────────────

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn make_join(room: &str, user: &str) -> Message {
    Message::Join {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        seq: None,
    }
}

pub fn make_leave(room: &str, user: &str) -> Message {
    Message::Leave {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        seq: None,
    }
}

pub fn make_message(room: &str, user: &str, content: impl Into<String>) -> Message {
    Message::Message {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        content: content.into(),
        seq: None,
    }
}

pub fn make_reply(
    room: &str,
    user: &str,
    reply_to: impl Into<String>,
    content: impl Into<String>,
) -> Message {
    Message::Reply {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        reply_to: reply_to.into(),
        content: content.into(),
        seq: None,
    }
}

pub fn make_command(
    room: &str,
    user: &str,
    cmd: impl Into<String>,
    params: Vec<String>,
) -> Message {
    Message::Command {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        cmd: cmd.into(),
        params,
        seq: None,
    }
}

pub fn make_system(room: &str, user: &str, content: impl Into<String>) -> Message {
    Message::System {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        content: content.into(),
        seq: None,
    }
}

pub fn make_dm(room: &str, user: &str, to: &str, content: impl Into<String>) -> Message {
    Message::DirectMessage {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        to: to.to_owned(),
        content: content.into(),
        seq: None,
    }
}

pub fn make_event(
    room: &str,
    user: &str,
    event_type: EventType,
    content: impl Into<String>,
    params: Option<serde_json::Value>,
) -> Message {
    Message::Event {
        id: new_id(),
        room: room.to_owned(),
        user: user.to_owned(),
        ts: Utc::now(),
        event_type,
        content: content.into(),
        params,
        seq: None,
    }
}

/// Extract @mentions from message content.
///
/// Matches `@username` patterns where usernames can contain alphanumerics, hyphens,
/// and underscores. Stops at whitespace, punctuation (except `-` and `_`), or end of
/// string. Skips email-like patterns (`user@domain`) by requiring the `@` to be at
/// the start of the string or preceded by whitespace.
///
/// Returns a deduplicated list of mentioned usernames (without the `@` prefix),
/// preserving first-occurrence order.
pub fn parse_mentions(content: &str) -> Vec<String> {
    let mut mentions = Vec::new();
    let mut seen = HashSet::new();

    for (i, _) in content.match_indices('@') {
        // Skip if preceded by a non-whitespace char (email-like pattern)
        if i > 0 {
            let prev = content.as_bytes()[i - 1];
            if !prev.is_ascii_whitespace() {
                continue;
            }
        }

        // Extract username chars after @
        let rest = &content[i + 1..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .unwrap_or(rest.len());
        let username = &rest[..end];

        if !username.is_empty() && seen.insert(username.to_owned()) {
            mentions.push(username.to_owned());
        }
    }

    mentions
}

/// Format a human-readable message ID from a room ID and sequence number.
///
/// The canonical format is `"<room>:<seq>"`, e.g. `"agent-room:42"`. This is a
/// display-only identifier used by `--from`, `--to`, and `--id` flags. The wire
/// format keeps `room` and `seq` as separate fields and never stores this string.
pub fn format_message_id(room: &str, seq: u64) -> String {
    format!("{room}:{seq}")
}

/// Parse a human-readable message ID back into `(room_id, seq)`.
///
/// Expects the format `"<room>:<seq>"` produced by [`format_message_id`].
/// Splits on the **last** colon so room IDs that themselves contain colons are
/// handled correctly (e.g. `"namespace:room:42"` → `("namespace:room", 42)`).
///
/// Returns `Err(String)` if the input has no colon or if the part after the
/// last colon cannot be parsed as a `u64`.
pub fn parse_message_id(id: &str) -> Result<(String, u64), String> {
    let colon = id
        .rfind(':')
        .ok_or_else(|| format!("no colon in message ID: {id:?}"))?;
    let room = &id[..colon];
    let seq_str = &id[colon + 1..];
    let seq = seq_str
        .parse::<u64>()
        .map_err(|_| format!("invalid sequence number in message ID: {id:?}"))?;
    Ok((room.to_owned(), seq))
}

/// Parse a raw line from a client socket.
/// JSON envelope → Message with broker-assigned id/room/ts.
/// Plain text → Message::Message with broker-assigned metadata.
pub fn parse_client_line(raw: &str, room: &str, user: &str) -> Result<Message, serde_json::Error> {
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Envelope {
        Message {
            content: String,
        },
        Reply {
            reply_to: String,
            content: String,
        },
        Command {
            cmd: String,
            params: Vec<String>,
        },
        #[serde(rename = "dm")]
        Dm {
            to: String,
            content: String,
        },
    }

    if raw.starts_with('{') {
        let env: Envelope = serde_json::from_str(raw)?;
        let msg = match env {
            Envelope::Message { content } => make_message(room, user, content),
            Envelope::Reply { reply_to, content } => make_reply(room, user, reply_to, content),
            Envelope::Command { cmd, params } => make_command(room, user, cmd, params),
            Envelope::Dm { to, content } => make_dm(room, user, &to, content),
        };
        Ok(msg)
    } else {
        Ok(make_message(room, user, raw))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_ts() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 3, 5, 10, 0, 0).unwrap()
    }

    fn fixed_id() -> String {
        "00000000-0000-0000-0000-000000000001".to_owned()
    }

    // ── Round-trip tests ─────────────────────────────────────────────────────

    #[test]
    fn join_round_trips() {
        let msg = Message::Join {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            seq: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn leave_round_trips() {
        let msg = Message::Leave {
            id: fixed_id(),
            room: "r".into(),
            user: "bob".into(),
            ts: fixed_ts(),
            seq: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn message_round_trips() {
        let msg = Message::Message {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            content: "hello world".into(),
            seq: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn reply_round_trips() {
        let msg = Message::Reply {
            id: fixed_id(),
            room: "r".into(),
            user: "bob".into(),
            ts: fixed_ts(),
            reply_to: "ffffffff-0000-0000-0000-000000000000".into(),
            content: "pong".into(),
            seq: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn command_round_trips() {
        let msg = Message::Command {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            cmd: "claim".into(),
            params: vec!["task-123".into(), "fix the bug".into()],
            seq: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn system_round_trips() {
        let msg = Message::System {
            id: fixed_id(),
            room: "r".into(),
            user: "broker".into(),
            ts: fixed_ts(),
            content: "5 users online".into(),
            seq: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    // ── JSON shape tests ─────────────────────────────────────────────────────

    #[test]
    fn join_json_has_type_field_at_top_level() {
        let msg = Message::Join {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            seq: None,
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "join");
        assert_eq!(v["user"], "alice");
        assert_eq!(v["room"], "r");
        assert!(
            v.get("content").is_none(),
            "join should not have content field"
        );
    }

    #[test]
    fn message_json_has_content_at_top_level() {
        let msg = Message::Message {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            content: "hi".into(),
            seq: None,
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"], "hi");
    }

    #[test]
    fn deserialize_join_from_literal() {
        let raw = r#"{"type":"join","id":"abc","room":"myroom","user":"alice","ts":"2026-03-05T10:00:00Z"}"#;
        let msg: Message = serde_json::from_str(raw).unwrap();
        assert!(matches!(msg, Message::Join { .. }));
        assert_eq!(msg.user(), "alice");
    }

    #[test]
    fn deserialize_message_from_literal() {
        let raw = r#"{"type":"message","id":"abc","room":"r","user":"bob","ts":"2026-03-05T10:00:00Z","content":"yo"}"#;
        let msg: Message = serde_json::from_str(raw).unwrap();
        assert!(matches!(&msg, Message::Message { content, .. } if content == "yo"));
    }

    #[test]
    fn deserialize_command_with_empty_params() {
        let raw = r#"{"type":"command","id":"x","room":"r","user":"u","ts":"2026-03-05T10:00:00Z","cmd":"status","params":[]}"#;
        let msg: Message = serde_json::from_str(raw).unwrap();
        assert!(
            matches!(&msg, Message::Command { cmd, params, .. } if cmd == "status" && params.is_empty())
        );
    }

    // ── parse_client_line tests ───────────────────────────────────────────────

    #[test]
    fn parse_plain_text_becomes_message() {
        let msg = parse_client_line("hello there", "myroom", "alice").unwrap();
        assert!(matches!(&msg, Message::Message { content, .. } if content == "hello there"));
        assert_eq!(msg.user(), "alice");
        assert_eq!(msg.room(), "myroom");
    }

    #[test]
    fn parse_json_message_envelope() {
        let raw = r#"{"type":"message","content":"from agent"}"#;
        let msg = parse_client_line(raw, "r", "bot1").unwrap();
        assert!(matches!(&msg, Message::Message { content, .. } if content == "from agent"));
    }

    #[test]
    fn parse_json_reply_envelope() {
        let raw = r#"{"type":"reply","reply_to":"deadbeef","content":"ack"}"#;
        let msg = parse_client_line(raw, "r", "bot1").unwrap();
        assert!(
            matches!(&msg, Message::Reply { reply_to, content, .. } if reply_to == "deadbeef" && content == "ack")
        );
    }

    #[test]
    fn parse_json_command_envelope() {
        let raw = r#"{"type":"command","cmd":"claim","params":["task-42"]}"#;
        let msg = parse_client_line(raw, "r", "agent").unwrap();
        assert!(
            matches!(&msg, Message::Command { cmd, params, .. } if cmd == "claim" && params == &["task-42"])
        );
    }

    #[test]
    fn parse_invalid_json_errors() {
        let result = parse_client_line(r#"{"type":"unknown_type"}"#, "r", "u");
        assert!(result.is_err());
    }

    #[test]
    fn parse_dm_envelope() {
        let raw = r#"{"type":"dm","to":"bob","content":"hey bob"}"#;
        let msg = parse_client_line(raw, "r", "alice").unwrap();
        assert!(
            matches!(&msg, Message::DirectMessage { to, content, .. } if to == "bob" && content == "hey bob")
        );
        assert_eq!(msg.user(), "alice");
    }

    #[test]
    fn dm_round_trips() {
        let msg = Message::DirectMessage {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            to: "bob".into(),
            content: "secret".into(),
            seq: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn dm_json_has_type_dm() {
        let msg = Message::DirectMessage {
            id: fixed_id(),
            room: "r".into(),
            user: "alice".into(),
            ts: fixed_ts(),
            to: "bob".into(),
            content: "hi".into(),
            seq: None,
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "dm");
        assert_eq!(v["to"], "bob");
        assert_eq!(v["content"], "hi");
    }

    // ── is_visible_to tests ───────────────────────────────────────────────────

    fn make_test_dm(from: &str, to: &str) -> Message {
        Message::DirectMessage {
            id: fixed_id(),
            room: "r".into(),
            user: from.into(),
            ts: fixed_ts(),
            seq: None,
            to: to.into(),
            content: "secret".into(),
        }
    }

    #[test]
    fn dm_visible_to_sender() {
        let msg = make_test_dm("alice", "bob");
        assert!(msg.is_visible_to("alice", None));
    }

    #[test]
    fn dm_visible_to_recipient() {
        let msg = make_test_dm("alice", "bob");
        assert!(msg.is_visible_to("bob", None));
    }

    #[test]
    fn dm_visible_to_host() {
        let msg = make_test_dm("alice", "bob");
        assert!(msg.is_visible_to("carol", Some("carol")));
    }

    #[test]
    fn dm_hidden_from_non_participant() {
        let msg = make_test_dm("alice", "bob");
        assert!(!msg.is_visible_to("carol", None));
    }

    #[test]
    fn dm_non_participant_not_elevated_by_different_host() {
        let msg = make_test_dm("alice", "bob");
        assert!(!msg.is_visible_to("carol", Some("dave")));
    }

    #[test]
    fn non_dm_always_visible() {
        let msg = make_message("r", "alice", "hello");
        assert!(msg.is_visible_to("bob", None));
        assert!(msg.is_visible_to("carol", Some("dave")));
    }

    #[test]
    fn join_always_visible() {
        let msg = make_join("r", "alice");
        assert!(msg.is_visible_to("bob", None));
    }

    // ── Accessor tests ────────────────────────────────────────────────────────

    #[test]
    fn accessors_return_correct_fields() {
        let ts = fixed_ts();
        let msg = Message::Message {
            id: fixed_id(),
            room: "testroom".into(),
            user: "carol".into(),
            ts,
            content: "x".into(),
            seq: None,
        };
        assert_eq!(msg.id(), fixed_id());
        assert_eq!(msg.room(), "testroom");
        assert_eq!(msg.user(), "carol");
        assert_eq!(msg.ts(), &fixed_ts());
    }

    // ── RoomVisibility tests ──────────────────────────────────────────────────

    #[test]
    fn room_visibility_serde_round_trip() {
        for vis in [
            RoomVisibility::Public,
            RoomVisibility::Private,
            RoomVisibility::Unlisted,
            RoomVisibility::Dm,
        ] {
            let json = serde_json::to_string(&vis).unwrap();
            let back: RoomVisibility = serde_json::from_str(&json).unwrap();
            assert_eq!(vis, back);
        }
    }

    #[test]
    fn room_visibility_rename_all_snake_case() {
        assert_eq!(
            serde_json::to_string(&RoomVisibility::Public).unwrap(),
            r#""public""#
        );
        assert_eq!(
            serde_json::to_string(&RoomVisibility::Dm).unwrap(),
            r#""dm""#
        );
    }

    // ── dm_room_id tests ──────────────────────────────────────────────────────

    #[test]
    fn dm_room_id_sorts_alphabetically() {
        assert_eq!(dm_room_id("alice", "bob").unwrap(), "dm-alice-bob");
        assert_eq!(dm_room_id("bob", "alice").unwrap(), "dm-alice-bob");
    }

    #[test]
    fn dm_room_id_same_user_errors() {
        let err = dm_room_id("alice", "alice").unwrap_err();
        assert_eq!(err, DmRoomError::SameUser("alice".to_owned()));
        assert_eq!(
            err.to_string(),
            "cannot create DM room: both users are 'alice'"
        );
    }

    #[test]
    fn dm_room_id_is_deterministic() {
        let id1 = dm_room_id("r2d2", "saphire").unwrap();
        let id2 = dm_room_id("saphire", "r2d2").unwrap();
        assert_eq!(id1, id2);
        assert_eq!(id1, "dm-r2d2-saphire");
    }

    #[test]
    fn dm_room_id_case_sensitive() {
        let id1 = dm_room_id("Alice", "bob").unwrap();
        let id2 = dm_room_id("alice", "bob").unwrap();
        // Uppercase sorts before lowercase in ASCII
        assert_eq!(id1, "dm-Alice-bob");
        assert_eq!(id2, "dm-alice-bob");
        assert_ne!(id1, id2);
    }

    #[test]
    fn dm_room_id_with_hyphens_in_usernames() {
        let id = dm_room_id("my-agent", "your-bot").unwrap();
        assert_eq!(id, "dm-my-agent-your-bot");
    }

    // ── is_dm_room tests ─────────────────────────────────────────────────────

    #[test]
    fn is_dm_room_identifies_dm_rooms() {
        assert!(is_dm_room("dm-alice-bob"));
        assert!(is_dm_room("dm-r2d2-saphire"));
    }

    #[test]
    fn is_dm_room_rejects_non_dm_rooms() {
        assert!(!is_dm_room("agent-room-2"));
        assert!(!is_dm_room("dev-chat"));
        assert!(!is_dm_room("dm"));
        assert!(!is_dm_room("dm-"));
        assert!(!is_dm_room(""));
    }

    #[test]
    fn is_dm_room_handles_edge_cases() {
        // A room starting with "dm-" but having no second hyphen
        assert!(!is_dm_room("dm-onlyoneuser"));
        // Hyphenated usernames create more dashes — still valid
        assert!(is_dm_room("dm-my-agent-your-bot"));
    }

    // ── DmRoomError tests ────────────────────────────────────────────────────

    #[test]
    fn dm_room_error_display() {
        let err = DmRoomError::SameUser("bb".to_owned());
        assert_eq!(
            err.to_string(),
            "cannot create DM room: both users are 'bb'"
        );
    }

    #[test]
    fn dm_room_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DmRoomError>();
    }

    // ── RoomConfig tests ──────────────────────────────────────────────────────

    #[test]
    fn room_config_public_defaults() {
        let config = RoomConfig::public("alice");
        assert_eq!(config.visibility, RoomVisibility::Public);
        assert!(config.max_members.is_none());
        assert!(config.invite_list.is_empty());
        assert_eq!(config.created_by, "alice");
    }

    #[test]
    fn room_config_dm_has_two_users() {
        let config = RoomConfig::dm("alice", "bob");
        assert_eq!(config.visibility, RoomVisibility::Dm);
        assert_eq!(config.max_members, Some(2));
        assert!(config.invite_list.contains("alice"));
        assert!(config.invite_list.contains("bob"));
        assert_eq!(config.invite_list.len(), 2);
    }

    #[test]
    fn room_config_serde_round_trip() {
        let config = RoomConfig::dm("alice", "bob");
        let json = serde_json::to_string(&config).unwrap();
        let back: RoomConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.visibility, RoomVisibility::Dm);
        assert_eq!(back.max_members, Some(2));
        assert!(back.invite_list.contains("alice"));
        assert!(back.invite_list.contains("bob"));
    }

    // ── RoomListEntry tests ───────────────────────────────────────────────────

    #[test]
    fn room_list_entry_serde_round_trip() {
        let entry = RoomListEntry {
            room_id: "dev-chat".into(),
            visibility: RoomVisibility::Public,
            member_count: 5,
            created_by: "alice".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: RoomListEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.room_id, "dev-chat");
        assert_eq!(back.visibility, RoomVisibility::Public);
        assert_eq!(back.member_count, 5);
    }

    // ── parse_mentions tests ────────────────────────────────────────────────

    #[test]
    fn parse_mentions_single() {
        assert_eq!(parse_mentions("hello @alice"), vec!["alice"]);
    }

    #[test]
    fn parse_mentions_multiple() {
        assert_eq!(
            parse_mentions("@alice and @bob should see this"),
            vec!["alice", "bob"]
        );
    }

    #[test]
    fn parse_mentions_at_start() {
        assert_eq!(parse_mentions("@alice hello"), vec!["alice"]);
    }

    #[test]
    fn parse_mentions_at_end() {
        assert_eq!(parse_mentions("hello @alice"), vec!["alice"]);
    }

    #[test]
    fn parse_mentions_with_hyphens_and_underscores() {
        assert_eq!(parse_mentions("cc @my-agent_2"), vec!["my-agent_2"]);
    }

    #[test]
    fn parse_mentions_deduplicates() {
        assert_eq!(parse_mentions("@alice @bob @alice"), vec!["alice", "bob"]);
    }

    #[test]
    fn parse_mentions_skips_email() {
        assert!(parse_mentions("send to user@example.com").is_empty());
    }

    #[test]
    fn parse_mentions_skips_bare_at() {
        assert!(parse_mentions("@ alone").is_empty());
    }

    #[test]
    fn parse_mentions_empty_content() {
        assert!(parse_mentions("").is_empty());
    }

    #[test]
    fn parse_mentions_no_mentions() {
        assert!(parse_mentions("just a normal message").is_empty());
    }

    #[test]
    fn parse_mentions_punctuation_after_username() {
        assert_eq!(parse_mentions("hey @alice, what's up?"), vec!["alice"]);
    }

    #[test]
    fn parse_mentions_multiple_at_signs() {
        // user@@foo — second @ is preceded by non-whitespace, so skipped
        assert_eq!(parse_mentions("@alice@@bob"), vec!["alice"]);
    }

    // ── content() and mentions() method tests ───────────────────────────────

    #[test]
    fn message_content_returns_text() {
        let msg = make_message("r", "alice", "hello @bob");
        assert_eq!(msg.content(), Some("hello @bob"));
    }

    #[test]
    fn join_content_returns_none() {
        let msg = make_join("r", "alice");
        assert!(msg.content().is_none());
    }

    #[test]
    fn message_mentions_extracts_usernames() {
        let msg = make_message("r", "alice", "hey @bob and @carol");
        assert_eq!(msg.mentions(), vec!["bob", "carol"]);
    }

    #[test]
    fn join_mentions_returns_empty() {
        let msg = make_join("r", "alice");
        assert!(msg.mentions().is_empty());
    }

    #[test]
    fn dm_mentions_works() {
        let msg = make_dm("r", "alice", "bob", "cc @carol on this");
        assert_eq!(msg.mentions(), vec!["carol"]);
    }

    #[test]
    fn reply_content_returns_text() {
        let msg = make_reply("r", "alice", "msg-1", "@bob noted");
        assert_eq!(msg.content(), Some("@bob noted"));
        assert_eq!(msg.mentions(), vec!["bob"]);
    }

    // ── format_message_id / parse_message_id tests ───────────────────────────

    #[test]
    fn format_message_id_basic() {
        assert_eq!(format_message_id("agent-room", 42), "agent-room:42");
    }

    #[test]
    fn format_message_id_seq_zero() {
        assert_eq!(format_message_id("r", 0), "r:0");
    }

    #[test]
    fn format_message_id_max_seq() {
        assert_eq!(format_message_id("r", u64::MAX), format!("r:{}", u64::MAX));
    }

    #[test]
    fn parse_message_id_basic() {
        let (room, seq) = parse_message_id("agent-room:42").unwrap();
        assert_eq!(room, "agent-room");
        assert_eq!(seq, 42);
    }

    #[test]
    fn parse_message_id_round_trips() {
        let id = format_message_id("dev-chat", 99);
        let (room, seq) = parse_message_id(&id).unwrap();
        assert_eq!(room, "dev-chat");
        assert_eq!(seq, 99);
    }

    #[test]
    fn parse_message_id_room_with_colon() {
        // Room ID that itself contains a colon — split on last colon.
        let (room, seq) = parse_message_id("namespace:room:7").unwrap();
        assert_eq!(room, "namespace:room");
        assert_eq!(seq, 7);
    }

    #[test]
    fn parse_message_id_no_colon_errors() {
        assert!(parse_message_id("nocolon").is_err());
    }

    #[test]
    fn parse_message_id_invalid_seq_errors() {
        assert!(parse_message_id("room:notanumber").is_err());
    }

    #[test]
    fn parse_message_id_negative_seq_errors() {
        // Negative numbers are not valid u64.
        assert!(parse_message_id("room:-1").is_err());
    }

    #[test]
    fn parse_message_id_empty_room_ok() {
        // Edge case: empty room component.
        let (room, seq) = parse_message_id(":5").unwrap();
        assert_eq!(room, "");
        assert_eq!(seq, 5);
    }

    // ── SubscriptionTier tests ───────────────────────────────────────────────

    #[test]
    fn subscription_tier_serde_round_trip() {
        for tier in [
            SubscriptionTier::Full,
            SubscriptionTier::MentionsOnly,
            SubscriptionTier::Unsubscribed,
        ] {
            let json = serde_json::to_string(&tier).unwrap();
            let back: SubscriptionTier = serde_json::from_str(&json).unwrap();
            assert_eq!(tier, back);
        }
    }

    #[test]
    fn subscription_tier_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&SubscriptionTier::Full).unwrap(),
            r#""full""#
        );
        assert_eq!(
            serde_json::to_string(&SubscriptionTier::MentionsOnly).unwrap(),
            r#""mentions_only""#
        );
        assert_eq!(
            serde_json::to_string(&SubscriptionTier::Unsubscribed).unwrap(),
            r#""unsubscribed""#
        );
    }

    #[test]
    fn subscription_tier_display() {
        assert_eq!(SubscriptionTier::Full.to_string(), "full");
        assert_eq!(SubscriptionTier::MentionsOnly.to_string(), "mentions_only");
        assert_eq!(SubscriptionTier::Unsubscribed.to_string(), "unsubscribed");
    }

    #[test]
    fn subscription_tier_from_str_canonical() {
        assert_eq!(
            "full".parse::<SubscriptionTier>().unwrap(),
            SubscriptionTier::Full
        );
        assert_eq!(
            "mentions_only".parse::<SubscriptionTier>().unwrap(),
            SubscriptionTier::MentionsOnly
        );
        assert_eq!(
            "unsubscribed".parse::<SubscriptionTier>().unwrap(),
            SubscriptionTier::Unsubscribed
        );
    }

    #[test]
    fn subscription_tier_from_str_aliases() {
        assert_eq!(
            "mentions-only".parse::<SubscriptionTier>().unwrap(),
            SubscriptionTier::MentionsOnly
        );
        assert_eq!(
            "mentions".parse::<SubscriptionTier>().unwrap(),
            SubscriptionTier::MentionsOnly
        );
        assert_eq!(
            "none".parse::<SubscriptionTier>().unwrap(),
            SubscriptionTier::Unsubscribed
        );
    }

    #[test]
    fn subscription_tier_from_str_invalid() {
        let err = "banana".parse::<SubscriptionTier>().unwrap_err();
        assert!(err.contains("unknown subscription tier"));
        assert!(err.contains("banana"));
    }

    #[test]
    fn subscription_tier_display_round_trips_through_from_str() {
        for tier in [
            SubscriptionTier::Full,
            SubscriptionTier::MentionsOnly,
            SubscriptionTier::Unsubscribed,
        ] {
            let s = tier.to_string();
            let back: SubscriptionTier = s.parse().unwrap();
            assert_eq!(tier, back);
        }
    }

    #[test]
    fn subscription_tier_is_copy() {
        let tier = SubscriptionTier::Full;
        let copy = tier;
        assert_eq!(tier, copy); // both valid — proves Copy
    }

    // ── EventType tests ─────────────────────────────────────────────────────

    #[test]
    fn event_type_serde_round_trip() {
        for et in [
            EventType::TaskPosted,
            EventType::TaskAssigned,
            EventType::TaskClaimed,
            EventType::TaskPlanned,
            EventType::TaskApproved,
            EventType::TaskUpdated,
            EventType::TaskReleased,
            EventType::TaskFinished,
            EventType::TaskCancelled,
            EventType::StatusChanged,
            EventType::ReviewRequested,
        ] {
            let json = serde_json::to_string(&et).unwrap();
            let back: EventType = serde_json::from_str(&json).unwrap();
            assert_eq!(et, back);
        }
    }

    #[test]
    fn event_type_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&EventType::TaskPosted).unwrap(),
            r#""task_posted""#
        );
        assert_eq!(
            serde_json::to_string(&EventType::TaskAssigned).unwrap(),
            r#""task_assigned""#
        );
        assert_eq!(
            serde_json::to_string(&EventType::ReviewRequested).unwrap(),
            r#""review_requested""#
        );
    }

    #[test]
    fn event_type_display() {
        assert_eq!(EventType::TaskPosted.to_string(), "task_posted");
        assert_eq!(EventType::TaskCancelled.to_string(), "task_cancelled");
        assert_eq!(EventType::StatusChanged.to_string(), "status_changed");
    }

    #[test]
    fn event_type_is_copy() {
        let et = EventType::TaskPosted;
        let copy = et;
        assert_eq!(et, copy);
    }

    #[test]
    fn event_type_from_str_all_variants() {
        let cases = [
            ("task_posted", EventType::TaskPosted),
            ("task_assigned", EventType::TaskAssigned),
            ("task_claimed", EventType::TaskClaimed),
            ("task_planned", EventType::TaskPlanned),
            ("task_approved", EventType::TaskApproved),
            ("task_updated", EventType::TaskUpdated),
            ("task_released", EventType::TaskReleased),
            ("task_finished", EventType::TaskFinished),
            ("task_cancelled", EventType::TaskCancelled),
            ("status_changed", EventType::StatusChanged),
            ("review_requested", EventType::ReviewRequested),
        ];
        for (s, expected) in cases {
            assert_eq!(s.parse::<EventType>().unwrap(), expected, "failed for {s}");
        }
    }

    #[test]
    fn event_type_from_str_invalid() {
        let err = "banana".parse::<EventType>().unwrap_err();
        assert!(err.contains("unknown event type"));
        assert!(err.contains("banana"));
    }

    #[test]
    fn event_type_display_round_trips_through_from_str() {
        for et in [
            EventType::TaskPosted,
            EventType::TaskAssigned,
            EventType::TaskClaimed,
            EventType::TaskPlanned,
            EventType::TaskApproved,
            EventType::TaskUpdated,
            EventType::TaskReleased,
            EventType::TaskFinished,
            EventType::TaskCancelled,
            EventType::StatusChanged,
            EventType::ReviewRequested,
        ] {
            let s = et.to_string();
            let back: EventType = s.parse().unwrap();
            assert_eq!(et, back);
        }
    }

    #[test]
    fn event_type_ord_is_deterministic() {
        let mut v = vec![
            EventType::ReviewRequested,
            EventType::TaskPosted,
            EventType::TaskApproved,
        ];
        v.sort();
        // Sorted alphabetically by string representation
        assert_eq!(v[0], EventType::ReviewRequested);
        assert_eq!(v[1], EventType::TaskApproved);
        assert_eq!(v[2], EventType::TaskPosted);
    }

    // ── EventFilter tests ───────────────────────────────────────────────────

    #[test]
    fn event_filter_default_is_all() {
        assert_eq!(EventFilter::default(), EventFilter::All);
    }

    #[test]
    fn event_filter_serde_all() {
        let f = EventFilter::All;
        let json = serde_json::to_string(&f).unwrap();
        let back: EventFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
        assert!(json.contains("\"all\""));
    }

    #[test]
    fn event_filter_serde_none() {
        let f = EventFilter::None;
        let json = serde_json::to_string(&f).unwrap();
        let back: EventFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
        assert!(json.contains("\"none\""));
    }

    #[test]
    fn event_filter_serde_only() {
        let mut types = BTreeSet::new();
        types.insert(EventType::TaskPosted);
        types.insert(EventType::TaskFinished);
        let f = EventFilter::Only { types };
        let json = serde_json::to_string(&f).unwrap();
        let back: EventFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
        assert!(json.contains("\"only\""));
        assert!(json.contains("task_posted"));
        assert!(json.contains("task_finished"));
    }

    #[test]
    fn event_filter_display_all() {
        assert_eq!(EventFilter::All.to_string(), "all");
    }

    #[test]
    fn event_filter_display_none() {
        assert_eq!(EventFilter::None.to_string(), "none");
    }

    #[test]
    fn event_filter_display_only() {
        let mut types = BTreeSet::new();
        types.insert(EventType::TaskPosted);
        types.insert(EventType::TaskFinished);
        let f = EventFilter::Only { types };
        let display = f.to_string();
        // BTreeSet is sorted, so order is deterministic
        assert!(display.contains("task_finished"));
        assert!(display.contains("task_posted"));
    }

    #[test]
    fn event_filter_from_str_all() {
        assert_eq!("all".parse::<EventFilter>().unwrap(), EventFilter::All);
    }

    #[test]
    fn event_filter_from_str_none() {
        assert_eq!("none".parse::<EventFilter>().unwrap(), EventFilter::None);
    }

    #[test]
    fn event_filter_from_str_empty_is_all() {
        assert_eq!("".parse::<EventFilter>().unwrap(), EventFilter::All);
    }

    #[test]
    fn event_filter_from_str_csv() {
        let f: EventFilter = "task_posted,task_finished".parse().unwrap();
        let mut expected = BTreeSet::new();
        expected.insert(EventType::TaskPosted);
        expected.insert(EventType::TaskFinished);
        assert_eq!(f, EventFilter::Only { types: expected });
    }

    #[test]
    fn event_filter_from_str_csv_with_spaces() {
        let f: EventFilter = "task_posted , task_finished".parse().unwrap();
        let mut expected = BTreeSet::new();
        expected.insert(EventType::TaskPosted);
        expected.insert(EventType::TaskFinished);
        assert_eq!(f, EventFilter::Only { types: expected });
    }

    #[test]
    fn event_filter_from_str_single() {
        let f: EventFilter = "task_posted".parse().unwrap();
        let mut expected = BTreeSet::new();
        expected.insert(EventType::TaskPosted);
        assert_eq!(f, EventFilter::Only { types: expected });
    }

    #[test]
    fn event_filter_from_str_invalid_type() {
        let err = "task_posted,banana".parse::<EventFilter>().unwrap_err();
        assert!(err.contains("unknown event type"));
        assert!(err.contains("banana"));
    }

    #[test]
    fn event_filter_from_str_trailing_comma() {
        let f: EventFilter = "task_posted,".parse().unwrap();
        let mut expected = BTreeSet::new();
        expected.insert(EventType::TaskPosted);
        assert_eq!(f, EventFilter::Only { types: expected });
    }

    #[test]
    fn event_filter_allows_all() {
        let f = EventFilter::All;
        assert!(f.allows(&EventType::TaskPosted));
        assert!(f.allows(&EventType::ReviewRequested));
    }

    #[test]
    fn event_filter_allows_none() {
        let f = EventFilter::None;
        assert!(!f.allows(&EventType::TaskPosted));
        assert!(!f.allows(&EventType::ReviewRequested));
    }

    #[test]
    fn event_filter_allows_only_matching() {
        let mut types = BTreeSet::new();
        types.insert(EventType::TaskPosted);
        types.insert(EventType::TaskFinished);
        let f = EventFilter::Only { types };
        assert!(f.allows(&EventType::TaskPosted));
        assert!(f.allows(&EventType::TaskFinished));
        assert!(!f.allows(&EventType::TaskAssigned));
        assert!(!f.allows(&EventType::ReviewRequested));
    }

    #[test]
    fn event_filter_display_round_trips_through_from_str() {
        let filters = vec![EventFilter::All, EventFilter::None, {
            let mut types = BTreeSet::new();
            types.insert(EventType::TaskPosted);
            types.insert(EventType::TaskFinished);
            EventFilter::Only { types }
        }];
        for f in filters {
            let s = f.to_string();
            let back: EventFilter = s.parse().unwrap();
            assert_eq!(f, back, "round-trip failed for {s}");
        }
    }

    // ── Event message tests ─────────────────────────────────────────────────

    #[test]
    fn event_round_trips() {
        let msg = Message::Event {
            id: fixed_id(),
            room: "r".into(),
            user: "plugin:taskboard".into(),
            ts: fixed_ts(),
            seq: None,
            event_type: EventType::TaskAssigned,
            content: "task tb-001 claimed by agent".into(),
            params: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn event_round_trips_with_params() {
        let params = serde_json::json!({"task_id": "tb-001", "assignee": "r2d2"});
        let msg = Message::Event {
            id: fixed_id(),
            room: "r".into(),
            user: "plugin:taskboard".into(),
            ts: fixed_ts(),
            seq: None,
            event_type: EventType::TaskAssigned,
            content: "task tb-001 assigned to r2d2".into(),
            params: Some(params),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn event_json_has_type_and_event_type() {
        let msg = Message::Event {
            id: fixed_id(),
            room: "r".into(),
            user: "plugin:taskboard".into(),
            ts: fixed_ts(),
            seq: None,
            event_type: EventType::TaskFinished,
            content: "task done".into(),
            params: None,
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "event");
        assert_eq!(v["event_type"], "task_finished");
        assert_eq!(v["content"], "task done");
        assert!(v.get("params").is_none(), "null params should be omitted");
    }

    #[test]
    fn event_json_includes_params_when_present() {
        let msg = Message::Event {
            id: fixed_id(),
            room: "r".into(),
            user: "broker".into(),
            ts: fixed_ts(),
            seq: None,
            event_type: EventType::StatusChanged,
            content: "alice set status: busy".into(),
            params: Some(serde_json::json!({"user": "alice", "status": "busy"})),
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["params"]["user"], "alice");
        assert_eq!(v["params"]["status"], "busy");
    }

    #[test]
    fn deserialize_event_from_literal() {
        let raw = r#"{"type":"event","id":"abc","room":"r","user":"bot","ts":"2026-03-05T10:00:00Z","event_type":"task_posted","content":"posted"}"#;
        let msg: Message = serde_json::from_str(raw).unwrap();
        assert!(matches!(
            &msg,
            Message::Event { event_type, content, .. }
            if *event_type == EventType::TaskPosted && content == "posted"
        ));
    }

    #[test]
    fn event_accessors_work() {
        let msg = make_event("r", "bot", EventType::TaskClaimed, "claimed", None);
        assert_eq!(msg.room(), "r");
        assert_eq!(msg.user(), "bot");
        assert_eq!(msg.content(), Some("claimed"));
        assert!(msg.seq().is_none());
    }

    #[test]
    fn event_set_seq() {
        let mut msg = make_event("r", "bot", EventType::TaskPosted, "posted", None);
        msg.set_seq(42);
        assert_eq!(msg.seq(), Some(42));
    }

    #[test]
    fn event_is_visible_to_everyone() {
        let msg = make_event("r", "bot", EventType::TaskFinished, "done", None);
        assert!(msg.is_visible_to("anyone", None));
        assert!(msg.is_visible_to("other", Some("host")));
    }

    #[test]
    fn event_mentions_extracted() {
        let msg = make_event(
            "r",
            "plugin:taskboard",
            EventType::TaskAssigned,
            "task assigned to @r2d2 by @ba",
            None,
        );
        assert_eq!(msg.mentions(), vec!["r2d2", "ba"]);
    }

    #[test]
    fn make_event_constructor() {
        let params = serde_json::json!({"key": "value"});
        let msg = make_event(
            "room1",
            "user1",
            EventType::ReviewRequested,
            "review pls",
            Some(params.clone()),
        );
        assert_eq!(msg.room(), "room1");
        assert_eq!(msg.user(), "user1");
        assert_eq!(msg.content(), Some("review pls"));
        if let Message::Event {
            event_type,
            params: p,
            ..
        } = &msg
        {
            assert_eq!(*event_type, EventType::ReviewRequested);
            assert_eq!(p.as_ref().unwrap(), &params);
        } else {
            panic!("expected Event variant");
        }
    }
}
