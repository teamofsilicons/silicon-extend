//! HomeKit Accessory Protocol pairing, as the Apple TV's Companion and AirPlay services use it.
//!
//! - **Pair-Setup** (once, with the PIN the Apple TV shows): SRP-6a proves both sides know the PIN;
//!   then each side sends its long-term Ed25519 public key, signed, encrypted under a key derived
//!   from the SRP session key. The result is [`Credentials`], kept on disk.
//! - **Pair-Verify** (every connection): an X25519 exchange, each side signing both ephemeral keys
//!   with its long-term key. The shared secret feeds HKDF for the session's ChaCha20-Poly1305 keys.
//!
//! Message bodies are TLV8. Transport (Companion frames or AirPlay HTTP) is up to the caller.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha512;
use x25519_dalek::{PublicKey, StaticSecret};

use super::srp;

// ───────────── TLV8 ─────────────

pub(crate) mod tag {
    pub const METHOD: u8 = 0x00;
    pub const IDENTIFIER: u8 = 0x01;
    pub const SALT: u8 = 0x02;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const PROOF: u8 = 0x04;
    pub const ENCRYPTED_DATA: u8 = 0x05;
    pub const SEQ_NO: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    pub const BACKOFF: u8 = 0x08;
    pub const SIGNATURE: u8 = 0x0A;
    /// Apple extension: OPACK dictionary with the device's name and model.
    pub const INFO: u8 = 0x11;
    pub const FLAGS: u8 = 0x13;
}

/// Encodes TLV8, splitting values longer than 255 bytes into consecutive items with the same tag.
pub(crate) fn tlv_encode(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (t, v) in items {
        if v.is_empty() {
            out.extend([*t, 0]);
            continue;
        }
        for chunk in v.chunks(255) {
            out.push(*t);
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
    }
    out
}

/// Decoded TLV8; fragments of one value are joined.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Tlv(pub Vec<(u8, Vec<u8>)>);

impl Tlv {
    pub fn get(&self, t: u8) -> Option<&[u8]> {
        self.0
            .iter()
            .find(|(k, _)| *k == t)
            .map(|(_, v)| v.as_slice())
    }

    pub fn require(&self, t: u8, what: &str) -> Result<&[u8], PairingError> {
        self.get(t)
            .ok_or_else(|| PairingError::Protocol(format!("the Apple TV's reply had no {what}")))
    }

    /// The HAP error in this message, if any.
    pub fn error(&self) -> Option<PairingError> {
        let code = *self.get(tag::ERROR)?.first()?;
        let backoff = self
            .get(tag::BACKOFF)
            .map(|b| b.iter().rev().fold(0u64, |a, x| (a << 8) | *x as u64));
        Some(match code {
            0x02 => PairingError::WrongCode,
            0x03 => PairingError::Busy(format!(
                "too many attempts; try again in {} s",
                backoff.unwrap_or(30)
            )),
            0x04 => PairingError::Busy("the Apple TV has too many paired controllers".into()),
            0x05 => {
                PairingError::Busy("too many wrong codes; restart pairing on the Apple TV".into())
            }
            0x06 => {
                PairingError::Busy("the Apple TV is already pairing with something else".into())
            }
            0x07 => PairingError::Busy("the Apple TV is busy; try again".into()),
            other => PairingError::Protocol(format!("pairing error {other:#04x}")),
        })
    }
}

pub(crate) fn tlv_decode(data: &[u8]) -> Result<Tlv, PairingError> {
    let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut i = 0;
    let mut last_tag = None;
    while i < data.len() {
        if i + 2 > data.len() {
            return Err(PairingError::Protocol("truncated TLV".into()));
        }
        let (t, len) = (data[i], data[i + 1] as usize);
        let v = data
            .get(i + 2..i + 2 + len)
            .ok_or_else(|| PairingError::Protocol("truncated TLV value".into()))?;
        match (last_tag, items.last_mut()) {
            (Some(prev), Some(last)) if prev == t => last.1.extend_from_slice(v),
            _ => items.push((t, v.to_vec())),
        }
        last_tag = Some(t);
        i += 2 + len;
    }
    Ok(Tlv(items))
}

// ───────────── Errors ─────────────

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PairingError {
    /// The PIN didn't match (or the proofs disagreed).
    WrongCode,
    /// The Apple TV refused for now (back-off, busy, too many controllers).
    Busy(String),
    /// Our stored credentials were rejected: the Apple TV no longer knows us.
    NotPaired,
    Protocol(String),
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongCode => f.write_str("the code didn't match"),
            Self::Busy(m) | Self::Protocol(m) => f.write_str(m),
            Self::NotPaired => {
                f.write_str("the Apple TV no longer recognises this Mac; pair it again")
            }
        }
    }
}

