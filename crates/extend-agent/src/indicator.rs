//! Durable local banner choices. A choice takes effect offline and is sent when a pair connects.
//! A delayed response cannot replace a newer choice, and another service never inherits it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use extend_protocol::model::InUseIndicator;
use serde::{Deserialize, Serialize};

use crate::config::{Config, write_private_file};

pub(crate) const COMPUTER: &str = "computer";

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Choice {
    pub value: InUseIndicator,
    pub pending: bool,
    pub revision: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Saved {
    service_url: String,
    choices: BTreeMap<String, Choice>,
}

pub(crate) struct Preferences {
    path: PathBuf,
    saved: Saved,
}

impl Preferences {
    pub fn load(config: &Config) -> Self {
        let path = config.state_dir.join("indicators.json");
        let saved = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
            .filter(|s| s.service_url == config.service_url.as_str())
            .unwrap_or_else(|| Saved {
                service_url: config.service_url.to_string(),
                ..Saved::default()
            });
        Self { path, saved }
    }

    pub fn get(&self, key: &str) -> Choice {
        self.saved.choices.get(key).copied().unwrap_or_default()
    }

    fn save(&self, saved: &Saved) -> Result<()> {
        write_private_file(&self.path, &serde_json::to_vec_pretty(saved)?)
    }

    /// Persist first: a successful UI action always survives an immediate process exit.
    pub fn choose(&mut self, key: &str, value: InUseIndicator) -> Result<()> {
        self.choose_many(&[key], value)
    }

    pub fn choose_many(&mut self, keys: &[&str], value: InUseIndicator) -> Result<()> {
        let mut next = self.saved.clone();
        for key in keys {
            next.choices.insert(
                (*key).into(),
                Choice {
                    value,
                    pending: true,
                    revision: self.get(key).revision + 1,
                },
            );
        }
        self.save(&next)?;
        self.saved = next;
        Ok(())
    }

    pub fn pending(&self) -> Vec<(String, Choice)> {
        self.saved
            .choices
            .iter()
            .filter(|(_, c)| c.pending)
            .map(|(k, c)| (k.clone(), *c))
            .collect()
    }

    /// Cache a service read only if nothing changed since that read began.
    pub fn observe(&mut self, key: &str, value: InUseIndicator, revision: u64) -> InUseIndicator {
        let current = self.get(key);
        if !current.pending && current.revision == revision && current.value != value {
            self.saved.choices.insert(
                key.into(),
                Choice {
                    value,
                    revision: revision + 1,
                    pending: false,
                },
            );
            if let Err(e) = self.save(&self.saved) {
                tracing::warn!("couldn't cache the in-use banner setting: {e:#}");
            }
        }
        self.get(key).value
    }

    pub fn acknowledge(&mut self, key: &str, revision: u64, value: InUseIndicator) -> Result<bool> {
        let current = self.get(key);
        if !current.pending || current.revision != revision {
            return Ok(false);
        }
        let mut next = self.saved.clone();
        next.choices.insert(
            key.into(),
            Choice {
                value,
                pending: false,
                revision: revision + 1,
            },
        );
        self.save(&next)?;
        self.saved = next;
        Ok(true)
    }

    pub fn remove(&mut self, key: &str) {
        if self.saved.choices.remove(key).is_some()
            && let Err(e) = self.save(&self.saved)
        {
            tracing::warn!("couldn't forget the removed device's banner setting: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_choices_survive_restart_and_delayed_replies_but_not_a_different_service() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::for_tests(dir.path(), "http://127.0.0.1:8480");
        let mut p = Preferences::load(&config);
        p.choose(COMPUTER, InUseIndicator::Hidden).unwrap();
        p.choose("aabbccdd", InUseIndicator::Hidden).unwrap();
        let mut p = Preferences::load(&config);
        assert_eq!(p.pending().len(), 2);
        let old = p.get(COMPUTER).revision;
        assert_eq!(p.observe(COMPUTER, InUseIndicator::Shown, old), InUseIndicator::Hidden);
        p.choose(COMPUTER, InUseIndicator::Shown).unwrap();
        assert!(!p.acknowledge(COMPUTER, old, InUseIndicator::Hidden).unwrap());
        let revision = p.get(COMPUTER).revision;
        assert!(p.acknowledge(COMPUTER, revision, InUseIndicator::Shown).unwrap());
        assert_eq!(
            p.observe(COMPUTER, InUseIndicator::Hidden, revision),
            InUseIndicator::Shown
        );
        assert_eq!(Preferences::load(&config).pending().len(), 1);
        p.remove("aabbccdd");
        assert!(Preferences::load(&config).pending().is_empty());
        p.choose(COMPUTER, InUseIndicator::Hidden).unwrap();
        let other = Config::for_tests(dir.path(), "http://127.0.0.1:8481");
        assert_eq!(Preferences::load(&other).get(COMPUTER).value, InUseIndicator::Shown);
    }
}
