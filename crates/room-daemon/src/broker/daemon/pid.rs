//! PID file management for the daemon process.

/// Write the current process's PID to `path` (creates or overwrites).
pub fn write_pid_file(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(path, std::process::id().to_string())
}

/// Returns `true` if the PID recorded in `path` belongs to a running process.
///
/// Returns `false` when the file is missing, unreadable, or unparseable, and
/// when the process is confirmed dead (ESRCH).
pub fn is_pid_alive(path: &std::path::Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(pid) = contents.trim().parse::<u32>() else {
        return false;
    };
    pid_alive(pid)
}

/// Remove the PID file, ignoring errors (best-effort cleanup).
pub fn remove_pid_file(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

/// Check whether a process with the given PID is currently running.
///
/// Uses POSIX `kill(pid, 0)` — signal 0 never delivers a signal but the kernel
/// validates whether the calling process may signal `pid`, returning:
/// - `0`  → process exists
/// - `-1` with `EPERM` (errno 1)  → process exists, permission denied
/// - `-1` with `ESRCH` (errno 3)  → no such process
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: kill(pid, 0) never delivers a signal; it only checks liveness.
    let ret = unsafe { kill(pid as i32, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM == 1 on Linux and macOS: process exists but we lack permission.
    std::io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    // Conservative: assume the process is alive on non-Unix platforms.
    true
}
