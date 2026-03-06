use std::process::Command;

fn agentroom_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentroom"))
}

#[test]
fn prints_deprecation_warning() {
    let output = agentroom_bin().output().expect("failed to run agentroom");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("deprecated"),
        "expected deprecation warning, got: {stderr}"
    );
}

#[test]
fn mentions_room_cli() {
    let output = agentroom_bin().output().expect("failed to run agentroom");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("room-cli"),
        "expected mention of room-cli, got: {stderr}"
    );
}

#[test]
fn mentions_cargo_install() {
    let output = agentroom_bin().output().expect("failed to run agentroom");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cargo install room-cli"),
        "expected migration command, got: {stderr}"
    );
}

#[test]
fn mentions_cargo_uninstall() {
    let output = agentroom_bin().output().expect("failed to run agentroom");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cargo uninstall agentroom"),
        "expected uninstall command, got: {stderr}"
    );
}

#[test]
fn exits_successfully() {
    let status = agentroom_bin().status().expect("failed to run agentroom");
    assert!(status.success(), "expected exit code 0, got: {status}");
}
