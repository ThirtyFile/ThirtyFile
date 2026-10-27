//! Control panel › Monitoring: whether `/metrics` answers, the token it asks for, and the addresses it answers to

use axum::{Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Memory;
use crate::{
    auth::{Admin, TrustProxy},
    db::{json_setting, set_setting},
    error::{AppError, AppResult},
    logs,
    state::AppState,
    util::{now, random_token, sha256_hex},
};

const SETTING: &str = "metrics";
/// Prefix of the token, so it is known for what it is when found somewhere it shouldn't be
pub const TOKEN_PREFIX: &str = "tfm_";
/// Most addresses or networks the list may hold
const MAX_ALLOWED: usize = 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// SHA-256 of the token (hex): the token itself is shown once, when it is made
    pub token_hash: String,
    pub token_created_at: i64,
    /// Addresses and networks (`203.0.113.7`, `10.1.0.0/16`) that may ask; empty: any
    pub allow: Vec<String>,
}

impl Settings {
    /// Whether a request from `ip` is let through by the list of addresses
    pub fn allows(&self, ip: std::net::IpAddr) -> bool {
        self.allow.is_empty() || TrustProxy::parse(&self.allow.join(",")).is_ok_and(|list| list.trusts(ip))
    }
}

pub async fn load(db: &sqlx::SqlitePool) -> Settings {
    match db.acquire().await {
        Ok(mut conn) => json_setting(&mut conn, SETTING).await.unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

fn view(s: &Settings) -> Value {
    json!({
        "enabled": s.enabled,
        "has_token": !s.token_hash.is_empty(),
        "token_created_at": (!s.token_hash.is_empty()).then_some(s.token_created_at),
        "allow": s.allow,
        "path": "/metrics",
    })
}

pub async fn get_settings(State(st): State<AppState>, _: Admin) -> Json<Value> {
    Json(view(&st.part::<Memory>().settings.read()))
}

#[derive(Deserialize)]
pub struct SaveReq {
    enabled: bool,
    #[serde(default)]
    allow: Vec<String>,
    /// Make a new token (the old one stops working); shown once in the answer
    #[serde(default)]
    new_token: bool,
    #[serde(flatten)]
    me: crate::tokens::Confirmation,
}

/// The addresses as they are kept: lower case, each an address or a network, no more than `MAX_ALLOWED`
fn clean_allow(list: &[String]) -> AppResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for entry in list.iter().flat_map(|e| e.split([',', ' ', '\n'])).map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()) {
        // Only addresses and networks: not the words true or false that the setting of the proxies also takes
        let valid = matches!(TrustProxy::parse(&entry), Ok(TrustProxy::Listed(l)) if l.len() == 1);
        if !valid {
            return Err(AppError::bad_request("Enter addresses or networks such as 203.0.113.7 or 10.1.0.0/16"));
        }
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    if out.len() > MAX_ALLOWED {
        return Err(AppError::bad_request(format!("At most {n} addresses or networks", n = MAX_ALLOWED)));
    }
    Ok(out)
}

pub async fn update_settings(State(st): State<AppState>, Admin(user): Admin, _headers: HeaderMap, Json(req): Json<SaveReq>) -> AppResult<Json<Value>> {
    let SaveReq { enabled, allow, new_token, me } = req;
    // What it shows is about the whole server, and the token opens it: whoever changes it says who they are
    me.check(&st, &user).await?;
    let allow = clean_allow(&allow)?;
    let mut s = st.part::<Memory>().settings.read().clone();
    let token = new_token.then(|| format!("{TOKEN_PREFIX}{}", random_token(43)));
    if let Some(t) = &token {
        s.token_hash = sha256_hex(t.as_bytes());
        s.token_created_at = now();
    }
    if enabled && s.token_hash.is_empty() {
        return Err(AppError::bad_request("Make a token first: it is what a monitoring system sends to be let in"));
    }
    s.enabled = enabled;
    s.allow = allow;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let user = crate::auth::require_admin(&mut tx, &user).await?;
        set_setting(&mut tx, SETTING, &serde_json::to_string(&s).map_err(AppError::internal)?).await?;
        let detail = format!(
            "Monitoring: {}{}{}",
            if s.enabled { "on" } else { "off" },
            if token.is_some() { ", new token" } else { "" },
            if s.allow.is_empty() { String::new() } else { format!(", addresses: {}", s.allow.join(", ")) }
        );
        logs::record_activity(&mut tx, &user, None, "settings", &detail).await?;
        tx.commit().await?;
        *st.part::<Memory>().settings.write() = s.clone();
    }
    let mut out = view(&s);
    // The token is in this answer and nowhere else
    if let Some(token) = token {
        out["token"] = json!(token);
    }
    Ok(Json(out))
}
