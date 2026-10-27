// Directory sign-in against a directory that speaks LDAP on a local port (ldap/testserver.rs): who may sign in and as
// whom, accounts made and linked, groups that follow the directory, people who leave, and what the settings refuse
use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::HeaderMap,
};
use serde_json::{Value, json};

use super::*;
use crate::{
    auth::Admin,
    ldap::testserver::{Directory, Options, Person, Server, serve},
    signin::LoginReq,
    testutil,
};

const BIND: &str = "cn=reader,dc=example,dc=com";
const AMY: &str = "uid=amy,ou=people,dc=example,dc=com";
const BEN: &str = "uid=ben,ou=people,dc=example,dc=com";
const STAFF: &str = "cn=staff,ou=groups,dc=example,dc=com";
const ADMINS: &str = "cn=admins,ou=groups,dc=example,dc=com";

fn person(dn: &str, uid: &str, name: &str, id: &str, groups: &[&str]) -> Person {
    Person::new(
        dn,
        &[("objectClass", &["person"]), ("uid", &[uid]), ("cn", &[name]), ("mail", &[&format!("{uid}@example.com")]), ("entryUUID", &[id]), ("memberOf", groups)],
    )
}

fn directory() -> Directory {
    Directory::default()
        .with(Person::new(BIND, &[("cn", &["reader"])]), Some("reader-secret"))
        .with(person(AMY, "amy", "Amy Wong", "11111111-aaaa", &[STAFF, ADMINS]), Some("amy-pw"))
        .with(person(BEN, "ben", "Ben Li", "22222222-bbbb", &[STAFF]), Some("ben-pw"))
        .with(Person::new(STAFF, &[("cn", &["staff"]), ("member", &[AMY, BEN])]), None)
}

fn addr() -> ConnectInfo<std::net::SocketAddr> {
    ConnectInfo("203.0.113.9:5000".parse().unwrap())
}

struct World {
    env: testutil::TestEnv,
    server: Server,
    staff: i64,
    admins: i64,
}

async fn group(env: &testutil::TestEnv, name: &str) -> i64 {
    let (id,): (i64,) = sqlx::query_as("INSERT INTO groups (name, created_at) VALUES (?, 0) RETURNING id").bind(name).fetch_one(&env.st.db).await.unwrap();
    id
}

fn settings_for(server: &Server, staff: i64, admins: i64) -> Settings {
    Settings {
        enabled: true,
        url: format!("ldap://127.0.0.1:{}", server.port),
        starttls: false,
        bind_dn: BIND.into(),
        bind_password: crate::secrets::seal(settings::SEAL, "reader-secret"),
        user_base: "dc=example,dc=com".into(),
        user_filter: "(&(objectClass=person)(|(uid={username})(mail={username})))".into(),
        attr_id: "entryUUID".into(),
        attr_username: "uid".into(),
        attr_name: "cn".into(),
        attr_email: "mail".into(),
        group_mode: GroupMode::MemberOf,
        mappings: vec![Mapping { dn: STAFF.into(), group_id: staff }, Mapping { dn: ADMINS.to_ascii_uppercase(), group_id: admins }],
        ..Settings::default()
    }
}

async fn world_with(options: Options) -> World {
    let env = testutil::env().await;
    let server = serve(directory(), options).await;
    let (staff, admins) = (group(&env, "Staff").await, group(&env, "Admins").await);
    let w = World { env, server, staff, admins };
    w.save(|_| {}).await;
    w
}

async fn world() -> World {
    world_with(Options::default()).await
}

impl World {
    /// Saves the settings with a change made to them
    async fn save(&self, change: impl FnOnce(&mut Settings)) {
        let mut s = settings_for(&self.server, self.staff, self.admins);
        change(&mut s);
        settings::store(&mut self.env.st.db.acquire().await.unwrap(), &s).await.unwrap();
    }

    async fn login(&self, name: &str, password: &str) -> Result<axum::response::Response, crate::error::AppError> {
        crate::signin::login(State(self.env.st.clone()), addr(), HeaderMap::new(), Json(LoginReq { username: name.into(), password: password.into() })).await
    }

    async fn account(&self, name: &str) -> Option<(i64, String, bool, bool)> {
        sqlx::query_as("SELECT id, source, disabled, directory_disabled FROM users WHERE username = ?").bind(name).fetch_optional(&self.env.st.db).await.unwrap()
    }

    async fn members(&self, group: i64) -> Vec<String> {
        sqlx::query_scalar("SELECT u.username FROM group_members m JOIN users u ON u.id = m.user_id WHERE m.group_id = ? ORDER BY u.username")
            .bind(group)
            .fetch_all(&self.env.st.db)
            .await
            .unwrap()
    }

