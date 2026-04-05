//! Secret storage abstraction for usage-meter.
//!
//! Provider cookies (and eventually any other sensitive material the app
//! needs to persist) live in the OS credential store — macOS Keychain,
//! Windows Credential Manager, or the Secret Service on Linux — via the
//! [`keyring`] crate. Plaintext on disk is never an option, hard stop.
//!
//! This module exposes a small [`SecretStore`] trait so the rest of the
//! codebase can depend on a behavior, not a backend. That keeps the
//! scrapers and scheduler unit-testable without touching the real
//! keychain (which would be unavailable or noisy in CI) and leaves room
//! to add alternative backends later (e.g. an encrypted-at-rest file for
//! portable Linux installs that lack a Secret Service daemon) without
//! rewriting callers.
//!
//! Two implementations live here:
//!
//! - [`KeychainStore`]: thin wrapper over `keyring::Entry` that namespaces
//!   all entries under a single service identifier.
//! - [`MemoryStore`]: in-process `HashMap` used by tests and early
//!   scaffolding. Deliberately compiled unconditionally so integration
//!   tests in sibling modules can use it without feature juggling.
//!
//! Cookie payloads are treated as opaque `String`s by this layer. The
//! provider modules are responsible for serializing their own cookie
//! sets (probably JSON) before handing the blob off. Keeping the
//! abstraction dumb means swapping the backend never forces a schema
//! migration.

use std::collections::HashMap;
use std::sync::Mutex;

use thiserror::Error;

/// Errors the secret store can surface to callers.
///
/// `NotFound` is modelled as an `Option<String>` return on `get` instead
/// of an error variant, since "no value yet" is a normal first-run state
/// rather than a failure. Everything else is a genuine backend problem.
#[derive(Debug, Error)]
pub enum SecretError {
    /// The underlying OS credential store returned an error (permission
    /// denied, keychain locked, IPC failure, etc.).
    #[error("credential store backend error: {0}")]
    Backend(String),

    /// The in-memory fake's mutex was poisoned by a panic in another
    /// thread. Practically unreachable in normal operation; surfaced as
    /// an error rather than a panic so tests can assert on it.
    #[error("in-memory store mutex poisoned")]
    Poisoned,
}

/// Behavior-only contract for storing and retrieving secrets by key.
///
/// Implementations must be `Send + Sync` so the scheduler (which runs on
/// a background tokio task) can share a single store with the Tauri
/// command handlers without extra locking at the call site.
pub trait SecretStore: Send + Sync {
    /// Returns the value for `key`, or `Ok(None)` if no entry exists.
    ///
    /// A missing entry is *not* an error — first-run state is a normal
    /// case that every caller has to handle anyway, so forcing them to
    /// pattern-match on an error variant would just create noise.
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;

    /// Writes `value` to `key`, overwriting any existing entry.
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;

    /// Removes `key`. Deleting a non-existent key is a no-op, to match
    /// the idempotent "make sure this is gone" semantics callers
    /// actually want during sign-out.
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

// ---------------------------------------------------------------------------
// KeychainStore — real backend
// ---------------------------------------------------------------------------

/// OS credential store backed by the [`keyring`] crate.
///
/// All entries are namespaced under a single `service` string so that
/// uninstalling the app (or a future "clear all secrets" UI action) can
/// enumerate and delete usage-meter's entries without touching unrelated
/// credentials the user may have stored.
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    /// Creates a store that reads and writes under the given service
    /// identifier. Convention: reverse-DNS, matching the app's bundle
    /// identifier (`com.chaceraiter.usage-meter`).
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, key).map_err(|e| SecretError::Backend(e.to_string()))
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        match self.entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            // `NoEntry` is the "key has never been set" case — translate
            // to `None` so callers get the shape they expect.
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Backend(e.to_string())),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.entry(key)?
            .set_password(value)
            .map_err(|e| SecretError::Backend(e.to_string()))
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) => Ok(()),
            // Deleting something already gone is success — see trait doc.
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Backend(e.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryStore — test fake
// ---------------------------------------------------------------------------

/// In-process `HashMap`-backed store for tests and local scaffolding.
///
/// Intentionally `pub` (not `#[cfg(test)]`) so integration tests living
/// in other modules — and early end-to-end smoke tests that do not want
/// to touch the real keychain — can share a single fake without feature
/// flag gymnastics. The cost is ~30 lines of code in release builds,
/// which is a rounding error next to `tauri` itself.
#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let guard = self.inner.lock().map_err(|_| SecretError::Poisoned)?;
        Ok(guard.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let mut guard = self.inner.lock().map_err(|_| SecretError::Poisoned)?;
        guard.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let mut guard = self.inner.lock().map_err(|_| SecretError::Poisoned)?;
        guard.remove(key);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// We only unit-test `MemoryStore` here. `KeychainStore` cannot be tested
// hermetically — on macOS it would pop authorization prompts and touch
// the user's real keychain, on Linux it depends on a running Secret
// Service daemon. Those backends are small wrappers over `keyring` and
// are better covered by a manual smoke test or an opt-in integration
// test gated behind an env var, added when the auth UX lands.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_missing_key_returns_none() {
        let store = MemoryStore::new();
        assert_eq!(store.get("claude.cookies").unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trips() {
        let store = MemoryStore::new();
        store.set("claude.cookies", "blob-v1").unwrap();
        assert_eq!(
            store.get("claude.cookies").unwrap(),
            Some("blob-v1".to_string())
        );
    }

    #[test]
    fn set_overwrites_existing_value() {
        let store = MemoryStore::new();
        store.set("k", "first").unwrap();
        store.set("k", "second").unwrap();
        assert_eq!(store.get("k").unwrap(), Some("second".to_string()));
    }

    #[test]
    fn delete_removes_existing_key() {
        let store = MemoryStore::new();
        store.set("k", "v").unwrap();
        store.delete("k").unwrap();
        assert_eq!(store.get("k").unwrap(), None);
    }

    #[test]
    fn delete_missing_key_is_noop() {
        let store = MemoryStore::new();
        // No panic, no error — idempotent by contract.
        store.delete("never-set").unwrap();
    }

    #[test]
    fn keys_are_independent() {
        let store = MemoryStore::new();
        store.set("claude.cookies", "a").unwrap();
        store.set("chatgpt.cookies", "b").unwrap();
        assert_eq!(
            store.get("claude.cookies").unwrap(),
            Some("a".to_string())
        );
        assert_eq!(
            store.get("chatgpt.cookies").unwrap(),
            Some("b".to_string())
        );
    }

    /// `SecretStore` must be usable as a trait object, since the
    /// scheduler and command layer will hold `Arc<dyn SecretStore>`
    /// rather than binding to a concrete backend at compile time.
    #[test]
    fn store_is_object_safe() {
        let store: Box<dyn SecretStore> = Box::new(MemoryStore::new());
        store.set("k", "v").unwrap();
        assert_eq!(store.get("k").unwrap(), Some("v".to_string()));
    }
}
