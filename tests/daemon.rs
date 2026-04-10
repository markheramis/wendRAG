/**
 * Integration tests for daemon-mode behavior: health endpoint, graceful
 * shutdown via SIGTERM, and clean exit under signal pressure.
 *
 * The test spawns the real `wend-rag` binary with an in-memory SQLite backend
 * (no external services required) and exercises the HTTP lifecycle.
 *
 * SIGTERM assertions are Unix-only; on Windows the test validates the health
 * endpoint but skips signal-based shutdown verification.
 */

#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Maximum time to wait for the server to become healthy before giving up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

/// Interval between health-check polls during startup.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Time to wait for the process to exit after a signal.
#[cfg(unix)]
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/**
 * Finds a port the OS confirms is available right now. A small race window
 * exists between releasing the socket and the child binding, but it is
 * acceptable for test purposes.
 */
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("failed to bind ephemeral port")
        .local_addr()
        .expect("failed to get local addr")
        .port()
}

/**
 * Returns the path to the compiled `wend-rag` test binary. Cargo places it
 * alongside the integration test executables in the target directory.
 */
fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe()
        .expect("failed to get current exe path")
        .parent()
        .expect("no parent dir")
        .parent()
        .expect("no grandparent dir")
        .to_path_buf();
    if cfg!(windows) {
        path.push("wend-rag.exe");
    } else {
        path.push("wend-rag");
    }
    assert!(
        path.exists(),
        "binary not found at {path:?} — run `cargo build` first"
    );
    path
}

/**
 * Spawns the server process configured for in-memory SQLite on the given port
 * with tracing routed to stderr so we can capture log output.
 */
fn spawn_server(port: u16) -> Child {
    Command::new(binary_path())
        .env("HOST", "127.0.0.1")
        .env("PORT", port.to_string())
        .env("MCP_TRANSPORT", "http")
        .env("STORAGE_BACKEND", "sqlite")
        .env("SQLITE_PATH", ":memory:")
        .env("EMBEDDING_PROVIDER", "openai-compatible")
        .env("EMBEDDING_API_KEY", "test")
        .env("EMBEDDING_BASE_URL", "http://127.0.0.1:1")
        .env("EMBEDDING_MODEL", "unused")
        .env("EMBEDDING_DIMENSIONS", "1024")
        .env("RUST_LOG", "info")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("failed to spawn wend-rag process")
}

/**
 * Polls GET /health until a 200 response arrives or the timeout expires.
 */
async fn wait_for_healthy(port: u16) {
    let url = format!("http://127.0.0.1:{port}/health");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
    loop {
        if tokio::time::Instant::now() > deadline {
            panic!("server did not become healthy within {STARTUP_TIMEOUT:?}");
        }
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => return,
            _ => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }
}

/**
 * Sends SIGTERM to a process on Unix. Not available on Windows.
 */
#[cfg(unix)]
fn send_sigterm(child: &Child) {
    let pid = child.id() as libc::pid_t;
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/**
 * Waits for the child process to exit, with a timeout. Returns the exit
 * status if the process exited in time.
 */
#[cfg(unix)]
fn wait_with_timeout(child: &mut Child) -> Option<std::process::ExitStatus> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() > SHUTDOWN_TIMEOUT {
                    child.kill().ok();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

/**
 * Collects all available stderr output from the child into a string.
 * Assumes stderr was captured (Stdio::piped).
 */
#[cfg(unix)]
fn collect_stderr(child: &mut Child) -> String {
    child
        .stderr
        .take()
        .map(|stderr| {
            BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let port = free_port();
    let mut child = spawn_server(port);

    wait_for_healthy(port).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/health"))
        .await
        .expect("GET /health failed");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("invalid JSON");
    assert_eq!(body["status"], "ok");

    child.kill().ok();
    child.wait().ok();
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_shutdown_on_sigterm() {
    let port = free_port();
    let mut child = spawn_server(port);

    wait_for_healthy(port).await;

    send_sigterm(&child);

    let status = wait_with_timeout(&mut child)
        .expect("server did not exit within the shutdown timeout");
    let logs = collect_stderr(&mut child);

    assert!(
        status.success(),
        "expected exit code 0 after SIGTERM, got {status:?}\nstderr:\n{logs}"
    );
    assert!(
        logs.contains("shut down gracefully") || logs.contains("SIGTERM"),
        "expected graceful shutdown log message in stderr:\n{logs}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn serves_requests_until_shutdown() {
    let port = free_port();
    let mut child = spawn_server(port);

    wait_for_healthy(port).await;

    // Confirm the server is still serving after the initial health check.
    for _ in 0..3 {
        let resp = reqwest::get(format!("http://127.0.0.1:{port}/health"))
            .await
            .expect("repeated GET /health failed");
        assert_eq!(resp.status(), 200);
    }

    send_sigterm(&child);

    let status = wait_with_timeout(&mut child)
        .expect("server did not exit within the shutdown timeout");
    assert!(
        status.success(),
        "expected clean exit after serving requests, got {status:?}"
    );
}
