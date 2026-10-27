use super::*;

fn eq(a: &str, v: &str) -> Filter {
    Filter::Equal(a.into(), v.as_bytes().to_vec())
}

#[test]
fn the_filters_of_active_directory_and_openldap_are_read() {
    assert_eq!(parse("(uid=amy)").unwrap(), eq("uid", "amy"));
    assert_eq!(
        parse("(&(objectClass=person)(|(sAMAccountName=amy)(userPrincipalName=amy@example.com)))").unwrap(),
        Filter::And(vec![eq("objectClass", "person"), Filter::Or(vec![eq("sAMAccountName", "amy"), eq("userPrincipalName", "amy@example.com")])])
    );
    assert_eq!(
        parse("(!(userAccountControl:1.2.840.113556.1.4.803:=2))").unwrap(),
        Filter::Not(Box::new(Filter::Extensible {
            rule: Some("1.2.840.113556.1.4.803".into()),
            attr: Some("userAccountControl".into()),
            value: b"2".to_vec(),
            dn_attrs: false,
        }))
    );
    // Nested groups in Active Directory: the in-chain rule
    assert_eq!(
        parse("(member:1.2.840.113556.1.4.1941:=CN=Amy,OU=People,DC=example,DC=com)").unwrap(),
        Filter::Extensible {
            rule: Some("1.2.840.113556.1.4.1941".into()),
            attr: Some("member".into()),
            value: b"CN=Amy,OU=People,DC=example,DC=com".to_vec(),
            dn_attrs: false
        }
    );
    assert_eq!(parse("(cn:dn:=Amy)").unwrap(), Filter::Extensible { rule: None, attr: Some("cn".into()), value: b"Amy".to_vec(), dn_attrs: true });
    assert_eq!(
        parse("(:caseExactMatch:=Amy)").unwrap(),
        Filter::Extensible { rule: Some("caseExactMatch".into()), attr: None, value: b"Amy".to_vec(), dn_attrs: false }
    );
    assert_eq!(parse("(mail=*)").unwrap(), Filter::Present("mail".into()));
    assert_eq!(
        parse("(cn=a*b*c)").unwrap(),
        Filter::Substrings { attr: "cn".into(), initial: Some(b"a".to_vec()), any: vec![b"b".to_vec()], last: Some(b"c".to_vec()) }
    );
    assert_eq!(parse("(cn=*b*)").unwrap(), Filter::Substrings { attr: "cn".into(), initial: None, any: vec![b"b".to_vec()], last: None });
    assert_eq!(parse("(age>=18)").unwrap(), Filter::GreaterOrEqual("age".into(), b"18".to_vec()));
    assert_eq!(parse("(age<=18)").unwrap(), Filter::LessOrEqual("age".into(), b"18".to_vec()));
    assert_eq!(parse("(cn~=amy)").unwrap(), Filter::Approx("cn".into(), b"amy".to_vec()));
    assert_eq!(parse("  (uid=amy)  ").unwrap(), eq("uid", "amy"));
}

#[test]
fn escapes_in_a_value_are_one_byte_each() {
    assert_eq!(parse(r"(cn=a\28b\29c\2ad\5ce)").unwrap(), eq("cn", "a(b)c*d\\e"));
    assert_eq!(parse(r"(cn=\c3\a9)").unwrap(), Filter::Equal("cn".into(), vec![0xc3, 0xa9]));
    // An escaped star is a star, not a wildcard
    assert_eq!(parse(r"(cn=a\2ab)").unwrap(), eq("cn", "a*b"));
}

#[test]
fn text_that_is_not_a_filter_is_refused_and_says_why() {
    for bad in [
        "",
        "uid=amy",
        "(uid=amy",
        "(uid=amy))",
        "(uid)",
        "(&)",
        "(|)",
        "(=amy)",
        "(u id=amy)",
        "(uid=a(b)",
        "(uid=a\\)",
        "(uid=a\\zz)",
        "(uid=amy)(uid=bob)",
        "(:=x)",
        "(cn:dn:dn:=x)",
    ] {
        assert!(parse(bad).is_err(), "{bad}");
    }
    let deep = format!("{}(uid=a){}", "(&".repeat(40), ")".repeat(40));
    assert_eq!(parse(&deep), Err(Invalid("The filter is nested too deeply")));
    assert_eq!(parse(&format!("(uid={})", "a".repeat(5000))), Err(Invalid("The filter is too long")));
}

#[test]
fn a_user_name_is_only_ever_a_value() {
    let template = "(&(objectClass=person)(|(uid={username})(mail={username})))";
    let filter = |name: &str| for_user(template, name).unwrap();
    let expected = |name: &[u8]| {
        Filter::And(vec![eq("objectClass", "person"), Filter::Or(vec![Filter::Equal("uid".into(), name.to_vec()), Filter::Equal("mail".into(), name.to_vec())])])
    };
    assert_eq!(filter("amy"), expected(b"amy"));
    // What would change the filter is only a name that finds nobody
    for name in ["*", "*)(uid=*", "amy)(|(uid=*", "a\\b", "amy*", "(", ")", "\0", "ami\u{e9}", "amy\n", "a=b", "uid=*)(&"] {
        assert_eq!(filter(name), expected(name.as_bytes()), "{name:?}");
    }
    // The text has to ask for the person
    assert_eq!(for_user("(objectClass=person)", "amy"), Err(Invalid("The filter has to contain {username}")));
}

#[test]
fn escaping_writes_what_must_not_be_bare() {
    assert_eq!(escape("amy"), "amy");
    assert_eq!(escape("a*b(c)d\\e"), r"a\2ab\28c\29d\5ce");
    assert_eq!(escape("\0\n"), r"\00\0a");
    assert_eq!(escape("\u{e9}"), r"\c3\a9");
    // Written and read again, any text comes back as it was
    for s in ["", "amy", "a*b", "()\\", "\u{4e2d}\u{6587}", "tab\there"] {
        assert_eq!(parse(&format!("(cn={})", escape(s))).unwrap(), Filter::Equal("cn".into(), s.as_bytes().to_vec()), "{s:?}");
    }
}

#[test]
fn every_kind_of_filter_survives_the_wire() {
    for text in [
        "(uid=amy)",
        "(&(a=1)(|(b=2)(c=3))(!(d=4)))",
        "(mail=*)",
        "(cn=a*b*c*d)",
        "(cn=*x)",
        "(cn=x*)",
        "(a>=1)",
        "(a<=1)",
        "(a~=1)",
        "(member:1.2.840.113556.1.4.1941:=cn=x)",
        "(cn:dn:=x)",
        "(:2.5.13.5:=x)",
    ] {
        let parsed = parse(text).unwrap();
        let wire = parsed.to_ber();
        let (tlv, rest) = ber::read(&wire).unwrap();
        assert!(rest.is_empty());
        assert_eq!(Filter::from_ber(tlv).unwrap(), parsed, "{text}");
    }
    // The bytes of the simplest one
    assert_eq!(eq("uid", "amy").to_ber(), [0xa3, 0x0a, 0x04, 0x03, b'u', b'i', b'd', 0x04, 0x03, b'a', b'm', b'y']);
    assert_eq!(Filter::Present("mail".into()).to_ber(), [0x87, 0x04, b'm', b'a', b'i', b'l']);
}
