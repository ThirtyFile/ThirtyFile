//! Administration: the single sign-on settings

use super::*;

pub(super) fn admin_view(st: &AppState, headers: &HeaderMap) -> Value {
    let cfg = st.part::<Memory>().settings.read().unwrap().clone();
    let base = base_url(st, headers);
    let provider = |p: &str| {
        let c = cfg.provider(p).unwrap();
        json!({
            "enabled": c.enabled,
            "client_id": c.client_id,
            "has_secret": !c.client_secret.is_empty(),
            "tenant": c.tenant,
            "name": c.name,
            "issuer": c.issuer,
            "redirect_uri": redirect_uri(&base, p),
            "provisioning": c.provisioning,
            "allowed_domains": c.allowed_domains,
            "defaults": c.defaults,
            "groups": c.groups,
        })
    };
    json!({
        "microsoft": provider("microsoft"),
        "google": provider("google"),
        "github": provider("github"),
        "oidc": provider("oidc"),
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
    let old = st.part::<Memory>().settings.read().unwrap().clone();
    for p in PROVIDERS {
        check_provider(&st, p, req.provider_mut(p).unwrap(), old.provider(p).unwrap()).await?;
    }
    req.allowed_domains = normalize_domains(&req.allowed_domains)?;
    req.domain_rules = check_domain_rules(&st, std::mem::take(&mut req.domain_rules)).await?;
    let detail = summary(&req);
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        store(&mut tx, &req).await?;
        logs::record_activity(&mut tx, &user, None, "settings", &detail).await?;
        tx.commit().await?;
    }
    *st.part::<Memory>().settings.write().unwrap() = req;
    Ok(Json(admin_view(&st, &headers)))
}

/// Checks and tidies one provider's new settings (`prev`: its current ones)
pub(super) async fn check_provider(st: &AppState, p: &str, new: &mut ProviderConfig, prev: &ProviderConfig) -> AppResult<()> {
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
    new.allowed_domains = normalize_domains(&new.allowed_domains)?;
    if let Some(q) = new.defaults.quota_bytes
        && q < 0
    {
        return Err(AppError::bad_request("The space size can't be negative"));
    }
    new.groups = existing_groups(st, &new.groups).await?;
    if p == "oidc" {
        discover(new, prev).await?;
    }
    Ok(())
}

/// The name on the sign-in button of the OpenID Connect provider
pub(super) fn shown_name(c: &ProviderConfig) -> String {
    if c.name.trim().is_empty() { label("oidc").to_string() } else { c.name.clone() }
}

/// OpenID Connect: checks the issuer and reads its endpoints from its discovery document (again only when the issuer
/// changed, or they are missing). The issuer must be an https URL, and the document must name that same issuer.
pub(super) async fn discover(new: &mut ProviderConfig, prev: &ProviderConfig) -> AppResult<()> {
    new.name = clean_name(&new.name).chars().take(40).collect();
    new.issuer = new.issuer.trim().trim_end_matches('/').to_string();
    if new.issuer.is_empty() {
        if new.enabled {
            return Err(AppError::bad_request("Enter the issuer URL of the OpenID Connect provider"));
        }
        return Ok(());
    }
    let https = |u: &str| u.starts_with("https://") || (cfg!(test) && u.starts_with("http://127.0.0.1"));
    if !https(&new.issuer) || new.issuer.contains(['?', '#', ' ']) {
        return Err(AppError::bad_request("The issuer must be an https URL, e.g. https://auth.example.com/realms/staff"));
    }
    if new.issuer == prev.issuer && !prev.authorize_url.is_empty() && !prev.token_url.is_empty() {
        new.authorize_url = prev.authorize_url.clone();
        new.token_url = prev.token_url.clone();
        return Ok(());
    }
    let fail = |why: String| AppError::bad_request(format!("The OpenID Connect provider's settings couldn't be read: {why}"));
    let url = format!("{}/.well-known/openid-configuration", new.issuer);
    let res = http().map_err(fail)?.get(&url).send().await.map_err(|e| fail(e.to_string()))?;
    if !res.status().is_success() {
        return Err(fail(format!("{url} answered {}", res.status())));
    }
    let doc: Value = serde_json::from_str(&res.text().await.map_err(|e| fail(e.to_string()))?).map_err(|e| fail(e.to_string()))?;
    let text = |k: &str| doc[k].as_str().unwrap_or_default().to_string();
    if text("issuer").trim_end_matches('/') != new.issuer {
        return Err(fail(format!("it names another issuer ({})", text("issuer"))));
    }
    let (authorize, token) = (text("authorization_endpoint"), text("token_endpoint"));
    if !https(&authorize) || !https(&token) {
        return Err(fail("its sign-in and token addresses must be https URLs".into()));
    }
    new.authorize_url = authorize;
    new.token_url = token;
    Ok(())
}

/// Domain rules: one per domain, valid domain, non-negative size, existing groups
pub(super) async fn check_domain_rules(st: &AppState, rules: Vec<DomainRule>) -> AppResult<Vec<DomainRule>> {
    let mut checked: Vec<DomainRule> = Vec::new();
    for mut r in rules {
        let domains = normalize_domains(std::slice::from_ref(&r.domain))?;
        let Some(domain) = domains.into_iter().next() else { continue };
        if checked.iter().any(|x| x.domain == domain) {
            return Err(AppError::bad_request(format!("There is more than one rule for {domain}")));
        }
        if r.quota_bytes.is_some_and(|q| q < 0) {
            return Err(AppError::bad_request("The space size can't be negative"));
        }
        // A location only matters when the accounts get a personal space
        r.personal_location = r.personal_location.map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && r.personal_space != Some(false));
        if let Some(l) = &r.personal_location {
            crate::db::check_location(&mut *st.db.acquire().await?, l).await?;
        }
        r.groups = existing_groups(st, &r.groups).await?;
        r.domain = domain;
        checked.push(r);
    }
    Ok(checked)
}

