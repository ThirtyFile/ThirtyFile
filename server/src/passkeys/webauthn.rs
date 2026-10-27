//! What a passkey sends, and what it has to prove (the Web Authentication standard, level 2): the registration of a new
//! passkey (attestation `none` only: no statements to check, no metadata service) and the assertion that signs in.
//!
//! Registration checks: the client data is a `webauthn.create` for this challenge and this origin, the attestation
//! object is `none`, the authenticator data is for this relying party's id, the user was present, and the key is one of
//! ES256, EdDSA and RS256. Assertion checks: a `webauthn.get` for this challenge and origin, this relying party's id, the
//! user present, a signature over the authenticator data and the hash of the client data by the stored key, and a
//! counter that goes forward (or stays 0, as synced passkeys keep it).

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{
    cbor::{self, Value},
    cose::PublicKey,
};

/// The flags of the authenticator data
pub const UP: u8 = 0x01;
pub const UV: u8 = 0x04;
pub const BE: u8 = 0x08;
pub const BS: u8 = 0x10;
pub const AT: u8 = 0x40;
pub const ED: u8 = 0x80;

/// Credential ids are 16 to 1023 bytes
const MIN_ID: usize = 16;
const MAX_ID: usize = 1023;

/// The site a passkey belongs to: its id (the host name) and the origin pages ask from (`https://files.example.com`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelyingParty {
    pub id: String,
    pub origin: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The client data isn't JSON of the kind a browser makes
    ClientData,
    /// Made for another kind of ceremony
    WrongType,
    /// Not the challenge that was given, or one that was used or expired
    Challenge,
    /// Made by a page of another origin, or embedded in another site
    Origin,
    /// The attestation object isn't CBOR of the form that is expected
    Attestation,
    /// An attestation that is not `none`
    AttestationFormat,
    /// The authenticator data is cut short, or says more or less than it should
    AuthenticatorData,
    /// Made for another relying party id
    RpId,
    /// The user wasn't present (no touch)
    UserNotPresent,
    /// The key is of a kind this program doesn't take, or malformed
    Key,
    /// A credential id of a length no authenticator uses
    CredentialId,
    /// The signature doesn't match
    Signature,
    /// The counter went back: the passkey may have been copied
    Counter,
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(rename = "crossOrigin", default)]
    cross_origin: Option<bool>,
}

/// The challenge a browser says it answered, so the matching one can be taken from those given out
pub fn challenge_in(client_data_json: &[u8]) -> Option<Vec<u8>> {
    let data: ClientData = serde_json::from_slice(client_data_json).ok()?;
    URL_SAFE_NO_PAD.decode(data.challenge).ok()
}

/// What the browser says about the ceremony, checked against what the server expects
fn check_client_data(rp: &RelyingParty, kind: &str, challenge: &[u8], client_data_json: &[u8]) -> Result<(), Error> {
    let data: ClientData = serde_json::from_slice(client_data_json).map_err(|_| Error::ClientData)?;
    if data.kind != kind {
        return Err(Error::WrongType);
    }
    let given = URL_SAFE_NO_PAD.decode(&data.challenge).map_err(|_| Error::Challenge)?;
    if !bool::from(given.ct_eq(challenge)) || challenge.is_empty() {
        return Err(Error::Challenge);
    }
    // The origin as it is, not a prefix of it or one with a path; and not a frame in another site
    if data.origin != rp.origin || data.cross_origin == Some(true) {
        return Err(Error::Origin);
    }
    Ok(())
}

