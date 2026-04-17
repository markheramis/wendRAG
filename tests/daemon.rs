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
    spawn_server_with_env(port, &[])
}

/**
 * Variant that lets a caller inject additional environment variables, used
 * by the auth tests to enable `WEND_RAG_API_KEY` without duplicating the
 * entire spawn block. Pins `WEND_RAG_KEYS_FILE` to a unique tempfile so
 * parallel test runs never collide and the developer's real keys store is
 * never touched.
 */
fn spawn_server_with_env(port: u16, extra_env: &[(&str, &str)]) -> Child {
    let keys_file = std::env::temp_dir().join(format!(
        "wend-rag-test-keys-{port}-{}.json",
        std::process::id()
    ));

    let mut command = Command::new(binary_path());
    command
        .arg("daemon")
        .env("WEND_RAG_HOST", "127.0.0.1")
        .env("WEND_RAG_PORT", port.to_string())
        .env("WEND_RAG_STORAGE_BACKEND", "sqlite")
        .env("WEND_RAG_SQLITE_PATH", ":memory:")
        .env("WEND_RAG_EMBEDDING_PROVIDER", "openai-compatible")
        .env("WEND_RAG_EMBEDDING_API_KEY", "test")
        .env("WEND_RAG_EMBEDDING_BASE_URL", "http://127.0.0.1:1")
        .env("WEND_RAG_EMBEDDING_MODEL", "unused")
        .env("WEND_RAG_EMBEDDING_DIMENSIONS", "1024")
        .env("WEND_RAG_KEYS_FILE", keys_file.to_string_lossy().to_string())
        .env("RUST_LOG", "info")
        .stderr(Stdio::piped())
        .stdout(Stdio::null());

    for (key, value) in extra_env {
        command.env(key, value);
    }

    command.spawn().expect("failed to spawn wend-rag process")
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

/**
 * Verifies that enabling `WEND_RAG_API_KEY` protects `/mcp` with Bearer
 * auth while leaving `/health` open. Covers the core auth flow without
 * needing a full MCP handshake.
 */
#[tokio::test]
async fn mcp_requires_bearer_token_when_api_key_is_set() {
    let port = free_port();
    let api_key = "wrag_integration_test_key_abcdef1234567890";
    let mut child = spawn_server_with_env(port, &[("WEND_RAG_API_KEY", api_key)]);

    wait_for_healthy(port).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // /health remains open even when auth is enabled.
    let health = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .expect("GET /health failed");
    assert_eq!(health.status(), 200, "/health must stay open");

    // POST /mcp without Authorization is rejected.
    let unauth = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("Content-Type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .expect("POST /mcp failed");
    assert_eq!(
        unauth.status(),
        401,
        "/mcp must return 401 without Authorization header"
    );

    // POST /mcp with an invalid Bearer token is rejected.
    let wrong = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("Authorization", "Bearer wrag_wrong_token")
        .header("Content-Type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .expect("POST /mcp with bad token failed");
    assert_eq!(
        wrong.status(),
        401,
        "/mcp must return 401 for an unknown token"
    );

    // POST /mcp with the correct Bearer token is NOT 401 -- the MCP layer
    // may return a different 4xx for a malformed JSON-RPC payload, but the
    // auth middleware itself must let the request through.
    let ok = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .expect("POST /mcp with good token failed");
    assert_ne!(
        ok.status(),
        401,
        "/mcp must accept the configured Bearer token; status was {}",
        ok.status()
    );

    child.kill().ok();
    child.wait().ok();
}

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
