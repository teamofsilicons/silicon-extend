//! OPACK, Apple's compact binary serialisation used by the Companion protocol.
//!
//! Byte tags (as reverse-engineered by pyatv, whose encoder this matches byte for byte):
//!
//! | Tag | Value |
//! |---|---|
//! | `01` / `02` | true / false |
//! | `04` | null |
//! | `05` + 16 bytes | UUID |
//! | `06` + 8 bytes | absolute time (kept as its raw integer) |
//! | `08`–`2F` | integers 0–39 |
//! | `30`–`33` + 1/2/4/8 bytes LE | integers |
//! | `35` / `36` | f32 / f64 LE |
//! | `40`–`60` | UTF-8 string of length 0–32 |
//! | `61`–`64` + 1–4 byte LE length | longer strings |
//! | `70`–`90` | bytes of length 0–32 |
//! | `91`–`94` + 1/2/4/8 byte LE length | longer bytes |
//! | `D0`–`DE` | array of 0–14 items; `DF` … `03` open-ended |
//! | `E0`–`EE` | dictionary of 0–14 pairs; `EF` … `03` open-ended |
//! | `A0`–`C0`, `C1`–`C4` + 1–4 byte index | reference to an earlier value |
//!
//! When decoding, values that take more than one byte (strings, data, larger numbers, UUIDs) are
//! remembered in order, and a reference tag stands for the value at that index. The encoder never
//! writes references: they only save space, every decoder accepts the literal form, and pyatv's
//! encoder numbers containers differently from its decoder, so the index rules for mixed content
//! aren't settled.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Int(u64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Uuid([u8; 16]),
    /// `06`: Apple absolute time, kept as the raw little-endian integer.
    Date(u64),
    Array(Vec<Value>),
    Dict(Vec<(Value, Value)>),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(s.to_owned())
    }

    /// A dictionary with string keys, in the given order.
    pub fn dict<const N: usize>(pairs: [(&str, Value); N]) -> Value {
        Value::Dict(pairs.into_iter().map(|(k, v)| (Value::str(k), v)).collect())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(pairs) => pairs
                .iter()
                .find(|(k, _)| matches!(k, Value::Str(s) if s == key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(*b as u64),
            _ => None,
        }
    }

    /// Converts to JSON (bytes become hex), for test logs.
    #[cfg(test)]
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => json!(b),
            Value::Int(i) => json!(i),
            Value::Float(f) => json!(f),
            Value::Str(s) => json!(s),
            Value::Bytes(b) => json!(hex::encode(b)),
            Value::Uuid(u) => json!(hex::encode(u)),
            Value::Date(d) => json!(d),
            Value::Array(a) => serde_json::Value::Array(a.iter().map(Value::to_json).collect()),
            Value::Dict(d) => {
                let mut m = serde_json::Map::new();
                for (k, v) in d {
                    let key = match k {
                        Value::Str(s) => s.clone(),
                        other => other.to_json().to_string(),
                    };
                    m.insert(key, v.to_json());
                }
                serde_json::Value::Object(m)
            }
        }
    }
}

pub(crate) fn encode(v: &Value) -> Vec<u8> {
    pack(v)
}

