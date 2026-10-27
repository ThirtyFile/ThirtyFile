//! Administrator alerts (Control panel › Administrator alerts): what needs an administrator's attention, sent by email
//! and, if chosen, to a webhook, instead of waiting on a page until someone looks.
//!
//! - What is looked at (conditions.rs), once a minute: a storage location over its usage threshold, failing or offline;
//!   the data folder's disk nearly full; failed sign-ins passing a limit from one address or for one account; an FTPS
//!   certificate that ends within 14 days. Backup and replica problems are told as they happen (`queue::notice`, from
//!   the same place that tells people in the app)
//! - Each is on or off. One problem is told once when it starts and once when it is over, with a cooldown so one that
//!   comes and goes doesn't send a message each time (queue.rs)
//! - Email goes to every administrator with an address, in their language, through the email server of Control panel ›
//!   Email. A webhook gets a JSON body signed with a shared secret, to an address that isn't on the private network
//!   unless allowed, and is tried again with longer waits when it fails (webhook.rs)
//! - Messages hold names of locations, addresses, accounts and numbers: never the name of an item (text.rs)

mod conditions;
mod queue;
mod text;
mod webhook;

#[cfg(test)]
mod tests;

use std::sync::atomic::AtomicI64;

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use queue::{notice, spawn};

use crate::{
    auth::Admin,
    db,
    error::{AppError, AppResult},
    state::AppState,
};

const SETTING: &str = "admin_alerts";
const SECRET: &str = "admin_alerts_secret";
/// What the secret is sealed with (`secrets::seal`)
const SEAL: &str = "settings:admin_alerts";
/// The shortest shared secret for signing
const MIN_SECRET: usize = 16;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Send emails to the administrators with an address
    pub email: bool,
    /// A webhook to post to as well; blank: none
    pub webhook_url: String,
    /// Allow a webhook on the private network or this computer
    pub allow_private: bool,
    /// A storage location over its usage threshold or failing (the thresholds of Control panel › Storage usage)
    pub usage: bool,
    /// A storage location that can't be reached
    pub offline: bool,
    /// The disk of the data folder nearly full
    pub data_disk: bool,
    /// Failed sign-ins passing `sign_in_failures` in `sign_in_minutes`, from one address or for one account
    pub sign_ins: bool,
    /// A backup or replica policy that fails or is overdue
    pub backups: bool,
    /// An FTPS certificate that ends within 14 days
    pub certificate: bool,
    pub sign_in_failures: u32,
    pub sign_in_minutes: u32,
    /// Minutes before a problem that comes back is told again
    pub cooldown_minutes: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            email: true,
            webhook_url: String::new(),
            allow_private: false,
            usage: true,
            offline: true,
            data_disk: true,
            sign_ins: true,
            backups: true,
            certificate: true,
            sign_in_failures: 10,
            sign_in_minutes: 10,
            cooldown_minutes: 30,
        }
    }
}

impl Config {
    /// Whether alerts of this kind (`conditions::Condition::kind`) are chosen
    pub fn wants(&self, kind: &str) -> bool {
        match kind {
            "usage_disk" | "usage_errors" => self.usage,
            "offline" | "directory" => self.offline,
            "data_disk" => self.data_disk,
            "sign_ins_ip" | "sign_ins_user" => self.sign_ins,
            "backup" | "replica" => self.backups,
            "certificate" => self.certificate,
            _ => false,
        }
    }
}

/// Administrator alerts' part of `AppState`
#[derive(Default)]
pub struct Memory {
    /// Wakes the scheduler: something was queued, or the settings changed
    pub wake: tokio::sync::Notify,
    /// When what is wrong was last looked at
    last_look: AtomicI64,
}

// ───────────── The page ─────────────

#[derive(Serialize, sqlx::FromRow)]
struct Active {
    kind: String,
    params: String,
    since: i64,
    announced: bool,
}

#[derive(Serialize, sqlx::FromRow)]
struct Recent {
    id: i64,
    channel: String,
    /// The administrator an email was for
    recipient: Option<String>,
    state: String,
    kind: String,
    params: String,
    created_at: i64,
    attempts: i64,
    done_at: Option<i64>,
    failed: bool,
    error: Option<String>,
}

#[derive(Serialize)]
pub struct Info {
    #[serde(flatten)]
    config: Config,
    has_secret: bool,
    email_ready: bool,
    active: Vec<Value>,
    recent: Vec<Value>,
}

/// A row's JSON, with its `params` as JSON too
fn with_params<T: Serialize>(row: &T, params: &str) -> Value {
    let mut v = serde_json::to_value(row).unwrap_or_default();
    v["params"] = serde_json::from_str(params).unwrap_or_default();
    v
}

