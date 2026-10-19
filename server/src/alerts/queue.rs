//! Telling administrators: what is wrong is compared with what was wrong a minute ago, an alert is queued once when a
//! problem starts and once when it is over, and the queue is delivered by email and webhook, tried again later when
//! that fails. Everything is in the database, so a restart loses nothing.
//!
//! - A problem that starts again within the cooldown of the last message about it waits until the cooldown is over (and
//!   is then told if it is still there), so one that comes and goes doesn't send a message each time
//! - "Resolved" is sent as soon as a problem that was told about is over
//! - A problem that wasn't told about (still waiting out its cooldown) says nothing when it is over

use serde_json::{Value, json};
use sqlx::SqliteConnection;

use super::{Config, Memory, SEAL, SECRET, conditions::Condition, text, webhook};
use crate::{
    backups::scheduler,
    db,
    error::{AppError, AppResult},
    state::AppState,
};

/// How long to wait after a failed attempt, for the first retries; after the last, a message is given up on
const BACKOFF: [i64; 5] = [60, 300, 1800, 7200, 21600];
/// Messages older than this are deleted, sent or not
const KEEP_DELIVERIES: i64 = 30 * 86400;
/// A problem that is over is remembered this long, for its cooldown
const KEEP_CONDITIONS: i64 = 86400;
/// Looking at what is wrong, at most this often
const LOOK_EVERY: i64 = 60;

/// Where a message goes now
pub struct Targets {
    /// The administrators with an email address, when emails can be sent
    pub admins: Vec<i64>,
    pub webhook: bool,
}

pub async fn secret(db: &sqlx::SqlitePool) -> AppResult<Option<String>> {
    let Some(sealed) = db::get_setting(db, SECRET).await?.filter(|s| !s.is_empty()) else { return Ok(None) };
    Ok(Some(crate::secrets::open(SEAL, &sealed).map_err(AppError::internal)?))
}

pub async fn targets(st: &AppState, c: &Config) -> AppResult<Targets> {
    let admins = if c.email && crate::mail::load(&st.db).await.ready() {
        sqlx::query_as::<_, (i64,)>("SELECT id FROM users WHERE role = 'admin' AND disabled = 0 AND email <> '' ORDER BY id")
            .fetch_all(&st.db)
            .await?
            .into_iter()
            .map(|(id,)| id)
            .collect()
    } else {
        vec![]
    };
    let webhook = !c.webhook_url.trim().is_empty() && secret(&st.db).await?.is_some();
    Ok(Targets { admins, webhook })
}