fn pack(v: &Value) -> Vec<u8> {
    match v {
        Value::Null => vec![0x04],
        Value::Bool(true) => vec![0x01],
        Value::Bool(false) => vec![0x02],
        Value::Uuid(u) => {
            let mut b = vec![0x05];
            b.extend_from_slice(u);
            b
        }
        Value::Date(d) => {
            let mut b = vec![0x06];
            b.extend_from_slice(&d.to_le_bytes());
            b
        }
        Value::Int(i) => match *i {
            i if i < 0x28 => vec![i as u8 + 8],
            i if i <= 0xFF => vec![0x30, i as u8],
            i if i <= 0xFFFF => [&[0x31][..], &(i as u16).to_le_bytes()].concat(),
            i if i <= 0xFFFF_FFFF => [&[0x32][..], &(i as u32).to_le_bytes()].concat(),
            i => [&[0x33][..], &i.to_le_bytes()].concat(),
        },
        Value::Float(f) => [&[0x36][..], &f.to_le_bytes()].concat(),
        Value::Str(s) => sized(s.as_bytes(), 0x40, 0x20, 0x60),
        Value::Bytes(b) => sized(b, 0x70, 0x20, 0x90),
        Value::Array(items) => {
            let mut b = vec![0xD0 + items.len().min(0xF) as u8];
            for it in items {
                b.extend(pack(it));
            }
            if items.len() >= 0xF {
                b.push(0x03);
            }
            b
        }
        Value::Dict(pairs) => {
            let mut b = vec![0xE0 + pairs.len().min(0xF) as u8];
            for (k, val) in pairs {
                b.extend(pack(k));
                b.extend(pack(val));
            }
            if pairs.len() >= 0xF {
                b.push(0x03);
            }
            b
        }
    }
}

/// Short form `base + len` up to `short_max`, else `long_base + n` with an n-byte length.
fn sized(data: &[u8], base: u8, short_max: usize, long_base: u8) -> Vec<u8> {
    let n = data.len();
    let mut b = if n <= short_max {
        vec![base + n as u8]
    } else if n <= 0xFF {
        vec![long_base + 1, n as u8]
    } else if n <= 0xFFFF {
        [&[long_base + 2][..], &(n as u16).to_le_bytes()].concat()
    } else if base == 0x40 && n <= 0xFF_FFFF {
        // Strings have a 3-byte length form (0x63); data goes straight to 4 bytes (0x93).
        [&[long_base + 3][..], &(n as u32).to_le_bytes()[..3]].concat()
    } else if base == 0x40 {
        [&[long_base + 4][..], &(n as u32).to_le_bytes()].concat()
    } else if n <= 0xFFFF_FFFF {
        [&[long_base + 3][..], &(n as u32).to_le_bytes()].concat()
    } else {
        [&[long_base + 4][..], &(n as u64).to_le_bytes()].concat()
    };
    b.extend_from_slice(data);
    b
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OPACK: {}", self.0)
    }
}

pub(crate) fn decode(data: &[u8]) -> Result<Value, DecodeError> {
    let mut seen = Vec::new();
    let (v, rest) = unpack(data, &mut seen)?;
    if !rest.is_empty() {
        return Err(DecodeError(format!("{} trailing bytes", rest.len())));
    }
    Ok(v)
}

fn take(data: &[u8], n: usize) -> Result<(&[u8], &[u8]), DecodeError> {
    if data.len() < n {
        return Err(DecodeError(format!("needed {n} bytes, have {}", data.len())));
    }
    Ok(data.split_at(n))
}

fn le(bytes: &[u8]) -> u64 {
    bytes.iter().rev().fold(0u64, |acc, b| (acc << 8) | *b as u64)
}

