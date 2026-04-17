//! API key authentication for the wendRAG MCP HTTP transport.
//!
//! Provides cryptographically strong key generation, SHA-256 hashed storage,
//! constant-time validation, and a runtime [`Authenticator`] that combines a
//! single static key from the `WEND_RAG_API_KEY` environment variable with
//! multiple named keys persisted in a local JSON file.
//!
//! # Threat model
//!
//! - Keys are only shown to the operator **once** at generation time.
//! - The on-disk store only contains SHA-256 hashes, never raw keys.
//! - Validation uses constant-time comparison to avoid timing side channels.
//! - Keys carry a stable `wrag_` prefix so accidental leaks can be detected
//!   by secret-scanning tooling.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Human-visible prefix prepended to every generated key.
///
/// The prefix is included inside the hashed material so that validation
/// matches the exact string the user copied.
pub const KEY_PREFIX: &str = "wrag_";

/// Number of random bytes sourced from the operating system CSPRNG. 32 bytes
/// (256 bits) exceeds NIST's 128-bit minimum for long-term key material.
pub const KEY_RANDOM_BYTES: usize = 32;

/// Override env var for the absolute path of the keys JSON file. Primarily
/// intended for tests and Docker deployments that want to control the path
/// without relying on home-directory discovery.
pub const KEYS_FILE_ENV: &str = "WEND_RAG_KEYS_FILE";

/// Env var holding a single static API key. When set, its SHA-256 hash is
/// added to the runtime [`Authenticator`] alongside any file-based keys.
pub const STATIC_KEY_ENV: &str = "WEND_RAG_API_KEY";

/// Errors that can occur while loading, persisting, or mutating the key store.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Filesystem error while reading or writing the keys file.
    #[error("I/O error on keys file: {0}")]
    Io(#[from] io::Error),

    /// The on-disk keys file is not valid JSON or the schema does not match.
    #[error("invalid keys file JSON: {0}")]
    Json(#[from] serde_json::Error),

    /// The operating system CSPRNG is unavailable.
    #[error("OS random number generator unavailable: {0}")]
    Random(String),

    /// `key:generate` was invoked with a name that already exists. Names must
    /// be unique so operators can unambiguously revoke a key later.
    #[error("key name '{0}' already exists")]
    DuplicateName(String),

    /// `key:revoke` was invoked for an unknown name.
    #[error("key name '{0}' not found")]
    NotFound(String),

    /// The key name was empty or whitespace-only.
    #[error("key name cannot be empty")]
    EmptyName,

    /// Neither `WEND_RAG_KEYS_FILE` nor a home/config directory was
    /// available, so the default path could not be determined.
    #[error("could not determine keys file location; set {KEYS_FILE_ENV} to override")]
    NoKeysPath,
}

/// A single registered API key as persisted in `keys.json`.
///
/// Only the SHA-256 hash and a short display prefix are stored; the raw key
/// material is never written to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredKey {
    /// Operator-assigned human-readable name (e.g. `"RaymonKey"`).
    pub name: String,
    /// First 8 hex characters of the key body (after `wrag_`), safe to
    /// display in logs and list output for identification.
    pub key_prefix: String,
    /// Hex-encoded SHA-256 hash of the full key string (including `wrag_`).
    pub key_hash: String,
    /// UTC timestamp of when the key was generated.
    pub created_at: DateTime<Utc>,
}

/// Persistent collection of named keys.
///
/// The store is a thin wrapper over a JSON array on disk. All public methods
/// that mutate the collection leave in-memory state unchanged on failure so
/// callers can retry after correcting the underlying condition.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyStore {
    keys: Vec<StoredKey>,
}

impl KeyStore {
    /// Loads the key store from the default location (see
    /// [`default_keys_path`]). Returns an empty store if the file does not
    /// exist yet.
    pub fn load_default() -> Result<Self, AuthError> {
        match default_keys_path() {
            Some(path) => Self::load_from(&path),
            None => Err(AuthError::NoKeysPath),
        }
    }

    /// Loads the key store from `path`. A missing file is treated as an
    /// empty store so first-run invocations don't fail.
    pub fn load_from(path: &Path) -> Result<Self, AuthError> {
        match fs::read(path) {
            Ok(bytes) if bytes.trim_ascii().is_empty() => Ok(Self::default()),
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(AuthError::Io(err)),
        }
    }

    /// Persists the store to the default location, creating parent
    /// directories as needed.
    pub fn save_default(&self) -> Result<(), AuthError> {
        let path = default_keys_path().ok_or(AuthError::NoKeysPath)?;
        self.save_to(&path)
    }

