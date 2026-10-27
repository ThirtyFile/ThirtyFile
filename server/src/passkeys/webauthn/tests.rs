use serde_json::json;

use super::*;
use crate::passkeys::testkit::{self as kit, Alg, Device};

fn rp() -> RelyingParty {
    RelyingParty { id: "files.example.com".into(), origin: "https://files.example.com".into() }
}

const CHALLENGE: &[u8] = b"a challenge of thirty-two bytes!";

/// A passkey made and kept, as the server keeps it
fn registered(alg: Alg) -> (Device, Registered) {
    let mut device = Device::new(alg);
    let a = device.register(&rp(), CHALLENGE, UP | UV);
    let r = verify_registration(&rp(), CHALLENGE, &a.client_data_json, &a.attestation_object).unwrap();
    (device, r)
}

fn stored(r: &Registered) -> Stored {
    Stored { key: PublicKey::from_cbor(&r.public_key).unwrap(), counter: r.counter }
}

fn check(rp: &RelyingParty, challenge: &[u8], a: &kit::Assertion, s: &Stored) -> Result<Asserted, Error> {
    verify_assertion(rp, challenge, &a.client_data_json, &a.authenticator_data, &a.signature, s)
}

#[test]
fn a_passkey_of_each_algorithm_registers_and_then_signs_in() {
    for alg in [Alg::Es256, Alg::Ed25519, Alg::Rs256] {
        let (mut device, r) = registered(alg);
        assert_eq!(r.credential_id, device.credential_id, "{alg:?}");
        assert_eq!(r.public_key, device.cose_key());
        assert!(r.user_verified && !r.backup_eligible && !r.backup_state);
        assert_eq!(r.counter, 1);
        let challenge = b"another challenge, all new to it";
        let a = device.assert(&rp(), challenge, UP | UV | BE | BS);
        let out = check(&rp(), challenge, &a, &stored(&r)).unwrap_or_else(|e| panic!("{alg:?}: {e:?}"));
        assert_eq!(out, Asserted { counter: 2, user_verified: true, backup_state: true });
    }
}

#[test]
fn the_flags_of_a_registration_are_kept() {
    let mut d = Device::new(Alg::Es256);
    let a = d.register(&rp(), CHALLENGE, UP | BE | BS);
    let r = verify_registration(&rp(), CHALLENGE, &a.client_data_json, &a.attestation_object).unwrap();
    assert_eq!((r.user_verified, r.backup_eligible, r.backup_state), (false, true, true), "a synced passkey, tapped but not verified");
}

/// The error of a registration made by a new device for `party`, with its client data or attestation object changed
fn registration_error(party: &RelyingParty, change: impl FnOnce(&mut Vec<u8>, &mut Vec<u8>)) -> Error {
    let a = Device::new(Alg::Es256).register(party, CHALLENGE, UP | UV);
    let (mut client, mut object) = (a.client_data_json, a.attestation_object);
    change(&mut client, &mut object);
    verify_registration(&rp(), CHALLENGE, &client, &object).unwrap_err()
}

fn create_data(origin: &str) -> Vec<u8> {
    kit::client_data("webauthn.create", CHALLENGE, origin)
}