// ───────────── Crypto helpers ─────────────

/// HKDF-SHA512 with string salt and info, 32-byte output.
pub(crate) fn hkdf(salt: &str, info: &str, ikm: &[u8]) -> [u8; 32] {
    let h = Hkdf::<Sha512>::new(Some(salt.as_bytes()), ikm);
    let mut out = [0u8; 32];
    h.expand(info.as_bytes(), &mut out)
        .expect("32 bytes is a valid HKDF length");
    out
}

/// A pairing-message nonce: four zero bytes, then the eight-byte label (`PS-Msg05`).
fn label_nonce(label: &[u8; 8]) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(label);
    n
}

pub(crate) fn seal(key: &[u8; 32], nonce: [u8; 12], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("ChaCha20-Poly1305 encryption can't fail for in-memory data")
}

pub(crate) fn open(
    key: &[u8; 32],
    nonce: [u8; 12],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, PairingError> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| PairingError::Protocol("couldn't decrypt the Apple TV's message".into()))
}

/// How a session cipher turns its message counter into a nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NonceStyle {
    /// Companion: the counter as 12 little-endian bytes.
    Counter12,
    /// HAP sessions (AirPlay): four zero bytes, then the counter as 8 little-endian bytes.
    Counter8,
}

/// ChaCha20-Poly1305 with separate keys and counters per direction.
pub(crate) struct SessionCipher {
    out_key: [u8; 32],
    in_key: [u8; 32],
    out_n: u64,
    in_n: u64,
    style: NonceStyle,
}

impl SessionCipher {
    pub fn new(out_key: [u8; 32], in_key: [u8; 32], style: NonceStyle) -> Self {
        Self {
            out_key,
            in_key,
            out_n: 0,
            in_n: 0,
            style,
        }
    }

    fn nonce(&self, n: u64) -> [u8; 12] {
        let mut out = [0u8; 12];
        match self.style {
            NonceStyle::Counter12 => out[..8].copy_from_slice(&n.to_le_bytes()),
            NonceStyle::Counter8 => out[4..].copy_from_slice(&n.to_le_bytes()),
        }
        out
    }

    pub fn encrypt(&mut self, plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
        let n = self.nonce(self.out_n);
        self.out_n += 1;
        seal(&self.out_key, n, plaintext, aad)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, PairingError> {
        let n = self.nonce(self.in_n);
        self.in_n += 1;
        open(&self.in_key, n, ciphertext, aad)
    }
}

// ───────────── Credentials ─────────────

/// What Pair-Setup leaves behind. Serialised as hex; `to_string` gives pyatv's
/// `ltpk:ltsk:atv_id:client_id` form, which pyatv's `atvremote --companion-credentials` accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Credentials {
    /// The Apple TV's long-term Ed25519 public key.
    #[serde(with = "hex_bytes")]
    pub ltpk: Vec<u8>,
    /// Our long-term Ed25519 secret key (seed).
    #[serde(with = "hex_bytes")]
    pub ltsk: Vec<u8>,
    /// The Apple TV's pairing identifier.
    #[serde(with = "hex_bytes")]
    pub atv_id: Vec<u8>,
    /// Our pairing identifier (a UUID string's bytes).
    #[serde(with = "hex_bytes")]
    pub client_id: Vec<u8>,
}

