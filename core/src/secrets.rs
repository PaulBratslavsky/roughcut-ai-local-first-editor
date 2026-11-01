//! Where the Hosted-mode API key is kept: the operating system's own secure
//! store, never a file, the database or a command line. One slot — the
//! Anthropic key — behind a trait so tests use memory instead of the
//! developer's real key store.

use crate::error::{CoreError, Result};
use std::sync::Mutex;

/// One secret, stored somewhere safe.
pub trait SecretStore: Send + Sync {
    fn get(&self) -> Result<Option<String>>;
    fn set(&self, secret: &str) -> Result<()>;
    fn delete(&self) -> Result<()>;
}

/// The OS key store (`keyring` crate): Keychain on macOS, Credential Manager
/// on Windows, the kernel keyring on Linux (kept until reboot).
pub struct KeyringStore {
    service: &'static str,
    account: &'static str,
}

impl KeyringStore {
    /// The Hosted-mode Anthropic key. A name of its own: nothing else in the
    /// app has ever written it.
    pub fn hosted() -> Self {
        Self { service: "dev.localfirst.roughcut.hosted", account: "anthropic" }
    }

    fn entry(&self) -> Result<keyring::Entry> {
        keyring::Entry::new(self.service, self.account).map_err(unavailable)
    }
}

fn unavailable(e: keyring::Error) -> CoreError {
    CoreError::Unavailable(format!(
        "this system's secure key store isn't available ({e}); Hosted mode needs one to keep the API key"
    ))
}

impl SecretStore for KeyringStore {
    fn get(&self) -> Result<Option<String>> {
        match self.entry()?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn set(&self, secret: &str) -> Result<()> {
        self.entry()?.set_password(secret).map_err(unavailable)
    }

    fn delete(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(unavailable(e)),
        }
    }
}

/// In memory, for tests.
#[derive(Default)]
pub struct MemoryStore(Mutex<Option<String>>);

impl SecretStore for MemoryStore {
    fn get(&self) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn set(&self, secret: &str) -> Result<()> {
        *self.0.lock().unwrap() = Some(secret.to_string());
        Ok(())
    }
    fn delete(&self) -> Result<()> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

/// The old "frontier" path wrote keys to `<data dir>/secrets/roughcut-*` in
/// plain text when the Keychain refused them. That path is gone; so are the
/// files. Best effort: a file that can't be removed is left as it was.
pub fn remove_legacy_plaintext() {
    let dir = crate::store::data_dir().join("secrets");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with("roughcut-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let _ = std::fs::remove_dir(&dir); // only if now empty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let s = MemoryStore::default();
        assert_eq!(s.get().unwrap(), None);
        s.set("sk-test").unwrap();
        assert_eq!(s.get().unwrap().as_deref(), Some("sk-test"));
        s.delete().unwrap();
        assert_eq!(s.get().unwrap(), None);
        s.delete().unwrap(); // deleting nothing is fine
    }

    /// Touches the real OS key store (and may prompt): run by hand with
    /// `cargo test -p roughcut-core keyring_round_trip -- --ignored`.
    #[test]
    #[ignore]
    fn keyring_round_trip() {
        let s = KeyringStore { service: "dev.localfirst.roughcut.test", account: "round-trip" };
        s.set("sk-test-value").unwrap();
        assert_eq!(s.get().unwrap().as_deref(), Some("sk-test-value"));
        s.delete().unwrap();
        assert_eq!(s.get().unwrap(), None);
    }
}