#[test]
fn a_registration_that_is_not_for_this_site_this_challenge_or_this_kind_is_refused() {
    let ours = rp();
    assert_eq!(registration_error(&ours, |c, _| *c = kit::client_data("webauthn.create", b"not the challenge to it......", &ours.origin)), Error::Challenge);
    assert_eq!(registration_error(&ours, |c, _| *c = kit::client_data("webauthn.get", CHALLENGE, &ours.origin)), Error::WrongType);
    // Another origin, one that only starts like ours, one with a port or a path, one in capitals, none
    for origin in [
        "https://evil.example.com",
        "https://files.example.com.evil.example",
        "http://files.example.com",
        "https://files.example.com:8443",
        "https://files.example.com/",
        "https://FILES.example.com",
        "",
    ] {
        assert_eq!(registration_error(&ours, |c, _| *c = create_data(origin)), Error::Origin, "{origin:?}");
    }
    // Framed in another site
    let framed = json!({ "type": "webauthn.create", "challenge": kit::b64(CHALLENGE), "origin": ours.origin, "crossOrigin": true }).to_string().into_bytes();
    assert_eq!(registration_error(&ours, |c, _| *c = framed), Error::Origin);
    // Not JSON, no challenge, a challenge that isn't base64
    assert_eq!(registration_error(&ours, |c, _| *c = b"not json".to_vec()), Error::ClientData);
    assert_eq!(registration_error(&ours, |c, _| *c = json!({ "type": "webauthn.create", "origin": ours.origin }).to_string().into_bytes()), Error::ClientData);
    assert_eq!(
        registration_error(&ours, |c, _| *c = json!({ "type": "webauthn.create", "challenge": "***", "origin": ours.origin }).to_string().into_bytes()),
        Error::Challenge
    );
    // A passkey made for another site's id
    let other = RelyingParty { id: "other.example.com".into(), origin: ours.origin.clone() };
    assert_eq!(registration_error(&other, |_, _| ()), Error::RpId);
}

fn auth_of(object: &[u8]) -> Vec<u8> {
    cbor::parse(object).unwrap().get_text("authData").and_then(Value::as_bytes).unwrap().to_vec()
}

fn object_of(fmt: &str, statement: Vec<u8>, auth: &[u8]) -> Vec<u8> {
    kit::map(vec![(kit::text("fmt"), kit::text(fmt)), (kit::text("attStmt"), statement), (kit::text("authData"), kit::bytes(auth))])
}

#[test]
fn a_registration_that_is_not_attestation_none_or_is_malformed_is_refused() {
    let ours = rp();
    assert_eq!(registration_error(&ours, |_, o| *o = object_of("packed", kit::map(vec![]), &auth_of(o))), Error::AttestationFormat);
    assert_eq!(registration_error(&ours, |_, o| *o = object_of("none", kit::map(vec![(kit::text("alg"), kit::int(-7))]), &auth_of(o))), Error::AttestationFormat);
    assert_eq!(registration_error(&ours, |_, o| *o = vec![0xff]), Error::Attestation);
    assert_eq!(registration_error(&ours, |_, o| *o = kit::int(7)), Error::Attestation);
    assert_eq!(
        registration_error(&ours, |_, o| *o = kit::map(vec![(kit::text("fmt"), kit::text("none")), (kit::text("attStmt"), kit::map(vec![]))])),
        Error::Attestation
    );
    assert_eq!(
        registration_error(&ours, |_, o| *o =
            kit::map(vec![(kit::text("fmt"), kit::text("none")), (kit::text("attStmt"), kit::map(vec![])), (kit::text("authData"), kit::text("x"))])),
        Error::Attestation
    );
}

/// The error of a registration whose authenticator data is changed by `edit`
fn auth_error(edit: impl FnOnce(Vec<u8>) -> Vec<u8>) -> Error {
    registration_error(&rp(), |_, o| *o = object_of("none", kit::map(vec![]), &edit(auth_of(o))))
}

#[test]
fn authenticator_data_that_is_cut_short_padded_or_odd_is_refused() {
    assert_eq!(auth_error(|a| a[..36].to_vec()), Error::AuthenticatorData);
    assert_eq!(auth_error(|a| a[..37].to_vec()), Error::AuthenticatorData, "the credential is announced but isn't there");
    assert_eq!(auth_error(|a| a[..60].to_vec()), Error::AuthenticatorData);
    assert_eq!(auth_error(|a| [a, vec![0]].concat()), Error::AuthenticatorData, "bytes after it");
    assert_eq!(
        auth_error(|mut a| {
            a[32] &= !AT;
            a
        }),
        Error::AuthenticatorData,
        "a registration without a credential"
    );
    assert_eq!(
        auth_error(|mut a| {
            a[32] &= !UP;
            a
        }),
        Error::UserNotPresent
    );
    // A credential id of 8 bytes, and of 1024
    for len in [8u16, 1024] {
        assert_eq!(
            auth_error(|mut a| {
                a[53..55].copy_from_slice(&len.to_be_bytes());
                a
            }),
            Error::CredentialId,
            "{len}"
        );
    }
    // A key of an algorithm that is not taken (the key starts after the 37 bytes, the model id, the id's length and the id)
    let weird = kit::map(vec![
        (kit::int(1), kit::int(2)),
        (kit::int(3), kit::int(-35)),
        (kit::int(-1), kit::int(2)),
        (kit::int(-2), kit::bytes(&[1; 48])),
        (kit::int(-3), kit::bytes(&[2; 48])),
    ]);
    assert_eq!(auth_error(|a| [&a[..37 + 18 + 32], &weird[..]].concat()), Error::Key);
}