fn unpack<'a>(data: &'a [u8], seen: &mut Vec<Value>) -> Result<(Value, &'a [u8]), DecodeError> {
    let (&tag, rest) = data.split_first().ok_or_else(|| DecodeError("unexpected end".into()))?;
    let mut remember = true;
    let (value, rest) = match tag {
        0x01 => {
            remember = false;
            (Value::Bool(true), rest)
        }
        0x02 => {
            remember = false;
            (Value::Bool(false), rest)
        }
        0x04 => {
            remember = false;
            (Value::Null, rest)
        }
        0x05 => {
            let (u, rest) = take(rest, 16)?;
            (Value::Uuid(u.try_into().expect("16 bytes")), rest)
        }
        0x06 => {
            let (d, rest) = take(rest, 8)?;
            (Value::Date(le(d)), rest)
        }
        0x08..=0x2F => {
            remember = false;
            (Value::Int((tag - 8) as u64), rest)
        }
        0x35 => {
            let (f, rest) = take(rest, 4)?;
            (
                Value::Float(f32::from_le_bytes(f.try_into().expect("4 bytes")) as f64),
                rest,
            )
        }
        0x36 => {
            let (f, rest) = take(rest, 8)?;
            (Value::Float(f64::from_le_bytes(f.try_into().expect("8 bytes"))), rest)
        }
        0x30..=0x33 => {
            let n = 1usize << (tag & 0x0F);
            let (i, rest) = take(rest, n)?;
            (Value::Int(le(i)), rest)
        }
        0x40..=0x60 => {
            let (s, rest) = take(rest, (tag - 0x40) as usize)?;
            (Value::Str(String::from_utf8_lossy(s).into_owned()), rest)
        }
        0x61..=0x64 => {
            let n = (tag & 0x0F) as usize;
            let (len, rest) = take(rest, n)?;
            let (s, rest) = take(rest, le(len) as usize)?;
            (Value::Str(String::from_utf8_lossy(s).into_owned()), rest)
        }
        0x70..=0x90 => {
            let (b, rest) = take(rest, (tag - 0x70) as usize)?;
            (Value::Bytes(b.to_vec()), rest)
        }
        0x91..=0x94 => {
            let n = 1usize << ((tag & 0x0F) - 1);
            let (len, rest) = take(rest, n)?;
            let (b, rest) = take(rest, le(len) as usize)?;
            (Value::Bytes(b.to_vec()), rest)
        }
        0xD0..=0xDF => {
            remember = false;
            let count = (tag & 0x0F) as usize;
            let mut items = Vec::new();
            let mut ptr = rest;
            if count == 0xF {
                loop {
                    match ptr.first() {
                        Some(0x03) => {
                            ptr = &ptr[1..];
                            break;
                        }
                        Some(_) => {
                            let (v, r) = unpack(ptr, seen)?;
                            items.push(v);
                            ptr = r;
                        }
                        None => return Err(DecodeError("unterminated array".into())),
                    }
                }
            } else {
                for _ in 0..count {
                    let (v, r) = unpack(ptr, seen)?;
                    items.push(v);
                    ptr = r;
                }
            }
            (Value::Array(items), ptr)
        }
        0xE0..=0xEF => {
            remember = false;
            let count = (tag & 0x0F) as usize;
            let mut pairs = Vec::new();
            let mut ptr = rest;
            if count == 0xF {
                loop {
                    match ptr.first() {
                        Some(0x03) => {
                            ptr = &ptr[1..];
                            break;
                        }
                        Some(_) => {
                            let (k, r) = unpack(ptr, seen)?;
                            let (v, r) = unpack(r, seen)?;
                            pairs.push((k, v));
                            ptr = r;
                        }
                        None => return Err(DecodeError("unterminated dictionary".into())),
                    }
                }
            } else {
                for _ in 0..count {
                    let (k, r) = unpack(ptr, seen)?;
                    let (v, r) = unpack(r, seen)?;
                    pairs.push((k, v));
                    ptr = r;
                }
            }
            (Value::Dict(pairs), ptr)
        }
        0xA0..=0xC0 => {
            let idx = (tag - 0xA0) as usize;
            let v = seen
                .get(idx)
                .cloned()
                .ok_or_else(|| DecodeError(format!("reference {idx} to nothing")))?;
            (v, rest)
        }
        0xC1..=0xC4 => {
            let n = (tag - 0xC0) as usize;
            let (i, rest) = take(rest, n)?;
            let idx = le(i) as usize;
            let v = seen
                .get(idx)
                .cloned()
                .ok_or_else(|| DecodeError(format!("reference {idx} to nothing")))?;
            (v, rest)
        }
        other => return Err(DecodeError(format!("unknown tag {other:#04x}"))),
    };
    if remember && !seen.contains(&value) {
        seen.push(value.clone());
    }
    Ok((value, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vectors() -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hap-vectors.json");
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    fn check(name: &str, v: Value) {
        let want = vectors()[format!("opack_{name}")].as_str().unwrap().to_owned();
        assert_eq!(hex::encode(encode(&v)), want, "encoding {name}");
        assert_eq!(decode(&hex::decode(&want).unwrap()).unwrap(), v, "decoding {name}");
    }

    #[test]
    fn matches_pyatv() {
        check(
            "small",
            Value::dict([
                ("_i", Value::str("_systemInfo")),
                ("_t", Value::Int(2)),
                ("_x", Value::Int(12345)),
            ]),
        );
        check(
            "types",
            Value::Array(vec![
                Value::Bool(true),
                Value::Bool(false),
                Value::Null,
                Value::Int(0),
                Value::Int(0x27),
                Value::Int(0x28),
                Value::Int(255),
                Value::Int(256),
                Value::Int(65535),
                Value::Int(65536),
                Value::Int(1 << 32),
                Value::Float(1.5),
                Value::str(""),
                Value::Str("a".repeat(32)),
                Value::Str("b".repeat(33)),
                Value::Str("c".repeat(300)),
                Value::Bytes(vec![]),
                Value::Bytes(vec![1; 32]),
                Value::Bytes(vec![2; 33]),
                Value::Bytes(vec![3; 300]),
            ]),
        );
        check(
            "uuid",
            Value::Uuid(
                hex::decode("12345678123456781234567812345678")
                    .unwrap()
                    .try_into()
                    .unwrap(),
            ),
        );
        check(
            "big_dict",
            Value::Dict((0..16).map(|i| (Value::Str(format!("k{i}")), Value::Int(i))).collect()),
        );
        check("big_list", Value::Array((0..16).map(Value::Int).collect()));
        // pyatv writes references for repeats; we decode them (and write repeats literally).
        let refs = Value::Array(vec![
            Value::str("hello"),
            Value::str("hello"),
            Value::Array(vec![Value::str("hello")]),
            Value::dict([("hello", Value::str("hello"))]),
        ]);
        // pyatv's bytes for [.., 1000, 1000] end in a reference its own decoder can't resolve (it
        // counts containers when encoding but not when decoding); the string references before it
        // are the part real devices send. Same bytes, with the array count set to 4:
        let pyatv = vectors()["opack_refs"].as_str().unwrap().to_owned();
        assert!(pyatv.starts_with("d64568656c6c6fa0d1a0e1a0a0"));
        assert_eq!(
            decode(&hex::decode("d44568656c6c6fa0d1a0e1a0a0").unwrap()).unwrap(),
            refs
        );
        assert_eq!(decode(&encode(&refs)).unwrap(), refs);
        // A `_hidC` request as pyatv sends it: the repeated "_hidC" is a reference (`a2`).
        let hid = Value::dict([
            ("_c", Value::dict([("_hBtS", Value::Int(1)), ("_hidC", Value::Int(6))])),
            ("_i", Value::str("_hidC")),
            ("_t", Value::Int(2)),
            ("_x", Value::Int(7)),
        ]);
        let pyatv = hex::decode(vectors()["opack_nested"].as_str().unwrap()).unwrap();
        assert_eq!(decode(&pyatv).unwrap(), hid);
        let ours = encode(&hid);
        assert_eq!(
            hex::encode(&ours),
            "e4425f63e2455f6842745309455f686964430e425f69455f68696443425f740a425f780f"
        );
        assert_eq!(decode(&ours).unwrap(), hid);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(&[0xE1, 0x41]).is_err());
        assert!(decode(&[0xA5]).is_err());
        assert!(decode(&[0x08, 0x08]).is_err());
        assert!(decode(&[0xFF]).is_err());
    }

    #[test]
    fn helpers() {
        let v = decode(&encode(&Value::dict([(
            "_c",
            Value::dict([("com.netflix.Netflix", Value::str("Netflix"))]),
        )])))
        .unwrap();
        assert_eq!(
            v.get("_c").unwrap().get("com.netflix.Netflix").unwrap().as_str(),
            Some("Netflix")
        );
        assert_eq!(v.to_json()["_c"]["com.netflix.Netflix"], "Netflix");
    }
}
