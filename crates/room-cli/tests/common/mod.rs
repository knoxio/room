/// Shared test helpers for broker lifecycle management.
///
/// Provides reusable utilities for test fixtures that spawn brokers:
/// - Port allocation
/// - Socket/TCP readiness polling
/// - Stale file cleanup
use std::path::Path;
use std::time::Duration;

/// Find a free ephemeral port by binding to port 0 and releasing.
pub fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Poll until a Unix socket file appears on disk, or panic after `timeout`.
pub async fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "socket did not appear at {} within {:?}",
            path.display(),
            timeout
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Poll until a TCP connection to `port` succeeds, or panic after `timeout`.
pub async fn wait_for_tcp(port: u16, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "TCP port {port} not ready within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Remove stale files from a previous test run. Ignores missing files.
pub fn cleanup_stale_files(paths: &[&str]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}
