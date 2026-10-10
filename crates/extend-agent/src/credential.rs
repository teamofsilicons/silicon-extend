//! Device credentials: the secrets a paired computer holds, one per pair (one per Carbon who
//! paired it; "Pair with another Carbon" adds one).
//!
//! They live in the OS secret store (macOS Keychain, Windows Credential Manager, the Secret Service
//! on Linux) under the service name `Silicon Extend`: one entry per pair, with the account
//! `{service_url}#{device_id}`, plus an index entry (`{service_url}#pairs`) listing the pairs.
//! When there is no secret store (a headless Linux server), they fall back to
//! `{state}/credential.json`, a JSON array readable only by this user.
//!
//! 1.0 kept one credential: a single entry (account `device-credential@{host}`) or a single JSON
//! object in `credential.json`. Both are read and moved to the new layout on first load.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use extend_protocol::DeviceId;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::{Config, CredentialStoreKind, write_private_file};

pub const KEYRING_SERVICE: &str = "Silicon Extend";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
    pub device_id: DeviceId,
    pub device_credential: String,
    /// The service that issued it; a credential for another service is ignored.
    pub service_url: String,
    /// The credential this one replaced when the service rotated it, kept until a connection
    /// with the new one succeeds: if the service never took the new one (its `credential_saved`
    /// was lost, or the service was rolled back), the app falls back to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_credential: Option<String>,
    /// Whether this pair was made by the app's first enrollment (the Carbon who installed Silicon
    /// Extend here) rather than "Pair with another Carbon". `GET /api/v1/device` says so from 1.1;
    /// kept here so the terminal rule for shared computers holds before that answer arrives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_pair: Option<bool>,
}

impl StoredCredential {
    pub fn new(device_id: DeviceId, device_credential: String, service_url: &Url) -> Self {
        Self {
            device_id,
            device_credential,
            service_url: service_url.to_string(),
            previous_credential: None,
            first_pair: None,
        }
    }
}

// Never print a credential, even in a debug log.
impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCredential")
            .field("device_id", &self.device_id)
            .field("service_url", &self.service_url)
            .field("previous_credential", &self.previous_credential.as_ref().map(|_| "…"))
            .field("first_pair", &self.first_pair)
            .finish_non_exhaustive()
    }
}

pub trait CredentialStore: Send + Sync {
    /// Every stored pair, oldest first.
    fn load_all(&self) -> Result<Vec<StoredCredential>>;
    /// Adds a pair, or replaces the one with the same device id.
    fn save(&self, credential: &StoredCredential) -> Result<()>;
    /// Forgets one pair (its Carbon revoked or removed it).
    fn remove(&self, device_id: &DeviceId) -> Result<()>;
    /// Forgets every pair.
    fn clear(&self) -> Result<()>;
    /// Where the credentials are kept, for `status`.
    fn describe(&self) -> String;

    /// The first stored pair (1.0 callers, and `extend-agent stop`).
    fn load(&self) -> Result<Option<StoredCredential>> {
        Ok(self.load_all()?.into_iter().next())
    }

    /// Stores a rotated credential for `device_id`, keeping the one it replaces as the fallback.
    /// Returns the updated record.
    fn replace(&self, device_id: &DeviceId, new_credential: &str) -> Result<StoredCredential> {
        let mut c = self
            .load_all()?
            .into_iter()
            .find(|c| &c.device_id == device_id)
            .with_context(|| format!("no stored credential for {device_id}"))?;
        if c.device_credential != new_credential {
            c.previous_credential = Some(std::mem::replace(&mut c.device_credential, new_credential.to_owned()));
        }
        self.save(&c)?;
        Ok(c)
    }
}

/// Adds or replaces `c` in `list`, keeping the order pairs were made in.
fn upsert(list: &mut Vec<StoredCredential>, c: &StoredCredential) {
    match list.iter_mut().find(|x| x.device_id == c.device_id) {
        Some(x) => *x = c.clone(),
        None => list.push(c.clone()),
    }
}

/// A 0600 JSON file: an array of pairs (1.0 wrote a single object, still read).
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn write(&self, list: &[StoredCredential]) -> Result<()> {
        if list.is_empty() {
            return self.clear();
        }
        write_private_file(&self.path, &serde_json::to_vec_pretty(list)?)
    }
}

/// `credential.json`, in either layout.
#[derive(Deserialize)]
#[serde(untagged)]
enum FileLayout {
    Pairs(Vec<StoredCredential>),
    One(StoredCredential),
}

