//! SRP-6a (RFC 5054) as HomeKit pairing uses it: the 3072-bit group, SHA-512, user name
//! `Pair-Setup` and the PIN the Apple TV shows as the password.
//!
//! Byte conventions follow `srptools`, which pyatv uses against real Apple TVs: `k = H(N | PAD(g))`,
//! `u = H(PAD(A) | PAD(B))`, `x = H(s | H(I ":" P))`, `K = H(S)`,
//! `M1 = H(H(N) xor H(g) | H(I) | s | A | B | K)`, `M2 = H(A | M1 | K)`, where every number is
//! written as its minimal big-endian bytes unless padded to the length of N. The client picks `a`
//! so that `A` has no leading zero byte, which makes the minimal and padded forms of `A` agree.

use num_bigint::BigUint;
use sha2::Digest;

/// An SRP group: prime modulus and generator.
#[derive(Debug, Clone)]
pub(crate) struct Group {
    pub n: BigUint,
    pub g: BigUint,
}

const PRIME_3072: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DD",
    "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA0510",
    "15728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B18177B200C",
    "BBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF",
);

/// RFC 5054's 3072-bit group (generator 5), the one HAP uses.
pub(crate) fn group_3072() -> Group {
    Group {
        n: BigUint::parse_bytes(PRIME_3072.as_bytes(), 16).expect("valid prime"),
        g: BigUint::from(5u8),
    }
}

/// RFC 5054's 1024-bit group (generator 2), for its published test vector.
#[cfg(test)]
pub(crate) fn group_1024() -> Group {
    let hex = concat!(
        "EEAF0AB9ADB38DD69C33F80AFA8FC5E86072618775FF3C0B9EA2314C9C256576D674DF7496EA81D3383B4813D692C6E0",
        "E0D5D8E250B98BE48E495C1D6089DAD15DC7D7B46154D6B6CE8EF4AD69B15D4982559B297BCF1885C529F566660E57EC",
        "68EDBC3C05726CC02FD4CBF4976EAA9AFD5138FE8376435B9FC61D2FC0EB06E3",
    );
    Group {
        n: BigUint::parse_bytes(hex.as_bytes(), 16).unwrap(),
        g: BigUint::from(2u8),
    }
}

/// Minimal big-endian bytes (zero is one zero byte, like srptools).
pub(crate) fn bytes(x: &BigUint) -> Vec<u8> {
    x.to_bytes_be()
}

impl Group {
    fn len(&self) -> usize {
        bytes(&self.n).len()
    }

    pub fn pad(&self, x: &BigUint) -> Vec<u8> {
        let b = bytes(x);
        let mut out = vec![0u8; self.len().saturating_sub(b.len())];
        out.extend(b);
        out
    }
}

fn hash<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
    let mut d = D::new();
    for p in parts {
        d.update(p);
    }
    d.finalize().to_vec()
}

fn hash_int<D: Digest>(parts: &[&[u8]]) -> BigUint {
    BigUint::from_bytes_be(&hash::<D>(parts))
}

/// Values both sides derive; exposed for the published test vectors.
pub(crate) struct Derived {
    pub k: BigUint,
    pub x: BigUint,
    pub u: BigUint,
}

pub(crate) fn derive<D: Digest>(
    group: &Group,
    user: &[u8],
    password: &[u8],
    salt: &[u8],
    a_pub: &BigUint,
    b_pub: &BigUint,
) -> Derived {
    let k = hash_int::<D>(&[&bytes(&group.n), &group.pad(&group.g)]);
    let inner = hash::<D>(&[user, b":", password]);
    let x = hash_int::<D>(&[&bytes(&BigUint::from_bytes_be(salt)), &inner]);
    let u = hash_int::<D>(&[&group.pad(a_pub), &group.pad(b_pub)]);
    Derived { k, x, u }
}

pub(crate) fn proof_m1<D: Digest>(
    group: &Group,
    user: &[u8],
    salt: &[u8],
    a_pub: &BigUint,
    b_pub: &BigUint,
    key: &[u8],
) -> Vec<u8> {
    let hn = hash_int::<D>(&[&bytes(&group.n)]);
    let hg = hash_int::<D>(&[&bytes(&group.g)]);
    let hi = hash_int::<D>(&[user]);
    hash::<D>(&[
        &bytes(&(hn ^ hg)),
        &bytes(&hi),
        &bytes(&BigUint::from_bytes_be(salt)),
        &bytes(a_pub),
        &bytes(b_pub),
        key,
    ])
}