    async fn sessions(&self, user: i64) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = ?").bind(user).fetch_one(&self.env.st.db).await.unwrap()
    }

    /// Changes what the directory holds
    fn directory(&self, change: impl FnOnce(&mut Directory)) {
        change(&mut self.server.directory.lock());
    }
}

async fn body(res: axum::response::Response) -> Value {
    serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap()
}

// ───────────── Signing in ─────────────

#[tokio::test]
async fn a_person_of_the_directory_signs_in_and_gets_an_account_with_their_groups() {
    let w = world().await;
    let res = w.login("amy", "amy-pw").await.unwrap();
    assert!(res.headers().get(axum::http::header::SET_COOKIE).is_some());
    let me = body(res).await;
    assert_eq!(me["username"], "amy");
    assert_eq!(me["display_name"], "Amy Wong");
    let (id, source, disabled, by_directory) = w.account("amy").await.unwrap();
    assert_eq!((source.as_str(), disabled, by_directory), ("ldap", false, false));
    // She has no password here, a personal space, the directory's name and address, and her groups follow the directory
    let (hash, email): (String, String) = sqlx::query_as("SELECT password_hash, email FROM users WHERE id = ?").bind(id).fetch_one(&w.env.st.db).await.unwrap();
    assert_eq!((hash.as_str(), email.as_str()), (crate::auth::NO_PASSWORD, "amy@example.com"));
    assert_eq!(w.members(w.staff).await, vec!["amy"]);
    assert_eq!(w.members(w.admins).await, vec!["amy"], "the group's DN is compared without regard to case");
    let method: String = sqlx::query_scalar("SELECT method FROM sessions WHERE user_id = ?").bind(id).fetch_one(&w.env.st.db).await.unwrap();
    assert_eq!(method, "ldap");
    let source: String = sqlx::query_scalar("SELECT source FROM groups WHERE id = ?").bind(w.staff).fetch_one(&w.env.st.db).await.unwrap();
    assert_eq!(source, "ldap");
    // The address works as the name too (the filter says so); the same person, the same account
    w.login("amy@example.com", "amy-pw").await.unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE source = 'ldap'").fetch_one(&w.env.st.db).await.unwrap();
    assert_eq!(n, 1);
    // The password is never kept: nothing here but the directory's bind
    let kept: Vec<(String, String)> = w.server.log.binds.lock().clone();
    assert!(kept.iter().any(|(dn, pw)| dn == AMY && pw == "amy-pw"));
}

#[tokio::test]
async fn a_wrong_password_an_unknown_name_and_a_name_that_is_a_filter_sign_nobody_in() {
    let w = world().await;
    for (name, password) in [("amy", "nope"), ("nobody", "amy-pw"), ("*", "amy-pw"), ("*)(uid=*", "amy-pw"), ("amy", ""), ("am*", "amy-pw")] {
        let err = w.login(name, password).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED, "{name:?} {password:?}");
        assert_eq!(err.message, "Incorrect username or password");
    }
    assert!(w.account("amy").await.is_none());
    // An empty password was never sent to the directory as a bind
    assert!(w.server.log.binds.lock().iter().all(|(dn, pw)| dn == BIND || !pw.is_empty()));
    // The failures count like any, so the limits of attempts hold
    let key = format!("u:amy|{}", crate::auth::limit_key_ip("203.0.113.9"));
    assert!(st_failures(&w, &key) >= 2);
}

fn st_failures(w: &World, key: &str) -> usize {
    w.env.st.part::<crate::auth::Memory>().login_failures.lock().get(key).map_or(0, Vec::len)
}

#[tokio::test]
async fn an_account_made_here_keeps_its_own_password_and_never_goes_to_the_directory() {
    let w = world().await;
    let local = w.env.user("amy", true).await;
    // The directory has an Amy with another password: the local Amy's password is the only one
    let err = w.login("amy", "amy-pw").await.unwrap_err();
    assert_eq!(err.message, "Incorrect username or password");
    w.login("amy", testutil::password()).await.unwrap();
    assert!(w.server.log.binds.lock().is_empty(), "the directory wasn't asked");
    assert_eq!(w.account("amy").await.unwrap().1, "password");
    let _ = local;

    // A person of the directory whose name an account made here has is refused, not given that account
    w.directory(|d| d.people.push(person("uid=amy2,ou=people,dc=example,dc=com", "amy", "Another Amy", "33333333-cccc", &[])));
    w.directory(|d| {
        d.passwords.insert("uid=amy2,ou=people,dc=example,dc=com".into(), "other-pw".into());
        d.people.retain(|p| p.dn != AMY);
    });
    // (typed as the address, which no account here has for a name)
    let err = w.login("amy@example.com", "other-pw").await.unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN, "{}", err.message);
    assert_eq!(w.account("amy").await.unwrap().1, "password");
}

