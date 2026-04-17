/*!
 * Integration tests for the `wend-rag key:*` CLI lifecycle.
 *
 * These tests spawn the real `wend-rag` binary with `WEND_RAG_KEYS_FILE`
 * pointed at a unique tempfile, then exercise the full generate -> list ->
 * revoke flow. They confirm that:
 *
 *   1. `key:generate` produces a `wrag_` prefixed 64-hex-char key,
 *      persists only a hash to disk, and echoes the raw key exactly once.
 *   2. `key:list` reports the stored name/prefix without ever printing the
 *      raw key material.
 *   3. A second `key:generate` with the same name is rejected.
 *   4. `key:revoke` removes the key so `key:list` returns the empty
 *      listing again.
 *   5. Revoking an unknown name fails with a non-zero exit code.
 */

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

/// Matches `wrag_` followed by 64 lowercase hex characters.
const RAW_KEY_LEN: usize = 5 + 64;

/**
 * Locates the compiled `wend-rag` binary next to the integration test
 * executable. Keeps test invocations hermetic (no reliance on PATH).
 */
fn binary_path() -> PathBuf {
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
        "binary not found at {path:?} -- run `cargo build` first"
    );
    path
}

/**
 * Creates a unique path inside the OS temp dir for a single test's keys
 * file. Using pid + test name keeps parallel runs isolated.
 */
fn unique_keys_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "wend-rag-keytest-{}-{}.json",
        std::process::id(),
        label,
    ))
}

/**
 * Runs `wend-rag <args...>` with `WEND_RAG_KEYS_FILE` set to `keys_path`
 * and returns the captured `Output`. Stdin is closed so commands that
 * would otherwise prompt fall through to their CLI flags or fail.
 */
fn run_cli(keys_path: &PathBuf, args: &[&str]) -> Output {
    Command::new(binary_path())
        .args(args)
        .env("WEND_RAG_KEYS_FILE", keys_path)
        .env("RUST_LOG", "error")
        .output()
        .expect("failed to invoke wend-rag CLI")
}

/**
 * Extracts the `Key: ...` line emitted by `key:generate`. Panics if the
 * expected line is absent so the caller sees the real stdout in the
 * failure message.
 */
fn extract_generated_key(stdout: &str) -> String {
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Key:") {
            return rest.trim().to_string();
        }
    }
    panic!("no `Key:` line in CLI output:\n{stdout}");
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/**
 * End-to-end test for the generate -> list -> revoke -> list flow.
 *
 * This is intentionally a single long test so we exercise the state
 * transitions with a shared keys file; splitting it would require
 * recreating the preconditions in each test and would not add coverage.
 */
