// Directory sign-in against a real OpenLDAP. The Build workflow starts one with the people of testdata/seed.ldif and
// sets THIRTYFILE_LDAP_URL (for example ldap://127.0.0.1:10389); without it the test does nothing, as it can't run
// anywhere without a directory. To try it here:
//   docker run -d -p 10389:389 -e LDAP_DOMAIN=example.org -e LDAP_ADMIN_PASSWORD=admin -e LDAP_TLS=false \
//     -v <this folder>/testdata:/container/service/slapd/assets/config/bootstrap/ldif/custom osixia/openldap:1.5.0 --copy-service
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::HeaderMap,
};

use super::*;
use crate::{signin::LoginReq, testutil};

fn settings(url: &str, staff: i64, admins: i64) -> Settings {
    Settings {
        enabled: true,
        url: url.into(),
        starttls: false,
        bind_dn: "cn=admin,dc=example,dc=org".into(),
        bind_password: crate::secrets::seal(SEAL, "admin"),
        user_base: "ou=people,dc=example,dc=org".into(),
        user_filter: "(&(objectClass=inetOrgPerson)(|(uid={username})(mail={username})))".into(),
        attr_id: "entryUUID".into(),
        attr_username: "uid".into(),
        attr_name: "cn".into(),
        attr_email: "mail".into(),
        group_mode: GroupMode::Search,
        group_base: "ou=groups,dc=example,dc=org".into(),
        group_filter: "(&(objectClass=groupOfNames)(member={dn}))".into(),
        mappings: vec![
            Mapping { dn: "cn=staff,ou=groups,dc=example,dc=org".into(), group_id: staff },
            Mapping { dn: "cn=admins,ou=groups,dc=example,dc=org".into(), group_id: admins },
        ],
        ..Settings::default()
    }
}

#[tokio::test]
async fn a_real_openldap_signs_people_in_and_their_groups_follow() {
    let Ok(url) = std::env::var("THIRTYFILE_LDAP_URL") else { return };
    let env = testutil::env().await;
    let group = |name: &'static str| {
        let db = env.st.db.clone();
        async move { sqlx::query_scalar::<_, i64>("INSERT INTO groups (name, created_at) VALUES (?, 0) RETURNING id").bind(name).fetch_one(&db).await.unwrap() }
    };
    let (staff, admins) = (group("Staff").await, group("Admins").await);
    let s = settings(&url, staff, admins);
    settings::store(&mut env.st.db.acquire().await.unwrap(), &s).await.unwrap();

    // The container takes a little while to load its people
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let ready = match open(&s).await {
            Ok(mut conn) => matches!(find_user(&mut conn, &s, "amy").await, Ok(Lookup::One(_))),
            Err(_) => false,
        };
        if ready {
            break;
        }
        assert!(Instant::now() < deadline, "the directory didn't come up with its people");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let login = |name: &'static str, password: &'static str| {
        let st = env.st.clone();
        async move {
            let addr = ConnectInfo("203.0.113.9:5000".parse::<std::net::SocketAddr>().unwrap());
            crate::signin::login(State(st), addr, HeaderMap::new(), Json(LoginReq { username: name.into(), password: password.into() })).await
        }
    };
    let members = |g: i64| {
        let db = env.st.db.clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT u.username FROM group_members m JOIN users u ON u.id = m.user_id WHERE m.group_id = ? ORDER BY u.username")
                .bind(g)
                .fetch_all(&db)
                .await
                .unwrap()
        }
    };

    // Amy by her user name, Ben by his address
    login("amy", "amy-pw").await.unwrap();
    login("ben@example.org", "ben-pw").await.unwrap();
    assert_eq!(members(staff).await, vec!["amy", "ben"]);
    assert_eq!(members(admins).await, vec!["amy"]);
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE source = 'ldap'").fetch_one(&env.st.db).await.unwrap();
    assert_eq!(n, 2);

    // A wrong password, an unknown name, and names that are filters
    for (name, password) in [("amy", "wrong"), ("nobody", "amy-pw"), ("*", "amy-pw"), ("*)(uid=*", "amy-pw"), ("a*", "amy-pw"), ("amy", "")] {
        let err = login(name, password).await.err().unwrap_or_else(|| panic!("{name:?} signed in"));
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED, "{name:?}");
    }
    // The sync finds both, and turns nobody off
    let report = sync::run(&env.st).await.unwrap();
    assert_eq!((report.checked, report.turned_off, report.error), (2, 0, None));
    // The service account's test: connect, bind, the person, the groups
    let mut conn = open(&s).await.unwrap();
    let Lookup::One(amy) = find_user(&mut conn, &s, "amy").await.unwrap() else { panic!("Amy isn't found") };
    assert!(stable_id(&amy, "entryUUID").is_some_and(|id| id.len() >= 32));
    let groups = memberships(&mut conn, &s, &amy).await.unwrap();
    assert_eq!(groups.len(), 2, "{groups:?}");
}