/// Queues an alert for each place it goes
async fn enqueue(conn: &mut SqliteConnection, to: &Targets, state: &str, kind: &str, key: &str, params: &Value, t: i64) -> AppResult<()> {
    let insert = "INSERT INTO alert_deliveries (channel, user_id, state, kind, key, params, created_at, next_attempt_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)";
    for admin in &to.admins {
        sqlx::query(insert).bind("email").bind(admin).bind(state).bind(kind).bind(key).bind(params.to_string()).bind(t).bind(t).execute(&mut *conn).await?;
    }
    if to.webhook {
        sqlx::query(insert).bind("webhook").bind(None::<i64>).bind(state).bind(kind).bind(key).bind(params.to_string()).bind(t).bind(t).execute(&mut *conn).await?;
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Row {
    key: String,
    kind: String,
    params: String,
    active: bool,
    announced: bool,
    last_sent_at: Option<i64>,
}

/// One look: the conditions now against the ones before; messages queued for what started and what is over
pub async fn evaluate(st: &AppState, t: i64) -> AppResult<()> {
    let c: Config = db::json_setting(&mut *st.db.acquire().await?, super::SETTING).await?;
    let now = super::conditions::current(st, &c, t).await?;
    let to = targets(st, &c).await?;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = db::begin_write(&st.db).await?;
        let res = apply(&mut tx, &c, &to, &now, t).await;
        db::settle(tx, res).await?;
    }
    st.part::<Memory>().wake.notify_one();
    Ok(())
}

async fn apply(conn: &mut SqliteConnection, c: &Config, to: &Targets, now: &[Condition], t: i64) -> AppResult<()> {
    let before: Vec<Row> = sqlx::query_as("SELECT key, kind, params, active, announced, last_sent_at FROM alert_conditions").fetch_all(&mut *conn).await?;
    let cooldown = i64::from(c.cooldown_minutes) * 60;
    for cond in now {
        let known = before.iter().find(|r| r.key == cond.key);
        if known.is_none() {
            sqlx::query("INSERT INTO alert_conditions (key, kind, params, since) VALUES (?, ?, ?, ?)")
                .bind(&cond.key)
                .bind(cond.kind)
                .bind(cond.params.to_string())
                .bind(t)
                .execute(&mut *conn)
                .await?;
        } else if known.is_some_and(|r| !r.active) {
            sqlx::query("UPDATE alert_conditions SET params = ?, since = ?, active = 1, announced = 0 WHERE key = ?")
                .bind(cond.params.to_string())
                .bind(t)
                .bind(&cond.key)
                .execute(&mut *conn)
                .await?;
        } else {
            sqlx::query("UPDATE alert_conditions SET params = ? WHERE key = ?").bind(cond.params.to_string()).bind(&cond.key).execute(&mut *conn).await?;
        }
        let told = known.is_some_and(|r| r.active && r.announced);
        if !told && known.and_then(|r| r.last_sent_at).is_none_or(|last| t - last >= cooldown) {
            enqueue(conn, to, "firing", cond.kind, &cond.key, &cond.params, t).await?;
            sqlx::query("UPDATE alert_conditions SET announced = 1, last_sent_at = ? WHERE key = ?").bind(t).bind(&cond.key).execute(&mut *conn).await?;
        }
    }
    for over in before.iter().filter(|r| r.active && !now.iter().any(|n| n.key == r.key)) {
        sqlx::query("UPDATE alert_conditions SET active = 0, announced = 0 WHERE key = ?").bind(&over.key).execute(&mut *conn).await?;
        // Not told about the kinds that were turned off since
        if over.announced && c.wants(&over.kind) {
            let params: Value = serde_json::from_str(&over.params).unwrap_or_default();
            enqueue(conn, to, "resolved", &over.kind, &over.key, &params, t).await?;
            sqlx::query("UPDATE alert_conditions SET last_sent_at = ? WHERE key = ?").bind(t).bind(&over.key).execute(&mut *conn).await?;
        }
    }
    sqlx::query("DELETE FROM alert_conditions WHERE active = 0 AND (last_sent_at IS NULL OR last_sent_at < ?)")
        .bind(t - KEEP_CONDITIONS)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM alert_deliveries WHERE created_at < ?").bind(t - KEEP_DELIVERIES).execute(&mut *conn).await?;
    Ok(())
}

/// A backup or replica policy that starts to fail, or works again (`backups::scheduler::alert`, which tells people in
/// the app once for each change): queued for the email and webhook too, when those are chosen
pub async fn notice(st: &AppState, notice: &str, problem: bool, data: &Value) {
    let kind = match notice {
        "backup" => "backup",
        "replica" => "replica",
        _ => return,
    };
    if let Err(e) = event(st, kind, problem, data).await {
        tracing::warn!("Administrator alert: {}", e.message);
    }
}

async fn event(st: &AppState, kind: &str, problem: bool, params: &Value) -> AppResult<()> {
    let c: Config = db::json_setting(&mut *st.db.acquire().await?, super::SETTING).await?;
    if !c.wants(kind) {
        return Ok(());
    }
    let to = targets(st, &c).await?;
    let key = format!("{kind}:{}", params["name"].as_str().unwrap_or_default());
    {
        let _w = st.write_lock.lock().await;
        let mut tx = db::begin_write(&st.db).await?;
        let res = enqueue(&mut tx, &to, if problem { "firing" } else { "resolved" }, kind, &key, params, crate::util::now()).await;
        db::settle(tx, res).await?;
    }
    st.part::<Memory>().wake.notify_one();
    Ok(())
}

// ───────────── Delivering ─────────────

#[derive(sqlx::FromRow)]
struct Due {
    id: i64,
    channel: String,
    user_id: Option<i64>,
    state: String,
    kind: String,
    key: String,
    params: String,
    created_at: i64,
    attempts: i64,
}

/// The body of a webhook: the alert, in English, with what it holds
pub fn payload(state: &str, kind: &str, key: &str, params: &Value, site: &str, at: i64) -> Value {
    json!({ "event": "alert", "state": state, "kind": kind, "key": key, "title": text::what(kind, params, crate::i18n::Lang::En), "params": params, "site": site, "at": at })
}

fn site_name(st: &AppState) -> String {
    st.part::<crate::branding::Memory>().settings.read().site_name.clone()
}

async fn send_email(st: &AppState, d: &Due, params: &Value) -> Result<(), String> {
    let smtp = crate::mail::load(&st.db).await;
    if !smtp.ready() {
        return Err("the email server isn't set up".into());
    }
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT email, chosen_lang, lang FROM users WHERE id = ? AND disabled = 0 AND role = 'admin' AND email <> ''")
            .bind(d.user_id)
            .fetch_optional(&st.db)
            .await
            .map_err(|e| e.to_string())?;
    // Not an administrator with an address any more: nothing to send
    let Some((to, chosen, last_used)) = row else { return Ok(()) };
    let lang = crate::i18n::recipient(&chosen, &last_used, crate::i18n::system_default(st));
    let (subject, body) = text::email(&d.state, &d.kind, params, lang);
    crate::mail::send(&smtp, &site_name(st), &crate::mail::Message { to: &to, subject: &subject, body: &body }).await
}

async fn send_webhook(st: &AppState, c: &Config, d: &Due, params: &Value, t: i64) -> Result<(), String> {
    let (url, secret) = (c.webhook_url.trim(), secret(&st.db).await.map_err(|e| e.message)?);
    // Taken away since it was queued: nothing to send
    let Some(secret) = secret.filter(|_| !url.is_empty()) else { return Ok(()) };
    webhook::post(url, c.allow_private, &secret, &payload(&d.state, &d.kind, &d.key, params, &site_name(st), d.created_at), t).await
}

/// Sends what is due; one that fails waits longer each time, and is given up on after the last wait
pub async fn deliver_due(st: &AppState, t: i64) -> AppResult<()> {
    let due: Vec<Due> = sqlx::query_as(
        "SELECT id, channel, user_id, state, kind, key, params, created_at, attempts FROM alert_deliveries WHERE done_at IS NULL AND failed = 0 AND next_attempt_at <= ? ORDER BY id LIMIT 20",
    )
    .bind(t)
    .fetch_all(&st.db)
    .await?;
    if due.is_empty() {
        return Ok(());
    }
    let c: Config = db::json_setting(&mut *st.db.acquire().await?, super::SETTING).await?;
    for d in due {
        let params: Value = serde_json::from_str(&d.params).unwrap_or_default();
        let result = if d.channel == "email" { send_email(st, &d, &params).await } else { send_webhook(st, &c, &d, &params, t).await };
        record(st, &d, result, t).await?;
    }
    Ok(())
}

async fn record(st: &AppState, d: &Due, result: Result<(), String>, t: i64) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    match result {
        Ok(()) => {
            sqlx::query("UPDATE alert_deliveries SET done_at = ?, attempts = attempts + 1, error = NULL WHERE id = ?").bind(t).bind(d.id).execute(&st.db).await?;
        }
        Err(error) => {
            tracing::warn!("Administrator alert {} ({}): {error}", d.key, d.channel);
            let attempts = d.attempts + 1;
            match BACKOFF.get(usize::try_from(attempts - 1).unwrap_or(usize::MAX)) {
                Some(wait) => {
                    sqlx::query("UPDATE alert_deliveries SET attempts = ?, next_attempt_at = ?, error = ? WHERE id = ?")
                        .bind(attempts)
                        .bind(t + wait)
                        .bind(&error)
                        .bind(d.id)
                        .execute(&st.db)
                        .await?;
                }
                None => {
                    sqlx::query("UPDATE alert_deliveries SET attempts = ?, failed = 1, error = ? WHERE id = ?")
                        .bind(attempts)
                        .bind(&error)
                        .bind(d.id)
                        .execute(&st.db)
                        .await?;
                }
            }
        }
    }
    Ok(())
}

/// What the scheduler does every few seconds: looks at what is wrong once a minute, and sends what is due
pub async fn tick(st: &AppState, t: i64) -> AppResult<()> {
    let mem = st.part::<Memory>();
    let looked = if t - mem.last_look.load(std::sync::atomic::Ordering::Acquire) >= LOOK_EVERY {
        mem.last_look.store(t, std::sync::atomic::Ordering::Release);
        evaluate(st, t).await
    } else {
        Ok(())
    };
    let sent = deliver_due(st, t).await;
    looked.and(sent)
}

pub fn spawn(st: AppState) {
    scheduler::spawn(st, "Administrator alerts", |st| &st.part::<Memory>().wake, |st, t| async move { tick(&st, t).await });
}