    /// Persists the store to `path`, creating parent directories as needed.
    /// The file is written with `0o600` permissions on Unix to prevent other
    /// local users from reading the hashes.
    pub fn save_to(&self, path: &Path) -> Result<(), AuthError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let serialized = serde_json::to_vec_pretty(self)?;
        fs::write(path, &serialized)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(path, perms)?;
        }

        Ok(())
    }

    /// Returns the list of stored keys. Useful for `key:list` output.
    pub fn keys(&self) -> &[StoredKey] {
        &self.keys
    }

    /// Returns `true` when no keys are registered.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Creates a new key with the given name, stores its hash, and returns
    /// the raw key string.
    ///
    /// # Errors
    ///
    /// - [`AuthError::EmptyName`] if `name` is whitespace-only.
    /// - [`AuthError::DuplicateName`] if another key shares the same name.
    /// - [`AuthError::Random`] if the OS CSPRNG fails.
    ///
    /// # Note
    ///
    /// The returned raw key is the *only* time the caller will see it. The
    /// store must be persisted by the caller (via [`Self::save_default`] or
    /// [`Self::save_to`]) after a successful call.
    pub fn add_key(&mut self, name: &str) -> Result<String, AuthError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(AuthError::EmptyName);
        }
        if self.keys.iter().any(|k| k.name == trimmed) {
            return Err(AuthError::DuplicateName(trimmed.to_string()));
        }

        let raw = generate_key_material()?;
        // `raw` is guaranteed to start with `KEY_PREFIX` so the slice is
        // safe; take the first 8 chars of the hex body for the display prefix.
        let body = &raw[KEY_PREFIX.len()..];
        let prefix_len = body.char_indices().nth(8).map(|(i, _)| i).unwrap_or(body.len());
        let key_prefix = format!("{KEY_PREFIX}{}", &body[..prefix_len]);

        self.keys.push(StoredKey {
            name: trimmed.to_string(),
            key_prefix,
            key_hash: hash_key(&raw),
            created_at: Utc::now(),
        });

        Ok(raw)
    }

    /// Removes the key with the given name.
    ///
    /// # Errors
    ///
    /// [`AuthError::NotFound`] if no key matches.
    pub fn revoke(&mut self, name: &str) -> Result<(), AuthError> {
        let trimmed = name.trim();
        let before = self.keys.len();
        self.keys.retain(|k| k.name != trimmed);
        if self.keys.len() == before {
            return Err(AuthError::NotFound(trimmed.to_string()));
        }
        Ok(())
    }
}

/// Runtime authenticator used by the HTTP middleware.
///
/// Combines an optional static key (from `WEND_RAG_API_KEY`) with the
/// file-based key hashes. Cloning is cheap enough to share across Axum
/// handler state: every hash is a short hex string.
#[derive(Debug, Clone, Default)]
pub struct Authenticator {
    static_key_hash: Option<String>,
    key_hashes: Vec<String>,
}

impl Authenticator {
    /// Builds an authenticator from the provided static key (already decoded
    /// from the env var by the caller) and key store.
    pub fn new(static_key: Option<&str>, store: &KeyStore) -> Self {
        let static_key_hash = static_key
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(hash_key);
        let key_hashes = store.keys.iter().map(|k| k.key_hash.clone()).collect();
        Self {
            static_key_hash,
            key_hashes,
        }
    }

    /// Loads the authenticator from the environment and default keys file.
    /// Intended for the daemon startup path.
    pub fn from_environment() -> Result<Self, AuthError> {
        let static_key = std::env::var(STATIC_KEY_ENV).ok();
        let store = match default_keys_path() {
            Some(path) => KeyStore::load_from(&path)?,
            None => KeyStore::default(),
        };
        Ok(Self::new(static_key.as_deref(), &store))
    }

    /// Returns `true` when at least one key is configured, i.e. the HTTP
    /// transport should require `Authorization: Bearer <token>`.
    pub fn is_auth_required(&self) -> bool {
        self.static_key_hash.is_some() || !self.key_hashes.is_empty()
    }

    /// Number of registered keys (static + file). Useful for startup logs.
    pub fn key_count(&self) -> usize {
        self.key_hashes.len() + usize::from(self.static_key_hash.is_some())
    }

    /// Validates a presented bearer token against all known key hashes.
    ///
    /// The comparison is constant-time per candidate so attackers cannot
    /// distinguish "wrong prefix" from "wrong suffix" via timing.
    pub fn validate(&self, presented: &str) -> bool {
        let presented_hash = hash_key(presented);
        if let Some(ref static_hash) = self.static_key_hash
            && ct_eq(static_hash.as_bytes(), presented_hash.as_bytes())
        {
            return true;
        }
        self.key_hashes
            .iter()
            .any(|h| ct_eq(h.as_bytes(), presented_hash.as_bytes()))
    }
}

/// Resolves the on-disk location of `keys.json`.
///
/// Resolution order:
/// 1. `WEND_RAG_KEYS_FILE` env var (explicit override).
/// 2. Platform default:
///    - Unix/Mac: `$HOME/.wend-rag/keys.json`
///    - Windows: `%APPDATA%/wend-rag/keys.json`
///
/// Returns `None` only when neither source yields a usable directory.
pub fn default_keys_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(KEYS_FILE_ENV)
        && !explicit.is_empty()
    {
        return Some(PathBuf::from(explicit));
    }

    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return Some(PathBuf::from(appdata).join("wend-rag").join("keys.json"));
        }
    }

    dirs::home_dir().map(|home| home.join(".wend-rag").join("keys.json"))
}