#[tokio::test]
async fn a_person_renamed_in_the_directory_is_the_same_account() {
    let w = world().await;
    w.login("amy", "amy-pw").await.unwrap();
    let (id, ..) = w.account("amy").await.unwrap();
    // Amy becomes Amy Smith: another user name and name, the same id
    w.directory(|d| {
        let amy = d.people.iter_mut().find(|p| p.dn == AMY).unwrap();
        amy.attributes.iter_mut().find(|(k, _)| k == "uid").unwrap().1 = vec!["asmith".into()];
        amy.attributes.iter_mut().find(|(k, _)| k == "cn").unwrap().1 = vec!["Amy Smith".into()];
    });
    let me = body(w.login("asmith", "amy-pw").await.unwrap()).await;
    assert_eq!(me["id"], id);
    assert_eq!(me["display_name"], "Amy Smith");
    assert!(w.account("asmith").await.is_none());
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE source = 'ldap'").fetch_one(&w.env.st.db).await.unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn groups_follow_the_directory_at_each_sign_in() {
    let w = world().await;
    w.login("ben", "ben-pw").await.unwrap();
    assert_eq!(w.members(w.staff).await, vec!["ben"]);
    assert!(w.members(w.admins).await.is_empty());
    // He is made an administrator there, and leaves staff
    w.directory(|d| {
        let ben = d.people.iter_mut().find(|p| p.dn == BEN).unwrap();
        ben.attributes.iter_mut().find(|(k, _)| k == "memberOf").unwrap().1 = vec![ADMINS.into()];
    });
    w.login("ben", "ben-pw").await.unwrap();
    assert!(w.members(w.staff).await.is_empty());
    assert_eq!(w.members(w.admins).await, vec!["ben"]);
    // Someone added by hand to a group of the directory who isn't from it is left alone
    let other = w.env.user("carol", true).await;
    sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES (?, ?)").bind(w.staff).bind(other.id).execute(&w.env.st.db).await.unwrap();
    w.login("ben", "ben-pw").await.unwrap();
    assert_eq!(w.members(w.staff).await, vec!["carol"]);
}

#[tokio::test]
async fn a_group_that_follows_the_directory_keeps_the_people_who_come_from_it() {
    let w = world().await;
    w.login("amy", "amy-pw").await.unwrap();
    let carol = w.env.user("carol", true).await;
    let admin = w.env.admin().await;
    let update = |members: Vec<i64>| {
        let (st, admin, staff) = (w.env.st.clone(), admin.clone(), w.staff);
        async move {
            let req = serde_json::from_value(json!({ "members": members })).unwrap();
            let Json(_) = crate::drives::update_group(State(st), Admin(admin), axum::extract::Path(staff), Json(req)).await.unwrap();
        }
    };
    // Taking everyone out by hand takes out only those who aren't the directory's; adding someone who isn't works
    update(vec![carol.id]).await;
    assert_eq!(w.members(w.staff).await, vec!["amy", "carol"]);
    update(vec![]).await;
    assert_eq!(w.members(w.staff).await, vec!["amy"]);
    // The list of groups says which people are the directory's
    let Json(groups) = crate::drives::list_groups(State(w.env.st.clone()), Admin(admin)).await.unwrap();
    let staff = serde_json::to_value(&groups).unwrap().as_array().unwrap().iter().find(|g| g["name"] == "Staff").cloned().unwrap();
    assert_eq!((staff["source"].clone(), staff["members"][0]["directory"].clone()), (json!("ldap"), json!(true)));
}

#[tokio::test]
async fn groups_can_be_found_by_searching_with_the_chain_rule_of_active_directory() {
    let w = world().await;
    // Staff holds Interns, who hold Dan: Dan is in staff through them
    w.directory(|d| {
        d.people.push(Person::new("cn=interns,ou=groups,dc=example,dc=com", &[("cn", &["interns"]), ("member", &["uid=dan,ou=people,dc=example,dc=com"])]));
        d.people
            .iter_mut()
            .find(|p| p.dn == STAFF)
            .unwrap()
            .attributes
            .iter_mut()
            .find(|(k, _)| k == "member")
            .unwrap()
            .1
            .push("cn=interns,ou=groups,dc=example,dc=com".into());
        d.people.push(person("uid=dan,ou=people,dc=example,dc=com", "dan", "Dan", "44444444-dddd", &[]));
        d.passwords.insert("uid=dan,ou=people,dc=example,dc=com".into(), "dan-pw".into());
    });
    w.save(|s| {
        s.group_mode = GroupMode::Search;
        s.group_base = "ou=groups,dc=example,dc=com".into();
        s.group_filter = "(member:1.2.840.113556.1.4.1941:={dn})".into();
    })
    .await;
    w.login("dan", "dan-pw").await.unwrap();
    assert_eq!(w.members(w.staff).await, vec!["dan"]);
}

