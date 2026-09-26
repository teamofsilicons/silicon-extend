//! The device credential: the one secret a paired computer holds.
//!
//! It lives in the OS secret store (macOS Keychain, Windows Credential Manager, the Secret Service
//! on Linux) under the service name `Silicon Extend`, one entry per Extend service. When there is
//! no secret store (a headless Linux server), it falls back to `{state}/credential.json`, readable
//! only by this user.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use extend_protocol::DeviceId;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::{Config, CredentialStoreKind, write_private_file};

pub const KEYRING_SERVICE: &str = "Silicon Extend";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
    pub device_id: DeviceId,
    pub device_credential: String,
    /// The service that issued it; a credential for another service is ignored.
    pub service_url: String,
}

pub trait CredentialStore: Send + Sync {
    fn load(&self) -> Result<Option<StoredCredential>>;
    fn save(&self, credential: &StoredCredential) -> Result<()>;
    fn clear(&self) -> Result<()>;
    /// Where the credential is kept, for `status`.
    fn describe(&self) -> String;
}

/// A 0600 JSON file.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl CredentialStore for FileStore {
    fn load(&self) -> Result<Option<StoredCredential>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| format!("{} is damaged", self.path.display()))?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("couldn't read {}", self.path.display())),
        }
    }
    fn save(&self, credential: &StoredCredential) -> Result<()> {
        write_private_file(&self.path, &serde_json::to_vec_pretty(credential)?)
    }
    fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("couldn't remove {}", self.path.display())),
        }
    }
    fn describe(&self) -> String {
        format!("file {}", self.path.display())
    }
}

/// The OS secret store.
pub struct KeyringStore {
    account: String,
}

impl KeyringStore {
    pub fn new(service_url: &Url) -> Self {
        let host = service_url.host_str().unwrap_or("localhost");
        let account = match service_url.port() {
            Some(p) => format!("device-credential@{host}:{p}"),
            None => format!("device-credential@{host}"),
        };
        Self { account }
    }
    fn entry(&self) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, &self.account).map_err(|e| anyhow::anyhow!("OS secret store unavailable: {e}"))
    }
}

impl CredentialStore for KeyringStore {
    fn load(&self) -> Result<Option<StoredCredential>> {
        match self.entry()?.get_password() {
            Ok(json) => Ok(Some(serde_json::from_str(&json).context("the stored credential is damaged")?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("couldn't read the OS secret store: {e}")),
        }
    }
    fn save(&self, credential: &StoredCredential) -> Result<()> {
        self.entry()?
            .set_password(&serde_json::to_string(credential)?)
            .map_err(|e| anyhow::anyhow!("couldn't write to the OS secret store: {e}"))
    }
    fn clear(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("couldn't remove the credential from the OS secret store: {e}")),
        }
    }
    fn describe(&self) -> String {
        format!("OS secret store ({KEYRING_SERVICE} / {})", self.account)
    }
}

/// The secret store first, the file when the secret store fails.
pub struct AutoStore {
    keyring: KeyringStore,
    file: FileStore,
}

impl CredentialStore for AutoStore {
    fn load(&self) -> Result<Option<StoredCredential>> {
        match self.keyring.load() {
            Ok(Some(c)) => Ok(Some(c)),
            Ok(None) => self.file.load(),
            Err(e) => {
                tracing::warn!("{e:#}; using the credential file instead");
                self.file.load()
            }
        }
    }
    fn save(&self, credential: &StoredCredential) -> Result<()> {
        match self.keyring.save(credential) {
            Ok(()) => {
                // A stale fallback copy would outlive a later revoke; drop it.
                let _ = self.file.clear();
                Ok(())
            }
            Err(e) => {
                tracing::warn!("{e:#}; keeping the credential in a private file instead");
                self.file.save(credential)
            }
        }
    }
    fn clear(&self) -> Result<()> {
        let a = self.keyring.clear();
        let b = self.file.clear();
        a.and(b)
    }
    fn describe(&self) -> String {
        format!("{} (falls back to {})", self.keyring.describe(), self.file.describe())
    }
}

/// Builds the store the config asks for.
pub fn store_for(config: &Config) -> Arc<dyn CredentialStore> {
    let file = FileStore::new(config.state_dir.join("credential.json"));
    match config.credential_store {
        CredentialStoreKind::File => Arc::new(file),
        CredentialStoreKind::Keyring => Arc::new(KeyringStore::new(&config.service_url)),
        CredentialStoreKind::Auto => Arc::new(AutoStore { keyring: KeyringStore::new(&config.service_url), file }),
    }
}

/// Loads the credential for this service, ignoring one issued by a different service.
pub fn load_for(store: &dyn CredentialStore, service_url: &Url) -> Result<Option<StoredCredential>> {
    Ok(store.load()?.filter(|c| c.service_url == service_url.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("credential.json"));
        assert_eq!(store.load().unwrap(), None);
        let c = StoredCredential {
            device_id: "7c1e09ab".parse().unwrap(),
            device_credential: "edc_x".into(),
            service_url: "http://127.0.0.1:8480/".into(),
        };
        store.save(&c).unwrap();
        assert_eq!(store.load().unwrap(), Some(c.clone()));
        let url = Url::parse("http://127.0.0.1:8480/").unwrap();
        assert_eq!(load_for(&store, &url).unwrap(), Some(c));
        let other = Url::parse("https://backend.extend.teamofsilicons.com/").unwrap();
        assert_eq!(load_for(&store, &other).unwrap(), None);
        store.clear().unwrap();
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn keyring_accounts_are_per_service() {
        let a = KeyringStore::new(&Url::parse("http://127.0.0.1:8480/").unwrap());
        let b = KeyringStore::new(&Url::parse("https://backend.extend.teamofsilicons.com/").unwrap());
        assert_eq!(a.account, "device-credential@127.0.0.1:8480");
        assert_eq!(b.account, "device-credential@backend.extend.teamofsilicons.com");
    }
}