/// The authenticator data (WebAuthn §6.1): the id's hash, the flags, the counter, and for a registration the new
/// credential
#[derive(Debug, Clone, PartialEq)]
pub struct AuthData {
    pub rp_id_hash: [u8; 32],
    pub flags: u8,
    pub counter: u32,
    pub credential: Option<Credential>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Credential {
    pub id: Vec<u8>,
    pub key: PublicKey,
    /// The key as the authenticator wrote it (COSE), which is what is kept
    pub key_cbor: Vec<u8>,
}

/// Reads authenticator data that is all of `bytes`
pub fn parse_auth_data(bytes: &[u8]) -> Result<AuthData, Error> {
    if bytes.len() < 37 {
        return Err(Error::AuthenticatorData);
    }
    let rp_id_hash: [u8; 32] = bytes[..32].try_into().map_err(|_| Error::AuthenticatorData)?;
    let flags = bytes[32];
    let counter = u32::from_be_bytes(bytes[33..37].try_into().map_err(|_| Error::AuthenticatorData)?);
    let mut rest = &bytes[37..];
    let mut credential = None;
    if flags & AT != 0 {
        // The authenticator's model id (16), the id's length (2) and the id, then the key as CBOR
        if rest.len() < 18 {
            return Err(Error::AuthenticatorData);
        }
        let len = usize::from(u16::from_be_bytes([rest[16], rest[17]]));
        if !(MIN_ID..=MAX_ID).contains(&len) {
            return Err(Error::CredentialId);
        }
        let id = rest.get(18..18 + len).ok_or(Error::AuthenticatorData)?.to_vec();
        rest = &rest[18 + len..];
        let (value, used) = cbor::parse_prefix(rest).map_err(|_| Error::Key)?;
        let key = PublicKey::from_value(&value).map_err(|_| Error::Key)?;
        credential = Some(Credential { id, key, key_cbor: rest[..used].to_vec() });
        rest = &rest[used..];
    }
    if flags & ED != 0 {
        // Extension outputs: a map, which nothing here asks for and so nothing here reads further than its shape
        let (_, used) = cbor::parse_prefix(rest).map_err(|_| Error::AuthenticatorData)?;
        rest = &rest[used..];
    }
    if !rest.is_empty() {
        return Err(Error::AuthenticatorData);
    }
    Ok(AuthData { rp_id_hash, flags, counter, credential })
}

fn check_rp(rp: &RelyingParty, data: &AuthData) -> Result<(), Error> {
    if !bool::from(Sha256::digest(rp.id.as_bytes()).as_slice().ct_eq(&data.rp_id_hash)) {
        return Err(Error::RpId);
    }
    if data.flags & UP == 0 {
        return Err(Error::UserNotPresent);
    }
    Ok(())
}

/// A passkey that was made
#[derive(Debug, Clone, PartialEq)]
pub struct Registered {
    pub credential_id: Vec<u8>,
    /// The COSE key, as kept
    pub public_key: Vec<u8>,
    pub counter: u32,
    /// The passkey can be backed up (synced), and is now
    pub backup_eligible: bool,
    pub backup_state: bool,
    pub user_verified: bool,
}

/// Checks the answer of `navigator.credentials.create`: `client_data_json` and the attestation object
pub fn verify_registration(rp: &RelyingParty, challenge: &[u8], client_data_json: &[u8], attestation_object: &[u8]) -> Result<Registered, Error> {
    check_client_data(rp, "webauthn.create", challenge, client_data_json)?;
    let object = cbor::parse(attestation_object).map_err(|_| Error::Attestation)?;
    // Only `none`: there is no statement to check and no list of devices to trust
    match (object.get_text("fmt").and_then(Value::as_text), object.get_text("attStmt")) {
        (Some("none"), Some(Value::Map(statement))) if statement.is_empty() => {}
        (Some(_), _) => return Err(Error::AttestationFormat),
        _ => return Err(Error::Attestation),
    }
    let auth = parse_auth_data(object.get_text("authData").and_then(Value::as_bytes).ok_or(Error::Attestation)?)?;
    check_rp(rp, &auth)?;
    let credential = auth.credential.ok_or(Error::AuthenticatorData)?;
    Ok(Registered {
        credential_id: credential.id,
        public_key: credential.key_cbor,
        counter: auth.counter,
        backup_eligible: auth.flags & BE != 0,
        backup_state: auth.flags & BS != 0,
        user_verified: auth.flags & UV != 0,
    })
}

/// A passkey as it is kept
#[derive(Debug, Clone)]
pub struct Stored {
    pub key: PublicKey,
    pub counter: u32,
}

/// A sign-in that checked out
#[derive(Debug, Clone, PartialEq)]
pub struct Asserted {
    /// The counter now, to be kept
    pub counter: u32,
    /// The passkey confirmed the person itself (a PIN, a fingerprint, a face), not only that it was touched
    pub user_verified: bool,
    pub backup_state: bool,
}

/// Checks the answer of `navigator.credentials.get`
pub fn verify_assertion(
    rp: &RelyingParty,
    challenge: &[u8],
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature: &[u8],
    stored: &Stored,
) -> Result<Asserted, Error> {
    check_client_data(rp, "webauthn.get", challenge, client_data_json)?;
    let auth = parse_auth_data(authenticator_data)?;
    check_rp(rp, &auth)?;
    // What was signed: the authenticator data followed by the hash of the client data
    let mut signed = authenticator_data.to_vec();
    signed.extend_from_slice(&Sha256::digest(client_data_json));
    if !stored.key.verify(&signed, signature) {
        return Err(Error::Signature);
    }
    // A counter that doesn't go forward means the passkey may have been copied; one that is always 0 is a passkey that
    // doesn't count (those kept in a password manager or a phone's keychain)
    if (stored.counter != 0 || auth.counter != 0) && auth.counter <= stored.counter {
        return Err(Error::Counter);
    }
    Ok(Asserted { counter: auth.counter, user_verified: auth.flags & UV != 0, backup_state: auth.flags & BS != 0 })
}

#[cfg(test)]
mod tests;
