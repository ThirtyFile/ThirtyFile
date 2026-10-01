//! Exchanging the provider's code for the person's identity

use super::*;

/// One HTTP client for every sign-in: it keeps its connections and TLS set-up instead of building them each time
pub(super) fn http() -> Result<reqwest::Client, String> {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c.clone());
    }
    let c = reqwest::Client::builder().timeout(HTTP_TIMEOUT).user_agent("ThirtyFile").build().map_err(|e| e.to_string())?;
    Ok(CLIENT.get_or_init(|| c).clone())
}

pub(super) async fn fetch_identity(provider: &str, cfg: &ProviderConfig, code: &str, p: &Pending) -> Result<Identity, String> {
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
pub(super) fn verify_id_token(provider: &str, cfg: &ProviderConfig, token: &str, nonce: &str) -> Result<Value, String> {
    let payload = token.split('.').nth(1).ok_or("malformed id_token")?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
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
            "oidc" => !cfg.issuer.is_empty() && iss.trim_end_matches('/') == cfg.issuer,
            _ => iss.starts_with("https://login.microsoftonline.com/") && iss.ends_with("/v2.0"),
        };
    if !iss_ok {
        return Err(format!("id_token issuer mismatch: {iss}"));
    }
    Ok(claims)
}

pub(super) async fn github_identity(client: &reqwest::Client, ep: &Endpoints, access: &str) -> Result<Identity, String> {
    let get = |url: String| client.get(url).header(header::AUTHORIZATION, format!("Bearer {access}")).header(header::ACCEPT, "application/vnd.github+json").send();
    let user: Value =
        serde_json::from_str(&get(ep.user.clone()).await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let subject = user["id"].as_i64().map(|i| i.to_string()).ok_or("GitHub returned no user id")?;
    let emails: Value =
        serde_json::from_str(&get(ep.emails.clone()).await.map_err(|e| e.to_string())?.text().await.map_err(|e| e.to_string())?).unwrap_or(Value::Null);
    // Only use the primary email that GitHub has verified
    let email = emails
        .as_array()
        .and_then(|list| list.iter().find(|e| e["primary"].as_bool() == Some(true) && e["verified"].as_bool() == Some(true)))
        .and_then(|e| e["email"].as_str())
        .map(str::to_ascii_lowercase);
    let name = clean_name(user["name"].as_str().or(user["login"].as_str()).unwrap_or_default());
    Ok(Identity { subject, email_verified: email.is_some(), email: email.unwrap_or_default(), name })
}