pub(crate) fn proof_m2<D: Digest>(a_pub: &BigUint, m1: &[u8], key: &[u8]) -> Vec<u8> {
    hash::<D>(&[&bytes(a_pub), m1, key])
}

/// The client side of one SRP exchange.
pub(crate) struct Client {
    group: Group,
    a: BigUint,
    pub a_pub: BigUint,
}

/// What the client learns once it has the server's salt and public key.
pub(crate) struct Session {
    /// K, the shared session key (64 bytes with SHA-512).
    pub key: Vec<u8>,
    /// M1, the client's proof.
    pub m1: Vec<u8>,
    /// M2, the proof the server must send back.
    pub m2: Vec<u8>,
}

impl Client {
    /// `private` is the random exponent `a` (32 random bytes in practice).
    pub fn new(group: Group, private: &[u8]) -> Self {
        let a = BigUint::from_bytes_be(private);
        let a_pub = group.g.modpow(&a, &group.n);
        Self { group, a, a_pub }
    }

    /// A fresh client whose public key has no leading zero byte.
    pub fn random(group: Group) -> Self {
        loop {
            let c = Self::new(group.clone(), &rand::random::<[u8; 32]>());
            if bytes(&c.a_pub).len() == c.group.len() {
                return c;
            }
        }
    }

    pub fn public(&self) -> Vec<u8> {
        bytes(&self.a_pub)
    }

    pub fn process<D: Digest>(
        &self,
        user: &[u8],
        password: &[u8],
        salt: &[u8],
        server_public: &[u8],
    ) -> Result<Session, String> {
        let n = &self.group.n;
        let b_pub = BigUint::from_bytes_be(server_public);
        if (&b_pub % n) == BigUint::ZERO {
            return Err("the Apple TV sent an invalid SRP public key".into());
        }
        let d = derive::<D>(&self.group, user, password, salt, &self.a_pub, &b_pub);
        if d.u == BigUint::ZERO {
            return Err("SRP scrambling parameter is zero".into());
        }
        let v = self.group.g.modpow(&d.x, n);
        let kv = (&d.k * &v) % n;
        let base = ((&b_pub % n) + n - kv) % n;
        let s = base.modpow(&(&self.a + &d.u * &d.x), n);
        let key = hash::<D>(&[&bytes(&s)]);
        let m1 = proof_m1::<D>(&self.group, user, salt, &self.a_pub, &b_pub, &key);
        let m2 = proof_m2::<D>(&self.a_pub, &m1, &key);
        Ok(Session { key, m1, m2 })
    }
}

/// The server side, for the mock Apple TV in tests.
#[cfg(test)]
pub(crate) struct Server {
    group: Group,
    b: BigUint,
    pub b_pub: BigUint,
    v: BigUint,
    user: Vec<u8>,
    pub salt: Vec<u8>,
}

#[cfg(test)]
impl Server {
    pub fn new<D: Digest>(group: Group, user: &[u8], password: &[u8], salt: &[u8], private: &[u8]) -> Self {
        let inner = hash::<D>(&[user, b":", password]);
        let x = hash_int::<D>(&[&bytes(&BigUint::from_bytes_be(salt)), &inner]);
        let v = group.g.modpow(&x, &group.n);
        let k = hash_int::<D>(&[&bytes(&group.n), &group.pad(&group.g)]);
        let b = BigUint::from_bytes_be(private);
        let b_pub = (&k * &v + group.g.modpow(&b, &group.n)) % &group.n;
        Self {
            group,
            b,
            b_pub,
            v,
            user: user.to_vec(),
            salt: salt.to_vec(),
        }
    }