#[tokio::test]
async fn only_the_members_of_the_required_group_may_sign_in() {
    let w = world().await;
    w.save(|s| s.require_group = ADMINS.into()).await;
    w.login("amy", "amy-pw").await.unwrap();
    let err = w.login("ben", "ben-pw").await.unwrap_err();
    assert_eq!(err.message, "Incorrect username or password");
    assert!(w.account("ben").await.is_none());
}

#[tokio::test]
async fn an_account_the_directory_has_turned_off_cannot_sign_in() {
    let w = world().await;
    // Active Directory's account control: bit 2 is "disabled"
    w.directory(|d| d.people.iter_mut().find(|p| p.dn == BEN).unwrap().attributes.push(("userAccountControl".into(), vec!["514".into()])));
    assert!(w.login("ben", "ben-pw").await.is_err());
    assert!(w.account("ben").await.is_none());
    // Another account turned on there: fine
    w.directory(|d| d.people.iter_mut().find(|p| p.dn == AMY).unwrap().attributes.push(("userAccountControl".into(), vec!["512".into()])));
    w.login("amy", "amy-pw").await.unwrap();
}

#[tokio::test]
async fn nothing_is_created_when_the_settings_say_accounts_are_not_made() {
    let w = world().await;
    w.save(|s| s.create_accounts = false).await;
    let err = w.login("amy", "amy-pw").await.unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
    assert!(w.account("amy").await.is_none());
}

