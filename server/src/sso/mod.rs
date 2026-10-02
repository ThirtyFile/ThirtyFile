//! Third-party sign-in (SSO): Microsoft Entra ID, Google, GitHub.
//!
//! OAuth 2.0 authorization code flow + PKCE:
//! 1. `/api/auth/sso/{provider}/start` generates state, nonce and PKCE, and redirects to the provider's sign-in page
//! 2. The provider redirects back to `/api/auth/sso/{provider}/callback`, and the server exchanges the authorization code with the provider for the identity (server to server, over TLS)
//! 3. The user is found by the external account's stable identifier and signed in; on first sign-in, an existing account is matched by email, or (when enabled) an account is created automatically
//!
//! Security: state is single-use and valid for 10 minutes; ID tokens are checked for aud, iss, exp and nonce;
//! Microsoft emails are trusted only when a specific tenant is set (with "any organization", other tenants can fill in someone else's email).

mod accounts;
mod identity;
mod linked;
mod settings;
mod signin;

use accounts::*;
use identity::*;
pub use linked::*;
pub use settings::*;
pub use signin::*;

use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Redirect, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    admin::{NewUser, create_user},
    auth::{Admin, NO_PASSWORD, User, client_ip},
    db::{get_setting, set_setting},
    error::{AppError, AppResult},
    logs::{self, record_login_via},
    signin::open_session,
    state::AppState,
    util::{now, random_token},
};

/// `oidc`: any OpenID Connect provider (Keycloak, Authentik, Authelia, Zitadel…), set up with its issuer URL
pub const PROVIDERS: [&str; 4] = ["microsoft", "google", "github", "oidc"];
const PENDING_TTL: Duration = Duration::from_secs(600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

pub fn label(provider: &str) -> &'static str {
    match provider {
        "microsoft" => "Microsoft",
        "google" => "Google",
        "oidc" => "OpenID Connect",
        _ => "GitHub",
    }
}

/// What happens when someone signs in with an external account that isn't linked to a user yet
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provisioning {
    /// Only accounts linked under "Sign-in methods" can sign in
    Off,
    /// An existing user whose username is the (verified) email is linked automatically
    #[default]
    Link,
    /// Like `Link`, and people without an account get one created automatically
    Create,
}

/// Settings of accounts a provider creates automatically
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NewUserDefaults {
    pub can_write: bool,
    pub can_delete: bool,
    pub can_share: bool,
    /// Personal space size in bytes (0 = unlimited); None = the system's default for new users
    pub quota_bytes: Option<i64>,
}

impl Default for NewUserDefaults {
    fn default() -> Self {
        Self { can_write: true, can_delete: true, can_share: true, quota_bytes: None }
    }
}

/// Settings for accounts created for one email domain (wins over the provider's defaults)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DomainRule {
    /// Lowercase, without @
    pub domain: String,
    pub can_write: bool,
    pub can_delete: bool,
    pub can_share: bool,
    /// Bytes (0 = unlimited); None = the system's default for new users
    pub quota_bytes: Option<i64>,
    pub groups: Vec<i64>,
    /// Give the account a personal space ("My files"); None = the system setting for new users
    pub personal_space: Option<bool>,
    /// The storage location of its personal space; None = the system setting's
    pub personal_location: Option<String>,
}

impl Default for DomainRule {
    fn default() -> Self {
        Self {
            domain: String::new(),
            can_write: true,
            can_delete: true,
            can_share: true,
            quota_bytes: None,
            groups: Vec::new(),
            personal_space: None,
            personal_location: None,
        }
    }
}

/// Accounts one provider may create per hour: a misconfigured tenant or domain list can't flood the user list
pub const MAX_CREATED_PER_HOUR: i64 = 50;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub enabled: bool,
    pub client_id: String,
    pub client_secret: String,
    /// Microsoft: tenant (directory) ID or domain; blank = any organization account
    pub tenant: String,
    pub provisioning: Provisioning,
    /// Email domains allowed for this provider (lowercase, without @); empty = the global list
    pub allowed_domains: Vec<String>,
    /// Permissions and space size of automatically created accounts
    pub defaults: NewUserDefaults,
    /// Groups automatically created accounts are added to
    pub groups: Vec<i64>,
    /// OpenID Connect: the name on the sign-in button, and the issuer (its discovery document is
    /// `<issuer>/.well-known/openid-configuration`)
    pub name: String,
    pub issuer: String,
    /// OpenID Connect: the endpoints its discovery document gave when the settings were saved
    pub authorize_url: String,
    pub token_url: String,
}

