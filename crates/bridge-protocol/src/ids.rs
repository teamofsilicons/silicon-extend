//! Identifiers Bridge mints, with their exact formats (TECHNICAL.md section 1).

use std::fmt;
use std::str::FromStr;

use base64::Engine as _;
use rand::Rng as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} must be {expected}, got {got:?}")]
pub struct IdError {
    pub kind: &'static str,
    pub expected: &'static str,
    pub got: String,
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

macro_rules! string_id {
    ($name:ident) => {
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                raw.parse().map_err(serde::de::Error::custom)
            }
        }
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

/// A paired device: 8 lowercase hexadecimal characters, random, never reused in its world.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(String);
string_id!(DeviceId);

impl DeviceId {
    pub fn random() -> Self {
        let n: u32 = rand::rng().random();
        Self(format!("{n:08x}"))
    }
}

impl FromStr for DeviceId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, IdError> {
        if s.len() == 8 && is_lower_hex(s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(IdError { kind: "device id", expected: "8 lowercase hexadecimal characters, like 7c1e09ab", got: s.to_owned() })
        }
    }
}

/// A session: lowercase hexadecimal, 3 characters to start, longer once shorter ids run out.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(String);
string_id!(SessionId);

impl SessionId {
    /// Builds the id with a given numeric value at a given length.
    pub fn from_parts(value: u64, len: usize) -> Self {
        Self(format!("{value:0len$x}"))
    }
    /// Number of distinct ids at a given length.
    pub fn space(len: usize) -> u64 {
        16u64.saturating_pow(len as u32)
    }
}

impl FromStr for SessionId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, IdError> {
        if s.len() >= 3 && s.len() <= 16 && is_lower_hex(s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(IdError { kind: "session id", expected: "3 or more lowercase hexadecimal characters, like a3f", got: s.to_owned() })
        }
    }
}

/// A pairing code: 6 hexadecimal characters, stored and shown uppercase, accepted in any case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PairingCode(String);
string_id!(PairingCode);

impl PairingCode {
    pub fn random() -> Self {
        let n: u32 = rand::rng().random_range(0..0x0100_0000);
        Self(format!("{n:06X}"))
    }
}

impl FromStr for PairingCode {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, IdError> {
        let trimmed: String = s.trim().chars().filter(|c| *c != '-' && *c != ' ').collect();
        if trimmed.len() == 6 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Self(trimmed.to_ascii_uppercase()))
        } else {
            Err(IdError { kind: "pairing code", expected: "6 hexadecimal characters, like 4F9C2A", got: s.to_owned() })
        }
    }
}

/// Generates an opaque secret: prefix + 32 random bytes as unpadded base64url (43 characters).
pub fn new_secret(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    format!("{prefix}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// True when `s` is `prefix` followed by exactly 43 base64url characters.
pub fn is_secret(prefix: &str, s: &str) -> bool {
    s.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == 43 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// SHA-256 of a secret as lowercase hex; the only form in which secrets are stored.
pub fn secret_digest(s: &str) -> String {
    hex_lower(&Sha256::digest(s.as_bytes()))
}

pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    out
}

pub const ENROLLMENT_SECRET_PREFIX: &str = "bes_";
pub const DEVICE_CREDENTIAL_PREFIX: &str = "bdc_";
pub const APP_SECRET_PREFIX: &str = "ask_";

/// A Carbon's public id starts `c:`, a Silicon's `si:`.
pub fn member_kind(id: &str) -> Option<crate::model::MemberKind> {
    if id.starts_with("c:") && id.len() > 2 {
        Some(crate::model::MemberKind::Carbon)
    } else if id.starts_with("si:") && id.len() > 3 {
        Some(crate::model::MemberKind::Silicon)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ids() {
        assert!("7c1e09ab".parse::<DeviceId>().is_ok());
        assert!("7C1E09AB".parse::<DeviceId>().is_err());
        assert!("7c1e09a".parse::<DeviceId>().is_err());
        for _ in 0..100 {
            let id = DeviceId::random();
            assert_eq!(id.as_str().parse::<DeviceId>().unwrap(), id);
        }
    }

    #[test]
    fn session_ids_grow() {
        assert_eq!(SessionId::from_parts(0xa3f, 3).as_str(), "a3f");
        assert_eq!(SessionId::from_parts(5, 3).as_str(), "005");
        assert_eq!(SessionId::from_parts(5, 4).as_str(), "0005");
        assert_eq!(SessionId::space(3), 4096);
        assert!("ab".parse::<SessionId>().is_err());
        assert!("abcd".parse::<SessionId>().is_ok());
    }

    #[test]
    fn pairing_codes_are_case_insensitive() {
        let a: PairingCode = "4f9c2a".parse().unwrap();
        let b: PairingCode = "4F9C2A".parse().unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "4F9C2A");
        assert_eq!("4F9-C2A".parse::<PairingCode>().unwrap(), b);
        assert!("4F9C2G".parse::<PairingCode>().is_err());
        for _ in 0..100 {
            assert_eq!(PairingCode::random().as_str().len(), 6);
        }
    }

    #[test]
    fn secrets() {
        let s = new_secret(DEVICE_CREDENTIAL_PREFIX);
        assert!(is_secret(DEVICE_CREDENTIAL_PREFIX, &s), "{s}");
        assert!(!is_secret(ENROLLMENT_SECRET_PREFIX, &s));
        assert_eq!(secret_digest(&s).len(), 64);
    }
}
