//! Signing in with a provider: the methods offered, starting a sign-in and the provider's callback

use super::*;

pub async fn providers(State(st): State<AppState>) -> Json<Value> {
    let cfg = st.sso.read().unwrap().clone();
    let list: Vec<Value> = PROVIDERS
        .iter()
        .filter(|p| cfg.provider(p).is_some_and(ProviderConfig::ready))
        .map(|p| json!({ "id": p, "label": if *p == "oidc" { shown_name(cfg.provider(p).unwrap()) } else { label(p).to_string() } }))
        .collect();
    Json(json!(list))
}

#[derive(Deserialize)]
pub struct StartQuery {
    pub(super) next: Option<String>,
    /// Link to the currently signed-in account ("My account › Sign-in methods"): a ticket from `link`
    pub(super) link: Option<String>,
}

pub(super) const LINK_TICKET_TTL: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
pub struct LinkReq {
    pub(super) next: Option<String>,
    /// The current password (and a two-factor code when the account has one)
    #[serde(default)]
    pub(super) password: Option<String>,
    #[serde(default)]
    pub(super) code: Option<String>,
}

/// Starts linking a sign-in method to the signed-in account. A POST, which other websites can't send on the user's
/// behalf (origin check), hands out a short-lived ticket for the page to navigate to `start` with; `start` accepts
/// linking only with such a ticket, so another website can't make a user link an account signed in in their browser.
/// A linked account signs in without the password, and keeps working after the password changes, so linking asks
/// for the password (and a two-factor code) first, as creating an app password does.
pub async fn start_link(State(st): State<AppState>, user: User, Path(provider): Path<String>, Json(req): Json<LinkReq>) -> AppResult<Json<Value>> {
    if st.sso.read().unwrap().provider(&provider).filter(|c| c.ready()).is_none() {
        return Err(AppError::bad_request("This sign-in method isn't enabled"));
    }
    crate::tokens::confirm_identity(&st, &user, req.password, req.code.as_deref(), "Sign out and sign in again, then link the account within 10 minutes").await?;
    let ticket = random_token(32);
    {
        let mut tickets = st.sso_link_tickets.lock().unwrap();
        tickets.retain(|_, (_, created)| created.elapsed() < LINK_TICKET_TTL);
        tickets.insert(ticket.clone(), (user.id, Instant::now()));
    }
    let next = safe_next(req.next.as_deref());
    // The provider name was just found among the configured ones, so it is a plain word
    let url = format!("/api/auth/sso/{provider}/start?{}", encode(&[("next", next.as_str()), ("link", ticket.as_str())]));
    Ok(Json(json!({ "url": url })))
}

/// The cookie that carries why a sign-in failed to the page shown next (read and removed by the page)
pub(super) const ERROR_COOKIE: &str = "tf_sso_error";

/// Goes back to the sign-in page (or, when linking, the page it started from) with the reason. The address only says
/// that there is one (`sso_error=1`) and the text travels in a short-lived cookie only this server can set, so a link
/// from elsewhere can't make the page show a message of its own.
pub(super) fn login_error(st: &AppState, message: &str, next: Option<&str>) -> Response {
    let url = match next {
        // Link mode: go back to the original page to show the error
        Some(n) => format!("{n}{}sso_error=1", if n.contains('?') { "&" } else { "?" }),
        None => "/login?sso_error=1".to_string(),
    };
    let text = percent_encoding::utf8_percent_encode(message, percent_encoding::NON_ALPHANUMERIC);
    let secure = if st.https() { "; Secure" } else { "" };
    // Read by the page's script, so not HttpOnly; it holds nothing but the message
    let cookie = format!("{ERROR_COOKIE}={text}; Path=/; SameSite=Lax; Max-Age=300{secure}");
    ([(header::SET_COOKIE, cookie)], Redirect::to(&url)).into_response()
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
        return login_error(&st, "This sign-in method isn't enabled", None);
    };
    let link_user = match q.link.as_deref() {
        Some(ticket) => {
            let Ok(u) = user else { return login_error(&st, "Sign in before linking an external account", None) };
            let issued = st.sso_link_tickets.lock().unwrap().remove(ticket);
            match issued {
                Some((id, created)) if id == u.id && created.elapsed() < LINK_TICKET_TTL => Some(u.id),
                _ => return login_error(&st, "The link request has expired. Try again.", Some(&next)),
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
        if st.https() { "; Secure" } else { "" }
    );
    ([(header::SET_COOKIE, cookie)], Redirect::to(&url)).into_response()
}

/// The state recorded by the browser that started the sign-in
pub(super) const STATE_COOKIE: &str = "tf_sso";

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub(super) code: Option<String>,
    pub(super) state: Option<String>,
    pub(super) error: Option<String>,
    pub(super) error_description: Option<String>,
}