impl ProviderConfig {
    fn ready(&self) -> bool {
        self.enabled && !self.client_id.trim().is_empty() && !self.client_secret.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SsoSettings {
    pub microsoft: ProviderConfig,
    pub google: ProviderConfig,
    pub github: ProviderConfig,
    pub oidc: ProviderConfig,
    /// Email domains allowed to sign in (lowercase, without @) unless a provider has its own list; empty = unrestricted
    pub allowed_domains: Vec<String>,
    /// Per-domain settings for automatically created accounts (a domain here still has to be allowed to sign in)
    pub domain_rules: Vec<DomainRule>,
}

impl SsoSettings {
    /// The rule for the email's domain, if any
    pub fn domain_rule(&self, email: &str) -> Option<&DomainRule> {
        let domain = email.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase())?;
        self.domain_rules.iter().find(|r| r.domain == domain)
    }
    pub fn provider(&self, p: &str) -> Option<&ProviderConfig> {
        match p {
            "microsoft" => Some(&self.microsoft),
            "google" => Some(&self.google),
            "github" => Some(&self.github),
            "oidc" => Some(&self.oidc),
            _ => None,
        }
    }
    /// Removes a deleted group from the groups new accounts join; returns whether anything changed
    pub fn forget_group(&mut self, id: i64) -> bool {
        let lists = [&mut self.microsoft.groups, &mut self.google.groups, &mut self.github.groups, &mut self.oidc.groups]
            .into_iter()
            .chain(self.domain_rules.iter_mut().map(|r| &mut r.groups));
        let mut changed = false;
        for list in lists {
            let before = list.len();
            list.retain(|g| *g != id);
            changed |= list.len() != before;
        }
        changed
    }
    fn provider_mut(&mut self, p: &str) -> Option<&mut ProviderConfig> {
        match p {
            "microsoft" => Some(&mut self.microsoft),
            "google" => Some(&mut self.google),
            "github" => Some(&mut self.github),
            "oidc" => Some(&mut self.oidc),
            _ => None,
        }
    }
}

pub async fn load(db: &sqlx::SqlitePool) -> SsoSettings {
    let mut s: SsoSettings = match get_setting(db, "sso").await {
        Ok(Some(v)) => serde_json::from_str(&v).unwrap_or_else(|e| {
            tracing::error!("Single sign-on settings can't be read and are ignored (saving the settings page will replace them): {e}");
            SsoSettings::default()
        }),
        _ => SsoSettings::default(),
    };
    for p in PROVIDERS {
        let c = s.provider_mut(p).unwrap();
        // Client secrets are stored encrypted (secrets.rs)
        match crate::secrets::open(&format!("sso:{p}"), &c.client_secret) {
            Ok(plain) => c.client_secret = plain,
            Err(e) => {
                tracing::error!("The {p} client secret can't be read ({e}); sign-in with {p} is off until it is entered again");
                c.client_secret.clear();
            }
        }
    }
    s
}

/// Saves the settings, with the client secrets encrypted
pub async fn store(conn: &mut sqlx::SqliteConnection, settings: &SsoSettings) -> Result<(), sqlx::Error> {
    let mut sealed = settings.clone();
    for p in PROVIDERS {
        let c = sealed.provider_mut(p).unwrap();
        c.client_secret = crate::secrets::seal(&format!("sso:{p}"), &c.client_secret);
    }
    set_setting(conn, "sso", &serde_json::to_string(&sealed).unwrap()).await
}

/// A sign-in in progress (between redirecting to the provider and coming back)
pub struct Pending {
    provider: String,
    verifier: String,
    nonce: String,
    next: String,
    redirect_uri: String,
    /// Link mode: link the external account to this signed-in user
    link_user: Option<i64>,
    created: Instant,
    /// Where the sign-in started (limits how many pending sign-ins one address may hold)
    ip: String,
}

/// Pending sign-ins kept at most; beyond this the oldest are evicted (the start endpoint needs no sign-in, so it must not be
/// possible to fill the table and lock everyone out)
const MAX_PENDING: usize = 5000;
/// Pending sign-ins one address may hold at once
const MAX_PENDING_PER_IP: usize = 20;

/// Microsoft's "no specific tenant": any organization (or personal account) can sign in, so the email can't be trusted
/// The provider's profile name as stored: trimmed, no control characters, at most 80 characters (same rules as display names)
fn clean_name(raw: &str) -> String {
    let trimmed: String = raw.trim().chars().filter(|c| !c.is_control()).collect();
    trimmed.chars().take(80).collect::<String>().trim().to_string()
}

fn ms_tenant_is_specific(tenant: &str) -> bool {
    let t = tenant.trim().to_ascii_lowercase();
    !t.is_empty() && !matches!(t.as_str(), "common" | "organizations" | "consumers")
}

struct Endpoints {
    authorize: String,
    token: String,
    /// GitHub: user profile and email
    user: String,
    emails: String,
    scope: &'static str,
}

#[cfg(test)]
pub static MOCK_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn endpoints(provider: &str, cfg: &ProviderConfig) -> Endpoints {
    #[cfg(test)]
    if let Some(base) = MOCK_BASE.lock().unwrap().clone() {
        return Endpoints {
            authorize: format!("{base}/{provider}/authorize"),
            token: format!("{base}/{provider}/token"),
            user: format!("{base}/{provider}/user"),
            emails: format!("{base}/{provider}/emails"),
            scope: "openid email",
        };
    }
    match provider {
        "microsoft" => {
            let tenant = if cfg.tenant.trim().is_empty() { "organizations" } else { cfg.tenant.trim() };
            Endpoints {
                authorize: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize"),
                token: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
                user: String::new(),
                emails: String::new(),
                scope: "openid profile email",
            }
        }
        "oidc" => Endpoints {
            authorize: cfg.authorize_url.clone(),
            token: cfg.token_url.clone(),
            user: String::new(),
            emails: String::new(),
            scope: "openid email profile",
        },
        "google" => Endpoints {
            authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            user: String::new(),
            emails: String::new(),
            scope: "openid email profile",
        },
        _ => Endpoints {
            authorize: "https://github.com/login/oauth/authorize".into(),
            token: "https://github.com/login/oauth/access_token".into(),
            user: "https://api.github.com/user".into(),
            emails: "https://api.github.com/user/emails".into(),
            scope: "read:user user:email",
        },
    }
}

/// The site's public URL: prefers the "Site URL" system setting, otherwise derived from the request's Host
pub fn base_url(st: &AppState, headers: &HeaderMap) -> String {
    let public = st.system.read().unwrap().public_url.clone();
    if !public.is_empty() {
        return public;
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    // Same as X-Forwarded-For: take the value the reverse proxy appended last
    let forwarded = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .filter(|_| st.trust_proxy.enabled())
        .and_then(|v| v.rsplit(',').next())
        .map(str::trim)
        .filter(|v| matches!(*v, "http" | "https"));
    let scheme = forwarded.unwrap_or(if st.https() { "https" } else { "http" });
    format!("{scheme}://{host}")
}

fn redirect_uri(base: &str, provider: &str) -> String {
    format!("{base}/api/auth/sso/{provider}/callback")
}

/// Only allows paths on this site, so it can't be abused to redirect to other sites
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.starts_with("/\\") && !n.chars().any(|c| c.is_control()) => n.to_string(),
        _ => "/files".into(),
    }
}