impl std::fmt::Display for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}",
            hex::encode(&self.ltpk),
            hex::encode(&self.ltsk),
            hex::encode(&self.atv_id),
            hex::encode(&self.client_id)
        )
    }
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        hex::decode(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Credentials {
    fn signing_key(&self) -> Result<SigningKey, PairingError> {
        let seed: [u8; 32] = self
            .ltsk
            .as_slice()
            .try_into()
            .map_err(|_| PairingError::Protocol("stored key is damaged".into()))?;
        Ok(SigningKey::from_bytes(&seed))
    }
}

// ───────────── Pair-Setup ─────────────

/// Client side of Pair-Setup (M1 … M6).
pub(crate) struct PairSetup {
    signing: SigningKey,
    pairing_id: Vec<u8>,
    salt: Vec<u8>,
    server_public: Vec<u8>,
    session: Option<srp::Session>,
    /// A transient pairing (AirPlay without a PIN) stops after M4.
    pub transient: bool,
}

impl PairSetup {
    /// `pairing_id` should stay the same for this Mac across pairings (a UUID string).
    pub fn new(pairing_id: &str) -> Self {
        Self {
            signing: SigningKey::from_bytes(&rand::random::<[u8; 32]>()),
            pairing_id: pairing_id.as_bytes().to_vec(),
            salt: vec![],
            server_public: vec![],
            session: None,
            transient: false,
        }
    }

    pub fn m1(&self) -> Vec<u8> {
        if self.transient {
            tlv_encode(&[
                (tag::METHOD, &[0]),
                (tag::SEQ_NO, &[1]),
                (tag::FLAGS, &[0x10]),
            ])
        } else {
            tlv_encode(&[(tag::METHOD, &[0]), (tag::SEQ_NO, &[1])])
        }
    }

    /// Takes the salt and SRP public key from M2 (the Apple TV now shows its PIN).
    pub fn handle_m2(&mut self, body: &[u8]) -> Result<(), PairingError> {
        let t = tlv_decode(body)?;
        if let Some(e) = t.error() {
            return Err(e);
        }
        self.salt = t.require(tag::SALT, "salt")?.to_vec();
        self.server_public = t.require(tag::PUBLIC_KEY, "public key")?.to_vec();
        Ok(())
    }

    /// M3: our SRP public key and proof for this PIN.
    pub fn m3(&mut self, pin: &str) -> Result<Vec<u8>, PairingError> {
        if self.server_public.is_empty() {
            return Err(PairingError::Protocol("pairing hasn't started".into()));
        }
        let client = srp::Client::random(srp::group_3072());
        let session = client
            .process::<Sha512>(
                b"Pair-Setup",
                pin.trim().as_bytes(),
                &self.salt,
                &self.server_public,
            )
            .map_err(PairingError::Protocol)?;
        let msg = tlv_encode(&[
            (tag::SEQ_NO, &[3]),
            (tag::PUBLIC_KEY, &client.public()),
            (tag::PROOF, &session.m1),
        ]);
        self.session = Some(session);
        Ok(msg)
    }

    /// Checks M4: the Apple TV's proof that it knows the PIN too.
    pub fn handle_m4(&self, body: &[u8]) -> Result<(), PairingError> {
        let t = tlv_decode(body)?;
        if let Some(e) = t.error() {
            return Err(e);
        }
        let proof = t.require(tag::PROOF, "proof")?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| PairingError::Protocol("no SRP session".into()))?;
        if proof != session.m2.as_slice() {
            return Err(PairingError::WrongCode);
        }
        Ok(())
    }

    /// The SRP session key K (transient pairings derive their session keys from it).
    pub fn session_key(&self) -> Option<&[u8]> {
        self.session.as_ref().map(|s| s.key.as_slice())
    }

    /// M5: our long-term public key, identifier and signature, encrypted. `name` is shown in the
    /// Apple TV's list of paired devices.
    pub fn m5(&self, name: Option<&str>) -> Result<Vec<u8>, PairingError> {
        let k = self
            .session_key()
            .ok_or_else(|| PairingError::Protocol("no SRP session".into()))?;
        let controller_x = hkdf(
            "Pair-Setup-Controller-Sign-Salt",
            "Pair-Setup-Controller-Sign-Info",
            k,
        );
        let enc_key = hkdf("Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info", k);
        let public = self.signing.verifying_key().to_bytes();
        let signature = self
            .signing
            .sign(&[&controller_x[..], &self.pairing_id, &public].concat())
            .to_bytes();
        let info = name.map(|n| {
            super::opack::encode(&super::opack::Value::dict([(
                "name",
                super::opack::Value::str(n),
            )]))
        });
        let mut items: Vec<(u8, &[u8])> = vec![
            (tag::IDENTIFIER, &self.pairing_id),
            (tag::PUBLIC_KEY, &public),
            (tag::SIGNATURE, &signature),
        ];
        if let Some(i) = &info {
            items.push((tag::INFO, i));
        }
        let sealed = seal(&enc_key, label_nonce(b"PS-Msg05"), &tlv_encode(&items), &[]);
        Ok(tlv_encode(&[
            (tag::SEQ_NO, &[5]),
            (tag::ENCRYPTED_DATA, &sealed),
        ]))
    }

    /// M6: the Apple TV's long-term key and identifier, whose signature we check. Returns the
    /// credentials and the Apple TV's OPACK info (name, model) when it sent one.
    pub fn handle_m6(
        &self,
        body: &[u8],
    ) -> Result<(Credentials, Option<super::opack::Value>), PairingError> {
        let t = tlv_decode(body)?;
        if let Some(e) = t.error() {
            return Err(e);
        }
        let k = self
            .session_key()
            .ok_or_else(|| PairingError::Protocol("no SRP session".into()))?;
        let enc_key = hkdf("Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info", k);
        let inner = tlv_decode(&open(
            &enc_key,
            label_nonce(b"PS-Msg06"),
            t.require(tag::ENCRYPTED_DATA, "encrypted data")?,
            &[],
        )?)?;
        let atv_id = inner.require(tag::IDENTIFIER, "identifier")?.to_vec();
        let ltpk = inner.require(tag::PUBLIC_KEY, "public key")?.to_vec();
        let signature = inner.require(tag::SIGNATURE, "signature")?;
        let accessory_x = hkdf(
            "Pair-Setup-Accessory-Sign-Salt",
            "Pair-Setup-Accessory-Sign-Info",
            k,
        );
        verify(
            &ltpk,
            &[&accessory_x[..], &atv_id, &ltpk].concat(),
            signature,
        )?;
        let info = inner
            .get(tag::INFO)
            .and_then(|i| super::opack::decode(i).ok());
        Ok((
            Credentials {
                ltpk,
                ltsk: self.signing.to_bytes().to_vec(),
                atv_id,
                client_id: self.pairing_id.clone(),
            },
            info,
        ))
    }
}

