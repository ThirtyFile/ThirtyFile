use super::*;

#[test]
fn lengths_are_short_or_long_and_read_back() {
    for len in [0usize, 1, 127, 128, 255, 256, 65_535, 65_536, 1_000_000] {
        let content = vec![7u8; len];
        let bytes = tlv(OCTET_STRING, &content);
        let (item, rest) = read(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!((item.tag, item.content.len()), (OCTET_STRING, len));
    }
    // The forms the standard gives
    assert_eq!(tlv(OCTET_STRING, b"hi"), [0x04, 0x02, b'h', b'i']);
    assert_eq!(&tlv(OCTET_STRING, &[0; 128])[..3], [0x04, 0x81, 0x80]);
    assert_eq!(&tlv(OCTET_STRING, &[0; 256])[..4], [0x04, 0x82, 0x01, 0x00]);
}

#[test]
fn integers_take_the_shortest_form_and_negative_ones_come_back() {
    assert_eq!(int(INTEGER, 0), [0x02, 0x01, 0x00]);
    assert_eq!(int(INTEGER, 127), [0x02, 0x01, 0x7f]);
    assert_eq!(int(INTEGER, 128), [0x02, 0x02, 0x00, 0x80]);
    assert_eq!(int(INTEGER, 256), [0x02, 0x02, 0x01, 0x00]);
    assert_eq!(int(INTEGER, -1), [0x02, 0x01, 0xff]);
    assert_eq!(int(INTEGER, -128), [0x02, 0x01, 0x80]);
    assert_eq!(int(INTEGER, -129), [0x02, 0x02, 0xff, 0x7f]);
    for v in [0i64, 1, -1, 127, 128, -128, -129, 65_535, 2_147_483_647, -2_147_483_648, i64::MAX, i64::MIN] {
        let bytes = int(INTEGER, v);
        assert_eq!(read(&bytes).unwrap().0.int().unwrap(), v, "{v}");
    }
}

#[test]
fn booleans_and_text() {
    assert_eq!(boolean(BOOLEAN, true), [0x01, 0x01, 0xff]);
    assert!(read(&[0x01, 0x01, 0x01]).unwrap().0.boolean().unwrap());
    assert!(!read(&[0x01, 0x01, 0x00]).unwrap().0.boolean().unwrap());
    assert_eq!(read(&[0x01, 0x02, 0, 0]).unwrap().0.boolean(), Err(Error::Malformed));
    assert_eq!(read(&[0x04, 0x03, b'a', 0xff, b'b']).unwrap().0.text(), "a\u{fffd}b");
}

#[test]
fn a_sequence_holds_its_parts_in_order() {
    let seq = constructed(SEQUENCE, &[int(INTEGER, 3), octets(OCTET_STRING, b"cn=admin"), boolean(BOOLEAN, false)]);
    let (outer, rest) = read(&seq).unwrap();
    assert!(rest.is_empty() && outer.is(SEQUENCE));
    let parts = items(outer.content).unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!((parts[0].int().unwrap(), parts[1].text(), parts[2].boolean().unwrap()), (3, "cn=admin".to_string(), false));
}

#[test]
fn what_is_cut_short_or_in_a_form_ldap_does_not_use_is_refused() {
    assert_eq!(read(&[]), Err(Error::Short));
    assert_eq!(read(&[0x04]), Err(Error::Short));
    assert_eq!(read(&[0x04, 0x05, 1, 2]), Err(Error::Short));
    assert_eq!(read(&[0x04, 0x82, 0x01]), Err(Error::Short));
    // Indefinite length, a length of five bytes, a tag of the long form
    assert_eq!(read(&[0x30, 0x80, 0, 0]), Err(Error::Unsupported));
    assert_eq!(read(&[0x04, 0x85, 0, 0, 0, 0, 1, 0]), Err(Error::Unsupported));
    assert_eq!(read(&[0x1f, 0x01, 0x00]), Err(Error::Unsupported));
    // A length that claims more than the largest buffer can hold, with nothing behind it
    assert_eq!(read(&[0x04, 0x84, 0xff, 0xff, 0xff, 0xff, 1]), Err(Error::Short));
    assert_eq!(items(&[0x04, 0x01]), Err(Error::Short));
    // Integers of no size or of more than eight bytes
    assert_eq!(read(&[0x02, 0x00]).unwrap().0.int(), Err(Error::Malformed));
    assert_eq!(read(&[0x02, 0x09, 1, 1, 1, 1, 1, 1, 1, 1, 1]).unwrap().0.int(), Err(Error::Malformed));
}

#[test]
fn nothing_the_bytes_say_makes_it_panic() {
    // Every prefix and a spread of byte values at each place: only errors, never a panic
    let sample = constructed(SEQUENCE, &[int(INTEGER, 300), octets(OCTET_STRING, &[9; 200]), constructed(SET, &[octets(OCTET_STRING, b"x")])]);
    for end in 0..=sample.len() {
        let _ = items(&sample[..end]);
    }
    for i in 0..sample.len() {
        for b in [0u8, 1, 0x7f, 0x80, 0x81, 0x84, 0xff] {
            let mut bytes = sample.clone();
            bytes[i] = b;
            let _ = items(&bytes);
            if let Ok((t, _)) = read(&bytes) {
                let _ = items(t.content);
                let _ = t.int();
                let _ = t.boolean();
            }
        }
    }
}