impl CredentialStore for FileStore {
    fn load_all(&self) -> Result<Vec<StoredCredential>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let layout: FileLayout =
                    serde_json::from_slice(&bytes).with_context(|| format!("{} is damaged", self.path.display()))?;
                Ok(match layout {
                    FileLayout::Pairs(list) => list,
                    FileLayout::One(c) => {
                        // 1.0's single object: rewritten as a list the first time it's read.
                        let list = vec![c];
                        if let Err(e) = self.write(&list) {
                            tracing::warn!("couldn't move the stored credential to the 1.1 layout: {e:#}");
                        }
                        list
                    }
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e).with_context(|| format!("couldn't read {}", self.path.display())),
        }
    }
    fn save(&self, credential: &StoredCredential) -> Result<()> {
        let mut list = self.load_all()?;
        upsert(&mut list, credential);
        self.write(&list)
    }
    fn remove(&self, device_id: &DeviceId) -> Result<()> {
        let mut list = self.load_all()?;
        list.retain(|c| &c.device_id != device_id);
        self.write(&list)
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

/// The OS secret store: an entry per pair plus an index of the pairs.
pub struct KeyringStore {
    service_url: String,
    /// 1.0's single entry, moved to the new layout on first load.
    legacy_account: String,
}

impl KeyringStore {
    pub fn new(service_url: &Url) -> Self {
        let host = service_url.host_str().unwrap_or("localhost");
        let legacy_account = match service_url.port() {
            Some(p) => format!("device-credential@{host}:{p}"),
            None => format!("device-credential@{host}"),
        };
        Self {
            service_url: service_url.to_string(),
            legacy_account,
        }
    }
    fn index_account(&self) -> String {
        format!("{}#pairs", self.service_url)
    }
    fn pair_account(&self, device_id: &DeviceId) -> String {
        format!("{}#{device_id}", self.service_url)
    }
    fn entry(account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, account).map_err(|e| anyhow::anyhow!("OS secret store unavailable: {e}"))
    }
    fn read(account: &str) -> Result<Option<String>> {
        match Self::entry(account)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("couldn't read the OS secret store: {e}")),
        }
    }
    fn write(account: &str, value: &str) -> Result<()> {
        Self::entry(account)?
            .set_password(value)
            .map_err(|e| anyhow::anyhow!("couldn't write to the OS secret store: {e}"))
    }
    fn delete(account: &str) -> Result<()> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!(
                "couldn't remove a credential from the OS secret store: {e}"
            )),
        }
    }
    fn index(&self) -> Result<Option<Vec<DeviceId>>> {
        match Self::read(&self.index_account())? {
            None => Ok(None),
            Some(json) => Ok(Some(
                serde_json::from_str(&json).context("the stored list of pairs is damaged")?,
            )),
        }
    }
    fn set_index(&self, ids: &[DeviceId]) -> Result<()> {
        Self::write(&self.index_account(), &serde_json::to_string(ids)?)
    }
    /// Moves 1.0's single entry to an entry of its own plus the index, then removes it (a copy
    /// left behind would outlive a later revoke).
    fn migrate_legacy(&self) -> Result<Vec<StoredCredential>> {
        let Some(json) = Self::read(&self.legacy_account)? else {
            return Ok(vec![]);
        };
        let c: StoredCredential = serde_json::from_str(&json).context("the stored credential is damaged")?;
        Self::write(&self.pair_account(&c.device_id), &serde_json::to_string(&c)?)?;
        self.set_index(std::slice::from_ref(&c.device_id))?;
        Self::delete(&self.legacy_account)?;
        tracing::info!("moved the stored credential of {} to the 1.1 layout", c.device_id);
        Ok(vec![c])
    }
}

impl CredentialStore for KeyringStore {
    fn load_all(&self) -> Result<Vec<StoredCredential>> {
        let Some(ids) = self.index()? else {
            return self.migrate_legacy();
        };
        let mut out = Vec::new();
        for id in ids {
            match Self::read(&self.pair_account(&id))? {
                Some(json) => out.push(
                    serde_json::from_str(&json).with_context(|| format!("the stored credential of {id} is damaged"))?,
                ),
                None => tracing::warn!("the list of pairs names {id}, but its credential is gone"),
            }
        }
        Ok(out)
    }
    fn save(&self, credential: &StoredCredential) -> Result<()> {
        Self::write(
            &self.pair_account(&credential.device_id),
            &serde_json::to_string(credential)?,
        )?;
        let mut ids = match self.index()? {
            Some(ids) => ids,
            None => self.migrate_legacy()?.into_iter().map(|c| c.device_id).collect(),
        };
        if !ids.contains(&credential.device_id) {
            ids.push(credential.device_id.clone());
        }
        self.set_index(&ids)
    }
    fn remove(&self, device_id: &DeviceId) -> Result<()> {
        Self::delete(&self.pair_account(device_id))?;
        if let Some(mut ids) = self.index()? {
            ids.retain(|i| i != device_id);
            self.set_index(&ids)?;
        }
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        let ids = self.index()?.unwrap_or_default();
        for id in &ids {
            Self::delete(&self.pair_account(id))?;
        }
        Self::delete(&self.index_account())?;
        Self::delete(&self.legacy_account)
    }
    fn describe(&self) -> String {
        format!("OS secret store ({KEYRING_SERVICE} / {}#…)", self.service_url)
    }
}