async fn info(st: &AppState) -> AppResult<Info> {
    let config: Config = db::json_setting(&mut *st.db.acquire().await?, SETTING).await?;
    let active: Vec<Active> =
        sqlx::query_as("SELECT kind, params, since, announced FROM alert_conditions WHERE active = 1 ORDER BY since").fetch_all(&st.db).await?;
    let recent: Vec<Recent> = sqlx::query_as(
        "SELECT d.id, d.channel, u.username AS recipient, d.state, d.kind, d.params, d.created_at, d.attempts, d.done_at, d.failed, d.error
         FROM alert_deliveries d LEFT JOIN users u ON u.id = d.user_id ORDER BY d.id DESC LIMIT 30",
    )
    .fetch_all(&st.db)
    .await?;
    Ok(Info {
        has_secret: queue::secret(&st.db).await?.is_some(),
        email_ready: crate::mail::load(&st.db).await.ready(),
        active: active.iter().map(|a| with_params(a, &a.params)).collect(),
        recent: recent.iter().map(|r| with_params(r, &r.params)).collect(),
        config,
    })
}

pub async fn get(State(st): State<AppState>, _: Admin) -> AppResult<Json<Info>> {
    Ok(Json(info(&st).await?))
}

#[derive(Deserialize)]
pub struct Change {
    #[serde(flatten)]
    pub config: Config,
    /// A new shared secret for the webhook; the one saved stays when left out
    #[serde(default)]
    pub secret: Option<String>,
}

fn check(c: &Config) -> AppResult<()> {
    if !(3..=1000).contains(&c.sign_in_failures) || !(1..=1440).contains(&c.sign_in_minutes) {
        return Err(AppError::bad_request("Failed sign-ins: 3 to 1,000 in 1 to 1,440 minutes"));
    }
    if c.cooldown_minutes > 1440 {
        return Err(AppError::bad_request("The cooldown is at most 1,440 minutes"));
    }
    let url = c.webhook_url.trim();
    if !url.is_empty() {
        let parsed = reqwest::Url::parse(url).map_err(|_| AppError::bad_request("Enter the webhook address as a full URL, such as https://example.com/hook"))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() || !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(AppError::bad_request("The webhook address must be http or https, without a user name or password in it"));
        }
    }
    Ok(())
}

pub async fn save(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<Change>) -> AppResult<Json<Info>> {
    let mut c = req.config;
    c.webhook_url = c.webhook_url.trim().to_string();
    check(&c)?;
    if let Some(s) = &req.secret
        && s.chars().count() < MIN_SECRET
    {
        return Err(AppError::bad_request(format!("The shared secret needs at least {MIN_SECRET} characters")));
    }
    {
        let _w = st.write_lock.lock().await;
        let mut tx = db::begin_write(&st.db).await?;
        let user = crate::auth::require_admin(&mut tx, &user).await?;
        if let Some(s) = &req.secret {
            db::set_setting(&mut tx, SECRET, &crate::secrets::seal(SEAL, s)).await?;
        }
        let (has,): (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM settings WHERE key = ? AND value <> '')").bind(SECRET).fetch_one(&mut *tx).await?;
        if !c.webhook_url.is_empty() && !has {
            return Err(AppError::bad_request("Set a shared secret to sign the webhook's messages with"));
        }
        db::set_json_setting(&mut tx, SETTING, &c).await?;
        crate::logs::record_activity(&mut tx, &user, None, "settings", "Updated administrator alerts").await?;
        tx.commit().await?;
    }
    st.part::<Memory>().wake.notify_one();
    Ok(Json(info(&st).await?))
}

#[derive(Deserialize)]
pub struct Test {
    /// "email" (to the administrator asking) or "webhook"
    pub channel: String,
}

/// Sends a test message now, and says why when it can't be sent
pub async fn test(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<Test>) -> AppResult<Json<Value>> {
    let c: Config = db::json_setting(&mut *st.db.acquire().await?, SETTING).await?;
    let site = st.part::<crate::branding::Memory>().settings.read().site_name.clone();
    let result = match req.channel.as_str() {
        "email" => test_email(&st, user.id, &site).await,
        "webhook" => test_webhook(&st, &c, &site).await,
        _ => return Err(AppError::bad_request("Choose email or webhook")),
    };
    result.map(|()| Json(json!({ "ok": true }))).map_err(AppError::bad_request)
}

async fn test_email(st: &AppState, user: i64, site: &str) -> Result<(), String> {
    let smtp = crate::mail::load(&st.db).await;
    if !smtp.ready() {
        return Err("Set up the email server first (Control panel › Email)".into());
    }
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT email, chosen_lang, lang FROM users WHERE id = ?").bind(user).fetch_optional(&st.db).await.map_err(|e| e.to_string())?;
    let Some((to, chosen, last_used)) = row.filter(|r| !r.0.is_empty()) else { return Err("Your account has no email address (set it in the account menu)".into()) };
    let lang = crate::i18n::recipient(&chosen, &last_used, crate::i18n::system_default(st));
    let (subject, body) = text::test_email(lang, site);
    crate::mail::send(&smtp, site, &crate::mail::Message { to: &to, subject: &subject, body: &body }).await
}

async fn test_webhook(st: &AppState, c: &Config, site: &str) -> Result<(), String> {
    if c.webhook_url.is_empty() {
        return Err("Save a webhook address first".into());
    }
    let Some(secret) = queue::secret(&st.db).await.map_err(|e| e.message)? else { return Err("Set a shared secret first".into()) };
    let now = crate::util::now();
    let body = json!({ "event": "test", "site": site, "at": now });
    webhook::post(&c.webhook_url, c.allow_private, &secret, &body, now).await
}