/// Identity provided by the provider
#[derive(Debug)]
pub(super) struct Identity {
    pub(super) subject: String,
    pub(super) email: String,
    /// The email is trustworthy (verified, and not something anyone can fill in themselves)
    pub(super) email_verified: bool,
    pub(super) name: String,
}

impl Identity {
    /// The email as it may be stored with the linked account: blank unless the provider verified it
    pub(super) fn verified_email(&self) -> &str {
        if self.email_verified { &self.email } else { "" }
    }
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
    let pending = match take_pending(&st, &provider, q.state.as_deref(), &headers, &user) {
        Ok(p) => p,
        Err(msg) => return login_error(&st, msg, None),
    };
    let link_next = pending.link_user.map(|_| pending.next.clone());
    if let Some(err) = q.error {
        // The user clicked cancel on the provider's page
        let msg = if err == "access_denied" { "Sign-in canceled".to_string() } else { format!("{} sign-in failed: {}", label(&provider), q.error_description.unwrap_or(err)) };
        return login_error(&st, &msg, link_next.as_deref());
    }
    let Some(code) = q.code else { return login_error(&st, "Sign-in failed: no authorization code was received", link_next.as_deref()) };
    let cfg = st.sso.read().unwrap().provider(&provider).cloned().filter(ProviderConfig::ready);
    let Some(cfg) = cfg else { return login_error(&st, "This sign-in method isn't enabled", link_next.as_deref()) };

    let ident = match fetch_identity(&provider, &cfg, &code, &pending).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("{} sign-in failed: {e}", label(&provider));
            return login_error(&st, &format!("{} sign-in failed. Try again later.", label(&provider)), link_next.as_deref());
        }
    };

    if let Some(user_id) = pending.link_user {
        return match link(&st, &provider, &ident, user_id, &ip).await {
            Ok(username) => {
                record_login_via(&st, Some(user_id), &username, "sso_link", &provider, &ip, &headers);
                let next = &pending.next;
                Redirect::to(&format!("{next}{}sso_linked={provider}", if next.contains('?') { "&" } else { "?" })).into_response()
            }
            Err(e) => login_error(&st, &e.message, link_next.as_deref()),
        };
    }
    sign_in(&st, &provider, &ident, &pending.next, &ip, &headers).await
}

/// The sign-in (or linking) this browser started with `state`, taken so it can't be used twice; the error to show when
/// it doesn't match, has expired, or a link was started by someone else
pub(super) fn take_pending(st: &AppState, provider: &str, state: Option<&str>, headers: &HeaderMap, user: &Result<User, AppError>) -> Result<Pending, &'static str> {
    // The returned state must match the one this browser recorded when starting the sign-in
    let same_browser = state.is_some_and(|s| crate::auth::get_cookie(headers, STATE_COOKIE) == Some(s));
    let pending = state.and_then(|s| st.sso_pending.lock().unwrap().remove(s));
    let Some(pending) = pending.filter(|p| same_browser && p.provider == provider && p.created.elapsed() < PENDING_TTL) else {
        return Err("The sign-in timed out or the link was already used. Sign in again.");
    };
    // Linking an external account must be completed by the same user who started it
    if let Some(uid) = pending.link_user
        && user.as_ref().map(|u| u.id).ok() != Some(uid)
    {
        return Err("Sign in before linking an external account");
    }
    Ok(pending)
}

/// Signs in the account the identity belongs to (created first when the provider's policy allows) and goes on to `next`
pub(super) async fn sign_in(st: &AppState, provider: &str, ident: &Identity, next: &str, ip: &str, headers: &HeaderMap) -> Response {
    match resolve_user(st, provider, ident).await {
        Ok((user_id, username, created)) => {
            if created {
                record_login_via(st, Some(user_id), &username, "sso_provisioned", provider, ip, headers);
            }
            let cookie = match open_session(st, user_id, provider, ip, headers).await {
                Ok(c) => c,
                Err(e) => return login_error(st, &e.message, None),
            };
            if let Err(e) = sync_profile(st, user_id, provider, ident).await {
                tracing::warn!("{} sign-in: couldn't update the profile of {username}: {}", label(provider), e.message);
            }
            record_login_via(st, Some(user_id), &username, "login", provider, ip, headers);
            ([(header::SET_COOKIE, cookie)], Redirect::to(next)).into_response()
        }
        Err(e) => {
            let who = if ident.email.is_empty() { format!("{}:{}", provider, ident.subject) } else { ident.email.clone() };
            record_login_via(st, None, &who, "sso_denied", provider, ip, headers);
            login_error(st, &e.message, None)
        }
    }
}