fn verify(public: &[u8], message: &[u8], signature: &[u8]) -> Result<(), PairingError> {
    let key: [u8; 32] = public
        .try_into()
        .map_err(|_| PairingError::Protocol("bad public key length".into()))?;
    let sig: [u8; 64] = signature
        .try_into()
        .map_err(|_| PairingError::Protocol("bad signature length".into()))?;
    VerifyingKey::from_bytes(&key)
        .map_err(|_| PairingError::Protocol("invalid public key".into()))?
        .verify(message, &Signature::from_bytes(&sig))
        .map_err(|_| PairingError::Protocol("the Apple TV's signature didn't verify".into()))
}

// ───────────── Pair-Verify ─────────────

/// Client side of Pair-Verify (M1 … M4).
pub(crate) struct PairVerify {
    secret: StaticSecret,
    public: PublicKey,
    shared: Option<[u8; 32]>,
}

impl Default for PairVerify {
    fn default() -> Self {
        let secret = StaticSecret::from(rand::random::<[u8; 32]>());
        let public = PublicKey::from(&secret);
        Self {
            secret,
            public,
            shared: None,
        }
    }
}

impl PairVerify {
    pub fn m1(&self) -> Vec<u8> {
        tlv_encode(&[
            (tag::SEQ_NO, &[1]),
            (tag::PUBLIC_KEY, self.public.as_bytes()),
        ])
    }