    /// Returns (K, expected M1, M2) for the client's public key.
    pub fn process<D: Digest>(&self, client_public: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let n = &self.group.n;
        let a_pub = BigUint::from_bytes_be(client_public);
        let u = hash_int::<D>(&[&self.group.pad(&a_pub), &self.group.pad(&self.b_pub)]);
        let s = (&a_pub * self.v.modpow(&u, n)).modpow(&self.b, n);
        let key = hash::<D>(&[&bytes(&s)]);
        let m1 = proof_m1::<D>(&self.group, &self.user, &self.salt, &a_pub, &self.b_pub, &key);
        let m2 = proof_m2::<D>(&a_pub, &m1, &key);
        (key, m1, m2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Sha512;

    fn vectors() -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hap-vectors.json");
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    fn big(hex: &str) -> BigUint {
        BigUint::parse_bytes(hex.as_bytes(), 16).unwrap()
    }

    /// RFC 5054 Appendix B: I = "alice", P = "password123", 1024-bit group, SHA-1.
    #[test]
    fn rfc5054_vector() {
        let v = vectors();
        let group = group_1024();
        let salt = hex::decode("BEB25379D1A8581EB5A727673A2441EE").unwrap();
        let a = hex::decode("60975527035CF2AD1989806F0407210BC81EDC04E2762A56AFD529DDDA2D4393").unwrap();
        let b = hex::decode("E487CB59D31AC550471E81F00F6928E01DDA08E974A004F49E61F5D105284D20").unwrap();
        let client = Client::new(group.clone(), &a);
        let server = Server::new::<sha1::Sha1>(group.clone(), b"alice", b"password123", &salt, &b);
        assert_eq!(client.a_pub, big(v["rfc_A"].as_str().unwrap()));
        assert_eq!(server.b_pub, big(v["rfc_B"].as_str().unwrap()));
        assert_eq!(server.v, big(v["rfc_v"].as_str().unwrap()));
        let d = derive::<sha1::Sha1>(&group, b"alice", b"password123", &salt, &client.a_pub, &server.b_pub);
        // The values printed in RFC 5054 Appendix B.
        assert_eq!(d.k, big("7556AA045AEF2CDD07ABAF0F665C3E818913186F"));
        assert_eq!(d.x, big("94B7555AABE9127CC58CCF4993DB6CF84D16C124"));
        assert_eq!(d.u, big("CE38B9593487DA98554ED47D70A7AE5F462EF019"));
        let s = client
            .process::<sha1::Sha1>(b"alice", b"password123", &salt, &bytes(&server.b_pub))
            .unwrap();
        let (skey, sm1, sm2) = server.process::<sha1::Sha1>(&client.public());
        assert_eq!(s.key, skey);
        assert_eq!((s.m1, s.m2), (sm1, sm2));
        // Premaster secret S from the RFC, through K = H(S).
        let expected_s = big(v["rfc_S"].as_str().unwrap());
        assert_eq!(s.key, hash::<sha1::Sha1>(&[&bytes(&expected_s)]));
    }

    /// HAP parameters against srptools (what pyatv pairs real Apple TVs with).
    #[test]
    fn hap_vector_matches_srptools() {
        let v = vectors();
        let salt = hex::decode("beb25379d1a8581eb5a727673a2441ee").unwrap();
        let a = hex::decode("60975527035cf2ad1989806f0407210bc81edc04e2762a56afd529ddda2d4393").unwrap();
        let b = hex::decode("e487cb59d31ac550471e81f00f6928e01dda08e974a004f49e61f5d105284d20").unwrap();
        let client = Client::new(group_3072(), &a);
        assert_eq!(hex::encode(client.public()), v["srp_A"].as_str().unwrap());
        let server = Server::new::<Sha512>(group_3072(), b"Pair-Setup", b"1234", &salt, &b);
        assert_eq!(hex::encode(bytes(&server.b_pub)), v["srp_B"].as_str().unwrap());
        let s = client
            .process::<Sha512>(
                b"Pair-Setup",
                b"1234",
                &salt,
                &hex::decode(v["srp_B"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
        assert_eq!(hex::encode(&s.key), v["srp_K"].as_str().unwrap());
        assert_eq!(hex::encode(&s.m1), v["srp_M1"].as_str().unwrap());
        assert_eq!(hex::encode(&s.m2), v["srp_M2"].as_str().unwrap());
        // A wrong PIN gives a proof the server rejects.
        let wrong = client
            .process::<Sha512>(b"Pair-Setup", b"9999", &salt, &bytes(&server.b_pub))
            .unwrap();
        assert_ne!(wrong.m1, s.m1);
    }

    #[test]
    fn random_clients_have_full_length_keys() {
        for _ in 0..8 {
            assert_eq!(Client::random(group_3072()).public().len(), 384);
        }
    }
}
