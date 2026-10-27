use std::time::Duration;

use super::{
    Connection, Error, Scope, Search, Security,
    filter::{self, Filter},
    testserver::{Directory, Options, Person, Server, serve},
};

const AMY_DN: &str = "uid=amy,ou=people,dc=example,dc=com";
const BIND_DN: &str = "cn=reader,dc=example,dc=com";

fn people() -> Directory {
    Directory::default()
        .with(Person::new(BIND_DN, &[("cn", &["reader"])]), Some("reader-secret"))
        .with(
            Person::new(
                AMY_DN,
                &[("uid", &["amy"]), ("cn", &["Amy Wong"]), ("mail", &["amy@example.com"]), ("memberOf", &["cn=staff,ou=groups,dc=example,dc=com"])],
            ),
            Some("amy-pw"),
        )
        .with(Person::new("uid=ben,ou=people,dc=example,dc=com", &[("uid", &["ben"]), ("cn", &["Ben"]), ("mail", &["ben@example.com"])]), Some("ben-pw"))
        .with(Person::new("cn=staff,ou=groups,dc=example,dc=com", &[("cn", &["staff"]), ("member", &[AMY_DN, "cn=interns,ou=groups,dc=example,dc=com"])]), None)
        .with(Person::new("cn=interns,ou=groups,dc=example,dc=com", &[("cn", &["interns"]), ("member", &["uid=ben,ou=people,dc=example,dc=com"])]), None)
}

fn search(text: &str) -> Search {
    Search {
        base: "dc=example,dc=com".into(),
        scope: Scope::Subtree,
        filter: filter::parse(text).unwrap(),
        attributes: Vec::new(),
        size_limit: 0,
        page_size: 0,
        max_entries: 1000,
    }
}

async fn open(server: &Server) -> Connection {
    Connection::connect(&server.config()).await.unwrap()
}

#[tokio::test]
async fn a_bind_with_the_right_password_works_and_a_wrong_one_does_not() {
    let server = serve(people(), Options::default()).await;
    let mut conn = open(&server).await;
    conn.bind(BIND_DN, "reader-secret").await.unwrap();
    let err = conn.bind(AMY_DN, "nope").await.unwrap_err();
    assert!(err.is_bad_credentials() && !err.is_unavailable(), "{err:?}");
    conn.bind(AMY_DN, "amy-pw").await.unwrap();
    conn.unbind().await;
}

#[tokio::test]
async fn a_bind_without_a_password_is_never_sent() {
    // Servers take it as an anonymous bind and say it worked: letting anyone in as anyone
    let server = serve(people(), Options::default()).await;
    let mut conn = open(&server).await;
    assert_eq!(conn.bind(AMY_DN, "").await, Err(Error::Invalid("A password is needed".into())));
    assert_eq!(conn.bind("", "x").await, Err(Error::Invalid("A bind needs the name of an entry".into())));
    assert!(server.log.binds.lock().is_empty());
}

#[tokio::test]
async fn a_search_finds_the_entries_with_their_attributes() {
    let server = serve(people(), Options::default()).await;
    let mut conn = open(&server).await;
    let found = conn.search(&search("(&(objectClass=*)(|(uid=amy)(mail=amy@example.com)))")).await;
    // (no objectClass on these entries: nothing)
    assert!(found.unwrap().entries.is_empty());
    let found = conn.search(&search("(|(uid=amy)(mail=ben@example.com))")).await.unwrap();
    let dns: Vec<&str> = found.entries.iter().map(|e| e.dn.as_str()).collect();
    assert_eq!(dns, vec![AMY_DN, "uid=ben,ou=people,dc=example,dc=com"]);
    let amy = &found.entries[0];
    assert_eq!((amy.text("CN").as_deref(), amy.text("mail").as_deref()), (Some("Amy Wong"), Some("amy@example.com")));
    assert_eq!(amy.texts("memberOf"), vec!["cn=staff,ou=groups,dc=example,dc=com"]);
    assert!(amy.values("nothing").is_empty() && amy.text("nothing").is_none());
    assert!(!found.truncated);
    // Only the attributes asked for
    let mut one = search("(uid=amy)");
    one.attributes = vec!["mail".into()];
    let found = conn.search(&one).await.unwrap();
    assert_eq!((found.entries[0].text("mail").as_deref(), found.entries[0].text("cn")), (Some("amy@example.com"), None));
}