/// The secret store first, the file when the secret store fails. A pair lives in one of the two.
pub struct AutoStore {
    keyring: KeyringStore,
    file: FileStore,
}

impl CredentialStore for AutoStore {
    fn load_all(&self) -> Result<Vec<StoredCredential>> {
        let mut list = match self.keyring.load_all() {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!("{e:#}; using the credential file instead");
                vec![]
            }
        };
        for c in self.file.load_all()? {
            if !list.iter().any(|x| x.device_id == c.device_id) {
                list.push(c);
            }
        }
        Ok(list)
    }
    fn save(&self, credential: &StoredCredential) -> Result<()> {
        match self.keyring.save(credential) {
            Ok(()) => {
                // A stale fallback copy would outlive a later revoke; drop it.
                let _ = self.file.remove(&credential.device_id);
                Ok(())
            }
            Err(e) => {
                tracing::warn!("{e:#}; keeping the credential in a private file instead");
                self.file.save(credential)
            }
        }
    }
    fn remove(&self, device_id: &DeviceId) -> Result<()> {
        let a = self.keyring.remove(device_id);
        let b = self.file.remove(device_id);
        a.and(b)
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
        CredentialStoreKind::Auto => Arc::new(AutoStore {
            keyring: KeyringStore::new(&config.service_url),
            file,
        }),
    }
}

/// The pairs for this service, ignoring credentials issued by a different service.
pub fn load_for(store: &dyn CredentialStore, service_url: &Url) -> Result<Vec<StoredCredential>> {
    Ok(store
        .load_all()?
        .into_iter()
        .filter(|c| c.service_url == service_url.as_str())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(id: &str, secret: &str) -> StoredCredential {
        StoredCredential::new(
            id.parse().unwrap(),
            secret.into(),
            &Url::parse("http://127.0.0.1:8480/").unwrap(),
        )
    }

    #[test]
    fn file_store_keeps_one_entry_per_pair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credential.json");
        let store = FileStore::new(path.clone());
        assert!(store.load_all().unwrap().is_empty());
        let a = cred("7c1e09ab", "edc_a");
        let b = cred("0d44e1f2", "edc_b");
        store.save(&a).unwrap();
        store.save(&b).unwrap();
        assert_eq!(store.load_all().unwrap(), vec![a.clone(), b.clone()]);
        let url = Url::parse("http://127.0.0.1:8480/").unwrap();
        assert_eq!(load_for(&store, &url).unwrap().len(), 2);
        let other = Url::parse("https://api.extend.teamofsilicons.com/").unwrap();
        assert!(load_for(&store, &other).unwrap().is_empty());

        // Rotation keeps the old secret as the fallback, in place.
        let rotated = store.replace(&a.device_id, "edc_a2").unwrap();
        assert_eq!(rotated.device_credential, "edc_a2");
        assert_eq!(rotated.previous_credential.as_deref(), Some("edc_a"));
        assert_eq!(store.load_all().unwrap()[0], rotated);
        assert!(store.replace(&"00000000".parse().unwrap(), "x").is_err());

        store.remove(&a.device_id).unwrap();
        assert_eq!(store.load_all().unwrap(), vec![b.clone()]);
        // The last pair gone: no file left behind.
        store.remove(&b.device_id).unwrap();
        assert!(!path.exists());
        store.clear().unwrap();
    }

    #[test]
    fn a_1_0_credential_file_is_read_and_rewritten_as_a_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credential.json");
        std::fs::write(
            &path,
            r#"{"device_id":"7c1e09ab","device_credential":"edc_x","service_url":"http://127.0.0.1:8480/"}"#,
        )
        .unwrap();
        let store = FileStore::new(path.clone());
        let list = store.load_all().unwrap();
        assert_eq!(list, vec![cred("7c1e09ab", "edc_x")]);
        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(raw.is_array(), "{raw}");
        assert_eq!(store.load().unwrap(), Some(cred("7c1e09ab", "edc_x")));
    }

    #[test]
    fn debug_output_never_shows_a_secret() {
        let mut c = cred("7c1e09ab", "edc_secretsecret");
        c.previous_credential = Some("edc_oldsecret".into());
        let shown = format!("{c:?}");
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("7c1e09ab"));
    }

    #[test]
    fn keyring_accounts_are_per_service_and_pair() {
        let a = KeyringStore::new(&Url::parse("http://127.0.0.1:8480/").unwrap());
        let b = KeyringStore::new(&Url::parse("https://api.extend.teamofsilicons.com/").unwrap());
        assert_eq!(a.legacy_account, "device-credential@127.0.0.1:8480");
        assert_eq!(b.legacy_account, "device-credential@api.extend.teamofsilicons.com");
        assert_eq!(a.index_account(), "http://127.0.0.1:8480/#pairs");
        assert_eq!(
            b.pair_account(&"7c1e09ab".parse().unwrap()),
            "https://api.extend.teamofsilicons.com/#7c1e09ab"
        );
    }
}
