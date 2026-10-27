//! The public key of a passkey (a COSE key, RFC 9052) and the check of a signature made with it: ES256 (ECDSA with P-256
//! and SHA-256), EdDSA (Ed25519) and RS256 (RSA PKCS#1 v1.5 with SHA-256), the three algorithms WebAuthn authenticators
//! use. The checks are `aws-lc-rs`, which the program already links for TLS.

use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, ED25519, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

use super::cbor::{self, Value};

/// The algorithms, by their COSE numbers
pub const ES256: i64 = -7;
pub const EDDSA: i64 = -8;
pub const RS256: i64 = -257;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicKey {
    Es256 { x: [u8; 32], y: [u8; 32] },
    Ed25519([u8; 32]),
    Rs256 { n: Vec<u8>, e: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not CBOR this program reads, or not a map
    Format,
    /// A key type, curve or algorithm that is not ES256, EdDSA or RS256 on its usual curve
    Unsupported,
    /// A part of the key that is missing or the wrong length
    Key,
}

impl PublicKey {
    /// Reads a COSE key from its CBOR bytes, which have to be the whole of `bytes`
    pub fn from_cbor(bytes: &[u8]) -> Result<PublicKey, Error> {
        let value = cbor::parse(bytes).map_err(|_| Error::Format)?;
        PublicKey::from_value(&value)
    }

    pub fn from_value(v: &Value) -> Result<PublicKey, Error> {
        if !matches!(v, Value::Map(_)) {
            return Err(Error::Format);
        }
        let (Some(kty), Some(alg)) = (v.get(1).and_then(Value::as_int), v.get(3).and_then(Value::as_int)) else { return Err(Error::Format) };
        let bytes = |key: i64| v.get(key).and_then(Value::as_bytes).ok_or(Error::Key);
        match (kty, alg) {
            // EC2, ES256: the curve is P-256 (1), the point's two coordinates are 32 bytes each
            (2, ES256) => {
                if v.get(-1).and_then(Value::as_int) != Some(1) {
                    return Err(Error::Unsupported);
                }
                Ok(PublicKey::Es256 { x: bytes(-2)?.try_into().map_err(|_| Error::Key)?, y: bytes(-3)?.try_into().map_err(|_| Error::Key)? })
            }
            // OKP, EdDSA: the curve is Ed25519 (6)
            (1, EDDSA) => {
                if v.get(-1).and_then(Value::as_int) != Some(6) {
                    return Err(Error::Unsupported);
                }
                Ok(PublicKey::Ed25519(bytes(-2)?.try_into().map_err(|_| Error::Key)?))
            }
            // RSA, RS256: a modulus of 2048 to 8192 bits (the bound the verifier works within) and an exponent
            (3, RS256) => {
                let (n, e) = (bytes(-1)?, bytes(-2)?);
                let n = strip(n);
                if !(256..=1024).contains(&n.len()) || e.is_empty() || e.len() > 8 || n.first().is_none_or(|b| *b == 0) {
                    return Err(Error::Key);
                }
                Ok(PublicKey::Rs256 { n: n.to_vec(), e: strip(e).to_vec() })
            }
            _ => Err(Error::Unsupported),
        }
    }

    /// The COSE number of its algorithm
    #[cfg(test)]
    pub fn algorithm(&self) -> i64 {
        match self {
            PublicKey::Es256 { .. } => ES256,
            PublicKey::Ed25519(_) => EDDSA,
            PublicKey::Rs256 { .. } => RS256,
        }
    }

    /// Whether `signature` is a valid signature of `message` by this key. An ES256 signature is DER (as WebAuthn
    /// sends it); an RS256 signature is the bytes as they are.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        match self {
            PublicKey::Es256 { x, y } => {
                let mut point = Vec::with_capacity(65);
                point.push(0x04);
                point.extend_from_slice(x);
                point.extend_from_slice(y);
                UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, point).verify(message, signature).is_ok()
            }
            PublicKey::Ed25519(key) => UnparsedPublicKey::new(&ED25519, key).verify(message, signature).is_ok(),
            PublicKey::Rs256 { n, e } => UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, rsa_public_key_der(n, e)).verify(message, signature).is_ok(),
        }
    }
}

/// Without leading zero bytes
fn strip(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    &bytes[start..]
}

/// A DER length
fn der_length(len: usize) -> Vec<u8> {
    if len < 0x80 {
        vec![len as u8]
    } else {
        let bytes: Vec<u8> = len.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        let mut out = vec![0x80 | bytes.len() as u8];
        out.extend(bytes);
        out
    }
}

/// A DER INTEGER holding the positive number `be` (big-endian, no leading zeros)
fn der_integer(be: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(be.len() + 1);
    // A set top bit would make it negative
    if be.first().is_some_and(|b| b & 0x80 != 0) {
        body.push(0);
    }
    body.extend_from_slice(be);
    let mut out = vec![0x02];
    out.extend(der_length(body.len()));
    out.extend(body);
    out
}

/// `RSAPublicKey ::= SEQUENCE { modulus INTEGER, publicExponent INTEGER }` (RFC 8017), which is what the verifier takes
pub fn rsa_public_key_der(n: &[u8], e: &[u8]) -> Vec<u8> {
    let mut body = der_integer(n);
    body.extend(der_integer(e));
    let mut out = vec![0x30];
    out.extend(der_length(body.len()));
    out.extend(body);
    out
}

#[cfg(test)]
mod tests;