#[tokio::test]
async fn a_user_name_that_is_a_filter_finds_nobody_and_the_server_sees_one_comparison() {
    let server = serve(people(), Options::default()).await;
    let mut conn = open(&server).await;
    for name in ["*", "*)(uid=*", "amy)(|(uid=*", "am*", "\\2a"] {
        let mut s = search("(uid=amy)");
        s.filter = filter::for_user("(|(uid={username})(mail={username}))", name).unwrap();
        assert!(conn.search(&s).await.unwrap().entries.is_empty(), "{name}");
    }
    let seen = server.log.searches.lock().clone();
    assert_eq!(seen.len(), 5);
    // What arrived is the same two comparisons with the name as a value, not a filter of the name's making
    for ((_, filter), name) in seen.iter().zip(["*", "*)(uid=*", "amy)(|(uid=*", "am*", "\\2a"]) {
        assert_eq!(
            *filter,
            Filter::Or(vec![Filter::Equal("uid".into(), name.as_bytes().to_vec()), Filter::Equal("mail".into(), name.as_bytes().to_vec())]),
            "{name}"
        );
    }
}

#[tokio::test]
async fn the_chain_rule_finds_the_members_of_groups_inside_groups() {
    let server = serve(people(), Options::default()).await;
    let mut conn = open(&server).await;
    // Ben is in interns, which is in staff
    let found = conn.search(&search("(member:1.2.840.113556.1.4.1941:=uid=ben,ou=people,dc=example,dc=com)")).await.unwrap();
    let mut names: Vec<String> = found.entries.iter().filter_map(|e| e.text("cn")).collect();
    names.sort();
    assert_eq!(names, vec!["interns", "staff"]);
}

#[tokio::test]
async fn a_limit_asked_for_ends_the_search_there_and_says_so() {
    let server = serve(people(), Options::default()).await;
    let mut conn = open(&server).await;
    let mut s = search("(|(uid=amy)(uid=ben))");
    s.size_limit = 1;
    let found = conn.search(&s).await.unwrap();
    assert_eq!(found.entries.len(), 1);
    assert!(found.truncated);
    // A directory's own limit, without one asked for, is its refusal
    let server = serve(people(), Options { page_limit: 1, ..Options::default() }).await;
    let mut conn = open(&server).await;
    let err = conn.search(&search("(|(uid=amy)(uid=ben))")).await.unwrap_err();
    assert!(matches!(err, Error::Server { code: 4, .. }), "{err:?}");
}

#[tokio::test]
async fn a_search_that_pages_collects_every_page() {
    let mut directory = people();
    for n in 0..11 {
        directory = directory.with(Person::new(&format!("uid=u{n:02},ou=people,dc=example,dc=com"), &[("uid", &[&format!("u{n:02}")]), ("cn", &["x"])]), None);
    }
    let server = serve(directory, Options { page_limit: 3, ..Options::default() }).await;
    let mut conn = open(&server).await;
    let mut s = search("(uid=u*)");
    s.page_size = 3;
    let found = conn.search(&s).await.unwrap();
    assert_eq!(found.entries.len(), 11);
    assert_eq!(found.entries[10].dn, "uid=u10,ou=people,dc=example,dc=com");
    // More than the most that is taken is refused, and not half read
    s.max_entries = 5;
    assert_eq!(conn.search(&s).await.unwrap_err(), Error::TooLarge);
}

#[tokio::test]
async fn referrals_are_not_followed_and_an_answer_of_referral_is_an_error() {
    let server = serve(people(), Options { referral: true, ..Options::default() }).await;
    let mut conn = open(&server).await;
    // A reference among the entries is left out
    assert_eq!(conn.search(&search("(uid=amy)")).await.unwrap().entries.len(), 1);
    let server = serve(people(), Options { result_code: Some(10), ..Options::default() }).await;
    let mut conn = open(&server).await;
    assert!(matches!(conn.search(&search("(uid=amy)")).await.unwrap_err(), Error::Server { code: 10, .. }));
}

