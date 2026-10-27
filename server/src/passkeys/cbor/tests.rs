use super::*;

fn h(hex: &str) -> Vec<u8> {
    hex::decode(hex).unwrap()
}

#[test]
fn the_examples_of_rfc_8949_appendix_a_are_read() {
    for (hex, want) in [
        ("00", Value::Int(0)),
        ("17", Value::Int(23)),
        ("1818", Value::Int(24)),
        ("1903e8", Value::Int(1000)),
        ("1a000f4240", Value::Int(1_000_000)),
        ("1b000000e8d4a51000", Value::Int(1_000_000_000_000)),
        ("20", Value::Int(-1)),
        ("29", Value::Int(-10)),
        ("3903e7", Value::Int(-1000)),
        ("3b7fffffffffffffff", Value::Int(i64::MIN)),
        ("1b7fffffffffffffff", Value::Int(i64::MAX)),
        ("40", Value::Bytes(vec![])),
        ("4401020304", Value::Bytes(vec![1, 2, 3, 4])),
        ("60", Value::Text(String::new())),
        ("6161", Value::Text("a".into())),
        ("6449455446", Value::Text("IETF".into())),
        ("62c3bc", Value::Text("ü".into())),
        ("80", Value::Array(vec![])),
        ("83010203", Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])),
        ("a0", Value::Map(vec![])),
        ("a201020304", Value::Map(vec![(Value::Int(1), Value::Int(2)), (Value::Int(3), Value::Int(4))])),
        ("f4", Value::Bool(false)),
        ("f5", Value::Bool(true)),
        ("f6", Value::Null),
    ] {
        assert_eq!(parse(&h(hex)).unwrap(), want, "{hex}");
    }
    let nested = parse(&h("a26161016162820203")).unwrap();
    assert_eq!(nested.get_text("a").and_then(Value::as_int), Some(1));
    assert_eq!(nested.get_text("b"), Some(&Value::Array(vec![Value::Int(2), Value::Int(3)])));
    assert_eq!(nested.get_text("c"), None);
}

#[test]
fn a_cose_key_is_a_map_of_integer_keys() {
    // {1: 2, 3: -7, -1: 1, -2: h'..32 bytes', -3: h'..32 bytes'}
    let mut cose = h("a5010203262001215820");
    cose.extend([0xaa; 32]);
    cose.extend(h("225820"));
    cose.extend([0xbb; 32]);
    let v = parse(&cose).unwrap();
    assert_eq!((v.get(1).and_then(Value::as_int), v.get(3).and_then(Value::as_int), v.get(-1).and_then(Value::as_int)), (Some(2), Some(-7), Some(1)));
    assert_eq!(v.get(-2).and_then(Value::as_bytes), Some(&[0xaa; 32][..]));
    assert_eq!(v.get(-3).and_then(Value::as_bytes), Some(&[0xbb; 32][..]));
    assert_eq!(v.get(9), None);
    assert_eq!(Value::Int(1).get(1), None, "only a map has keys");
}

#[test]
fn what_this_reader_does_not_take_is_refused() {
    for (what, hex) in [
        ("a float", "f90000"),
        ("a double", "fb3ff199999999999a"),
        ("a tag", "c11a514b67b0"),
        ("an indefinite array", "9fff"),
        ("an indefinite byte string", "5f4101ff"),
        ("an indefinite map", "bf6161ffff"),
        ("an integer past 63 bits", "1bffffffffffffffff"),
        ("a negative integer past 64 bits", "3bffffffffffffffff"),
        ("undefined", "f7"),
        ("a simple value", "f0"),
        ("a reserved additional number", "1c"),
    ] {
        assert_eq!(parse(&h(hex)), Err(Error::Unsupported), "{what}");
    }
    assert_eq!(parse(&h("62c328")), Err(Error::Text), "text that isn't UTF-8");
}

#[test]
fn a_message_that_ends_early_or_goes_on_is_refused() {
    assert_eq!(parse(&[]), Err(Error::Truncated));
    assert_eq!(parse(&h("1903")), Err(Error::Truncated));
    assert_eq!(parse(&h("4401020304")[..4]), Err(Error::Truncated));
    assert_eq!(parse(&h("8301")), Err(Error::Truncated));
    assert_eq!(parse(&h("a1 01".replace(' ', "").as_str())), Err(Error::Truncated));
    assert_eq!(parse(&h("0000")), Err(Error::Trailing));
    // parse_prefix says how much it read: the public key in authenticator data is followed by more
    let (v, used) = parse_prefix(&h("a1010200ff")).unwrap();
    assert_eq!((v, used), (Value::Map(vec![(Value::Int(1), Value::Int(2))]), 3));
}

#[test]
fn a_lying_length_or_a_deep_or_big_message_cannot_use_up_memory_or_the_stack() {
    // A length of billions of elements or bytes with nothing behind it
    assert_eq!(parse(&h("9bffffffffffffffff")), Err(Error::TooBig).or(Err(Error::Truncated)));
    assert!(parse(&h("9a7fffffff")).is_err());
    assert!(parse(&h("5a7fffffff")).is_err());
    assert!(parse(&h("5affffffff")).is_err());
    // Nested more deeply than anything real
    let deep = |n: usize| {
        let mut v = vec![0x81; n];
        v.push(0x00);
        v
    };
    assert!(parse(&deep(MAX_DEPTH)).is_ok());
    assert_eq!(parse(&deep(MAX_DEPTH + 1)), Err(Error::TooBig));
    assert_eq!(parse(&deep(100_000)), Err(Error::TooBig));
    // More items than a message may hold
    let mut many = h("9901 2c".replace(' ', "").as_str());
    many.extend(vec![0u8; 300]);
    assert_eq!(parse(&many), Err(Error::TooBig));
    // A string longer than the limit, which the message really holds
    let mut long = vec![0x59];
    long.extend(((MAX_STRING + 1) as u16).to_be_bytes());
    long.extend(vec![1u8; MAX_STRING + 1]);
    assert_eq!(parse(&long), Err(Error::TooBig));
    let mut ok = vec![0x59];
    ok.extend((MAX_STRING as u16).to_be_bytes());
    ok.extend(vec![1u8; MAX_STRING]);
    assert!(parse(&ok).is_ok());
}

#[test]
fn nothing_the_bytes_say_makes_it_panic() {
    // Every prefix of a valid message, and a stream of random bytes and of mutated ones: an error or a value, never a panic
    let valid = h("a26161016162820203");
    assert!(parse(&valid).is_ok());
    for n in 0..valid.len() {
        let _ = parse(&valid[..n]);
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..20_000 {
        let len = (next() % 64) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let _ = parse(&bytes);
        let _ = parse_prefix(&bytes);
        let mut mutated = valid.clone();
        let at = (next() as usize) % mutated.len();
        mutated[at] = next() as u8;
        let _ = parse(&mutated);
    }
}