fn encode(pairs: &[(&str, &str)]) -> String {
    let mut s = form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::{
        Router,
        routing::{get as rget, post as rpost},
    };
    use std::sync::{Arc, Mutex};

    /// Mock provider: the token endpoint returns the id_token content specified by the test; GitHub also has user and email endpoints
    #[derive(Default)]
    struct Mock {
        claims: Value,
        gh_user: Value,
        gh_emails: Value,
        last_form: String,
    }

    fn jwt(claims: &Value) -> String {
        format!("{}.{}.sig", URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#), URL_SAFE_NO_PAD.encode(claims.to_string()))
    }

    async fn mock_server() -> (Arc<Mutex<Mock>>, String) {
        let m = Arc::new(Mutex::new(Mock::default()));
        let (m1, m2, m3) = (m.clone(), m.clone(), m.clone());
        let app = Router::new()
            .route(
                "/{provider}/token",
                rpost(move |body: String| {
                    let m = m1.clone();
                    async move {
                        let mut m = m.lock().unwrap();
                        m.last_form = body;
                        Json(json!({ "access_token": "at", "id_token": jwt(&m.claims) }))
                    }
                }),
            )
            .route(
                "/github/user",
                rget(move || {
                    let m = m2.clone();
                    async move { Json(m.lock().unwrap().gh_user.clone()) }
                }),
            )
            .route(
                "/github/emails",
                rget(move || {
                    let m = m3.clone();
                    async move { Json(m.lock().unwrap().gh_emails.clone()) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        // An OpenID Connect provider's discovery document (the issuer is `<base>/realm`)
        let doc = json!({
            "issuer": format!("{base}/realm"),
            "authorization_endpoint": format!("{base}/oidc/authorize"),
            "token_endpoint": format!("{base}/oidc/token"),
        });
        let other = json!({ "issuer": "https://elsewhere.example", "authorization_endpoint": "https://elsewhere.example/a", "token_endpoint": "https://elsewhere.example/t" });
        let app = app
            .route("/realm/.well-known/openid-configuration", rget(move || async move { Json(doc) }))
            .route("/other/.well-known/openid-configuration", rget(move || async move { Json(other) }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (m, base)
    }

    fn enable(env: &testutil::TestEnv, f: impl FnOnce(&mut SsoSettings)) {
        let mut s = SsoSettings::default();
        for p in PROVIDERS {
            let c = s.provider_mut(p).unwrap();
            c.enabled = true;
            c.client_id = format!("{p}-client");
            c.client_secret = crate::testutil::password().into();
        }
        f(&mut s);
        *env.st.sso.write().unwrap() = s;
    }

    fn location(r: &Response) -> String {
        r.headers()[header::LOCATION].to_str().unwrap().to_string()
    }

    fn query_param(url: &str, key: &str) -> String {
        let q = url.split_once('?').unwrap().1;
        form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned()).unwrap_or_default()
    }

    /// A linking ticket, as the account menu gets it before navigating to `start`
    async fn ticket(env: &testutil::TestEnv, user: &User, provider: &str) -> String {
        let req = LinkReq { next: None, password: Some(testutil::password().into()), code: None };
        let Json(v) = start_link(State(env.st.clone()), user.clone(), Path(provider.into()), Json(req)).await.unwrap();
        query_param(v["url"].as_str().unwrap(), "link")
    }

    #[tokio::test]
    async fn linking_asks_for_the_password_and_tells_the_owner() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        enable(&env, |_| {});
        let amy = env.user("amy", true).await;
        let start_with =
            |password: Option<String>| start_link(State(env.st.clone()), amy.clone(), Path("google".into()), Json(LinkReq { next: None, password, code: None }));
        // A session alone doesn't link an account that then signs in without the password
        assert!(start_with(None).await.is_err());
        assert!(start_with(Some(testutil::wrong_password())).await.is_err());
        assert!(start_with(Some(testutil::password().into())).await.is_ok());
        // With two-factor sign-in, a code too
        let codes = crate::twofactor::tests::set_up_for(&env, &amy).await;
        assert!(start_with(Some(testutil::password().into())).await.is_err());
        let req = LinkReq { next: None, password: Some(testutil::password().into()), code: Some(codes[0].clone()) };
        assert!(start_link(State(env.st.clone()), amy.clone(), Path("google".into()), Json(req)).await.is_ok());

        // Once linked, the owner is told in the app (and by email when set up)
        let ben = env.user("ben", true).await;
        let r = login(&env, &m, "google", Some(ben.clone()), |n| google(n, "g-7", "ben@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc?sso_linked=google");
        let (kind, data): (String, String) =
            sqlx::query_as("SELECT kind, data FROM notifications WHERE user_id = ?").bind(ben.id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(kind, "sign_in_method");
        let data: Value = serde_json::from_str(&data).unwrap();
        assert_eq!((data["provider"].as_str(), data["account"].as_str()), (Some("google"), Some("ben@example.com")));
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn linking_follows_the_allowed_domains_and_keeps_only_verified_emails() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        let amy = env.user("amy", true).await;
        let linked = || async {
            sqlx::query_as::<_, (String,)>("SELECT email FROM user_identities WHERE user_id = ?").bind(amy.id).fetch_optional(&env.st.db).await.unwrap()
        };

        // Without a domain list any account can be linked, but an address the provider hasn't verified isn't kept
        // (it could later match an account by its email)
        enable(&env, |_| {});
        let r = login(&env, &m, "google", Some(amy.clone()), |n| google(n, "g-1", "amy_files@example.com", false)).await;
        assert_eq!(location(&r), "/files/abc?sso_linked=google");
        assert_eq!(linked().await, Some((String::new(),)));
        sqlx::query("DELETE FROM user_identities").execute(&env.st.db).await.unwrap();

        // With a domain list, only a verified address in it
        enable(&env, |s| s.allowed_domains = vec!["example.com".into()]);
        let r = login(&env, &m, "google", Some(amy.clone()), |n| google(n, "g-2", "amy@elsewhere.example", true)).await;
        assert!(location(&r).contains("sso_error="), "{}", location(&r));
        let r = login(&env, &m, "google", Some(amy.clone()), |n| google(n, "g-3", "amy@example.com", false)).await;
        assert!(location(&r).contains("sso_error="), "{}", location(&r));
        assert_eq!(linked().await, None);
        let r = login(&env, &m, "google", Some(amy.clone()), |n| google(n, "g-4", "amy@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc?sso_linked=google");
        assert_eq!(linked().await, Some(("amy@example.com".into(),)));

        // A later sign-in without a verified address doesn't replace the one kept
        let r = login(&env, &m, "google", None, |n| google(n, "g-4", "someone@example.com", false)).await;
        assert_eq!(location(&r), "/files/abc");
        assert_eq!(linked().await, Some(("amy@example.com".into(),)));

        // An administrator resetting the password removes the linked sign-in methods
        let admin = env.admin().await;
        let req = serde_json::from_value(json!({ "password": crate::util::random_token(20) })).unwrap();
        let _ = crate::admin::update(State(env.st.clone()), Admin(admin), Path(amy.id), Json(req)).await.unwrap();
        assert_eq!(linked().await, None);
        *MOCK_BASE.lock().unwrap() = None;
    }

    /// Runs the whole flow: start sign-in → (provider) → callback; claims are generated from the nonce
    async fn login(env: &testutil::TestEnv, m: &Arc<Mutex<Mock>>, provider: &str, user: Option<User>, make: impl FnOnce(&str) -> Value) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "drive.test".parse().unwrap());
        let link = match &user {
            Some(u) => Some(ticket(env, u, provider).await),
            None => None,
        };
        let r = start(
            State(env.st.clone()),
            Path(provider.into()),
            Query(StartQuery { next: Some("/files/abc".into()), link }),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            headers.clone(),
            user.clone().ok_or_else(AppError::unauthorized),
        )
        .await;
        let url = location(&r);
        // Same browser: send back the state cookie set by start
        let set = r.headers()[header::SET_COOKIE].to_str().unwrap();
        headers.insert(header::COOKIE, set.split(';').next().unwrap().parse().unwrap());
        assert!(url.contains("code_challenge_method=S256"), "{url}");
        assert_eq!(query_param(&url, "redirect_uri"), format!("http://drive.test/api/auth/sso/{provider}/callback"));
        let (state, nonce) = (query_param(&url, "state"), query_param(&url, "nonce"));
        m.lock().unwrap().claims = make(&nonce);
        let q = CallbackQuery { code: Some("code-1".into()), state: Some(state), error: None, error_description: None };
        callback(
            State(env.st.clone()),
            Path(provider.into()),
            Query(q),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            headers,
            user.ok_or_else(AppError::unauthorized),
        )
        .await
    }

    fn google(nonce: &str, sub: &str, email: &str, verified: bool) -> Value {
        json!({ "iss": "https://accounts.google.com", "aud": "google-client", "exp": now() + 600, "nonce": nonce, "sub": sub, "email": email, "email_verified": verified, "name": "Test" })
    }

    /// An account that signs in only through single sign-on, as one it created
    async fn without_password(env: &testutil::TestEnv, user: &User) {
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?").bind(NO_PASSWORD).bind(user.id).execute(&env.st.db).await.unwrap();
    }

    async fn identities_of(env: &testutil::TestEnv, user: &User) -> i64 {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM user_identities WHERE user_id = ?").bind(user.id).fetch_one(&env.st.db).await.unwrap();
        n
    }

    /// Tests share MOCK_BASE: only one runs at a time
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn callback_must_come_from_the_browser_that_started() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        enable(&env, |_| {});
        env.user("amy@example.com", true).await;
        // An attacker starts a sign-in and hands the callback URL to another browser (without the state cookie)
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "drive.test".parse().unwrap());
        let r = start(
            State(env.st.clone()),
            Path("google".into()),
            Query(StartQuery { next: None, link: None }),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            headers.clone(),
            Err(AppError::unauthorized()),
        )
        .await;
        let url = location(&r);
        let (state, nonce) = (query_param(&url, "state"), query_param(&url, "nonce"));
        m.lock().unwrap().claims = google(&nonce, "g-1", "amy@example.com", true);
        let q = CallbackQuery { code: Some("code-1".into()), state: Some(state), error: None, error_description: None };
        let r =
            callback(State(env.st.clone()), Path("google".into()), Query(q), ConnectInfo("127.0.0.1:1".parse().unwrap()), headers, Err(AppError::unauthorized()))
                .await;
        // The reason goes to the sign-in page in a cookie: the address only says there is one, so a link can't make
        // the page show a text of its own
        assert_eq!(location(&r), "/login?sso_error=1");
        let set: Vec<&str> = r.headers().get_all(header::SET_COOKIE).iter().map(|v| v.to_str().unwrap()).collect();
        assert!(set.iter().all(|c| !c.starts_with(crate::auth::SESSION_COOKIE)), "no sign-in session was created: {set:?}");
        let reason = set.iter().find_map(|c| c.strip_prefix("tf_sso_error=")).expect("the reason is in a cookie");
        assert!(reason.contains("Max-Age=") && reason.contains("Path=/") && !reason.contains("HttpOnly"), "{reason}");
        let text = reason.split(';').next().unwrap();
        assert!(!text.is_empty() && percent_encoding::percent_decode_str(text).decode_utf8().unwrap().len() > 10, "{reason}");
    }

    #[tokio::test]
    async fn linking_needs_a_ticket_from_the_same_user() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (_m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        enable(&env, |_| {});
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "drive.test".parse().unwrap());
        let go = |user: &User, link: Option<String>| {
            start(
                State(env.st.clone()),
                Path("google".into()),
                Query(StartQuery { next: Some("/files".into()), link }),
                ConnectInfo("127.0.0.1:1".parse().unwrap()),
                headers.clone(),
                Ok(user.clone()),
            )
        };
        // A link started from another website carries no ticket, or one that doesn't exist
        assert!(location(&go(&amy, Some("made-up".into())).await).contains("sso_error="));
        // Someone else's ticket doesn't work
        let bens = ticket(&env, &ben, "google").await;
        assert!(location(&go(&amy, Some(bens)).await).contains("sso_error="));
        // Her own works once
        let amys = ticket(&env, &amy, "google").await;
        assert!(location(&go(&amy, Some(amys.clone())).await).contains("code_challenge_method=S256"));
        assert!(location(&go(&amy, Some(amys)).await).contains("sso_error="));
    }

    #[tokio::test]
    async fn link_must_be_completed_by_the_same_user() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        enable(&env, |_| {});
        let attacker = env.user("mallory", true).await;
        let victim = env.user("victim", true).await;
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "drive.test".parse().unwrap());
        let link = Some(ticket(&env, &attacker, "google").await);
        let r = start(
            State(env.st.clone()),
            Path("google".into()),
            Query(StartQuery { next: None, link }),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            headers.clone(),
            Ok(attacker),
        )
        .await;
        let url = location(&r);
        let set = r.headers()[header::SET_COOKIE].to_str().unwrap().to_string();
        headers.insert(header::COOKIE, set.split(';').next().unwrap().parse().unwrap());
        let (state, nonce) = (query_param(&url, "state"), query_param(&url, "nonce"));
        m.lock().unwrap().claims = google(&nonce, "g-9", "victim@example.com", true);
        let q = CallbackQuery { code: Some("code-1".into()), state: Some(state), error: None, error_description: None };
        let r = callback(State(env.st.clone()), Path("google".into()), Query(q), ConnectInfo("127.0.0.1:1".parse().unwrap()), headers, Ok(victim)).await;
        assert!(location(&r).contains("sso_error="), "{}", location(&r));
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM user_identities").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 0, "no external account was linked");
    }

    #[tokio::test]
    async fn microsoft_tenant_must_be_an_id_or_domain() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        for (tenant, ok) in
            [("evil.example/x?", false), ("a@b", false), ("contoso.onmicrosoft.com", true), ("72f988bf-86f1-41af-91ab-2d7cd011db47", true), ("", true)]
        {
            let mut s = SsoSettings::default();
            s.microsoft.tenant = tenant.into();
            let res = update_settings(State(env.st.clone()), Admin(admin.clone()), HeaderMap::new(), Json(s)).await;
            assert_eq!(res.is_ok(), ok, "{tenant}");
        }
    }

    #[test]
    fn sso_username_follows_the_account_rules() {
        assert_eq!(sso_username("amy+drive@example.com"), "amy_drive@example.com");
        assert_eq!(sso_username("a@b"), "a@b");
        let long = sso_username("a.very.long.name.that.keeps.going@subsidiary.example.com");
        assert!(long.chars().count() <= 28 && !long.contains('@'));
        // CJK local parts must be replaced with allowed characters
        for name in ["amy+drive@example.com", "王小明@example.com", "x@y", "+@z"] {
            assert!(crate::admin::validate_username(&sso_username(name)).is_ok(), "{name}");
        }
    }

    #[tokio::test]
    async fn sso_login_links_existing_users_and_respects_policy() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        enable(&env, |_| {});
        let amy = env.user("amy@example.com", true).await;
        without_password(&env, &amy).await;

        // The username is the email: link automatically, sign in, and go back to the originally requested page
        let r = login(&env, &m, "google", None, |n| google(n, "g-1", "Amy@Example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        assert!(r.headers().contains_key(header::SET_COOKIE));
        assert!(m.lock().unwrap().last_form.contains("code_verifier="), "PKCE: the token exchange must include code_verifier");
        // Afterwards sign-in uses the external account's identifier (even if the email changes)
        let r = login(&env, &m, "google", None, |n| google(n, "g-1", "renamed@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");

        // No account and automatic creation off: reject with an explanation
        let r = login(&env, &m, "google", None, |n| google(n, "g-2", "ben@example.com", true)).await;
        assert!(location(&r).starts_with("/login?sso_error="));
        // Automatic creation on: create the account (with the default space size)
        enable(&env, |s| s.google.provisioning = Provisioning::Create);
        env.st.system.write().unwrap().default_user_quota = 5 << 30;
        let r = login(&env, &m, "google", None, |n| google(n, "g-2", "ben@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        let (quota,): (i64,) = sqlx::query_as("SELECT quota_bytes FROM users WHERE username = 'ben@example.com'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(quota, 5 << 30);

        // Domain restriction, unverified email
        enable(&env, |s| {
            s.google.provisioning = Provisioning::Create;
            s.allowed_domains = vec!["example.com".into()];
        });
        let r = login(&env, &m, "google", None, |n| google(n, "g-3", "eve@evil.example", true)).await;
        assert!(location(&r).contains("sso_error"));
        let r = login(&env, &m, "google", None, |n| google(n, "g-4", "eve@example.com", false)).await;
        assert!(location(&r).contains("sso_error"));

        // nonce mismatch (replaying someone else's token), aud mismatch
        let r = login(&env, &m, "google", None, |_| google("other", "g-1", "amy@example.com", true)).await;
        assert!(location(&r).contains("sso_error"));
        let r = login(&env, &m, "google", None, |n| json!({ "aud": "someone-else", "exp": now() + 600, "nonce": n, "sub": "g-1" })).await;
        assert!(location(&r).contains("sso_error"));

        // Nonexistent or already used state
        let q = CallbackQuery { code: Some("c".into()), state: Some("not-a-state".into()), error: None, error_description: None };
        let r = callback(
            State(env.st.clone()),
            Path("google".into()),
            Query(q),
            ConnectInfo("127.0.0.1:1".parse().unwrap()),
            HeaderMap::new(),
            Err(AppError::unauthorized()),
        )
        .await;
        assert!(location(&r).contains("sso_error"));

        // Disabled users can't sign in
        sqlx::query("UPDATE users SET disabled = 1 WHERE username = 'amy@example.com'").execute(&env.st.db).await.unwrap();
        let r = login(&env, &m, "google", None, |n| google(n, "g-1", "amy@example.com", true)).await;
        assert!(location(&r).contains("sso_error"));
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn any_openid_connect_provider_is_set_up_from_its_issuer() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        let admin = env.admin().await;
        let save = |oidc: Value| {
            let req = serde_json::from_value(json!({ "oidc": oidc })).unwrap();
            update_settings(State(env.st.clone()), Admin(admin.clone()), HeaderMap::new(), Json(req))
        };
        let oidc = |issuer: &str| json!({ "enabled": true, "client_id": "oidc-client", "client_secret": testutil::password(), "name": "Company login", "issuer": issuer, "provisioning": "create" });
        // An issuer that isn't https, or whose document names another issuer: refused
        assert!(save(oidc("http://auth.example.com")).await.is_err());
        assert!(save(oidc(&format!("{base}/other"))).await.is_err());
        // Its endpoints are read from the discovery document
        let _ = save(oidc(&format!("{base}/realm/"))).await.unwrap();
        let cfg = env.st.sso.read().unwrap().oidc.clone();
        assert_eq!((cfg.issuer, cfg.authorize_url, cfg.token_url), (format!("{base}/realm"), format!("{base}/oidc/authorize"), format!("{base}/oidc/token")));
        let Json(list) = providers(State(env.st.clone())).await;
        assert!(list.as_array().unwrap().iter().any(|p| p["id"] == "oidc" && p["label"] == "Company login"), "{list}");

        // Signing in works like with the others: an account is created for the verified email
        *MOCK_BASE.lock().unwrap() = Some(base);
        let r = login(&env, &m, "oidc", None, |n| {
            json!({ "iss": "https://auth.example.com/realm", "aud": "oidc-client", "exp": now() + 600, "nonce": n, "sub": "k-1", "email": "kim@example.com", "email_verified": true, "name": "Kim" })
        })
        .await;
        assert_eq!(location(&r), "/files/abc");
        let (source,): (String,) = sqlx::query_as("SELECT source FROM users WHERE username = 'kim@example.com'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(source, "oidc");
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn an_address_only_signs_in_to_the_account_made_for_it() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        enable(&env, |s| s.google.provisioning = Provisioning::Create);
        let user_of = |subject: &'static str| {
            let db = env.st.db.clone();
            async move {
                let (id,): (i64,) = sqlx::query_as("SELECT user_id FROM user_identities WHERE subject = ?").bind(subject).fetch_one(&db).await.unwrap();
                id
            }
        };

        // "amy+files@…" gets the username "amy_files@…"
        let r = login(&env, &m, "google", None, |n| google(n, "g-30", "amy+files@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        let amy = user_of("g-30").await;
        let (name,): (String,) = sqlx::query_as("SELECT username FROM users WHERE id = ?").bind(amy).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(name, "amy_files@example.com");
        // Someone else whose address really is "amy_files@…" gets an account of their own, not Amy's
        let r = login(&env, &m, "google", None, |n| google(n, "g-31", "amy_files@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        assert_ne!(user_of("g-31").await, amy);

        // The same address from another provider still finds the account made for it
        let r = login(&env, &m, "google", None, |n| google(n, "g-32", "ben@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        let ben = user_of("g-32").await;
        let r = login(&env, &m, "google", None, |n| google(n, "g-33", "Ben@Example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        assert_eq!(user_of("g-33").await, ben);
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn microsoft_email_is_only_trusted_with_a_specific_tenant() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        let amy = env.user("amy@example.com", true).await;
        without_password(&env, &amy).await;
        let ms = |n: &str| json!({ "iss": "https://login.microsoftonline.com/t1/v2.0", "aud": "microsoft-client", "exp": now() + 600, "nonce": n, "tid": "t1", "oid": "o1", "preferred_username": "amy@example.com" });

        // "Any organization": other tenants can fill in the email themselves, so it can't be used to match accounts
        enable(&env, |_| {});
        let r = login(&env, &m, "microsoft", None, ms).await;
        assert!(location(&r).contains("sso_error"));
        // Works once a specific tenant is set
        enable(&env, |s| s.microsoft.tenant = "t1".into());
        let r = login(&env, &m, "microsoft", None, ms).await;
        assert_eq!(location(&r), "/files/abc");
        let (subject,): (String,) = sqlx::query_as("SELECT subject FROM user_identities WHERE provider = 'microsoft'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(subject, "t1:o1");
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn accounts_with_a_password_or_second_factor_are_linked_only_by_their_owner() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        enable(&env, |_| {});

        // The username is the verified email, but signing in through the provider would skip the account's password
        let amy = env.user("amy@example.com", true).await;
        let r = login(&env, &m, "google", None, |n| google(n, "g-40", "amy@example.com", true)).await;
        assert!(location(&r).contains("sso_error"), "{}", location(&r));
        assert!(r.headers().get_all(header::SET_COOKIE).iter().all(|c| !c.to_str().unwrap().starts_with(crate::auth::SESSION_COOKIE)));
        assert_eq!(identities_of(&env, &amy).await, 0);
        // Its owner links it from the account menu, with the password, and then it signs in
        let r = login(&env, &m, "google", Some(amy.clone()), |n| google(n, "g-40", "amy@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc?sso_linked=google");
        let r = login(&env, &m, "google", None, |n| google(n, "g-40", "amy@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");

        // Nor an account with two-factor sign-in
        let ben = env.user("ben@example.com", true).await;
        without_password(&env, &ben).await;
        sqlx::query("UPDATE users SET totp_secret = 'sealed' WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        let r = login(&env, &m, "google", None, |n| google(n, "g-41", "ben@example.com", true)).await;
        assert!(location(&r).contains("sso_error"), "{}", location(&r));
        assert_eq!(identities_of(&env, &ben).await, 0);

        // An account with neither is linked by its email, and its owner is told, as when they link one themselves
        let cat = env.user("cat@example.com", true).await;
        without_password(&env, &cat).await;
        let r = login(&env, &m, "google", None, |n| google(n, "g-42", "cat@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        assert_eq!(identities_of(&env, &cat).await, 1);
        let (kind, data): (String, String) =
            sqlx::query_as("SELECT kind, data FROM notifications WHERE user_id = ?").bind(cat.id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(kind, "sign_in_method");
        let data: Value = serde_json::from_str(&data).unwrap();
        assert_eq!((data["provider"].as_str(), data["account"].as_str()), (Some("google"), Some("cat@example.com")));
        // Signing in again with it isn't news
        let r = login(&env, &m, "google", None, |n| google(n, "g-42", "cat@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notifications WHERE user_id = ?").bind(cat.id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 1);
        // Nor is an account created for the person
        enable(&env, |s| s.google.provisioning = Provisioning::Create);
        let r = login(&env, &m, "google", None, |n| google(n, "g-43", "dan@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notifications n JOIN users u ON u.id = n.user_id WHERE u.username = 'dan@example.com'")
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        assert_eq!(n, 0);
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn microsoft_guests_are_matched_by_email_only_when_its_domain_is_verified() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        let amy = env.user("amy@example.com", true).await;
        without_password(&env, &amy).await;
        enable(&env, |s| s.microsoft.tenant = "t1".into());
        let ms = |oid: &str, extra: Value| {
            let oid = oid.to_string();
            move |n: &str| {
                let mut v = json!({ "iss": "https://login.microsoftonline.com/t1/v2.0", "aud": "microsoft-client", "exp": now() + 600, "nonce": n, "tid": "t1", "oid": oid, "email": "amy@example.com" });
                v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
                v
            }
        };

        // A guest signs in through this tenant, but the email comes from their own organization
        for guest in [
            json!({ "idp": "https://sts.windows.net/t2/" }),
            json!({ "idp": "live.com" }),
            json!({ "acct": 1 }),
            json!({ "idp": "https://sts.windows.net/t2/", "xms_edov": false }),
        ] {
            let r = login(&env, &m, "microsoft", None, ms("o1", guest.clone())).await;
            assert!(location(&r).contains("sso_error"), "{guest}: {}", location(&r));
            assert_eq!(identities_of(&env, &amy).await, 0, "{guest}");
        }
        // Unless Microsoft says the owner of the email's domain is verified (the optional claim xms_edov)
        let r = login(&env, &m, "microsoft", None, ms("o1", json!({ "idp": "https://sts.windows.net/t2/", "xms_edov": true }))).await;
        assert_eq!(location(&r), "/files/abc");
        assert_eq!(identities_of(&env, &amy).await, 1);
        sqlx::query("DELETE FROM user_identities").execute(&env.st.db).await.unwrap();
        // Members of the tenant are matched as before
        let r = login(&env, &m, "microsoft", None, ms("o2", json!({ "idp": "https://sts.windows.net/t1/", "acct": 0 }))).await;
        assert_eq!(location(&r), "/files/abc");
        assert_eq!(identities_of(&env, &amy).await, 1);
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn github_uses_the_verified_primary_email_and_accounts_can_be_linked() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        enable(&env, |_| {});
        let amy = env.user("amy", true).await;
        {
            let mut mm = m.lock().unwrap();
            mm.gh_user = json!({ "id": 42, "login": "amy-gh", "name": "Amy" });
            mm.gh_emails = json!([{ "email": "amy@personal.example", "primary": true, "verified": true }]);
        }
        // The username isn't the email: can't match automatically
        let r = login(&env, &m, "github", None, |_| json!({})).await;
        assert!(location(&r).contains("sso_error"));
        // Link it yourself after signing in
        let r = login(&env, &m, "github", Some(amy.clone()), |_| json!({})).await;
        assert_eq!(location(&r), "/files/abc?sso_linked=github");
        let r = login(&env, &m, "github", None, |_| json!({})).await;
        assert_eq!(location(&r), "/files/abc");
        // The same external account can't be linked to someone else
        let ben = env.user("ben", true).await;
        let r = login(&env, &m, "github", Some(ben), |_| json!({})).await;
        assert!(location(&r).contains("sso_error"));
        // After unlinking it can't be used to sign in
        let _ = unlink(State(env.st.clone()), amy, Path("github".into()), ConnectInfo("127.0.0.1:1".parse().unwrap()), HeaderMap::new()).await.unwrap();
        let r = login(&env, &m, "github", None, |_| json!({})).await;
        assert!(location(&r).contains("sso_error"));
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[test]
    fn next_must_stay_on_this_site() {
        assert_eq!(safe_next(Some("/files/1?x=2")), "/files/1?x=2");
        for bad in ["https://evil.example", "//evil.example", "/\\evil.example", "javascript:alert(1)"] {
            assert_eq!(safe_next(Some(bad)), "/files", "{bad}");
        }
    }

    #[tokio::test]
    async fn provisioning_policy_is_per_provider() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        let (gid,): (i64,) =
            sqlx::query_as("INSERT INTO groups (name, description, created_at) VALUES ('staff', '', 0) RETURNING id").fetch_one(&env.st.db).await.unwrap();
        env.user("carol@example.com", true).await;

        // Google creates accounts with its own defaults, domain list and group; GitHub is linked accounts only
        enable(&env, |s| {
            s.allowed_domains = vec!["other.example".into()];
            s.google.provisioning = Provisioning::Create;
            s.google.allowed_domains = vec!["example.com".into()];
            s.google.defaults = NewUserDefaults { can_write: true, can_delete: false, can_share: false, quota_bytes: Some(1 << 30) };
            s.google.groups = vec![gid, 9999];
            s.github.provisioning = Provisioning::Off;
        });
        let r = login(&env, &m, "google", None, |n| google(n, "g-10", "dana@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        let row: (bool, bool, bool, i64, String, Option<String>) =
            sqlx::query_as("SELECT can_write, can_delete, can_share, quota_bytes, source, provisioned_by FROM users WHERE username = 'dana@example.com'")
                .fetch_one(&env.st.db)
                .await
                .unwrap();
        assert_eq!(row, (true, false, false, 1 << 30, "google".into(), Some("g-10".into())));
        // The new account has no password, so its only linked account can't be removed until an administrator sets one
        {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (dana_id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE username = 'dana@example.com'").fetch_one(&mut *conn).await.unwrap();
            let dana = crate::auth::user_by_id(&env.st, &mut conn, dana_id).await.unwrap().unwrap();
            drop(conn);
            let from = || ConnectInfo("127.0.0.1:1".parse().unwrap());
            assert!(unlink(State(env.st.clone()), dana.clone(), Path("google".into()), from(), HeaderMap::new()).await.is_err());
            // A provider that isn't linked: nothing to do
            assert!(unlink(State(env.st.clone()), dana.clone(), Path("github".into()), from(), HeaderMap::new()).await.is_ok());
            let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM user_identities WHERE user_id = ?").bind(dana_id).fetch_one(&env.st.db).await.unwrap();
            assert_eq!(left, 1);
        }
        let (member,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM group_members m JOIN users u ON u.id = m.user_id WHERE u.username = 'dana@example.com' AND m.group_id = ?")
                .bind(gid)
                .fetch_one(&env.st.db)
                .await
                .unwrap();
        assert_eq!(member, 1);
        // The provider's own domain list replaces the global one
        let r = login(&env, &m, "google", None, |n| google(n, "g-11", "eve@other.example", true)).await;
        assert!(location(&r).contains("sso_error"));

        // GitHub "off": even an existing user whose username is the email isn't matched
        {
            let mut mm = m.lock().unwrap();
            mm.gh_user = json!({ "id": 77, "login": "carol" });
            mm.gh_emails = json!([{ "email": "carol@example.com", "primary": true, "verified": true }]);
        }
        let r = login(&env, &m, "github", None, |_| json!({})).await;
        assert!(location(&r).contains("sso_error"));

        // Accounts created per hour are capped
        sqlx::query("UPDATE users SET created_at = ? WHERE source = 'google'").bind(now()).execute(&env.st.db).await.unwrap();
        for i in 0..(MAX_CREATED_PER_HOUR - 1) {
            let hash = crate::auth::hash_password("x".into()).await.unwrap();
            let mut c = env.st.db.acquire().await.unwrap();
            crate::admin::create_user(
                &mut c,
                NewUser {
                    username: &format!("bulk{i}@example.com"),
                    password_hash: &hash,
                    role: "user",
                    can_write: true,
                    can_delete: true,
                    can_share: true,
                    quota_bytes: 0,
                    source: "google",
                    provisioned_by: None,
                    personal_space: None,
                    space_folders: None,
                },
            )
            .await
            .unwrap();
        }
        let r = login(&env, &m, "google", None, |n| google(n, "g-12", "frank@example.com", true)).await;
        assert!(location(&r).contains("sso_error"), "creation paused after {MAX_CREATED_PER_HOUR} accounts in an hour");
        // Existing users still sign in
        let r = login(&env, &m, "google", None, |n| google(n, "g-10", "dana@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn domain_rules_and_profile_sync() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        let (gid,): (i64,) =
            sqlx::query_as("INSERT INTO groups (name, description, created_at) VALUES ('partners', '', 0) RETURNING id").fetch_one(&env.st.db).await.unwrap();
        enable(&env, |s| {
            s.google.provisioning = Provisioning::Create;
            s.google.defaults = NewUserDefaults { can_write: true, can_delete: true, can_share: true, quota_bytes: None };
            s.domain_rules = vec![DomainRule {
                domain: "partner.example".into(),
                can_write: true,
                can_delete: false,
                can_share: false,
                quota_bytes: Some(512 << 20),
                groups: vec![gid],
                ..Default::default()
            }];
        });
        let named = |n: &str, sub: &str, email: &str, name: &str| {
            let mut v = google(n, sub, email, true);
            v["name"] = json!(name);
            v
        };

        // The domain rule wins over the provider defaults; the display name comes from the provider
        let r = login(&env, &m, "google", None, |n| named(n, "g-20", "pat@partner.example", "Pat Partner")).await;
        assert_eq!(location(&r), "/files/abc");
        let row: (bool, bool, i64, String) =
            sqlx::query_as("SELECT can_write, can_delete, quota_bytes, display_name FROM users WHERE username = 'pat@partner.example'")
                .fetch_one(&env.st.db)
                .await
                .unwrap();
        assert_eq!(row, (true, false, 512 << 20, "Pat Partner".into()));
        let (member,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM group_members m JOIN users u ON u.id = m.user_id WHERE u.username = 'pat@partner.example' AND m.group_id = ?")
                .bind(gid)
                .fetch_one(&env.st.db)
                .await
                .unwrap();
        assert_eq!(member, 1);
        // Other domains get the provider defaults
        let r = login(&env, &m, "google", None, |n| named(n, "g-21", "zoe@example.com", "Zoe")).await;
        assert_eq!(location(&r), "/files/abc");
        let (del,): (bool,) = sqlx::query_as("SELECT can_delete FROM users WHERE username = 'zoe@example.com'").fetch_one(&env.st.db).await.unwrap();
        assert!(del);

        // The provider's name follows on later sign-ins…
        let r = login(&env, &m, "google", None, |n| named(n, "g-20", "pat@partner.example", "Pat P. Partner")).await;
        assert_eq!(location(&r), "/files/abc");
        let (name,): (String,) = sqlx::query_as("SELECT display_name FROM users WHERE username = 'pat@partner.example'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(name, "Pat P. Partner");
        // …unless an administrator set a different display name
        sqlx::query("UPDATE users SET display_name = 'Patricia' WHERE username = 'pat@partner.example'").execute(&env.st.db).await.unwrap();
        let r = login(&env, &m, "google", None, |n| named(n, "g-20", "pat@partner.example", "Pat Renamed")).await;
        assert_eq!(location(&r), "/files/abc");
        let (name,): (String,) = sqlx::query_as("SELECT display_name FROM users WHERE username = 'pat@partner.example'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(name, "Patricia");
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn domain_rules_choose_the_personal_space_and_a_missing_folder_doesnt_stop_the_sign_in() {
        let _g = SERIAL.lock().await;
        let env = testutil::folders_env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        // A Local folder location whose folder isn't there (a share that isn't mounted)
        let nas = env.dir.join("nas");
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES ('nas', 'NAS', 'local', ?, 0, 0)")
            .bind(json!({ "path": nas.to_string_lossy() }).to_string())
            .execute(&env.st.db)
            .await
            .unwrap();
        let rules = vec![
            DomainRule { domain: "guest.example".into(), personal_space: Some(false), ..Default::default() },
            DomainRule { domain: "nas.example".into(), personal_location: Some("nas".into()), ..Default::default() },
        ];
        // A rule's location must exist
        let bad = vec![DomainRule { domain: "x.example".into(), personal_location: Some("nope".into()), ..Default::default() }];
        assert!(check_domain_rules(&env.st, bad).await.is_err());
        let rules = check_domain_rules(&env.st, rules).await.unwrap();
        enable(&env, |s| {
            s.google.provisioning = Provisioning::Create;
            s.domain_rules = rules;
        });
        let db = env.st.db.clone();
        let space = |email: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
                    "SELECT u.root_id, (SELECT location_id FROM drives WHERE kind = 'personal' AND owner_id = u.id), u.personal_pending FROM users u WHERE username = ?",
                )
                .bind(email)
                .fetch_one(&db)
                .await
                .unwrap()
            }
        };

        // No personal space for guests
        let r = login(&env, &m, "google", None, |n| google(n, "g-30", "gia@guest.example", true)).await;
        assert_eq!(location(&r), "/files/abc");
        assert_eq!(space("gia@guest.example").await, (None, None, None));
        // On the rule's location: its folder is missing, but signing in works and the space waits for it
        let r = login(&env, &m, "google", None, |n| google(n, "g-31", "ned@nas.example", true)).await;
        assert_eq!(location(&r), "/files/abc");
        assert_eq!(space("ned@nas.example").await, (None, None, Some("nas".into())));
        // Once the folder is back (with the location's marker), the next sign-in creates it there
        crate::storage::claim_folder(&nas, "nas").unwrap();
        let r = login(&env, &m, "google", None, |n| google(n, "g-31", "ned@nas.example", true)).await;
        assert_eq!(location(&r), "/files/abc");
        let (root, at, pending) = space("ned@nas.example").await;
        assert!(root.is_some());
        assert_eq!((at.as_deref(), pending), (Some("nas"), None));
        assert!(nas.join("users/ned@nas.example").is_dir());
        // Other domains follow the system setting
        let r = login(&env, &m, "google", None, |n| google(n, "g-32", "zoe@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        assert_eq!(space("zoe@example.com").await.1.as_deref(), Some(crate::locations::BUILTIN));
        *MOCK_BASE.lock().unwrap() = None;
    }
}