#[test]
fn key_generate_list_revoke_round_trip() {
    let keys_path = unique_keys_path("roundtrip");
    // Make sure nothing is left from an earlier aborted run.
    fs::remove_file(&keys_path).ok();

    // 1. Starting state: list is empty.
    let before = run_cli(&keys_path, &["key:list"]);
    assert!(
        before.status.success(),
        "key:list on empty store must succeed, stderr:\n{}",
        String::from_utf8_lossy(&before.stderr)
    );
    let before_stdout = String::from_utf8_lossy(&before.stdout);
    assert!(
        before_stdout.contains("No keys registered"),
        "empty listing must mention that no keys exist, got:\n{before_stdout}"
    );

    // 2. Generate a key non-interactively and capture it.
    let generated = run_cli(&keys_path, &["key:generate", "--name", "integration"]);
    assert!(
        generated.status.success(),
        "key:generate must succeed, stderr:\n{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let gen_stdout = String::from_utf8_lossy(&generated.stdout).into_owned();
    assert!(
        gen_stdout.contains("Key Created"),
        "generate output missing success banner:\n{gen_stdout}"
    );
    let raw_key = extract_generated_key(&gen_stdout);

    // Structural checks on the key itself.
    assert!(raw_key.starts_with("wrag_"), "wrong prefix: {raw_key}");
    assert_eq!(raw_key.len(), RAW_KEY_LEN, "wrong length: {raw_key}");
    assert!(
        raw_key[5..].chars().all(|c| c.is_ascii_hexdigit()),
        "body must be hex: {raw_key}"
    );

    // 3. The keys file must exist and must NOT contain the raw key.
    let persisted = fs::read_to_string(&keys_path)
        .expect("keys file must be written by key:generate");
    assert!(
        !persisted.contains(&raw_key),
        "keys.json must store hashes, never the raw key"
    );
    assert!(
        persisted.contains("integration"),
        "keys.json must record the key name: {persisted}"
    );
    assert!(
        persisted.contains("key_hash"),
        "keys.json must record a key_hash field: {persisted}"
    );

    // 4. list must report the new key by name without exposing the body.
    let list = run_cli(&keys_path, &["key:list"]);
    assert!(list.status.success(), "key:list must succeed");
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(list_stdout.contains("integration"));
    assert!(
        !list_stdout.contains(&raw_key),
        "list must never print the raw key"
    );

    // 5. Duplicate name must fail.
    let dup = run_cli(&keys_path, &["key:generate", "--name", "integration"]);
    assert!(
        !dup.status.success(),
        "duplicate name must be rejected; stdout: {}",
        String::from_utf8_lossy(&dup.stdout)
    );

    // 6. Revoke removes the key.
    let revoke = run_cli(&keys_path, &["key:revoke", "integration"]);
    assert!(
        revoke.status.success(),
        "key:revoke must succeed, stderr:\n{}",
        String::from_utf8_lossy(&revoke.stderr)
    );

    let after = run_cli(&keys_path, &["key:list"]);
    assert!(after.status.success());
    let after_stdout = String::from_utf8_lossy(&after.stdout);
    assert!(
        after_stdout.contains("No keys registered"),
        "list after revoke must be empty, got:\n{after_stdout}"
    );

    // 7. Revoking again must error (NotFound).
    let revoke_again = run_cli(&keys_path, &["key:revoke", "integration"]);
    assert!(
        !revoke_again.status.success(),
        "revoking an unknown name must exit non-zero"
    );

    fs::remove_file(&keys_path).ok();
}

/**
 * The CLI must reject whitespace-only key names before touching the
 * keys file. Confirms the empty-name guard in `KeyStore::add_key`
 * surfaces through to the user-facing exit code.
 */
#[test]
fn key_generate_rejects_empty_name() {
    let keys_path = unique_keys_path("empty-name");
    fs::remove_file(&keys_path).ok();

    let out = run_cli(&keys_path, &["key:generate", "--name", "   "]);
    assert!(
        !out.status.success(),
        "empty/whitespace name must be rejected; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // File should not have been created since add_key failed before save.
    assert!(
        !keys_path.exists(),
        "keys.json must not be written when add_key errors"
    );
}

/**
 * Two successive generations must produce different keys and accumulate
 * in the store. Also confirms that generated keys always have a unique
 * display prefix recorded alongside the name.
 */
#[test]
fn key_generate_produces_unique_keys() {
    let keys_path = unique_keys_path("unique");
    fs::remove_file(&keys_path).ok();

    let first = run_cli(&keys_path, &["key:generate", "--name", "first"]);
    assert!(first.status.success());
    let first_key = extract_generated_key(&String::from_utf8_lossy(&first.stdout));

    let second = run_cli(&keys_path, &["key:generate", "--name", "second"]);
    assert!(second.status.success());
    let second_key = extract_generated_key(&String::from_utf8_lossy(&second.stdout));

    assert_ne!(
        first_key, second_key,
        "CSPRNG must produce distinct keys across generate calls"
    );

    let list = run_cli(&keys_path, &["key:list"]);
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(list_stdout.contains("first"));
    assert!(list_stdout.contains("second"));

    fs::remove_file(&keys_path).ok();
}