#[test]
fn extension_data_is_allowed_when_it_is_flagged_and_nothing_is_read_from_it() {
    let (mut d, party) = (Device::new(Alg::Es256), rp());
    let att = d.register(&party, CHALLENGE, UP | UV);
    let mut auth = auth_of(&att.attestation_object);
    auth[32] |= ED;
    auth.extend(kit::map(vec![(kit::text("credProtect"), kit::int(2))]));
    let object = object_of("none", kit::map(vec![]), &auth);
    assert!(verify_registration(&party, CHALLENGE, &att.client_data_json, &object).is_ok());
    // Flagged, but not there or not CBOR
    let mut flagged = auth_of(&att.attestation_object);
    flagged[32] |= ED;
    assert_eq!(verify_registration(&party, CHALLENGE, &att.client_data_json, &object_of("none", kit::map(vec![]), &flagged)).unwrap_err(), Error::AuthenticatorData);
}

#[test]
fn a_sign_in_that_is_for_another_challenge_kind_or_site_is_refused() {
    let (mut d, r) = registered(Alg::Es256);
    let s = stored(&r);
    let signed_for = |d: &mut Device, kind: &str, challenge: &[u8], origin: &str| d.assert_with(&rp(), UP | UV, kit::client_data(kind, challenge, origin));
    // An earlier challenge (a replay), a registration's client data, another origin
    let a = signed_for(&mut d, "webauthn.get", b"an earlier challenge, long enough..", &rp().origin);
    assert_eq!(check(&rp(), CHALLENGE, &a, &s), Err(Error::Challenge));
    let a = signed_for(&mut d, "webauthn.create", CHALLENGE, &rp().origin);
    assert_eq!(check(&rp(), CHALLENGE, &a, &s), Err(Error::WrongType));
    let a = signed_for(&mut d, "webauthn.get", CHALLENGE, "https://evil.example.com");
    assert_eq!(check(&rp(), CHALLENGE, &a, &s), Err(Error::Origin));
    // The right signature, for the id of another site
    let a = d.assert(&RelyingParty { id: "other.example.com".into(), origin: rp().origin }, CHALLENGE, UP | UV);
    assert_eq!(check(&rp(), CHALLENGE, &a, &s), Err(Error::RpId));
}

#[test]
fn a_signature_of_another_key_or_over_changed_data_is_refused() {
    let (mut d, r) = registered(Alg::Es256);
    let s = stored(&r);
    let (mut other, _) = registered(Alg::Es256);
    let a = other.assert(&rp(), CHALLENGE, UP | UV);
    assert_eq!(check(&rp(), CHALLENGE, &a, &s), Err(Error::Signature), "a device whose key isn't the one kept");
    let mut tamper = |change: &dyn Fn(&mut kit::Assertion)| {
        let mut a = d.assert(&rp(), CHALLENGE, UP | UV);
        change(&mut a);
        check(&rp(), CHALLENGE, &a, &s)
    };
    assert_eq!(tamper(&|a| *a.signature.last_mut().unwrap() ^= 1), Err(Error::Signature));
    assert_eq!(tamper(&|a| a.signature.clear()), Err(Error::Signature));
    assert_eq!(tamper(&|a| a.client_data_json.extend(b" ")), Err(Error::Signature), "the client data is part of what was signed");
    assert_eq!(tamper(&|a| a.authenticator_data[32] |= BE), Err(Error::Signature), "so is the authenticator data");
    assert_eq!(tamper(&|a| a.authenticator_data[36] ^= 1), Err(Error::Signature), "and its counter");
}