/// Generates a fresh `wrag_`-prefixed hex key using the OS CSPRNG.
///
/// The output is exactly `KEY_PREFIX.len() + KEY_RANDOM_BYTES * 2` characters
/// long (default: 69 chars, `wrag_` + 64 hex).
pub fn generate_key_material() -> Result<String, AuthError> {
    let mut bytes = [0u8; KEY_RANDOM_BYTES];
    // `getrandom::fill` replaced `getrandom::getrandom` in the 0.3 API; the
    // behaviour is identical — block until the OS CSPRNG can return
    // high-quality entropy, then fill the buffer in-place.
    getrandom::fill(&mut bytes).map_err(|e| AuthError::Random(e.to_string()))?;

    let mut out = String::with_capacity(KEY_PREFIX.len() + bytes.len() * 2);
    out.push_str(KEY_PREFIX);
    for b in &bytes {
        write!(out, "{b:02x}").expect("writing to String is infallible");
    }
    Ok(out)
}

/// SHA-256 hashes an arbitrary key string and returns the lowercase hex digest.
fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();

    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        write!(hex, "{b:02x}").expect("writing to String is infallible");
    }
    hex
}

/// Constant-time byte-slice comparison. Returns `true` only when both slices
/// are the same length *and* have identical contents.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generated_keys_have_prefix_and_length() {
        let key = generate_key_material().expect("CSPRNG available");
        assert!(key.starts_with(KEY_PREFIX));
        assert_eq!(key.len(), KEY_PREFIX.len() + KEY_RANDOM_BYTES * 2);
        assert!(key[KEY_PREFIX.len()..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generated_keys_are_unique() {
        let a = generate_key_material().unwrap();
        let b = generate_key_material().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn add_and_validate_roundtrip() {
        let mut store = KeyStore::default();
        let raw = store.add_key("primary").unwrap();
        let auth = Authenticator::new(None, &store);
        assert!(auth.is_auth_required());
        assert!(auth.validate(&raw));
        assert!(!auth.validate("wrag_deadbeef"));
    }

    #[test]
    fn duplicate_name_rejected() {
        let mut store = KeyStore::default();
        store.add_key("alpha").unwrap();
        let err = store.add_key("alpha").unwrap_err();
        assert!(matches!(err, AuthError::DuplicateName(_)));
    }

    #[test]
    fn empty_name_rejected() {
        let mut store = KeyStore::default();
        let err = store.add_key("   ").unwrap_err();
        assert!(matches!(err, AuthError::EmptyName));
    }

    #[test]
    fn revoke_removes_key() {
        let mut store = KeyStore::default();
        let raw = store.add_key("alpha").unwrap();
        store.revoke("alpha").unwrap();
        let auth = Authenticator::new(None, &store);
        assert!(!auth.validate(&raw));
        assert!(!auth.is_auth_required());
    }

    #[test]
    fn revoke_unknown_errors() {
        let mut store = KeyStore::default();
        let err = store.revoke("ghost").unwrap_err();
        assert!(matches!(err, AuthError::NotFound(_)));
    }

    #[test]
    fn disk_roundtrip_preserves_keys() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("subdir").join("keys.json");

        let mut store = KeyStore::default();
        let raw = store.add_key("disk").unwrap();
        store.save_to(&path).unwrap();

        let loaded = KeyStore::load_from(&path).unwrap();
        assert_eq!(loaded.keys().len(), 1);
        assert_eq!(loaded.keys()[0].name, "disk");

        let auth = Authenticator::new(None, &loaded);
        assert!(auth.validate(&raw));
    }

    #[test]
    fn load_missing_file_is_empty_store() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does-not-exist.json");
        let store = KeyStore::load_from(&path).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn static_env_key_validates() {
        let store = KeyStore::default();
        let auth = Authenticator::new(Some("wrag_static_key"), &store);
        assert!(auth.is_auth_required());
        assert!(auth.validate("wrag_static_key"));
        assert!(!auth.validate("wrag_other_key"));
    }

    #[test]
    fn no_keys_means_auth_not_required() {
        let auth = Authenticator::new(None, &KeyStore::default());
        assert!(!auth.is_auth_required());
        assert_eq!(auth.key_count(), 0);
    }

    #[test]
    fn ct_eq_is_length_sensitive() {
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
    }

    #[test]
    fn key_prefix_is_short_and_visible() {
        let mut store = KeyStore::default();
        let raw = store.add_key("visible").unwrap();
        let stored = &store.keys()[0];
        assert!(stored.key_prefix.starts_with(KEY_PREFIX));
        // wrag_ + 8 hex chars = 13 chars
        assert_eq!(stored.key_prefix.len(), KEY_PREFIX.len() + 8);
        assert!(raw.starts_with(&stored.key_prefix));
    }
}
