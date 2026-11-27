//! Sending what is queued: each webhook on its own, oldest message first, so one that is slow or can't be reached
//! holds up nothing else. A message may arrive more than once (its `id` tells), and after a failure not in order.

use serde_json::Value;

use super::{Memory, seal_context};
use crate::{
    backups::scheduler,
    db,
    error::{AppError, AppResult},
    state::AppState,
    util::now,
};

/// How long to wait after a failed attempt, for the first retries; after the last, a message is given up on
const BACKOFF: [i64; 5] = [60, 300, 1800, 7200, 21600];
/// A message not sent after this long is given up on, however few attempts it had
const GIVE_UP_AFTER: i64 = 86400;
/// Messages are kept this long to be looked at: sent ones less, and every one at most
const KEEP_SENT: i64 = 7 * 86400;
const KEEP: i64 = 30 * 86400;
/// Messages sent to one webhook in one go
const BATCH: usize = 100;

/// Where a webhook's messages go
#[derive(Clone, sqlx::FromRow)]
pub struct Target {
    pub id: String,
    url: String,
    secret: String,
    allow_private: bool,
}

pub async fn target(st: &AppState, id: &str) -> AppResult<Option<Target>> {
    Ok(sqlx::query_as("SELECT id, url, secret, allow_private FROM file_webhooks WHERE id = ?").bind(id).fetch_optional(&st.db).await?)
}

/// Sends one message, signed (alerts/webhook.rs); the error is a short English reason
pub async fn post(st: &AppState, to: &Target, mut body: Value) -> Result<(), String> {
    let secret = crate::secrets::open(&seal_context(&to.id), &to.secret)?;
    body["site"] = st.part::<crate::branding::Memory>().settings.read().site_name.clone().into();
    crate::alerts::webhook::post(&to.url, to.allow_private, &secret, &body, now()).await
}

/// Gives up on messages that waited too long, and removes old ones
async fn tidy(st: &AppState, t: i64) -> AppResult<()> {
    let (due,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM file_webhook_deliveries
           WHERE created_at < ?1 AND done_at IS NULL AND failed = 0 OR created_at < ?3 OR created_at < ?2 AND done_at IS NOT NULL)",
    )
    .bind(t - GIVE_UP_AFTER)
    .bind(t - KEEP_SENT)
    .bind(t - KEEP)
    .fetch_one(&st.db)
    .await?;
    if !due {
        return Ok(());
    }
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    sqlx::query(
        "UPDATE file_webhook_deliveries SET failed = 1, error = COALESCE(error, 'not sent within a day')
         WHERE created_at < ? AND done_at IS NULL AND failed = 0",
    )
    .bind(t - GIVE_UP_AFTER)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM file_webhook_deliveries WHERE created_at < ?1 OR created_at < ?2 AND done_at IS NOT NULL")
        .bind(t - KEEP)
        .bind(t - KEEP_SENT)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// The webhooks with a message due at `t`
async fn due(st: &AppState, t: i64) -> AppResult<Vec<Target>> {
    Ok(sqlx::query_as(
        "SELECT id, url, secret, allow_private FROM file_webhooks w WHERE enabled = 1 AND EXISTS (
           SELECT 1 FROM file_webhook_deliveries d WHERE d.webhook_id = w.id AND d.done_at IS NULL AND d.failed = 0 AND d.next_attempt_at <= ?)",
    )
    .bind(t)
    .fetch_all(&st.db)
    .await?)
}

/// Sends a webhook's due messages, oldest first, until one fails; whether more may be due
async fn send_due(st: &AppState, to: &Target, t: i64) -> AppResult<bool> {
    let waiting: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT id, body, attempts FROM file_webhook_deliveries
         WHERE webhook_id = ? AND done_at IS NULL AND failed = 0 AND next_attempt_at <= ? ORDER BY id LIMIT ?",
    )
    .bind(&to.id)
    .bind(t)
    .bind(BATCH as i64)
    .fetch_all(&st.db)
    .await?;
    let full = waiting.len() == BATCH;
    for (id, body, attempts) in waiting {
        let body: Value = serde_json::from_str(&body).map_err(AppError::internal)?;
        let result = post(st, to, body).await;
        let failed = result.is_err();
        record(st, to, id, attempts + 1, result, t).await?;
        if failed {
            return Ok(false);
        }
    }
    Ok(full)
}

async fn record(st: &AppState, to: &Target, id: i64, attempts: i64, result: Result<(), String>, t: i64) -> AppResult<()> {
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    match result {
        Ok(()) => {
            sqlx::query("UPDATE file_webhook_deliveries SET done_at = ?, attempts = ?, error = NULL WHERE id = ?")
                .bind(t)
                .bind(attempts)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        Err(error) => {
            tracing::warn!("File webhook {}: {error}", to.id);
            let wait = BACKOFF.get(usize::try_from(attempts - 1).unwrap_or(usize::MAX)).copied();
            sqlx::query("UPDATE file_webhook_deliveries SET attempts = ?, next_attempt_at = ?, failed = ?, error = ? WHERE id = ?")
                .bind(attempts)
                .bind(t + wait.unwrap_or_default())
                .bind(wait.is_none())
                .bind(&error)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            // The others wait as long: a receiver that can't be reached isn't tried with each of them in turn, and
            // they follow in order once it can
            sqlx::query("UPDATE file_webhook_deliveries SET next_attempt_at = MAX(next_attempt_at, ?) WHERE webhook_id = ? AND done_at IS NULL AND failed = 0")
                .bind(t + wait.unwrap_or(BACKOFF[0]))
                .bind(&to.id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// A webhook being sent to, until dropped
struct Sending(AppState, String);

impl Sending {
    fn start(st: &AppState, id: &str) -> Option<Sending> {
        st.part::<Memory>().sending.lock().insert(id.to_string()).then(|| Sending(st.clone(), id.to_string()))
    }
}

impl Drop for Sending {
    fn drop(&mut self) {
        self.0.part::<Memory>().sending.lock().remove(&self.1);
    }
}

/// What the sender does every few seconds: starts sending to each webhook with something due that isn't being sent to
/// already
async fn tick(st: &AppState, t: i64) -> AppResult<()> {
    tidy(st, t).await?;
    for to in due(st, t).await? {
        let Some(turn) = Sending::start(st, &to.id) else { continue };
        let st = st.clone();
        tokio::spawn(async move {
            let more = send_due(&st, &to, t).await;
            drop(turn);
            match more {
                Ok(true) => st.part::<Memory>().wake.notify_one(),
                Ok(false) => {}
                Err(e) => tracing::warn!("File webhooks: {}", e.message),
            }
        });
    }
    Ok(())
}

/// `tick` that waits for the sending to end (tests)
#[cfg(test)]
pub async fn deliver(st: &AppState, t: i64) -> AppResult<()> {
    tidy(st, t).await?;
    for to in due(st, t).await? {
        send_due(st, &to, t).await?;
    }
    Ok(())
}

pub fn spawn(st: AppState) {
    scheduler::spawn(st, "File webhooks", |st| &st.part::<Memory>().wake, |st, t| async move { tick(&st, t).await });
}