    /// Checks the Apple TV's M2 against the stored credentials and returns M3.
    pub fn handle_m2(&mut self, creds: &Credentials, body: &[u8]) -> Result<Vec<u8>, PairingError> {
        let t = tlv_decode(body)?;
        if let Some(e) = t.error() {
            return Err(if e == PairingError::WrongCode {
                PairingError::NotPaired
            } else {
                e
            });
        }
        let server_public: [u8; 32] = t
            .require(tag::PUBLIC_KEY, "public key")?
            .try_into()
            .map_err(|_| PairingError::Protocol("bad key length".into()))?;
        let shared = self
            .secret
            .diffie_hellman(&PublicKey::from(server_public))
            .to_bytes();
        let key = hkdf(
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
            &shared,
        );
        let inner = tlv_decode(&open(
            &key,
            label_nonce(b"PV-Msg02"),
            t.require(tag::ENCRYPTED_DATA, "encrypted data")?,
            &[],
        )?)?;
        let id = inner.require(tag::IDENTIFIER, "identifier")?;
        if id != creds.atv_id.as_slice() {
            return Err(PairingError::NotPaired);
        }
        let signature = inner.require(tag::SIGNATURE, "signature")?;
        verify(
            &creds.ltpk,
            &[&server_public[..], id, self.public.as_bytes()].concat(),
            signature,
        )
        .map_err(|_| PairingError::NotPaired)?;
        let ours = creds
            .signing_key()?
            .sign(
                &[
                    self.public.as_bytes(),
                    &creds.client_id[..],
                    &server_public[..],
                ]
                .concat(),
            )
            .to_bytes();
        let sealed = seal(
            &key,
            label_nonce(b"PV-Msg03"),
            &tlv_encode(&[(tag::IDENTIFIER, &creds.client_id), (tag::SIGNATURE, &ours)]),
            &[],
        );
        self.shared = Some(shared);
        Ok(tlv_encode(&[
            (tag::SEQ_NO, &[3]),
            (tag::ENCRYPTED_DATA, &sealed),
        ]))
    }

