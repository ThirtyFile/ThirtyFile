//! Third-party sign-in (SSO): Microsoft Entra ID, Google, GitHub.
//!
//! OAuth 2.0 authorization code flow + PKCE:
//! 1. `/api/auth/sso/{provider}/start` generates state, nonce and PKCE, and redirects to the provider's sign-in page
//! 2. The provider redirects back to `/api/auth/sso/{provider}/callback`, and the server exchanges the authorization code with the provider for the identity (server to server, over TLS)
//! 3. The user is found by the external account's stable identifier and signed in; on first sign-in, an existing account is matched by email, or (when enabled) an account is created automatically
//!
//! Security: state is single-use and valid for 10 minutes; ID tokens are checked for aud, iss, exp and nonce;
//! Microsoft emails are trusted only when a specific tenant is set (with "any organization", other tenants can fill in someone else's email).

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
    auth::{Admin, User, client_ip, open_session},
    db::{NewUser, create_user, get_setting, set_setting},
    error::{AppError, AppResult},
    logs::record_login_via,
    state::AppState,
    tree,
    util::{now, random_token},
};

pub const PROVIDERS: [&str; 3] = ["microsoft", "google", "github"];
const PENDING_TTL: Duration = Duration::from_secs(600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Stored instead of a password hash for accounts that sign in through a provider only (it isn't a valid hash, so nothing matches it)
pub const NO_PASSWORD: &str = "!";

pub fn label(provider: &str) -> &'static str {
    match provider {
        "microsoft" => "Microsoft",
        "google" => "Google",
        _ => "GitHub",
    }
}

/// What happens when someone signs in with an external account that isn't linked to a user yet
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provisioning {
    /// Only accounts linked under "Sign-in methods" can sign in
    Off,
    /// An existing user whose username is the (verified) email is linked automatically
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
}

impl Default for DomainRule {
    fn default() -> Self {
        Self { domain: String::new(), can_write: true, can_delete: true, can_share: true, quota_bytes: None, groups: Vec::new() }
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
    /// None = not decided yet (settings saved by an older version): derived from the legacy `auto_create` flag at load time
    pub provisioning: Option<Provisioning>,
    /// Email domains allowed for this provider (lowercase, without @); empty = the global list
    pub allowed_domains: Vec<String>,
    /// Permissions and space size of automatically created accounts
    pub defaults: NewUserDefaults,
    /// Groups automatically created accounts are added to
    pub groups: Vec<i64>,
}

impl ProviderConfig {
    fn ready(&self) -> bool {
        self.enabled && !self.client_id.trim().is_empty() && !self.client_secret.is_empty()
    }
    pub fn provisioning(&self) -> Provisioning {
        self.provisioning.unwrap_or(Provisioning::Link)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SsoSettings {
    pub microsoft: ProviderConfig,
    pub google: ProviderConfig,
    pub github: ProviderConfig,
    /// Email domains allowed to sign in (lowercase, without @) unless a provider has its own list; empty = unrestricted
    pub allowed_domains: Vec<String>,
    /// Per-domain settings for automatically created accounts (a domain here still has to be allowed to sign in)
    pub domain_rules: Vec<DomainRule>,
    /// Legacy (settings saved before per-provider policies): replaced by `ProviderConfig::provisioning`
    #[serde(skip_serializing)]
    pub auto_create: bool,
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
            _ => None,
        }
    }
    /// Removes a deleted group from the groups new accounts join; returns whether anything changed
    pub fn forget_group(&mut self, id: i64) -> bool {
        let lists = [&mut self.microsoft.groups, &mut self.google.groups, &mut self.github.groups]
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
    // Settings saved before per-provider policies: the old global flag becomes each provider's policy
    let legacy = if s.auto_create { Provisioning::Create } else { Provisioning::Link };
    for p in PROVIDERS {
        let c = s.provider_mut(p).unwrap();
        c.provisioning.get_or_insert(legacy);
    }
    s
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
    let scheme = forwarded.unwrap_or(if st.secure_cookie { "https" } else { "http" });
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

// ───────────── Public: available sign-in methods, starting sign-in, provider callback ─────────────

pub async fn providers(State(st): State<AppState>) -> Json<Value> {
    let cfg = st.sso.read().unwrap().clone();
    let list: Vec<Value> = PROVIDERS
        .iter()
        .filter(|p| cfg.provider(p).is_some_and(ProviderConfig::ready))
        .map(|p| json!({ "id": p, "label": label(p) }))
        .collect();
    Json(json!(list))
}

#[derive(Deserialize)]
pub struct StartQuery {
    next: Option<String>,
    /// Link to the currently signed-in account ("My account › Sign-in methods"): a ticket from `link`
    link: Option<String>,
}

/// One-time tickets for linking a sign-in method: ticket → (user, created)
fn link_tickets() -> &'static std::sync::Mutex<std::collections::HashMap<String, (i64, Instant)>> {
    static T: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (i64, Instant)>>> = std::sync::OnceLock::new();
    T.get_or_init(Default::default)
}
const LINK_TICKET_TTL: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
pub struct LinkReq {
    next: Option<String>,
}

/// Starts linking a sign-in method to the signed-in account. A POST, which other websites can't send on the user's
/// behalf (origin check), hands out a short-lived ticket for the page to navigate to `start` with; `start` accepts
/// linking only with such a ticket, so another website can't make a user link an account signed in in their browser.
pub async fn start_link(State(st): State<AppState>, user: User, Path(provider): Path<String>, Json(req): Json<LinkReq>) -> AppResult<Json<Value>> {
    if st.sso.read().unwrap().provider(&provider).filter(|c| c.ready()).is_none() {
        return Err(AppError::bad_request("This sign-in method isn't enabled"));
    }
    let ticket = random_token(32);
    {
        let mut tickets = link_tickets().lock().unwrap();
        tickets.retain(|_, (_, created)| created.elapsed() < LINK_TICKET_TTL);
        tickets.insert(ticket.clone(), (user.id, Instant::now()));
    }
    let next = safe_next(req.next.as_deref());
    // The provider name was just found among the configured ones, so it is a plain word
    let url = format!("/api/auth/sso/{provider}/start?{}", encode(&[("next", next.as_str()), ("link", ticket.as_str())]));
    Ok(Json(json!({ "url": url })))
}

fn login_error(message: &str, next: Option<&str>) -> Response {
    let url = match next {
        // Link mode: go back to the original page to show the error
        Some(n) => format!("{n}{}{}", if n.contains('?') { "&" } else { "?" }, encode(&[("sso_error", message)])),
        None => format!("/login?{}", encode(&[("sso_error", message)])),
    };
    Redirect::to(&url).into_response()
}

pub async fn start(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<StartQuery>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    user: Result<User, AppError>,
) -> Response {
    let next = safe_next(q.next.as_deref());
    let cfg = st.sso.read().unwrap().provider(&provider).cloned();
    let Some(cfg) = cfg.filter(ProviderConfig::ready) else {
        return login_error("This sign-in method isn't enabled", None);
    };
    let link_user = match q.link.as_deref() {
        Some(ticket) => {
            let Ok(u) = user else { return login_error("Sign in before linking an external account", None) };
            let issued = link_tickets().lock().unwrap().remove(ticket);
            match issued {
                Some((id, created)) if id == u.id && created.elapsed() < LINK_TICKET_TTL => Some(u.id),
                _ => return login_error("The link request has expired. Try again.", Some(&next)),
            }
        }
        None => None,
    };
    let (state, verifier, nonce) = (random_token(32), random_token(64), random_token(24));
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let uri = redirect_uri(&base_url(&st, &headers), &provider);
    let ep = endpoints(&provider, &cfg);
    let mut params = vec![
        ("client_id", cfg.client_id.trim()),
        ("redirect_uri", uri.as_str()),
        ("response_type", "code"),
        ("scope", ep.scope),
        ("state", state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    if provider != "github" {
        params.push(("nonce", nonce.as_str()));
        // Let the user choose which account to use instead of automatically reusing the one currently signed in to the browser
        params.push(("prompt", "select_account"));
    }
    let url = format!("{}?{}", ep.authorize, encode(&params));
    {
        let ip = client_ip(&st, addr, &headers);
        let mut pending = st.sso_pending.lock().unwrap();
        pending.retain(|_, p| p.created.elapsed() < PENDING_TTL);
        // One address (an office behind NAT, or an attacker) keeps at most MAX_PENDING_PER_IP sign-ins in flight:
        // beyond that its oldest one is dropped, so nobody gets locked out by colleagues who closed the provider's page
        while pending.values().filter(|p| p.ip == ip).count() >= MAX_PENDING_PER_IP {
            let Some(oldest) = pending.iter().filter(|(_, p)| p.ip == ip).min_by_key(|(_, p)| p.created).map(|(k, _)| k.clone()) else { break };
            pending.remove(&oldest);
        }
        while pending.len() >= MAX_PENDING {
            // Evict the oldest entry: that sign-in will have to be started again
            let Some(oldest) = pending.iter().min_by_key(|(_, p)| p.created).map(|(k, _)| k.clone()) else { break };
            pending.remove(&oldest);
        }
        pending.insert(state.clone(), Pending { provider, verifier, nonce, next, redirect_uri: uri, link_user, created: Instant::now(), ip });
    }
    // The state is also stored in the browser that started the sign-in: the provider must redirect back to the same browser,
    // so an attacker can't hand their own authorization result (or link request) to someone else to open
    let cookie = format!(
        "{STATE_COOKIE}={state}; HttpOnly; SameSite=Lax; Path=/api/auth/sso; Max-Age={}{}",
        PENDING_TTL.as_secs(),
        if st.secure_cookie { "; Secure" } else { "" }
    );
    ([(header::SET_COOKIE, cookie)], Redirect::to(&url)).into_response()
}

/// The state recorded by the browser that started the sign-in
const STATE_COOKIE: &str = "tf_sso";

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Identity provided by the provider
#[derive(Debug)]
struct Identity {
    subject: String,
    email: String,
    /// The email is trustworthy (verified, and not something anyone can fill in themselves)
    email_verified: bool,
    name: String,
}

pub async fn callback(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<CallbackQuery>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    user: Result<User, AppError>,
) -> Response {
    let ip = client_ip(&st, addr, &headers);
    // The returned state must match the one this browser recorded when starting the sign-in
    let same_browser = q.state.as_deref().is_some_and(|s| crate::auth::get_cookie(&headers, STATE_COOKIE) == Some(s));
    let pending = q.state.as_deref().and_then(|s| st.sso_pending.lock().unwrap().remove(s));
    let Some(pending) = pending.filter(|p| same_browser && p.provider == provider && p.created.elapsed() < PENDING_TTL) else {
        return login_error("The sign-in timed out or the link was already used. Sign in again.", None);
    };
    // Linking an external account must be completed by the same user who started it
    if let Some(uid) = pending.link_user
        && user.as_ref().map(|u| u.id).ok() != Some(uid)
    {
        return login_error("Sign in before linking an external account", None);
    }
    let link_next = pending.link_user.map(|_| pending.next.clone());
    if let Some(err) = q.error {
        // The user clicked cancel on the provider's page
        let msg = if err == "access_denied" { "Sign-in canceled".to_string() } else { format!("{} sign-in failed: {}", label(&provider), q.error_description.unwrap_or(err)) };
        return login_error(&msg, link_next.as_deref());
    }
    let Some(code) = q.code else { return login_error("Sign-in failed: no authorization code was received", link_next.as_deref()) };
    let cfg = st.sso.read().unwrap().provider(&provider).cloned().filter(ProviderConfig::ready);
    let Some(cfg) = cfg else { return login_error("This sign-in method isn't enabled", link_next.as_deref()) };

    let ident = match fetch_identity(&provider, &cfg, &code, &pending).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("{} sign-in failed: {e}", label(&provider));
            return login_error(&format!("{} sign-in failed. Try again later.", label(&provider)), link_next.as_deref());
        }
    };

    if let Some(user_id) = pending.link_user {
        return match link(&st, &provider, &ident, user_id).await {
            Ok(username) => {
                record_login_via(&st, Some(user_id), &username, "sso_link", &provider, &ip, &headers);
                let next = &pending.next;
                Redirect::to(&format!("{next}{}sso_linked={provider}", if next.contains('?') { "&" } else { "?" })).into_response()
            }
            Err(e) => login_error(&e.message, link_next.as_deref()),
        };
    }

    match resolve_user(&st, &provider, &ident).await {
        Ok((user_id, username, created)) => {
            if created {
                record_login_via(&st, Some(user_id), &username, "sso_provisioned", &provider, &ip, &headers);
            }
            let cookie = match open_session(&st, user_id).await {
                Ok(c) => c,
                Err(e) => return login_error(&e.message, None),
            };
            if let Err(e) = sync_profile(&st, user_id, &provider, &ident).await {
                tracing::warn!("{} sign-in: couldn't update the profile of {username}: {}", label(&provider), e.message);
            }
            record_login_via(&st, Some(user_id), &username, "login", &provider, &ip, &headers);
            ([(header::SET_COOKIE, cookie)], Redirect::to(&pending.next)).into_response()
        }
        Err(e) => {
            let who = if ident.email.is_empty() { format!("{}:{}", provider, ident.subject) } else { ident.email.clone() };
            record_login_via(&st, None, &who, "sso_denied", &provider, &ip, &headers);
            login_error(&e.message, None)
        }
    }
}

// ───────────── Exchanging the code for an identity ─────────────

/// One HTTP client for every sign-in: it keeps its connections and TLS set-up instead of building them each time
fn http() -> Result<reqwest::Client, String> {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c.clone());
    }
    let c = reqwest::Client::builder().timeout(HTTP_TIMEOUT).user_agent("ThirtyFile").build().map_err(|e| e.to_string())?;
    Ok(CLIENT.get_or_init(|| c).clone())
}

async fn fetch_identity(provider: &str, cfg: &ProviderConfig, code: &str, p: &Pending) -> Result<Identity, String> {
    let ep = endpoints(provider, cfg);
    let client = http()?;
    let body = encode(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", p.redirect_uri.as_str()),
        ("client_id", cfg.client_id.trim()),
        ("client_secret", cfg.client_secret.as_str()),
        ("code_verifier", p.verifier.as_str()),
    ]);
    let res = client
        .post(&ep.token)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("token exchange failed: {e}"))?;
    let status = res.status();
    let token: Value = serde_json::from_str(&res.text().await.map_err(|e| e.to_string())?).map_err(|e| format!("malformed token response: {e}"))?;
    if !status.is_success() || token.get("error").is_some() {
        return Err(format!("token exchange failed ({status}): {}", token.get("error_description").or(token.get("error")).unwrap_or(&Value::Null)));
    }
    if provider == "github" {
        let access = token["access_token"].as_str().ok_or("no access_token received")?;
        return github_identity(&client, &ep, access).await;
    }
    let id_token = token["id_token"].as_str().ok_or("no id_token received")?;
    let claims = verify_id_token(provider, cfg, id_token, &p.nonce)?;
    let text = |k: &str| claims.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    if provider == "microsoft" {
        // oid is the user's stable identifier within the tenant; tid is added to distinguish organizations
        let subject = match (claims.get("tid").and_then(Value::as_str), claims.get("oid").and_then(Value::as_str)) {
            (Some(tid), Some(oid)) => format!("{tid}:{oid}"),
            _ => text("sub"),
        };
        let email = Some(text("email")).filter(|e| e.contains('@')).unwrap_or_else(|| text("preferred_username"));
        let email = if email.contains('@') { email.to_ascii_lowercase() } else { String::new() };
        Ok(Identity { subject, email_verified: !email.is_empty() && ms_tenant_is_specific(&cfg.tenant), email, name: clean_name(&text("name")) })
    } else {
        Ok(Identity {
            subject: text("sub"),
            email: text("email").to_ascii_lowercase(),
            email_verified: claims.get("email_verified").is_some_and(|v| v.as_bool() == Some(true) || v.as_str() == Some("true")),
            name: clean_name(&text("name")),
        })
    }
}

