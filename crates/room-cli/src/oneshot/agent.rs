use super::transport::{resolve_socket_target, send_message_with_token_target as transport_send};
use crate::message::Message;

/// Send an `/agent` command to the broker and print the response.
///
/// Builds a command envelope with `cmd: "agent"` and the given params,
/// sends it via the oneshot transport, and prints the broker's reply.
async fn send_agent_command(
    room_id: &str,
    token: &str,
    params: Vec<String>,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let target = resolve_socket_target(room_id, socket);
    let wire = serde_json::json!({"type": "command", "cmd": "agent", "params": params}).to_string();
    let msg = transport_send(&target, token, &wire).await.map_err(|e| {
        if e.to_string().contains("invalid token") {
            anyhow::anyhow!("invalid token — run: room join <username>")
        } else {
            e
        }
    })?;

    match &msg {
        Message::System { content, .. } => println!("{content}"),
        _ => println!("{}", serde_json::to_string(&msg)?),
    }
    Ok(())
}

/// One-shot `room agent spawn` — spawn a ralph agent in the room.
pub async fn cmd_agent_spawn(
    room_id: &str,
    token: &str,
    username: &str,
    model: Option<&str>,
    issue: Option<&str>,
    personality: Option<&str>,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let mut params = vec!["spawn".to_owned(), username.to_owned()];
    if let Some(m) = model {
        params.push("--model".to_owned());
        params.push(m.to_owned());
    }
    if let Some(i) = issue {
        params.push("--issue".to_owned());
        params.push(i.to_owned());
    }
    if let Some(p) = personality {
        params.push("--personality".to_owned());
        params.push(p.to_owned());
    }
    send_agent_command(room_id, token, params, socket).await
}

/// One-shot `room agent list` — list agents in the room.
pub async fn cmd_agent_list(
    room_id: &str,
    token: &str,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    send_agent_command(room_id, token, vec!["list".to_owned()], socket).await
}

/// One-shot `room agent stop` — stop a running agent.
pub async fn cmd_agent_stop(
    room_id: &str,
    token: &str,
    username: &str,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    send_agent_command(
        room_id,
        token,
        vec!["stop".to_owned(), username.to_owned()],
        socket,
    )
    .await
}

/// One-shot `room agent logs` — view agent logs.
pub async fn cmd_agent_logs(
    room_id: &str,
    token: &str,
    username: &str,
    tail: usize,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    send_agent_command(
        room_id,
        token,
        vec![
            "logs".to_owned(),
            username.to_owned(),
            "--tail".to_owned(),
            tail.to_string(),
        ],
        socket,
    )
    .await
}

#[cfg(test)]
mod tests {
    #[test]
    fn agent_spawn_wire_payload() {
        let params = vec!["spawn".to_owned(), "testbot".to_owned()];
        let wire =
            serde_json::json!({"type": "command", "cmd": "agent", "params": params}).to_string();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "agent");
        let p: Vec<&str> = v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(p, vec!["spawn", "testbot"]);
    }

    #[test]
    fn agent_spawn_with_flags_wire_payload() {
        let mut params = vec!["spawn".to_owned(), "mybot".to_owned()];
        params.push("--model".to_owned());
        params.push("sonnet".to_owned());
        params.push("--issue".to_owned());
        params.push("42".to_owned());
        let wire =
            serde_json::json!({"type": "command", "cmd": "agent", "params": params}).to_string();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        let p: Vec<&str> = v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(
            p,
            vec!["spawn", "mybot", "--model", "sonnet", "--issue", "42"]
        );
    }

    #[test]
    fn agent_list_wire_payload() {
        let wire =
            serde_json::json!({"type": "command", "cmd": "agent", "params": ["list"]}).to_string();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["cmd"], "agent");
        assert_eq!(v["params"][0], "list");
    }

    #[test]
    fn agent_stop_wire_payload() {
        let wire =
            serde_json::json!({"type": "command", "cmd": "agent", "params": ["stop", "bot1"]})
                .to_string();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["cmd"], "agent");
        let p: Vec<&str> = v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(p, vec!["stop", "bot1"]);
    }

    #[test]
    fn agent_logs_wire_payload() {
        let wire = serde_json::json!({
            "type": "command",
            "cmd": "agent",
            "params": ["logs", "bot1", "--tail", "100"]
        })
        .to_string();
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["cmd"], "agent");
        let p: Vec<&str> = v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(p, vec!["logs", "bot1", "--tail", "100"]);
    }
}