#[tokio::test]
async fn a_server_that_cannot_be_reached_or_does_not_answer_is_said_to_be_unavailable() {
    // Nothing listens
    let closed = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let config = super::Config { host: "127.0.0.1".into(), port: closed, security: Security::None, roots: vec![], timeout: Duration::from_millis(500) };
    let err = Connection::connect(&config).await.err().unwrap();
    // (a closed port is refused at once, or, on some systems, after the time allowed)
    assert!(matches!(err, Error::Unreachable(_) | Error::Timeout) && err.is_unavailable(), "{err:?}");
    // Says nothing
    let silent = serve(people(), Options { silent: true, ..Options::default() }).await;
    let mut conn = open(&silent).await;
    let err = conn.bind(BIND_DN, "reader-secret").await.unwrap_err();
    assert_eq!(err, Error::Timeout);
    assert!(err.is_unavailable());
    // Slower than the time allowed
    let slow = serve(people(), Options { delay: Duration::from_millis(1200), ..Options::default() }).await;
    let mut config = slow.config();
    config.timeout = Duration::from_millis(300);
    let mut conn = Connection::connect(&config).await.unwrap();
    assert_eq!(conn.bind(BIND_DN, "reader-secret").await.unwrap_err(), Error::Timeout);
    // Not LDAP at all
    let garbage = serve(people(), Options { garbage: true, ..Options::default() }).await;
    let mut conn = open(&garbage).await;
    let err = conn.bind(BIND_DN, "reader-secret").await.unwrap_err();
    assert!(matches!(err, Error::Protocol(_)) && err.is_unavailable(), "{err:?}");
}

#[tokio::test]
async fn an_answer_that_claims_to_be_huge_is_refused_before_it_is_read() {
    let server = serve(people(), Options { claim_length: Some(900 * 1024 * 1024), ..Options::default() }).await;
    let mut conn = open(&server).await;
    assert_eq!(conn.search(&search("(uid=amy)")).await.unwrap_err(), Error::TooLarge);
}

#[tokio::test]
async fn the_connection_is_secure_with_ldaps_and_with_starttls() {
    let ldaps = serve(people(), Options { ldaps: true, ..Options::default() }).await;
    let mut conn = Connection::connect(&ldaps.secure_config(Security::Ldaps)).await.unwrap();
    conn.bind(BIND_DN, "reader-secret").await.unwrap();
    assert_eq!(conn.search(&search("(uid=amy)")).await.unwrap().entries.len(), 1);
    let starttls = serve(people(), Options { starttls: true, ..Options::default() }).await;
    let mut conn = Connection::connect(&starttls.secure_config(Security::StartTls)).await.unwrap();
    conn.bind(BIND_DN, "reader-secret").await.unwrap();
    assert_eq!(conn.search(&search("(uid=amy)")).await.unwrap().entries.len(), 1);
    // A server that doesn't offer StartTLS: nothing is sent in the clear afterwards
    let plain = serve(people(), Options::default()).await;
    let err = Connection::connect(&plain.secure_config(Security::StartTls)).await.err().unwrap();
    assert!(matches!(err, Error::Server { code: 2, .. }), "{err:?}");
    assert!(plain.log.binds.lock().is_empty());
}

#[tokio::test]
async fn a_certificate_that_is_not_trusted_is_refused_and_says_what_to_do() {
    let server = serve(people(), Options { ldaps: true, ..Options::default() }).await;
    // Without the authority's certificate
    let mut config = server.secure_config(Security::Ldaps);
    config.roots.clear();
    let err = Connection::connect(&config).await.err().unwrap();
    assert!(matches!(&err, Error::Tls(m) if m.contains("isn't trusted")), "{err:?}");
    assert!(err.is_unavailable());
    // With another authority's
    let other = serve(people(), Options { ldaps: true, ..Options::default() }).await;
    let mut config = server.secure_config(Security::Ldaps);
    config.roots = vec![other.root.clone()];
    assert!(matches!(Connection::connect(&config).await.err().unwrap(), Error::Tls(_)));
    // A server that isn't speaking TLS
    let plain = serve(people(), Options::default()).await;
    assert!(Connection::connect(&plain.secure_config(Security::Ldaps)).await.is_err());
}