    /// Checks M4 (an error there means the Apple TV rejected our signature).
    pub fn handle_m4(&self, body: &[u8]) -> Result<(), PairingError> {
        match tlv_decode(body)?.error() {
            Some(PairingError::WrongCode) => Err(PairingError::NotPaired),
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Session keys (output, input) from the shared secret.
    pub fn keys(&self, salt: &str, out_info: &str, in_info: &str) -> Option<([u8; 32], [u8; 32])> {
        let s = self.shared?;
        Some((hkdf(salt, out_info, &s), hkdf(salt, in_info, &s)))
    }
}

// ───────────── Accessory side, for the mock Apple TV ─────────────

#[cfg(test)]
pub(crate) mod accessory {
    use super::*;

    /// A HomeKit accessory's pairing state machine (what an Apple TV runs).
    pub(crate) struct Accessory {
        pub id: Vec<u8>,
        pub signing: SigningKey,
        pub pin: String,
        pub info: Option<Vec<u8>>,
        srp: Option<srp::Server>,
        key: Option<Vec<u8>>,
        /// Controllers that completed Pair-Setup: identifier → long-term public key.
        pub paired: Vec<(Vec<u8>, Vec<u8>)>,
        verify_secret: Option<StaticSecret>,
        verify_shared: Option<[u8; 32]>,
        verify_client_public: Option<[u8; 32]>,
    }

    impl Accessory {
        pub fn new(pin: &str) -> Self {
            Self {
                id: b"AA:BB:CC:DD:EE:FF".to_vec(),
                signing: SigningKey::from_bytes(&[7u8; 32]),
                pin: pin.into(),
                info: Some(super::super::opack::encode(
                    &super::super::opack::Value::dict([
                        ("name", super::super::opack::Value::str("Living Room")),
                        ("model", super::super::opack::Value::str("AppleTV14,1")),
                    ]),
                )),
                srp: None,
                key: None,
                paired: vec![],
                verify_secret: None,
                verify_shared: None,
                verify_client_public: None,
            }
        }

        /// Handles one Pair-Setup message and returns the reply.
        pub fn setup(&mut self, body: &[u8]) -> Vec<u8> {
            let t = tlv_decode(body).unwrap();
            match t.get(tag::SEQ_NO).map(|s| s[0]) {
                Some(1) => {
                    let salt = rand::random::<[u8; 16]>();
                    let server = srp::Server::new::<Sha512>(
                        srp::group_3072(),
                        b"Pair-Setup",
                        self.pin.as_bytes(),
                        &salt,
                        &rand::random::<[u8; 32]>(),
                    );
                    let b = srp::bytes(&server.b_pub);
                    self.srp = Some(server);
                    tlv_encode(&[
                        (tag::SEQ_NO, &[2]),
                        (tag::SALT, &salt),
                        (tag::PUBLIC_KEY, &b),
                    ])
                }
                Some(3) => {
                    let server = self.srp.as_ref().unwrap();
                    let (key, m1, m2) = server.process::<Sha512>(t.get(tag::PUBLIC_KEY).unwrap());
                    if t.get(tag::PROOF).unwrap() != m1.as_slice() {
                        return tlv_encode(&[(tag::SEQ_NO, &[4]), (tag::ERROR, &[2])]);
                    }
                    self.key = Some(key);
                    tlv_encode(&[(tag::SEQ_NO, &[4]), (tag::PROOF, &m2)])
                }
                Some(5) => {
                    let k = self.key.clone().unwrap();
                    let enc = hkdf("Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info", &k);
                    let inner = tlv_decode(
                        &open(
                            &enc,
                            label_nonce(b"PS-Msg05"),
                            t.get(tag::ENCRYPTED_DATA).unwrap(),
                            &[],
                        )
                        .unwrap(),
                    )
                    .unwrap();
                    let (cid, cpk, sig) = (
                        inner.get(tag::IDENTIFIER).unwrap(),
                        inner.get(tag::PUBLIC_KEY).unwrap(),
                        inner.get(tag::SIGNATURE).unwrap(),
                    );
                    let x = hkdf(
                        "Pair-Setup-Controller-Sign-Salt",
                        "Pair-Setup-Controller-Sign-Info",
                        &k,
                    );
                    verify(cpk, &[&x[..], cid, cpk].concat(), sig).expect("controller signature");
                    self.paired.push((cid.to_vec(), cpk.to_vec()));
                    let ax = hkdf(
                        "Pair-Setup-Accessory-Sign-Salt",
                        "Pair-Setup-Accessory-Sign-Info",
                        &k,
                    );
                    let pk = self.signing.verifying_key().to_bytes();
                    let s = self
                        .signing
                        .sign(&[&ax[..], &self.id, &pk].concat())
                        .to_bytes();
                    let mut items: Vec<(u8, &[u8])> = vec![
                        (tag::IDENTIFIER, &self.id),
                        (tag::PUBLIC_KEY, &pk),
                        (tag::SIGNATURE, &s),
                    ];
                    if let Some(i) = &self.info {
                        items.push((tag::INFO, i));
                    }
                    let sealed = seal(&enc, label_nonce(b"PS-Msg06"), &tlv_encode(&items), &[]);
                    tlv_encode(&[(tag::SEQ_NO, &[6]), (tag::ENCRYPTED_DATA, &sealed)])
                }
                _ => tlv_encode(&[(tag::ERROR, &[1])]),
            }
        }

        /// Handles one Pair-Verify message and returns the reply.
        pub fn verify(&mut self, body: &[u8]) -> Vec<u8> {
            let t = tlv_decode(body).unwrap();
            match t.get(tag::SEQ_NO).map(|s| s[0]) {
                Some(1) => {
                    let client: [u8; 32] = t.get(tag::PUBLIC_KEY).unwrap().try_into().unwrap();
                    let secret = StaticSecret::from(rand::random::<[u8; 32]>());
                    let public = PublicKey::from(&secret);
                    let shared = secret.diffie_hellman(&PublicKey::from(client)).to_bytes();
                    let key = hkdf(
                        "Pair-Verify-Encrypt-Salt",
                        "Pair-Verify-Encrypt-Info",
                        &shared,
                    );
                    let s = self
                        .signing
                        .sign(&[public.as_bytes(), &self.id[..], &client[..]].concat())
                        .to_bytes();
                    let sealed = seal(
                        &key,
                        label_nonce(b"PV-Msg02"),
                        &tlv_encode(&[(tag::IDENTIFIER, &self.id), (tag::SIGNATURE, &s)]),
                        &[],
                    );
                    self.verify_secret = Some(secret);
                    self.verify_shared = Some(shared);
                    self.verify_client_public = Some(client);
                    tlv_encode(&[
                        (tag::SEQ_NO, &[2]),
                        (tag::PUBLIC_KEY, public.as_bytes()),
                        (tag::ENCRYPTED_DATA, &sealed),
                    ])
                }
                Some(3) => {
                    let shared = self.verify_shared.unwrap();
                    let key = hkdf(
                        "Pair-Verify-Encrypt-Salt",
                        "Pair-Verify-Encrypt-Info",
                        &shared,
                    );
                    let inner = tlv_decode(
                        &open(
                            &key,
                            label_nonce(b"PV-Msg03"),
                            t.get(tag::ENCRYPTED_DATA).unwrap(),
                            &[],
                        )
                        .unwrap(),
                    )
                    .unwrap();
                    let cid = inner.get(tag::IDENTIFIER).unwrap();
                    let Some((_, cpk)) = self.paired.iter().find(|(id, _)| id == cid) else {
                        return tlv_encode(&[(tag::SEQ_NO, &[4]), (tag::ERROR, &[2])]);
                    };
                    let public = PublicKey::from(self.verify_secret.as_ref().unwrap());
                    let msg = [
                        &self.verify_client_public.unwrap()[..],
                        cid,
                        public.as_bytes(),
                    ]
                    .concat();
                    if verify(cpk, &msg, inner.get(tag::SIGNATURE).unwrap()).is_err() {
                        return tlv_encode(&[(tag::SEQ_NO, &[4]), (tag::ERROR, &[2])]);
                    }
                    tlv_encode(&[(tag::SEQ_NO, &[4])])
                }
                _ => tlv_encode(&[(tag::ERROR, &[1])]),
            }
        }

        /// The SRP session key once M3 checked out (transient pairings derive keys from it).
        pub fn session_key(&self) -> Option<&[u8]> {
            self.key.as_deref()
        }

        /// Session keys from the accessory's point of view (its output is the controller's input).
        pub fn keys(&self, salt: &str, out_info: &str, in_info: &str) -> ([u8; 32], [u8; 32]) {
            let s = self.verify_shared.unwrap();
            (hkdf(salt, out_info, &s), hkdf(salt, in_info, &s))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::accessory::Accessory;
    use super::*;

    fn vectors() -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/hap-vectors.json");
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    #[test]
    fn tlv_matches_pyatv() {
        let v = vectors();
        assert_eq!(
            hex::encode(tlv_encode(&[(6, &[1]), (0, &[0])])),
            v["tlv_short"].as_str().unwrap()
        );
        let long: Vec<u8> = (0..=255u8).chain(0..=255u8).collect();
        let enc = tlv_encode(&[(3, &long), (6, &[3])]);
        assert_eq!(hex::encode(&enc), v["tlv_long"].as_str().unwrap());
        let d = tlv_decode(&enc).unwrap();
        assert_eq!(d.get(3).unwrap(), long.as_slice());
        assert_eq!(d.get(6).unwrap(), &[3]);
        assert!(tlv_decode(&[6, 5, 1]).is_err());
        assert_eq!(
            tlv_decode(&tlv_encode(&[(7, &[2])])).unwrap().error(),
            Some(PairingError::WrongCode)
        );
    }

    #[test]
    fn hkdf_and_chacha_match_pyatv() {
        let v = vectors();
        let secret: Vec<u8> = (0..32).collect();
        for (salt, info) in [
            ("Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info"),
            ("Pair-Verify-Encrypt-Salt", "Pair-Verify-Encrypt-Info"),
            ("", "ClientEncrypt-main"),
            ("Control-Salt", "Control-Write-Encryption-Key"),
        ] {
            assert_eq!(
                hex::encode(hkdf(salt, info, &secret)),
                v[format!("hkdf_{salt}|{info}")].as_str().unwrap(),
                "{salt}/{info}"
            );
        }
        let key: [u8; 32] = secret.clone().try_into().unwrap();
        assert_eq!(
            hex::encode(seal(&key, label_nonce(b"PS-Msg05"), b"hello pairing", &[])),
            v["chacha_ps_msg05"].as_str().unwrap()
        );
        let mut c12 = SessionCipher::new(key, key, NonceStyle::Counter12);
        assert_eq!(
            hex::encode(c12.encrypt(b"frame0", &[8, 0, 0, 0x16])),
            v["chacha_counter12_0"].as_str().unwrap()
        );
        assert_eq!(
            hex::encode(c12.encrypt(b"frame1", &[8, 0, 0, 0x16])),
            v["chacha_counter12_1"].as_str().unwrap()
        );
        let mut c8 = SessionCipher::new(key, key, NonceStyle::Counter8);
        assert_eq!(
            hex::encode(c8.encrypt(b"GET / RTSP/1.0", &[14, 0])),
            v["chacha_counter8_0"].as_str().unwrap()
        );
        assert_eq!(
            hex::encode(c8.encrypt(b"x", &[1, 0])),
            v["chacha_counter8_1"].as_str().unwrap()
        );
        // Decrypt what we encrypted (in key == out key here).
        let mut d = SessionCipher::new(key, key, NonceStyle::Counter8);
        let ct = hex::decode(v["chacha_counter8_0"].as_str().unwrap()).unwrap();
        assert_eq!(d.decrypt(&ct, &[14, 0]).unwrap(), b"GET / RTSP/1.0");
    }

    #[test]
    fn full_pairing_against_an_accessory() {
        let mut atv = Accessory::new("4321");
        let mut setup = PairSetup::new("5D3B1A4C-0000-4000-8000-000000000001");
        setup.handle_m2(&atv.setup(&setup.m1())).unwrap();
        let m3 = setup.m3("4321").unwrap();
        setup.handle_m4(&atv.setup(&m3)).unwrap();
        let (creds, info) = setup
            .handle_m6(&atv.setup(&setup.m5(Some("Silicon Bridge")).unwrap()))
            .unwrap();
        assert_eq!(creds.atv_id, b"AA:BB:CC:DD:EE:FF");
        assert_eq!(
            info.unwrap().get("model").unwrap().as_str(),
            Some("AppleTV14,1")
        );
        assert_eq!(creds.to_string().split(':').count(), 4);

        let mut pv = PairVerify::default();
        let m3 = pv.handle_m2(&creds, &atv.verify(&pv.m1())).unwrap();
        pv.handle_m4(&atv.verify(&m3)).unwrap();
        let (out, inp) = pv
            .keys("", "ClientEncrypt-main", "ServerEncrypt-main")
            .unwrap();
        let (a_out, a_in) = atv.keys("", "ServerEncrypt-main", "ClientEncrypt-main");
        assert_eq!((out, inp), (a_in, a_out));

        // Stored credentials survive a round trip through JSON.
        let json = serde_json::to_string(&creds).unwrap();
        assert_eq!(serde_json::from_str::<Credentials>(&json).unwrap(), creds);
    }

    #[test]
    fn wrong_pin_and_unknown_controller() {
        let mut atv = Accessory::new("4321");
        let mut setup = PairSetup::new("id-1");
        setup.handle_m2(&atv.setup(&setup.m1())).unwrap();
        let m3 = setup.m3("1111").unwrap();
        assert_eq!(
            setup.handle_m4(&atv.setup(&m3)),
            Err(PairingError::WrongCode)
        );

        // Credentials the accessory never saw fail verification.
        let creds = Credentials {
            ltpk: atv.signing.verifying_key().to_bytes().to_vec(),
            ltsk: vec![9; 32],
            atv_id: atv.id.clone(),
            client_id: b"stranger".to_vec(),
        };
        let mut pv = PairVerify::default();
        let m3 = pv.handle_m2(&creds, &atv.verify(&pv.m1())).unwrap();
        assert_eq!(pv.handle_m4(&atv.verify(&m3)), Err(PairingError::NotPaired));
    }
}