/// Checks the ID token's claims. The token was obtained by the server directly from the provider over TLS (not relayed by the browser),
/// so per OpenID Connect Core 3.1.3.7 TLS can authenticate the issuer without separately verifying the signature
fn verify_id_token(provider: &str, cfg: &ProviderConfig, token: &str, nonce: &str) -> Result<Value, String> {
    let payload = token.split('.').nth(1).ok_or("malformed id_token")?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let client_id = cfg.client_id.trim();
    let aud_ok = match &claims["aud"] {
        Value::String(a) => a == client_id,
        Value::Array(list) => list.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err("id_token aud mismatch".into());
    }
    if claims["exp"].as_i64().is_none_or(|exp| exp < now() - 60) {
        return Err("id_token expired".into());
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return Err("id_token nonce mismatch".into());
    }
    let iss = claims["iss"].as_str().unwrap_or_default();
    let iss_ok = cfg!(test)
        || match provider {
            "google" => iss == "https://accounts.google.com" || iss == "accounts.google.com",
            _ => iss.starts_with("https://login.microsoftonline.com/") && iss.ends_with("/v2.0"),
        };
    if !iss_ok {
        return Err(format!("id_token issuer mismatch: {iss}"));
    }
    Ok(claims)
}

async fn github_identity(client: &reqwest::Client, ep: &Endpoints, access: &str) -> Result<Identity, String> {
    let get = |url: String| {
        client
            .get(url)
            .header(header::AUTHORIZATION, format!("Bearer {access}"))
            .header(header::ACCEPT, "application/vnd.github+json")
            .send()
    };
    let user: Value = serde_json::from_str(&get(ep.user.clone()).await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let subject = user["id"].as_i64().map(|i| i.to_string()).ok_or("GitHub returned no user id")?;
    let emails: Value = serde_json::from_str(&get(ep.emails.clone()).await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?)
        .unwrap_or(Value::Null);
    // Only use the primary email that GitHub has verified
    let email = emails
        .as_array()
        .and_then(|list| list.iter().find(|e| e["primary"].as_bool() == Some(true) && e["verified"].as_bool() == Some(true)))
        .and_then(|e| e["email"].as_str())
        .map(str::to_ascii_lowercase);
    let name = clean_name(user["name"].as_str().or(user["login"].as_str()).unwrap_or_default());
    Ok(Identity { subject, email_verified: email.is_some(), email: email.unwrap_or_default(), name })
}

// ───────────── Mapping to a user ─────────────

/// After a sign-in: remember the provider's current email and name, and keep the user's display name in step with the
/// provider's name. A display name set by an administrator (different from the name the provider reported last time) is kept.
async fn sync_profile(st: &AppState, user_id: i64, provider: &str, ident: &Identity) -> AppResult<()> {
    let previous: Option<(String,)> = sqlx::query_as("SELECT name FROM user_identities WHERE provider = ? AND subject = ?")
        .bind(provider)
        .bind(&ident.subject)
        .fetch_optional(&st.db)
        .await?;
    let (current,): (String,) = sqlx::query_as("SELECT display_name FROM users WHERE id = ?").bind(user_id).fetch_one(&st.db).await?;
    let name = crate::admin::validate_display_name(&ident.name).unwrap_or("");
    // Compare with the previous name as it would have been stored (trimmed), so surrounding spaces don't break the follow-up
    let previous = previous.map(|(p,)| crate::admin::validate_display_name(&p).unwrap_or("").to_string());
    let follow = !name.is_empty() && name != current && (current.is_empty() || previous.as_deref() == Some(current.as_str()));
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    // A later sign-in without a verified email (e.g. a multi-tenant Microsoft app) keeps the email recorded earlier
    sqlx::query("UPDATE user_identities SET last_login_at = ?, email = COALESCE(NULLIF(?, ''), email), name = ? WHERE provider = ? AND subject = ?")
        .bind(now())
        .bind(&ident.email)
        .bind(&ident.name)
        .bind(provider)
        .bind(&ident.subject)
        .execute(&mut *tx)
        .await?;
    if follow {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?").bind(name).bind(user_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The provider's own domain list wins; otherwise the global list; an empty list allows every domain
fn domain_allowed(settings: &SsoSettings, cfg: &ProviderConfig, email: &str) -> bool {
    let list = if cfg.allowed_domains.is_empty() { &settings.allowed_domains } else { &cfg.allowed_domains };
    if list.is_empty() {
        return true;
    }
    let domain = email.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()).unwrap_or_default();
    list.contains(&domain)
}

/// Finds (or creates) the user to sign in, per the provider's policy: already linked → existing user whose username is the email → create automatically.
/// Returns (user id, username, whether the account was just created)
async fn resolve_user(st: &AppState, provider: &str, ident: &Identity) -> AppResult<(i64, String, bool)> {
    let settings = st.sso.read().unwrap().clone();
    let cfg = settings.provider(provider).cloned().unwrap_or_default();
    let linked: Option<(i64, String, bool)> = sqlx::query_as(
        "SELECT u.id, u.username, u.disabled FROM user_identities i JOIN users u ON u.id = i.user_id WHERE i.provider = ? AND i.subject = ?",
    )
    .bind(provider)
    .bind(&ident.subject)
    .fetch_optional(&st.db)
    .await?;
    if let Some((id, username, disabled)) = linked {
        if disabled {
            return Err(AppError::forbidden("This account is disabled. Contact your administrator."));
        }
        return Ok((id, username, false));
    }
    if cfg.provisioning() == Provisioning::Off {
        return Err(AppError::forbidden(
            "This external account isn't linked yet. Sign in with your username and password, then link it in \"My account › Sign-in methods\".",
        ));
    }
    if !ident.email_verified {
        return Err(AppError::forbidden(if provider == "microsoft" && !ms_tenant_is_specific(&settings.microsoft.tenant) {
            "This external account isn't linked yet. Sign in with your username and password, then link it in \"My account › Sign-in methods\", or ask your administrator to enter the Microsoft tenant ID in the single sign-on settings."
        } else {
            "This external account has no verified email, so it can't be matched to an account automatically. Sign in with your username and password, then link it in \"My account › Sign-in methods\"."
        }));
    }
    if !domain_allowed(&settings, &cfg, &ident.email) {
        return Err(AppError::forbidden(format!("The domain of {} isn't allowed to sign in to this site", ident.email)));
    }
    // An existing user whose username is this email: link automatically
    let existing: Option<(i64, String, bool)> =
        sqlx::query_as("SELECT id, username, disabled FROM users WHERE username = ?").bind(&ident.email).fetch_optional(&st.db).await?;
    let mut created = false;
    let (id, username) = match existing {
        Some((_, _, true)) => return Err(AppError::forbidden("This account is disabled. Contact your administrator.")),
        Some((id, username, false)) => (id, username),
        None if cfg.provisioning() == Provisioning::Create => {
            created = true;
            create_sso_user(st, provider, &cfg, ident).await?
        }
        None => {
            return Err(AppError::forbidden(format!(
                "{} doesn't have an account on this site. Ask your administrator to create one (using your email as the username links it automatically).",
                ident.email
            )));
        }
    };
    let _w = st.write_lock.lock().await;
    sqlx::query("INSERT OR IGNORE INTO user_identities (provider, subject, user_id, email, name, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(provider)
        .bind(&ident.subject)
        .bind(id)
        .bind(&ident.email)
        .bind(&ident.name)
        .bind(now())
        .execute(&st.db)
        .await?;
    Ok((id, username, created))
}

/// Creates an account automatically: the username is the email (or the part before @ when too long) with a random password (third-party sign-in only; an administrator can set a password)
/// Username for automatically created accounts: based on the email, replacing characters usernames don't allow (e.g. `+`), with the same rules as accounts created by administrators
fn sso_username(email: &str) -> String {
    let clean = |s: &str| -> String { s.chars().map(|c| if c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@') { c } else { '_' }).collect() };
    let full = clean(email);
    let base: String = if full.chars().count() <= 32 { full } else { clean(email.split('@').next().unwrap_or("")).chars().take(28).collect() };
    if base.chars().count() < 2 { format!("user{base}") } else { base }
}

async fn create_sso_user(st: &AppState, provider: &str, cfg: &ProviderConfig, ident: &Identity) -> AppResult<(i64, String)> {
    let base = sso_username(&ident.email);
    // Settings for the new account: the rule for the email's domain, otherwise the provider's defaults
    let rule = st.sso.read().unwrap().domain_rule(&ident.email).cloned();
    let (perms, quota_setting, groups) = match &rule {
        Some(r) => ((r.can_write, r.can_delete, r.can_share), r.quota_bytes, r.groups.clone()),
        None => ((cfg.defaults.can_write, cfg.defaults.can_delete, cfg.defaults.can_share), cfg.defaults.quota_bytes, cfg.groups.clone()),
    };
    let quota = quota_setting.unwrap_or_else(|| st.system.read().unwrap().default_user_quota).max(0);
    // Sign-in is only possible through the provider: no password can match this value until an administrator sets one
    let password_hash = NO_PASSWORD.to_string();
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    // Two sign-ins of the same person at the same time: the second one finds what the first created (checked under the lock)
    let linked: Option<(i64, String)> =
        sqlx::query_as("SELECT u.id, u.username FROM user_identities i JOIN users u ON u.id = i.user_id WHERE i.provider = ? AND i.subject = ?")
            .bind(provider)
            .bind(&ident.subject)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(found) = linked {
        return Ok(found);
    }
    let existing: Option<(i64, String)> = sqlx::query_as("SELECT id, username FROM users WHERE username = ?").bind(&ident.email).fetch_optional(&mut *tx).await?;
    if let Some(found) = existing {
        return Ok(found);
    }
    // A misconfigured tenant or domain list must not fill the user list: at most MAX_CREATED_PER_HOUR new accounts per provider
    let (recent,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE source = ? AND created_at > ?")
        .bind(provider)
        .bind(now() - 3600)
        .fetch_one(&mut *tx)
        .await?;
    if recent >= MAX_CREATED_PER_HOUR {
        tracing::warn!("{} sign-in: account creation paused, {recent} accounts were created in the last hour", label(provider));
        return Err(AppError::forbidden("Too many accounts were created in the last hour. Try again later or ask your administrator to create your account."));
    }
    let mut username = base.clone();
    for i in 2..100 {
        let (taken,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE username = ?").bind(&username).fetch_one(&mut *tx).await?;
        if taken == 0 {
            break;
        }
        username = format!("{}-{i}", base.chars().take(28).collect::<String>());
    }
    crate::admin::validate_username(&username)?;
    let id = create_user(
        &mut tx,
        NewUser {
            username: &username,
            password_hash: &password_hash,
            role: "user",
            can_write: perms.0,
            can_delete: perms.1,
            can_share: perms.2,
            quota_bytes: quota,
            source: provider,
            provisioned_by: Some(&ident.subject),
        },
    )
    .await?;
    if let Ok(name) = crate::admin::validate_display_name(&ident.name)
        && !name.is_empty()
    {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?").bind(name).bind(id).execute(&mut *tx).await?;
    }
    // Groups configured for the domain rule or the provider (ones deleted since are skipped)
    let mut joined = Vec::new();
    for g in &groups {
        let name: Option<(String,)> = sqlx::query_as("SELECT name FROM groups WHERE id = ?").bind(g).fetch_optional(&mut *tx).await?;
        if let Some((name,)) = name {
            sqlx::query("INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?, ?)").bind(g).bind(id).execute(&mut *tx).await?;
            joined.push(name);
        }
    }
    if let Some(user) = crate::auth::user_by_id(st, &mut tx, id).await? {
        let detail = format!(
            "{username} (created automatically by {} sign-in{}{})",
            label(provider),
            rule.as_ref().map(|r| format!(", domain rule {}", r.domain)).unwrap_or_default(),
            if joined.is_empty() { String::new() } else { format!(", groups: {}", joined.join(", ")) }
        );
        tree::log(&mut tx, &user, None, "user_create", &detail).await?;
    }
    tx.commit().await?;
    tracing::info!("Automatically created account {username} via {} sign-in", label(provider));
    Ok((id, username))
}

async fn link(st: &AppState, provider: &str, ident: &Identity, user_id: i64) -> AppResult<String> {
    let other: Option<(i64,)> =
        sqlx::query_as("SELECT user_id FROM user_identities WHERE provider = ? AND subject = ?").bind(provider).bind(&ident.subject).fetch_optional(&st.db).await?;
    if other.is_some_and(|(id,)| id != user_id) {
        return Err(AppError::conflict(format!("This {} account is already linked to another user", label(provider))));
    }
    let (username,): (String,) = sqlx::query_as("SELECT username FROM users WHERE id = ?").bind(user_id).fetch_one(&st.db).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    // One linked account per sign-in method: relinking replaces the previous one
    sqlx::query("DELETE FROM user_identities WHERE user_id = ? AND provider = ?").bind(user_id).bind(provider).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO user_identities (provider, subject, user_id, email, name, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(provider)
        .bind(&ident.subject)
        .bind(user_id)
        .bind(&ident.email)
        .bind(&ident.name)
        .bind(now())
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            // Linked by someone else between the check above and here
            sqlx::Error::Database(d) if d.is_unique_violation() => AppError::conflict(format!("This {} account is already linked to another user", label(provider))),
            _ => AppError::from(e),
        })?;
    tx.commit().await?;
    Ok(username)
}

// ───────────── My sign-in methods ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct IdentityRow {
    provider: String,
    email: String,
    name: String,
    created_at: i64,
    last_login_at: Option<i64>,
}

pub async fn my_identities(State(st): State<AppState>, user: User) -> AppResult<Json<Value>> {
    let rows: Vec<IdentityRow> = sqlx::query_as("SELECT provider, email, name, created_at, last_login_at FROM user_identities WHERE user_id = ? ORDER BY provider")
        .bind(user.id)
        .fetch_all(&st.db)
        .await?;
    let cfg = st.sso.read().unwrap().clone();
    let available: Vec<&str> = PROVIDERS.iter().copied().filter(|p| cfg.provider(p).is_some_and(ProviderConfig::ready)).collect();
    Ok(Json(json!({ "linked": rows, "available": available })))
}

pub async fn unlink(
    State(st): State<AppState>,
    user: User,
    Path(provider): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let removed = {
        let _w = st.write_lock.lock().await;
        // Without a password (an account created by single sign-on that never got one), the last linked account can't be removed
        let (password_hash, linked, this_one): (String, i64, i64) = sqlx::query_as(
            "SELECT password_hash,
                    (SELECT COUNT(*) FROM user_identities WHERE user_id = users.id),
                    (SELECT COUNT(*) FROM user_identities WHERE user_id = users.id AND provider = ?)
             FROM users WHERE id = ?",
        )
        .bind(&provider)
        .bind(user.id)
        .fetch_one(&st.db)
        .await?;
        if this_one == 0 {
            return Ok(Json(json!({ "ok": true })));
        }
        if password_hash == NO_PASSWORD && linked <= 1 {
            return Err(AppError::bad_request("This is the only way to sign in to this account. Ask an administrator to set a password first."));
        }
        sqlx::query("DELETE FROM user_identities WHERE user_id = ? AND provider = ?").bind(user.id).bind(&provider).execute(&st.db).await?.rows_affected()
    };
    if removed > 0 {
        record_login_via(&st, Some(user.id), &user.username, "sso_unlink", &provider, &client_ip(&st, addr, &headers), &headers);
    }
    Ok(Json(json!({ "ok": true })))
}

// ───────────── Administration: single sign-on settings ─────────────

fn admin_view(st: &AppState, headers: &HeaderMap) -> Value {
    let cfg = st.sso.read().unwrap().clone();
    let base = base_url(st, headers);
    let provider = |p: &str| {
        let c = cfg.provider(p).unwrap();
        json!({
            "enabled": c.enabled,
            "client_id": c.client_id,
            "has_secret": !c.client_secret.is_empty(),
            "tenant": c.tenant,
            "redirect_uri": redirect_uri(&base, p),
            "provisioning": c.provisioning(),
            "allowed_domains": c.allowed_domains,
            "defaults": c.defaults,
            "groups": c.groups,
        })
    };
    json!({
        "microsoft": provider("microsoft"),
        "google": provider("google"),
        "github": provider("github"),
        "allowed_domains": cfg.allowed_domains,
        "domain_rules": cfg.domain_rules,
        "max_created_per_hour": MAX_CREATED_PER_HOUR,
        "public_url_set": !st.system.read().unwrap().public_url.is_empty(),
    })
}

pub async fn get_settings(State(st): State<AppState>, _: Admin, headers: HeaderMap) -> Json<Value> {
    Json(admin_view(&st, &headers))
}

pub async fn update_settings(State(st): State<AppState>, Admin(user): Admin, headers: HeaderMap, Json(mut req): Json<SsoSettings>) -> AppResult<Json<Value>> {
    let old = st.sso.read().unwrap().clone();
    for p in PROVIDERS {
        let (new, prev) = (req.provider_mut(p).unwrap(), old.provider(p).unwrap());
        new.client_id = new.client_id.trim().to_string();
        new.tenant = new.tenant.trim().to_string();
        // The tenant goes into the sign-in URL's path: only accept a tenant ID (GUID) or domain name; the URL's host is always Microsoft's own
        let valid_tenant = new.tenant.len() <= 100 && new.tenant.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'));
        if !valid_tenant {
            return Err(AppError::bad_request("Invalid tenant: enter a tenant ID (GUID) or domain, e.g. contoso.onmicrosoft.com"));
        }
        // A blank secret keeps the existing one
        if new.client_secret.is_empty() {
            new.client_secret = prev.client_secret.clone();
        }
        if new.enabled && (new.client_id.is_empty() || new.client_secret.is_empty()) {
            return Err(AppError::bad_request(format!("Enter a Client ID and Client Secret to enable {} sign-in", label(p))));
        }
        new.provisioning.get_or_insert(Provisioning::Link);
        new.allowed_domains = normalize_domains(&new.allowed_domains)?;
        if let Some(q) = new.defaults.quota_bytes
            && q < 0
        {
            return Err(AppError::bad_request("The space size can't be negative"));
        }
        // Only groups that exist (the list comes from the groups page, but it may be stale)
        let mut groups = Vec::new();
        for g in &new.groups {
            let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM groups WHERE id = ?").bind(g).fetch_optional(&st.db).await?;
            if exists.is_some() && !groups.contains(g) {
                groups.push(*g);
            }
        }
        new.groups = groups;
    }
    req.allowed_domains = normalize_domains(&req.allowed_domains)?;
    req.auto_create = false;
    // Domain rules: one per domain, valid domain, non-negative size, existing groups
    let mut rules: Vec<DomainRule> = Vec::new();
    for mut r in std::mem::take(&mut req.domain_rules) {
        let domains = normalize_domains(std::slice::from_ref(&r.domain))?;
        let Some(domain) = domains.into_iter().next() else { continue };
        if rules.iter().any(|x| x.domain == domain) {
            return Err(AppError::bad_request(format!("There is more than one rule for {domain}")));
        }
        if r.quota_bytes.is_some_and(|q| q < 0) {
            return Err(AppError::bad_request("The space size can't be negative"));
        }
        let mut groups = Vec::new();
        for g in &r.groups {
            let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM groups WHERE id = ?").bind(g).fetch_optional(&st.db).await?;
            if exists.is_some() && !groups.contains(g) {
                groups.push(*g);
            }
        }
        r.domain = domain;
        r.groups = groups;
        rules.push(r);
    }
    req.domain_rules = rules;
    // Every provider appears in the summary, enabled or not, so the log shows the whole policy
    let policy = |p: &str| match req.provider(p).map(ProviderConfig::provisioning) {
        Some(Provisioning::Create) => "creates accounts",
        Some(Provisioning::Off) => "linked accounts only",
        _ => "matches by email",
    };
    let providers: Vec<String> = PROVIDERS
        .iter()
        .map(|p| format!("{} {} ({})", label(p), if req.provider(p).is_some_and(|c| c.enabled) { "on" } else { "off" }, policy(p)))
        .collect();
    let detail = format!(
        "Single sign-on settings: {}{}{}",
        providers.join("; "),
        if req.allowed_domains.is_empty() { String::new() } else { format!("; allowed domains: {}", req.allowed_domains.join(", ")) },
        if req.domain_rules.is_empty() { String::new() } else { format!("; domain rules: {}", req.domain_rules.iter().map(|r| r.domain.as_str()).collect::<Vec<_>>().join(", ")) },
    );
    {
        let _w = st.write_lock.lock().await;
        let mut tx = st.db.begin().await?;
        set_setting(&mut tx, "sso", &serde_json::to_string(&req).unwrap()).await?;
        tree::log(&mut tx, &user, None, "settings", &detail).await?;
        tx.commit().await?;
    }
    *st.sso.write().unwrap() = req;
    Ok(Json(admin_view(&st, &headers)))
}

pub type PendingMap = std::sync::Mutex<HashMap<String, Pending>>;

/// Domain list as entered (commas, spaces or line breaks between entries) → lowercase, deduplicated, validated
fn normalize_domains(raw: &[String]) -> AppResult<Vec<String>> {
    let mut domains: Vec<String> = raw
        .iter()
        .flat_map(|d| d.split([',', ' ', '\n', ';']))
        .map(|d| d.trim().trim_start_matches('@').to_ascii_lowercase())
        .filter(|d| !d.is_empty())
        .collect();
    domains.sort();
    domains.dedup();
    if let Some(bad) = domains.iter().find(|d| !d.contains('.') || !d.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
        return Err(AppError::bad_request(format!("Invalid domain: {bad}")));
    }
    Ok(domains)
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
        let Json(v) = start_link(State(env.st.clone()), user.clone(), Path(provider.into()), Json(LinkReq { next: None })).await.unwrap();
        query_param(v["url"].as_str().unwrap(), "link")
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
        callback(State(env.st.clone()), Path(provider.into()), Query(q), ConnectInfo("127.0.0.1:1".parse().unwrap()), headers, user.ok_or_else(AppError::unauthorized)).await
    }

    fn google(nonce: &str, sub: &str, email: &str, verified: bool) -> Value {
        json!({ "iss": "https://accounts.google.com", "aud": "google-client", "exp": now() + 600, "nonce": nonce, "sub": sub, "email": email, "email_verified": verified, "name": "Test" })
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
        let r = start(State(env.st.clone()), Path("google".into()), Query(StartQuery { next: None, link: None }), ConnectInfo("127.0.0.1:1".parse().unwrap()), headers.clone(), Err(AppError::unauthorized())).await;
        let url = location(&r);
        let (state, nonce) = (query_param(&url, "state"), query_param(&url, "nonce"));
        m.lock().unwrap().claims = google(&nonce, "g-1", "amy@example.com", true);
        let q = CallbackQuery { code: Some("code-1".into()), state: Some(state), error: None, error_description: None };
        let r = callback(State(env.st.clone()), Path("google".into()), Query(q), ConnectInfo("127.0.0.1:1".parse().unwrap()), headers, Err(AppError::unauthorized())).await;
        assert!(location(&r).starts_with("/login?sso_error="), "{}", location(&r));
        assert!(r.headers().get(header::SET_COOKIE).is_none(), "no sign-in session was created");
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
            start(State(env.st.clone()), Path("google".into()), Query(StartQuery { next: Some("/files".into()), link }), ConnectInfo("127.0.0.1:1".parse().unwrap()), headers.clone(), Ok(user.clone()))
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
        let r = start(State(env.st.clone()), Path("google".into()), Query(StartQuery { next: None, link }), ConnectInfo("127.0.0.1:1".parse().unwrap()), headers.clone(), Ok(attacker)).await;
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
        for (tenant, ok) in [("evil.example/x?", false), ("a@b", false), ("contoso.onmicrosoft.com", true), ("72f988bf-86f1-41af-91ab-2d7cd011db47", true), ("", true)] {
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
        env.user("amy@example.com", true).await;

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
        enable(&env, |s| s.google.provisioning = Some(Provisioning::Create));
        env.st.system.write().unwrap().default_user_quota = 5 << 30;
        let r = login(&env, &m, "google", None, |n| google(n, "g-2", "ben@example.com", true)).await;
        assert_eq!(location(&r), "/files/abc");
        let (quota,): (i64,) = sqlx::query_as("SELECT quota_bytes FROM users WHERE username = 'ben@example.com'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(quota, 5 << 30);

        // Domain restriction, unverified email
        enable(&env, |s| {
            s.google.provisioning = Some(Provisioning::Create);
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
        let r = callback(State(env.st.clone()), Path("google".into()), Query(q), ConnectInfo("127.0.0.1:1".parse().unwrap()), HeaderMap::new(), Err(AppError::unauthorized())).await;
        assert!(location(&r).contains("sso_error"));

        // Disabled users can't sign in
        sqlx::query("UPDATE users SET disabled = 1 WHERE username = 'amy@example.com'").execute(&env.st.db).await.unwrap();
        let r = login(&env, &m, "google", None, |n| google(n, "g-1", "amy@example.com", true)).await;
        assert!(location(&r).contains("sso_error"));
        *MOCK_BASE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn microsoft_email_is_only_trusted_with_a_specific_tenant() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        env.user("amy@example.com", true).await;
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
        let (gid,): (i64,) = sqlx::query_as("INSERT INTO groups (name, description, created_at) VALUES ('staff', '', 0) RETURNING id").fetch_one(&env.st.db).await.unwrap();
        env.user("carol@example.com", true).await;

        // Google creates accounts with its own defaults, domain list and group; GitHub is linked accounts only
        enable(&env, |s| {
            s.allowed_domains = vec!["other.example".into()];
            s.google.provisioning = Some(Provisioning::Create);
            s.google.allowed_domains = vec!["example.com".into()];
            s.google.defaults = NewUserDefaults { can_write: true, can_delete: false, can_share: false, quota_bytes: Some(1 << 30) };
            s.google.groups = vec![gid, 9999];
            s.github.provisioning = Some(Provisioning::Off);
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
        let (member,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM group_members m JOIN users u ON u.id = m.user_id WHERE u.username = 'dana@example.com' AND m.group_id = ?")
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
            crate::db::create_user(
                &mut c,
                NewUser { username: &format!("bulk{i}@example.com"), password_hash: &hash, role: "user", can_write: true, can_delete: true, can_share: true, quota_bytes: 0, source: "google", provisioned_by: None },
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
    async fn legacy_auto_create_flag_becomes_the_provider_policy() {
        let env = testutil::env().await;
        let mut c = env.st.db.acquire().await.unwrap();
        crate::db::set_setting(&mut c, "sso", r#"{"google":{"enabled":true},"auto_create":true}"#).await.unwrap();
        let s = load(&env.st.db).await;
        assert_eq!(s.google.provisioning(), Provisioning::Create);
        assert_eq!(s.github.provisioning(), Provisioning::Create);
        crate::db::set_setting(&mut c, "sso", r#"{"google":{"enabled":true,"provisioning":"off"}}"#).await.unwrap();
        let s = load(&env.st.db).await;
        assert_eq!(s.google.provisioning(), Provisioning::Off);
        assert_eq!(s.microsoft.provisioning(), Provisioning::Link);
    }

    #[tokio::test]
    async fn domain_rules_and_profile_sync() {
        let _g = SERIAL.lock().await;
        let env = testutil::env().await;
        let (m, base) = mock_server().await;
        *MOCK_BASE.lock().unwrap() = Some(base);
        let (gid,): (i64,) = sqlx::query_as("INSERT INTO groups (name, description, created_at) VALUES ('partners', '', 0) RETURNING id").fetch_one(&env.st.db).await.unwrap();
        enable(&env, |s| {
            s.google.provisioning = Some(Provisioning::Create);
            s.google.defaults = NewUserDefaults { can_write: true, can_delete: true, can_share: true, quota_bytes: None };
            s.domain_rules = vec![DomainRule { domain: "partner.example".into(), can_write: true, can_delete: false, can_share: false, quota_bytes: Some(512 << 20), groups: vec![gid] }];
        });
        let named = |n: &str, sub: &str, email: &str, name: &str| {
            let mut v = google(n, sub, email, true);
            v["name"] = json!(name);
            v
        };

        // The domain rule wins over the provider defaults; the display name comes from the provider
        let r = login(&env, &m, "google", None, |n| named(n, "g-20", "pat@partner.example", "Pat Partner")).await;
        assert_eq!(location(&r), "/files/abc");
        let row: (bool, bool, i64, String) = sqlx::query_as("SELECT can_write, can_delete, quota_bytes, display_name FROM users WHERE username = 'pat@partner.example'")
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        assert_eq!(row, (true, false, 512 << 20, "Pat Partner".into()));
        let (member,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM group_members m JOIN users u ON u.id = m.user_id WHERE u.username = 'pat@partner.example' AND m.group_id = ?")
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
}
