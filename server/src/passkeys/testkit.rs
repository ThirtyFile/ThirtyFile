//! A software authenticator for the tests: it makes keys, and answers a registration and a sign-in as a device does
//! (the same bytes a browser hands over), so the checks are run against messages built the way the standard says.

use aws_lc_rs::{
    rand::SystemRandom,
    rsa::KeySize,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, Ed25519KeyPair, KeyPair, RSA_PKCS1_SHA256, RsaKeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

use super::{cose, webauthn::RelyingParty};

// ───────────── A CBOR writer ─────────────

fn head(major: u8, n: u64) -> Vec<u8> {
    let m = major << 5;
    match n {
        0..=23 => vec![m | n as u8],
        24..=0xff => vec![m | 24, n as u8],
        0x100..=0xffff => [vec![m | 25], (n as u16).to_be_bytes().to_vec()].concat(),
        0x1_0000..=0xffff_ffff => [vec![m | 26], (n as u32).to_be_bytes().to_vec()].concat(),
        _ => [vec![m | 27], n.to_be_bytes().to_vec()].concat(),
    }
}

pub fn int(i: i64) -> Vec<u8> {
    if i >= 0 { head(0, i as u64) } else { head(1, (-1 - i) as u64) }
}

pub fn bytes(b: &[u8]) -> Vec<u8> {
    [head(2, b.len() as u64), b.to_vec()].concat()
}

pub fn text(t: &str) -> Vec<u8> {
    [head(3, t.len() as u64), t.as_bytes().to_vec()].concat()
}

pub fn map(pairs: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    let mut out = head(5, pairs.len() as u64);
    for (k, v) in pairs {
        out.extend(k);
        out.extend(v);
    }
    out
}

pub fn b64(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

// ───────────── The device ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alg {
    Es256,
    Ed25519,
    Rs256,
}

enum Signer {
    Es256(EcdsaKeyPair),
    Ed25519(Ed25519KeyPair),
    Rsa(RsaKeyPair),
}

pub struct Device {
    signer: Signer,
    pub credential_id: Vec<u8>,
    /// Counts up with every sign-in, as devices do
    pub counter: u32,
    /// False for a passkey that doesn't count (synced ones keep 0)
    pub counts: bool,
}

/// What a sign-in hands over
pub struct Assertion {
    pub credential_id: Vec<u8>,
    pub client_data_json: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    pub signature: Vec<u8>,
}

/// What a registration hands over
pub struct Attestation {
    pub client_data_json: Vec<u8>,
    pub attestation_object: Vec<u8>,
}

pub fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
    serde_json::json!({ "type": kind, "challenge": b64(challenge), "origin": origin, "crossOrigin": false }).to_string().into_bytes()
}

impl Device {
    pub fn new(alg: Alg) -> Device {
        let rng = SystemRandom::new();
        let signer = match alg {
            Alg::Es256 => Signer::Es256(
                EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap().as_ref())
                    .unwrap(),
            ),
            Alg::Ed25519 => Signer::Ed25519(Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref()).unwrap()),
            Alg::Rs256 => Signer::Rsa(RsaKeyPair::generate(KeySize::Rsa2048).unwrap()),
        };
        Device { signer, credential_id: rand::random::<[u8; 32]>().to_vec(), counter: 0, counts: true }
    }

    /// The public key as COSE
    pub fn cose_key(&self) -> Vec<u8> {
        match &self.signer {
            Signer::Es256(k) => {
                let point = k.public_key().as_ref();
                map(vec![(int(1), int(2)), (int(3), int(cose::ES256)), (int(-1), int(1)), (int(-2), bytes(&point[1..33])), (int(-3), bytes(&point[33..65]))])
            }
            Signer::Ed25519(k) => map(vec![(int(1), int(1)), (int(3), int(cose::EDDSA)), (int(-1), int(6)), (int(-2), bytes(k.public_key().as_ref()))]),
            Signer::Rsa(k) => {
                let components: aws_lc_rs::rsa::PublicKeyComponents<Vec<u8>> = k.public_key().into();
                map(vec![(int(1), int(3)), (int(3), int(cose::RS256)), (int(-1), bytes(&components.n)), (int(-2), bytes(&components.e))])
            }
        }
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        match &self.signer {
            Signer::Es256(k) => k.sign(&rng, message).unwrap().as_ref().to_vec(),
            Signer::Ed25519(k) => k.sign(message).as_ref().to_vec(),
            Signer::Rsa(k) => {
                let mut signature = vec![0; k.public_modulus_len()];
                k.sign(&RSA_PKCS1_SHA256, &rng, message, &mut signature).unwrap();
                signature
            }
        }
    }

    /// The authenticator data of a sign-in (`attested`: with the new credential, as a registration carries it)
    fn auth_data(&mut self, rp: &RelyingParty, flags: u8, attested: bool) -> Vec<u8> {
        if self.counts {
            self.counter += 1;
        }
        let mut out = Sha256::digest(rp.id.as_bytes()).to_vec();
        out.push(flags | if attested { super::webauthn::AT } else { 0 });
        out.extend(self.counter.to_be_bytes());
        if attested {
            out.extend([0u8; 16]);
            out.extend((self.credential_id.len() as u16).to_be_bytes());
            out.extend(&self.credential_id);
            out.extend(self.cose_key());
        }
        out
    }

    /// Registers: what `navigator.credentials.create` returns for `challenge`
    pub fn register(&mut self, rp: &RelyingParty, challenge: &[u8], flags: u8) -> Attestation {
        let auth_data = self.auth_data(rp, flags, true);
        let attestation_object = map(vec![(text("fmt"), text("none")), (text("attStmt"), map(vec![])), (text("authData"), bytes(&auth_data))]);
        Attestation { client_data_json: client_data("webauthn.create", challenge, &rp.origin), attestation_object }
    }

    /// Signs in: what `navigator.credentials.get` returns for `challenge`
    pub fn assert(&mut self, rp: &RelyingParty, challenge: &[u8], flags: u8) -> Assertion {
        let client_data_json = client_data("webauthn.get", challenge, &rp.origin);
        self.assert_with(rp, flags, client_data_json)
    }

    /// Signs in with client data made by the caller
    pub fn assert_with(&mut self, rp: &RelyingParty, flags: u8, client_data_json: Vec<u8>) -> Assertion {
        let authenticator_data = self.auth_data(rp, flags, false);
        let mut signed = authenticator_data.clone();
        signed.extend(Sha256::digest(&client_data_json));
        let signature = self.sign(&signed);
        Assertion { credential_id: self.credential_id.clone(), client_data_json, authenticator_data, signature }
    }
}