/// Only groups that exist, each once (the list comes from the groups page, but it may be stale)
pub(super) async fn existing_groups(st: &AppState, ids: &[i64]) -> AppResult<Vec<i64>> {
    let mut groups = Vec::new();
    for g in ids {
        let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM groups WHERE id = ?").bind(g).fetch_optional(&st.db).await?;
        if exists.is_some() && !groups.contains(g) {
            groups.push(*g);
        }
    }
    Ok(groups)
}

/// The settings as the activity log records them
pub(super) fn summary(req: &SsoSettings) -> String {
    // Every provider appears in the summary, enabled or not, so the log shows the whole policy
    let policy = |p: &str| match req.provider(p).map(|c| c.provisioning) {
        Some(Provisioning::Create) => "creates accounts",
        Some(Provisioning::Off) => "linked accounts only",
        _ => "matches by email",
    };
    let providers: Vec<String> =
        PROVIDERS.iter().map(|p| format!("{} {} ({})", label(p), if req.provider(p).is_some_and(|c| c.enabled) { "on" } else { "off" }, policy(p))).collect();
    format!(
        "Single sign-on settings: {}{}{}",
        providers.join("; "),
        if req.allowed_domains.is_empty() { String::new() } else { format!("; allowed domains: {}", req.allowed_domains.join(", ")) },
        if req.domain_rules.is_empty() {
            String::new()
        } else {
            format!("; domain rules: {}", req.domain_rules.iter().map(|r| r.domain.as_str()).collect::<Vec<_>>().join(", "))
        },
    )
}

pub type PendingMap = std::sync::Mutex<HashMap<String, Pending>>;

/// Domain list as entered (commas, spaces or line breaks between entries) → lowercase, deduplicated, validated
pub(super) fn normalize_domains(raw: &[String]) -> AppResult<Vec<String>> {
    let mut domains: Vec<String> =
        raw.iter().flat_map(|d| d.split([',', ' ', '\n', ';'])).map(|d| d.trim().trim_start_matches('@').to_ascii_lowercase()).filter(|d| !d.is_empty()).collect();
    domains.sort();
    domains.dedup();
    if let Some(bad) = domains.iter().find(|d| !d.contains('.') || !d.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
        return Err(AppError::bad_request(format!("Invalid domain: {bad}")));
    }
    Ok(domains)
}
