//! The end date is read from real certificates, and nothing that isn't a certificate gives a date or a panic.

use super::{GENERALIZED_TIME, SEQUENCE, UTC_TIME, VERSION, not_after};

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match content.len() {
        n if n < 0x80 => out.push(n as u8),
        n if n < 0x100 => out.extend([0x81, n as u8]),
        n => out.extend([0x82, (n >> 8) as u8, n as u8]),
    }
    out.extend(content);
    out
}

/// A certificate with only the fields the reader walks through: `version` is left out for a version 1 certificate, and the issuer is
/// `issuer_bytes` long (to make the lengths take one, two and three bytes)
fn certificate(version: bool, issuer_bytes: usize, not_after_tag: u8, not_after_text: &str) -> Vec<u8> {
    certificate_with(version, &tlv(SEQUENCE, &tlv(0x0c, &vec![b'x'; issuer_bytes])), not_after_tag, not_after_text)
}

/// The same, with the bytes of the issuer element as they are
fn certificate_with(version: bool, issuer: &[u8], not_after_tag: u8, not_after_text: &str) -> Vec<u8> {
    let mut fields = Vec::new();
    if version {
        fields.extend(tlv(VERSION, &tlv(0x02, &[2])));
    }
    fields.extend(tlv(0x02, &[1, 2, 3]));
    fields.extend(tlv(SEQUENCE, &tlv(0x06, &[42, 134, 72])));
    fields.extend(issuer);
    let mut validity = tlv(UTC_TIME, b"200101000000Z");
    validity.extend(tlv(not_after_tag, not_after_text.as_bytes()));
    fields.extend(tlv(SEQUENCE, &validity));
    fields.extend(tlv(SEQUENCE, b""));
    let mut whole = tlv(SEQUENCE, &fields);
    whole.extend(tlv(SEQUENCE, &tlv(0x06, &[42, 134, 72])));
    whole.extend(tlv(0x03, &[0, 1, 2, 3]));
    tlv(SEQUENCE, &whole)
}

fn real(not_after: (i32, u8, u8)) -> Vec<u8> {
    let mut params = rcgen::CertificateParams::new(vec!["example.com".into()]).unwrap();
    params.not_before = rcgen::date_time_ymd(2000, 1, 1);
    params.not_after = rcgen::date_time_ymd(not_after.0, not_after.1, not_after.2);
    params.self_signed(&rcgen::KeyPair::generate().unwrap()).unwrap().der().to_vec()
}

#[test]
fn the_end_date_of_certificates_from_a_real_issuer_is_read_in_both_time_forms() {
    // UTCTime (to the year 2049) and GeneralizedTime (from 2050, and before 1950)
    for (date, expected) in [((2030, 1, 1), 1_893_456_000), ((2049, 12, 31), 2_524_521_600), ((2050, 1, 1), 2_524_608_000), ((2051, 1, 1), 2_556_144_000)] {
        assert_eq!(not_after(&real(date)), Some(expected), "{date:?}");
    }
    assert_eq!(not_after(&real((1970, 1, 1))), Some(0));
    assert_eq!(not_after(&real((1949, 12, 31))), Some(-631_238_400));
}

#[test]
fn utc_time_years_below_fifty_are_2000s_and_from_fifty_are_1900s() {
    for (text, expected) in [("491231235959Z", 2_524_607_999), ("500101000000Z", -631_152_000), ("000229123045Z", 951_827_445), ("240229123045Z", 1_709_209_845)] {
        assert_eq!(not_after(&certificate(true, 10, UTC_TIME, text)), Some(expected), "{text}");
    }
    assert_eq!(not_after(&certificate(true, 10, GENERALIZED_TIME, "20240229123045Z")), Some(1_709_209_845));
}

#[test]
fn version_one_certificates_have_no_version_field_and_long_lengths_are_read() {
    for version in [true, false] {
        for issuer in [0, 100, 127, 128, 255, 256, 5000] {
            assert_eq!(not_after(&certificate(version, issuer, UTC_TIME, "300101000000Z")), Some(1_893_456_000), "version {version}, issuer {issuer}");
        }
    }
}

