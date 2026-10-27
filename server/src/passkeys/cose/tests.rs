use sha2::Digest;

use super::*;
use crate::passkeys::testkit::{self as kit, Alg, Device};

#[test]
fn a_key_of_each_algorithm_is_read_and_checks_its_own_signatures() {
    for alg in [Alg::Es256, Alg::Ed25519, Alg::Rs256] {
        let mut device = Device::new(alg);
        let key = PublicKey::from_cbor(&device.cose_key()).unwrap_or_else(|e| panic!("{alg:?}: {e:?}"));
        assert_eq!(
            key.algorithm(),
            match alg {
                Alg::Es256 => ES256,
                Alg::Ed25519 => EDDSA,
                Alg::Rs256 => RS256,
            }
        );
        let rp = crate::passkeys::webauthn::RelyingParty { id: "x.example.com".into(), origin: "https://x.example.com".into() };
        let a = device.assert(&rp, b"challenge", 0x01);
        let mut signed = a.authenticator_data.clone();
        signed.extend(sha2::Sha256::digest(&a.client_data_json));
        assert!(key.verify(&signed, &a.signature), "{alg:?}");
        // Another message, one changed byte of the signature, a shorter signature, and another device's key
        assert!(!key.verify(b"another message", &a.signature), "{alg:?}");
        let mut flipped = a.signature.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert!(!key.verify(&signed, &flipped), "{alg:?}");
        assert!(!key.verify(&signed, &a.signature[..a.signature.len() - 1]), "{alg:?}");
        assert!(!key.verify(&signed, &[]), "{alg:?}");
        let other = PublicKey::from_cbor(&Device::new(alg).cose_key()).unwrap();
        assert!(!other.verify(&signed, &a.signature), "{alg:?}");
    }
}

#[test]
fn a_key_that_is_not_one_of_the_three_or_is_malformed_is_refused() {
    let ec = |alg: i64, crv: i64, x: &[u8], y: &[u8]| {
        kit::map(vec![
            (kit::int(1), kit::int(2)),
            (kit::int(3), kit::int(alg)),
            (kit::int(-1), kit::int(crv)),
            (kit::int(-2), kit::bytes(x)),
            (kit::int(-3), kit::bytes(y)),
        ])
    };
    assert!(PublicKey::from_cbor(&ec(ES256, 1, &[1; 32], &[2; 32])).is_ok());
    assert_eq!(PublicKey::from_cbor(&ec(ES256, 2, &[1; 32], &[2; 32])), Err(Error::Unsupported), "P-384");
    assert_eq!(PublicKey::from_cbor(&ec(-35, 2, &[1; 32], &[2; 32])), Err(Error::Unsupported), "ES384");
    assert_eq!(PublicKey::from_cbor(&ec(ES256, 1, &[1; 31], &[2; 32])), Err(Error::Key));
    assert_eq!(PublicKey::from_cbor(&ec(ES256, 1, &[1; 32], &[2; 33])), Err(Error::Key));
    // EdDSA on another curve, with a short key; a key of a kind that doesn't go with the algorithm
    let okp = |crv: i64, x: &[u8]| {
        kit::map(vec![(kit::int(1), kit::int(1)), (kit::int(3), kit::int(EDDSA)), (kit::int(-1), kit::int(crv)), (kit::int(-2), kit::bytes(x))])
    };
    assert!(PublicKey::from_cbor(&okp(6, &[3; 32])).is_ok());
    assert_eq!(PublicKey::from_cbor(&okp(7, &[3; 32])), Err(Error::Unsupported));
    assert_eq!(PublicKey::from_cbor(&okp(6, &[3; 31])), Err(Error::Key));
    let wrong_kty = kit::map(vec![
        (kit::int(1), kit::int(1)),
        (kit::int(3), kit::int(ES256)),
        (kit::int(-1), kit::int(1)),
        (kit::int(-2), kit::bytes(&[1; 32])),
        (kit::int(-3), kit::bytes(&[2; 32])),
    ]);
    assert_eq!(PublicKey::from_cbor(&wrong_kty), Err(Error::Unsupported));
    // RSA: too small a modulus, an exponent that isn't, a modulus that starts with a zero
    let rsa = |n: Vec<u8>, e: Vec<u8>| {
        kit::map(vec![(kit::int(1), kit::int(3)), (kit::int(3), kit::int(RS256)), (kit::int(-1), kit::bytes(&n)), (kit::int(-2), kit::bytes(&e))])
    };
    assert!(PublicKey::from_cbor(&rsa(vec![0x80; 256], vec![1, 0, 1])).is_ok());
    assert_eq!(PublicKey::from_cbor(&rsa(vec![0x80; 128], vec![1, 0, 1])), Err(Error::Key), "1024 bits");
    assert_eq!(PublicKey::from_cbor(&rsa(vec![0x80; 2000], vec![1, 0, 1])), Err(Error::Key));
    assert_eq!(PublicKey::from_cbor(&rsa(vec![0x80; 256], vec![])), Err(Error::Key));
    assert_eq!(PublicKey::from_cbor(&rsa(vec![0x80; 256], vec![1; 9])), Err(Error::Key));
    // A modulus with a zero in front is the same number: read as the 256 bytes it is
    let padded = [vec![0u8], vec![0x80; 256]].concat();
    assert_eq!(PublicKey::from_cbor(&rsa(padded, vec![1, 0, 1])).unwrap(), PublicKey::Rs256 { n: vec![0x80; 256], e: vec![1, 0, 1] });
    // Not a map, not CBOR, bytes after it, the parts missing
    assert_eq!(PublicKey::from_cbor(&kit::int(5)), Err(Error::Format));
    assert_eq!(PublicKey::from_cbor(&[0xff]), Err(Error::Format));
    assert_eq!(PublicKey::from_cbor(&[ec(ES256, 1, &[1; 32], &[2; 32]), vec![0]].concat()), Err(Error::Format));
    assert_eq!(PublicKey::from_cbor(&kit::map(vec![(kit::int(1), kit::int(2))])), Err(Error::Format));
    assert_eq!(PublicKey::from_cbor(&kit::map(vec![(kit::int(1), kit::int(2)), (kit::int(3), kit::int(ES256)), (kit::int(-1), kit::int(1))])), Err(Error::Key));
}

#[test]
fn the_der_of_an_rsa_key_is_what_the_standard_says() {
    assert_eq!(der_length(5), [5]);
    assert_eq!(der_length(127), [0x7f]);
    assert_eq!(der_length(128), [0x81, 0x80]);
    assert_eq!(der_length(255), [0x81, 0xff]);
    assert_eq!(der_length(256), [0x82, 0x01, 0x00]);
    assert_eq!(der_length(65535), [0x82, 0xff, 0xff]);
    assert_eq!(der_integer(&[0x01, 0x00, 0x01]), [0x02, 0x03, 0x01, 0x00, 0x01]);
    assert_eq!(der_integer(&[0x80]), [0x02, 0x02, 0x00, 0x80], "a set top bit gets a zero in front, or it would be negative");
    assert_eq!(rsa_public_key_der(&[0x80, 0x01], &[0x03]), [0x30, 0x08, 0x02, 0x03, 0x00, 0x80, 0x01, 0x02, 0x01, 0x03]);
    assert_eq!(strip(&[0, 0, 5, 0]), [5, 0]);
    assert!(strip(&[0, 0]).is_empty());
}