#[tokio::test]
async fn where_passwords_are_off_the_directory_password_is_too() {
    let w = world().await;
    w.login("amy", "amy-pw").await.unwrap();
    for mode in ["nobody", "administrators"] {
        sqlx::query("INSERT INTO settings (key, value) VALUES ('sso', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
            .bind(json!({ "password_sign_in": mode }).to_string())
            .execute(&w.env.st.db)
            .await
            .unwrap();
        let err = w.login("amy", "amy-pw").await.unwrap_err();
        assert_eq!(err.message, "Incorrect username or password", "{mode}");
        // Nor is a new account made for a person who couldn't use it
        assert!(w.login("ben", "ben-pw").await.is_err());
        assert!(w.account("ben").await.is_none());
    }
}

#[tokio::test]
async fn a_directory_that_cannot_be_reached_is_said_so_and_is_not_a_wrong_password() {
    let w = world().await;
    // Nothing listens on the port any more
    let closed = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    w.save(|s| s.url = format!("ldap://127.0.0.1:{closed}")).await;
    for _ in 0..8 {
        let err = w.login("amy", "amy-pw").await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }
    // Eight failures would have locked the account: it still signs in once the directory is back
    w.save(|_| {}).await;
    w.login("amy", "amy-pw").await.unwrap();
}

#[tokio::test]
async fn a_service_account_with_the_wrong_password_is_the_administrators_to_fix() {
    let w = world().await;
    w.save(|s| s.bind_password = crate::secrets::seal(settings::SEAL, "not-the-password")).await;
    let err = w.login("amy", "amy-pw").await.unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(w.account("amy").await.is_none());
}

#[tokio::test]
async fn two_factor_sign_in_applies_on_top_of_the_directory_password() {
    let w = world().await;
    w.env.st.system.write().require_two_factor = true;
    let v = body(w.login("amy", "amy-pw").await.unwrap()).await;
    assert_eq!(v["two_factor"], "setup");
    let (id, ..) = w.account("amy").await.unwrap();
    assert_eq!(w.sessions(id).await, 0);
}

#[tokio::test]
async fn directory_sign_in_is_off_until_it_is_turned_on_and_set_up() {
    let w = world().await;
    w.save(|s| s.enabled = false).await;
    assert!(w.login("amy", "amy-pw").await.is_err());
    // Turned on but not set up: the same
    w.save(|s| s.user_filter = String::new()).await;
    assert!(w.login("amy", "amy-pw").await.is_err());
    assert!(w.account("amy").await.is_none());
}

// ───────────── Staying in step ─────────────

#[tokio::test]
async fn a_person_who_leaves_the_directory_is_turned_off_and_signed_out_and_back_when_they_return() {
    let w = world().await;
    w.login("amy", "amy-pw").await.unwrap();
    w.login("ben", "ben-pw").await.unwrap();
    let (amy, ..) = w.account("amy").await.unwrap();
    assert_eq!(w.sessions(amy).await, 1);

    let removed = {
        let mut d = w.server.directory.lock();
        let at = d.people.iter().position(|p| p.dn == BEN).unwrap();
        d.people.remove(at)
    };
    let report = sync::run(&w.env.st).await.unwrap();
    assert_eq!((report.checked, report.turned_off, report.error), (2, 1, None));
    let (ben, _, disabled, by_directory) = w.account("ben").await.unwrap();
    assert!(disabled && by_directory);
    assert_eq!(w.sessions(ben).await, 0);
    // Never deleted: the account and its files stay, and the other one is untouched
    assert!(!w.account("amy").await.unwrap().2);
    assert_eq!(w.sessions(amy).await, 1);
    // He can't sign in meanwhile
    assert!(w.login("ben", "ben-pw").await.is_err());

    // Back in the directory: turned on again
    w.directory(|d| d.people.push(removed));
    let report = sync::run(&w.env.st).await.unwrap();
    assert_eq!((report.turned_on, report.turned_off), (1, 0));
    assert!(!w.account("ben").await.unwrap().2);
    w.login("ben", "ben-pw").await.unwrap();
}

#[tokio::test]
async fn an_account_an_administrator_turned_off_stays_off_whatever_the_directory_says() {
    let w = world().await;
    w.login("ben", "ben-pw").await.unwrap();
    sqlx::query("UPDATE users SET disabled = 1 WHERE username = 'ben'").execute(&w.env.st.db).await.unwrap();
    let report = sync::run(&w.env.st).await.unwrap();
    assert_eq!(report.turned_on, 0);
    assert!(w.account("ben").await.unwrap().2);
    assert!(w.login("ben", "ben-pw").await.is_err());
}

#[tokio::test]
async fn a_search_that_finds_no_one_turns_nobody_off() {
    let w = world().await;
    w.login("amy", "amy-pw").await.unwrap();
    // The search base is wrong: it sees nobody
    w.save(|s| s.user_base = "dc=nowhere,dc=com".into()).await;
    let report = sync::run(&w.env.st).await.unwrap();
    assert_eq!(report.turned_off, 0);
    assert!(report.error.as_deref().is_some_and(|e| e.contains("found no one")), "{report:?}");
    assert!(!w.account("amy").await.unwrap().2);
}

#[tokio::test]
async fn the_sync_brings_everyones_groups_up_to_date_and_says_when_the_directory_is_down() {
    let w = world().await;
    w.login("amy", "amy-pw").await.unwrap();
    w.login("ben", "ben-pw").await.unwrap();
    w.directory(|d| d.people.iter_mut().find(|p| p.dn == BEN).unwrap().attributes.iter_mut().find(|(k, _)| k == "memberOf").unwrap().1 = vec![ADMINS.into()]);
    let report = sync::run(&w.env.st).await.unwrap();
    assert_eq!(report.groups_changed, 2, "{report:?}");
    assert_eq!(w.members(w.staff).await, vec!["amy"]);
    assert_eq!(w.members(w.admins).await, vec!["amy", "ben"]);
    assert_eq!(sync::last(&w.env.st.db).await.unwrap().groups_changed, 2);
    // The directory is gone: nothing changes, and the report says why
    let closed = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    w.save(|s| s.url = format!("ldap://127.0.0.1:{closed}")).await;
    let report = sync::run(&w.env.st).await.unwrap();
    assert!(report.error.is_some());
    assert_eq!(w.members(w.admins).await, vec!["amy", "ben"]);
    assert!(!w.account("ben").await.unwrap().2);
}

// ───────────── The settings ─────────────

#[test]
fn the_address_says_where_and_how() {
    let s = |url: &str, starttls: bool| Settings { url: url.into(), starttls, ..Settings::default() };
    let t = s("ldaps://dc1.example.com", true).target().unwrap();
    assert_eq!((t.host.as_str(), t.port, t.security), ("dc1.example.com", 636, ldap::Security::Ldaps));
    let t = s("ldap://dc1.example.com:3389", true).target().unwrap();
    assert_eq!((t.port, t.security), (3389, ldap::Security::StartTls));
    assert_eq!(s("ldap://10.0.0.5", false).target().unwrap().security, ldap::Security::None);
    assert_eq!(s("ldap://[fd00::1]", true).target().unwrap().host, "fd00::1");
    for bad in [
        "",
        "dc1.example.com",
        "http://dc1.example.com",
        "ldaps://",
        "ldaps://user:pw@dc1.example.com",
        "ldaps://dc1.example.com/dc=example",
        "ldaps://dc1.example.com?x=y",
    ] {
        assert!(s(bad, true).target().is_err(), "{bad}");
    }
}

#[test]
fn the_settings_are_checked_before_they_are_used() {
    let ok = Settings {
        url: "ldaps://dc.example.com".into(),
        bind_dn: BIND.into(),
        user_base: "dc=example,dc=com".into(),
        user_filter: "(uid={username})".into(),
        ..Settings::default()
    };
    assert_eq!(ok.check(), Ok(()));
    let problem = |change: &dyn Fn(&mut Settings)| {
        let mut s = ok.clone();
        change(&mut s);
        s.check().unwrap_err()
    };
    assert!(problem(&|s| s.user_filter = "(uid=amy)".into()).contains("{username}"));
    assert!(problem(&|s| s.user_filter = "uid={username}".into()).contains("filter"));
    assert!(problem(&|s| s.bind_dn = String::new()).contains("service account"));
    assert!(problem(&|s| s.user_base = String::new()).contains("search base"));
    assert!(problem(&|s| s.ca_pem = "not a certificate".into()).contains("PEM"));
    // Groups: a required group needs a way to find groups, and a search needs its filter and base
    assert!(problem(&|s| s.require_group = ADMINS.into()).contains("Choose how groups are found"));
    assert!(problem(&|s| s.group_mode = GroupMode::Search).contains("search for groups"));
    assert!(problem(&|s| (s.group_mode, s.group_base) = (GroupMode::Search, "ou=g".into())).contains("{dn}"));
    assert_eq!(Settings::normalize_dn("CN=Staff, OU=Groups ,DC=Example"), "cn=staff,ou=groups,dc=example");
}

#[tokio::test]
async fn the_service_accounts_password_is_kept_sealed_and_never_sent_back() {
    let w = world().await;
    let admin = w.env.admin().await;
    let me = || crate::tokens::Confirmation { my_password: Some(testutil::password().into()), my_code: None };
    let request = |change: &dyn Fn(&mut Value)| {
        let mut v = serde_json::to_value(settings_for(&w.server, w.staff, w.admins)).unwrap();
        v["bind_password"] = json!("");
        change(&mut v);
        v
    };
    // Saved with a new password: sealed, and the answer doesn't carry it
    let req = request(&|v| v["bind_password"] = json!("brand-new-secret"));
    let save_req = |v: Value, me: crate::tokens::Confirmation| {
        let mut v = v;
        v["my_password"] = json!(me.my_password);
        serde_json::from_value::<admin::SaveReq>(v).unwrap()
    };
    let Json(v) = admin::save(State(w.env.st.clone()), Admin(admin.clone()), Json(save_req(req, me()))).await.unwrap();
    assert_eq!((v["settings"]["bind_password"].clone(), v["has_bind_password"].clone()), (json!(""), json!(true)));
    let stored = load(&w.env.st.db).await;
    assert_ne!(stored.bind_password, "brand-new-secret");
    assert_eq!(stored.password().unwrap(), "brand-new-secret");
    // Left blank: the one saved stays
    let Json(_) = admin::save(State(w.env.st.clone()), Admin(admin.clone()), Json(save_req(request(&|_| {}), me()))).await.unwrap();
    assert_eq!(load(&w.env.st.db).await.password().unwrap(), "brand-new-secret");
    // Without saying who is asking, nothing is saved
    let err =
        admin::save(State(w.env.st.clone()), Admin(admin.clone()), Json(save_req(request(&|_| {}), crate::tokens::Confirmation::default()))).await.err().unwrap();
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    // A directory group followed by a group that doesn't exist, a public address without TLS, a filter that doesn't ask for the person
    assert!(
        admin::save(State(w.env.st.clone()), Admin(admin.clone()), Json(save_req(request(&|v| v["mappings"] = json!([{ "dn": STAFF, "group_id": 99_999 }])), me())))
            .await
            .is_err()
    );
    let public =
        admin::save(State(w.env.st.clone()), Admin(admin.clone()), Json(save_req(request(&|v| v["url"] = json!("ldap://8.8.8.8")), me()))).await.err().unwrap();
    assert!(public.message.contains("private network"), "{}", public.message);
    assert!(admin::save(State(w.env.st.clone()), Admin(admin), Json(save_req(request(&|v| v["user_filter"] = json!("(uid=amy)")), me()))).await.is_err());
}

#[tokio::test]
async fn the_test_says_where_it_goes_wrong() {
    let w = world().await;
    let admin = w.env.admin().await;
    let run = |change: &dyn Fn(&mut Value), name: &str| {
        let mut v = serde_json::to_value(settings_for(&w.server, w.staff, w.admins)).unwrap();
        v["bind_password"] = json!("");
        change(&mut v);
        v["username"] = json!(name);
        let (st, admin) = (w.env.st.clone(), admin.clone());
        async move { admin::test(State(st), Admin(admin), Json(serde_json::from_value(v).unwrap())).await.unwrap().0 }
    };
    let steps = |v: &Value| {
        v["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["step"].as_str().unwrap().to_string(), s["ok"].as_bool().unwrap(), s["detail"].as_str().unwrap().to_string()))
            .collect::<Vec<_>>()
    };
    // All of it: connected, the service account, the person with what was found, and their groups
    let all = steps(&run(&|_| {}, "amy").await);
    assert_eq!(
        all.iter().map(|s| (s.0.as_str(), s.1)).collect::<Vec<_>>(),
        vec![("Connect", true), ("Service account", true), ("Find the person", true), ("Groups", true)]
    );
    assert!(
        all[2].2.contains("DN: uid=amy,ou=people") && all[2].2.contains("Stable id (entryUUID): 11111111-aaaa") && all[2].2.contains("amy@example.com"),
        "{}",
        all[2].2
    );
    assert!(all[3].2.contains(STAFF) && all[3].2.contains("2 of these are followed"), "{}", all[3].2);
    // Without a name: only the connection and the service account
    assert_eq!(steps(&run(&|_| {}, "").await).len(), 2);
    // A person who isn't there, and a name that matches more than one
    let none = steps(&run(&|_| {}, "nobody").await);
    assert!(!none[2].1 && none[2].2.contains("found nothing"), "{none:?}");
    w.directory(|d| d.people.push(person("uid=amy-b,ou=people,dc=example,dc=com", "amy", "Amy Two", "55555555-eeee", &[])));
    let many = steps(&run(&|_| {}, "amy").await);
    assert!(!many[2].1 && many[2].2.contains("More than one"), "{many:?}");
    // The service account's password, with the exact reason
    let bad = steps(&run(&|v| v["bind_password"] = json!("wrong"), "amy").await);
    assert_eq!(bad.len(), 2);
    assert!(!bad[1].1 && bad[1].2.contains("invalid credentials"), "{bad:?}");
    // A server that isn't there
    let closed = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let gone = steps(
        &run(
            &|v| {
                v["url"] = json!(format!("ldap://127.0.0.1:{closed}"));
                v["bind_password"] = json!("reader-secret");
            },
            "amy",
        )
        .await,
    );
    assert_eq!(gone.len(), 1);
    assert!(!gone[0].1 && gone[0].0 == "Connect", "{gone:?}");
    // A filter that is not one is told before anything is tried
    let filter = steps(&run(&|v| v["user_filter"] = json!("(uid=amy"), "amy").await);
    assert_eq!((filter[0].0.as_str(), filter[0].1), ("Settings", false));
}

// ───────────── What a review found ─────────────

#[tokio::test]
async fn a_new_person_with_the_name_of_one_who_left_is_another_account() {
    let w = world().await;
    w.login("ben", "ben-pw").await.unwrap();
    let (old, ..) = w.account("ben").await.unwrap();
    // Ben leaves: his account is turned off. Another Ben, with another id, gets the same name.
    w.directory(|d| d.people.retain(|p| p.dn != BEN));
    sync::run(&w.env.st).await.unwrap();
    assert!(w.account("ben").await.unwrap().2);
    w.directory(|d| {
        d.people.push(person("uid=ben2,ou=people,dc=example,dc=com", "ben", "Another Ben", "99999999-zzzz", &[]));
        d.passwords.insert("uid=ben2,ou=people,dc=example,dc=com".into(), "ben2-pw".into());
    });
    let me = body(w.login("ben", "ben2-pw").await.unwrap()).await;
    assert_ne!(me["id"], old);
    assert_eq!(me["username"], "ben-2");
    // The old account, with its files, shares and grants, is still turned off and untouched
    assert!(w.account("ben").await.unwrap().2);
    assert_eq!(w.sessions(old).await, 0);
}

#[tokio::test]
async fn the_saved_password_goes_only_where_it_was_saved_for() {
    let w = world().await;
    let admin = w.env.admin().await;
    let run = |change: &dyn Fn(&mut Value)| {
        let mut v = serde_json::to_value(settings_for(&w.server, w.staff, w.admins)).unwrap();
        v["bind_password"] = json!("");
        change(&mut v);
        let (st, admin) = (w.env.st.clone(), admin.clone());
        async move { admin::test(State(st), Admin(admin), Json(serde_json::from_value(v).unwrap())).await.unwrap().0 }
    };
    // Another server, or another service account, with the password left blank: nothing is tried
    for change in [&(|v: &mut Value| v["url"] = json!("ldaps://attacker.example.com")) as &dyn Fn(&mut Value), &|v: &mut Value| {
        v["bind_dn"] = json!("cn=someone,dc=example,dc=com")
    }] {
        let v = run(change).await;
        let steps = v["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!((steps[0]["step"].clone(), steps[0]["ok"].clone()), (json!("Settings"), json!(false)));
    }
    assert!(w.server.log.binds.lock().is_empty());
    // As saved: tried
    assert_eq!(run(&|_| {}).await["steps"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn someone_taken_out_of_the_required_group_is_turned_off_at_the_next_sync() {
    let w = world().await;
    w.save(|s| s.require_group = STAFF.into()).await;
    w.login("ben", "ben-pw").await.unwrap();
    let (ben, ..) = w.account("ben").await.unwrap();
    assert_eq!(w.sessions(ben).await, 1);
    // Out of the group, though still in the directory and turned on there
    w.directory(|d| d.people.iter_mut().find(|p| p.dn == BEN).unwrap().attributes.iter_mut().find(|(k, _)| k == "memberOf").unwrap().1 = vec![]);
    let report = sync::run(&w.env.st).await.unwrap();
    assert_eq!(report.turned_off, 1, "{report:?}");
    assert!(w.account("ben").await.unwrap().2);
    assert_eq!(w.sessions(ben).await, 0);
    // Back in it: back on
    w.directory(|d| d.people.iter_mut().find(|p| p.dn == BEN).unwrap().attributes.iter_mut().find(|(k, _)| k == "memberOf").unwrap().1 = vec![STAFF.into()]);
    assert_eq!(sync::run(&w.env.st).await.unwrap().turned_on, 1);
    assert!(!w.account("ben").await.unwrap().2);
}

#[tokio::test]
async fn a_name_is_paused_after_a_few_wrong_passwords_from_any_address_before_the_directory_locks_it() {
    let w = world().await;
    let from = |n: u8| ConnectInfo(format!("203.0.113.{n}:5000").parse::<std::net::SocketAddr>().unwrap());
    let guess = |n: u8, password: &'static str| {
        let st = w.env.st.clone();
        async move { crate::signin::login(State(st), from(n), HeaderMap::new(), Json(LoginReq { username: "amy".into(), password: password.into() })).await }
    };
    for n in 1..=5 {
        assert_eq!(guess(n, "wrong").await.unwrap_err().status, axum::http::StatusCode::UNAUTHORIZED);
    }
    let amy_binds = || w.server.log.binds.lock().iter().filter(|(dn, _)| dn == AMY).count();
    assert_eq!(amy_binds(), 5);
    // The sixth, from an address that never tried, isn't sent to the directory, and not even the right password is
    assert_eq!(guess(6, "wrong").await.unwrap_err().status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(guess(7, "amy-pw").await.unwrap_err().status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(amy_binds(), 5);
    // Another name is not paused with it
    assert!(w.login("ben", "ben-pw").await.is_ok());
}

#[tokio::test]
async fn an_administrator_turning_an_account_off_is_not_undone_by_the_directory() {
    let w = world().await;
    w.login("ben", "ben-pw").await.unwrap();
    // The directory turned him off once, an administrator turned him on, and now turns him off themselves
    sqlx::query("UPDATE users SET disabled = 1, directory_disabled = 1 WHERE username = 'ben'").execute(&w.env.st.db).await.unwrap();
    let (ben, ..) = w.account("ben").await.unwrap();
    let admin = w.env.admin().await;
    let set = |disabled: bool| {
        let (st, admin) = (w.env.st.clone(), admin.clone());
        async move {
            let req = serde_json::from_value(json!({ "disabled": disabled })).unwrap();
            let Json(_) = crate::admin::update(State(st), Admin(admin), axum::extract::Path(ben), Json(req)).await.unwrap();
        }
    };
    set(false).await;
    set(true).await;
    assert!(!w.account("ben").await.unwrap().3);
    sync::run(&w.env.st).await.unwrap();
    assert!(w.account("ben").await.unwrap().2, "still turned off");
    assert!(w.login("ben", "ben-pw").await.is_err());
}