#[test]
fn a_date_that_is_not_a_date_gives_none() {
    for (tag, text) in [
        (UTC_TIME, "300101000000"),              // no Z
        (UTC_TIME, "300101000000+0100"),         // an offset
        (UTC_TIME, "3001010000Z"),               // no seconds
        (UTC_TIME, "20300101000000Z"),           // the wrong form for the tag
        (GENERALIZED_TIME, "300101000000Z"),     // the wrong form for the tag
        (GENERALIZED_TIME, "20300101000000.5Z"), // fractions of a second
        (UTC_TIME, "301301000000Z"),             // month 13
        (UTC_TIME, "300230000000Z"),             // 30 February
        (UTC_TIME, "300101246000Z"),             // hour 24, minute 60
        (UTC_TIME, "30010100000aZ"),             // a letter
        (UTC_TIME, "-30101000000Z"),             // a sign
        (UTC_TIME, ""),                          // empty
        (0x04, "300101000000Z"),                 // not a time at all
    ] {
        assert_eq!(not_after(&certificate(true, 10, tag, text)), None, "{text}");
    }
}

#[test]
fn every_part_of_a_certificate_cut_short_gives_none_and_never_panics() {
    for der in [real((2030, 6, 1)), certificate(true, 300, UTC_TIME, "300101000000Z"), certificate(false, 5, GENERALIZED_TIME, "20510101000000Z")] {
        assert!(not_after(&der).is_some());
        for length in 0..der.len() {
            assert_eq!(not_after(&der[..length]), None, "cut at {length}");
        }
    }
}

#[test]
fn altered_and_random_bytes_never_panic() {
    let der = real((2030, 6, 1));
    for at in 0..der.len() {
        for value in [0, 0x30, 0x7f, 0x80, 0x81, 0x84, 0x85, 0xff] {
            let mut altered = der.clone();
            altered[at] = value;
            let _ = not_after(&altered);
        }
    }
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..20_000 {
        let length = (next() % 200) as usize;
        let mut bytes: Vec<u8> = (0..length).map(|_| next() as u8).collect();
        if let Some(first) = bytes.first_mut() {
            *first = SEQUENCE;
        }
        let _ = not_after(&bytes);
    }
}

#[test]
fn unusual_encodings_are_not_certificates() {
    // An indefinite length, a length of five bytes, a tag in several bytes, trailing data inside an empty certificate
    assert_eq!(not_after(&[0x30, 0x80, 0x30, 0x80, 0, 0, 0, 0]), None);
    assert_eq!(not_after(&[0x30, 0x85, 0, 0, 0, 0, 2, 0x30, 0]), None);
    assert_eq!(not_after(&[0x3f, 0x01, 0x00]), None);
    assert_eq!(not_after(&[0x30, 0x00]), None);
    assert_eq!(not_after(&[0x31, 0x00]), None);
    assert_eq!(not_after(b""), None);
}

#[test]
fn indefinite_lengths_and_tags_in_several_bytes_are_refused() {
    let issuer = tlv(SEQUENCE, &tlv(0x0c, b"x"));
    assert_eq!(not_after(&certificate_with(true, &issuer, UTC_TIME, "300101000000Z")), Some(1_893_456_000));
    // An element read as empty would let the bytes after it pass as the next fields
    assert_eq!(not_after(&certificate_with(true, &[SEQUENCE, 0x80], UTC_TIME, "300101000000Z")), None);
    assert_eq!(not_after(&certificate_with(true, &[0x1f, 0x00], UTC_TIME, "300101000000Z")), None);
}

#[test]
fn a_length_in_up_to_four_bytes_is_read_and_in_five_is_not() {
    let der = certificate(true, 10, UTC_TIME, "300101000000Z");
    let content = &der[2..];
    assert_eq!(content.len(), usize::from(der[1]));
    for (count, expected) in [(1, Some(1_893_456_000)), (4, Some(1_893_456_000)), (5, None)] {
        let mut longer = vec![SEQUENCE, 0x80 | count];
        longer.extend(std::iter::repeat_n(0, usize::from(count) - 1));
        longer.push(content.len() as u8);
        longer.extend(content);
        assert_eq!(not_after(&longer), expected, "{count} bytes");
    }
}