#[test]
fn the_user_has_to_be_present_for_a_sign_in_and_not_verified_is_said() {
    let (mut d, r) = registered(Alg::Es256);
    let a = d.assert(&rp(), CHALLENGE, UV);
    assert_eq!(check(&rp(), CHALLENGE, &a, &stored(&r)), Err(Error::UserNotPresent));
    // Present but not verified is a sign-in that says so (the caller decides what it is worth)
    let a = d.assert(&rp(), CHALLENGE, UP);
    assert!(!check(&rp(), CHALLENGE, &a, &stored(&r)).unwrap().user_verified);
}

#[test]
fn the_counter_has_to_go_forward() {
    let (mut d, r) = registered(Alg::Es256);
    let mut s = stored(&r);
    for expected in 2..6 {
        let a = d.assert(&rp(), CHALLENGE, UP | UV);
        let out = check(&rp(), CHALLENGE, &a, &s).unwrap();
        assert_eq!(out.counter, expected);
        s.counter = out.counter;
    }
    // A device that went back (a copy of the key), or stood still
    for counter in [1u32, 4, 5] {
        d.counter = counter - 1;
        let a = d.assert(&rp(), CHALLENGE, UP | UV);
        assert_eq!(check(&rp(), CHALLENGE, &a, &s), Err(Error::Counter), "{counter}");
    }
}

#[test]
fn a_passkey_that_never_counts_is_not_refused_for_it() {
    let mut synced = Device::new(Alg::Ed25519);
    synced.counts = false;
    let a = synced.register(&rp(), CHALLENGE, UP | UV | BE | BS);
    let r = verify_registration(&rp(), CHALLENGE, &a.client_data_json, &a.attestation_object).unwrap();
    assert_eq!(r.counter, 0);
    for _ in 0..3 {
        let a = synced.assert(&rp(), CHALLENGE, UP | UV | BE | BS);
        assert_eq!(check(&rp(), CHALLENGE, &a, &stored(&r)).unwrap().counter, 0);
    }
    // One that starts to count is fine; once it counts, going back to 0 is a counter that went back
    synced.counts = true;
    let a = synced.assert(&rp(), CHALLENGE, UP | UV | BE | BS);
    assert_eq!(check(&rp(), CHALLENGE, &a, &stored(&r)).unwrap().counter, 1);
    synced.counts = false;
    synced.counter = 0;
    let a = synced.assert(&rp(), CHALLENGE, UP | UV | BE | BS);
    assert_eq!(check(&rp(), CHALLENGE, &a, &Stored { counter: 1, ..stored(&r) }), Err(Error::Counter));
}

#[test]
fn an_empty_challenge_is_never_accepted() {
    let (mut d, r) = registered(Alg::Es256);
    let a = d.assert(&rp(), b"", UP | UV);
    assert_eq!(check(&rp(), b"", &a, &stored(&r)), Err(Error::Challenge));
}

#[test]
fn nothing_a_hostile_client_sends_makes_the_checks_panic() {
    let (mut d, r) = registered(Alg::Es256);
    let a = d.assert(&rp(), CHALLENGE, UP | UV);
    let mut seed = 0x1234_5678_9abc_def1u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..5_000 {
        let mut auth = a.authenticator_data.clone();
        let n = auth.len();
        auth[(next() as usize) % n] = next() as u8;
        auth.truncate(((next() as usize) % n) + 1);
        let junk: Vec<u8> = (0..(next() % 80) as usize).map(|_| next() as u8).collect();
        let _ = verify_assertion(&rp(), CHALLENGE, &a.client_data_json, &auth, &a.signature, &stored(&r));
        let _ = verify_assertion(&rp(), CHALLENGE, &junk, &auth, &junk, &stored(&r));
        let _ = verify_registration(&rp(), CHALLENGE, &junk, &auth);
        let _ = parse_auth_data(&junk);
    }
}
