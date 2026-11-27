//! File event webhooks (Control panel › Webhooks): other systems told when files change, so they needn't keep asking.
//!
//! - An administrator gives an address, a secret to sign with, the events to send and, if wanted, the spaces
//! - A message is queued in the transaction of the change (`queue`, called where the activity log is written): a change
//!   that fails sends nothing, and a restart loses nothing. Sending happens apart from it (delivery.rs), so a slow or
//!   unreachable receiver never delays a change
//! - Personal spaces and restricted folders are left out, also when only one end of a move or copy is in one.
//!   Messages hold names and ids, never contents, share link addresses or passwords
//! - Changes made on the server's disk in folder spaces, which scans find, send nothing

mod delivery;

#[cfg(test)]
mod tests;

use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

pub use delivery::spawn;

use crate::{
    auth::{Admin, User},
    db,
    error::{AppError, AppResult},
    logs,
    state::AppState,
    util::{new_id, now},
};

/// The events a webhook can be sent, named as in the activity log
pub const EVENTS: [&str; 14] = [
    "upload",
    "create_folder",
    "edit",
    "rename",
    "move",
    "copy",
    "compress",
    "extract",
    "trash",
    "restore",
    "delete",
    "share_create",
    "share_update",
    "share_delete",
];

/// Webhooks at most
const MAX_WEBHOOKS: i64 = 20;

/// What a secret is sealed with (`secrets::seal`)
pub fn seal_context(id: &str) -> String {
    format!("file-webhook:{id}")
}

/// File webhooks' part of `AppState`
#[derive(Default)]
pub struct Memory {
    /// Wakes the sender: something was queued by a test, or a webhook changed
    pub wake: tokio::sync::Notify,
    /// The webhooks being sent to now
    sending: crate::sync::Mutex<std::collections::HashSet<String>>,
}

// ───────────── Queuing ─────────────

#[derive(sqlx::FromRow)]
struct Chosen {
    id: String,
    drives: String,
}

/// The webhooks that are sent `event`
async fn wanting(conn: &mut SqliteConnection, event: &str) -> AppResult<Vec<Chosen>> {
    Ok(sqlx::query_as(
        "SELECT id, drives FROM file_webhooks
         WHERE enabled = 1 AND EXISTS (SELECT 1 FROM json_each(file_webhooks.events) WHERE value = ?)",
    )
    .bind(event)
    .fetch_all(conn)
    .await?)
}

/// Queues the news of a change, within its transaction: `event` happened to `item`, as it was before the change, by
/// `actor` (None: ThirtyFile itself). `into` is the folder a move or copy went to. `restricted`: the item, where it was
/// or where it went is in a restricted folder, so nothing is sent
pub async fn queue(conn: &mut SqliteConnection, actor: Option<&User>, item: &dyn logs::Item, into: Option<&str>, event: &str, restricted: bool) -> AppResult<()> {
    if restricted || !EVENTS.contains(&event) {
        return Ok(());
    }
    let mut hooks = wanting(conn, event).await?;
    if hooks.is_empty() {
        return Ok(());
    }
    // The item as it is after the change (gone: as it was)
    let now_is: Option<(String, String, Option<String>, String)> =
        sqlx::query_as("SELECT name, kind, parent_id, drive_id FROM nodes WHERE id = ?").bind(item.id()).fetch_optional(&mut *conn).await?;
    let dest: Option<(String, String)> = match into {
        Some(id) => sqlx::query_as("SELECT name, drive_id FROM nodes WHERE id = ?").bind(id).fetch_optional(&mut *conn).await?,
        None => None,
    };
    let mut involved: Vec<&str> = item.space().into_iter().chain(now_is.as_ref().map(|n| n.3.as_str())).chain(dest.as_ref().map(|d| d.1.as_str())).collect();
    involved.sort_unstable();
    involved.dedup();
    let spaces: Vec<(String, String, String)> = sqlx::query_as("SELECT id, name, kind FROM drives WHERE id IN (SELECT value FROM json_each(?))")
        .bind(serde_json::to_string(&involved).map_err(AppError::internal)?)
        .fetch_all(&mut *conn)
        .await?;
    // A space that isn't there can't be told apart from a personal one
    if involved.is_empty() || spaces.len() != involved.len() || spaces.iter().any(|(_, _, kind)| kind == "personal") {
        return Ok(());
    }
    hooks.retain(|h| {
        let only: Vec<String> = serde_json::from_str(&h.drives).unwrap_or_default();
        only.is_empty() || only.iter().any(|d| involved.contains(&d.as_str()))
    });
    if hooks.is_empty() {
        return Ok(());
    }
    let space = |id: &str| spaces.iter().find(|s| s.0 == id).map(|(id, name, _)| json!({ "id": id, "name": name }));
    let (name, kind, parent) = match &now_is {
        Some((name, kind, parent, _)) => (name.as_str(), Some(kind.as_str()), parent.as_deref()),
        None => (item.name(), None, None),
    };
    let at = now();
    let mut body = json!({
        "id": new_id(),
        "event": event,
        "at": at,
        "user": actor.map(|u| json!({ "id": u.id, "name": u.username })),
        "space": item.space().and_then(space),
        "item": { "id": item.id(), "name": name, "kind": kind, "parent_id": parent },
    });
    if name != item.name() {
        body["previous_name"] = item.name().into();
    }
    if let (Some(id), Some((folder, drive))) = (into, &dest) {
        body["to"] = json!({ "folder_id": id, "name": folder, "space": space(drive) });
    }
    let body = body.to_string();
    for hook in hooks {
        sqlx::query("INSERT INTO file_webhook_deliveries (webhook_id, event, body, created_at, next_attempt_at) VALUES (?, ?, ?, ?, ?)")
            .bind(hook.id)
            .bind(event)
            .bind(&body)
            .bind(at)
            .bind(at)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// `queue` for an item deleted for good without an entry of its own in the activity log (Empty trash, and the trash
/// emptied after its retention time)
pub async fn queue_deleted(conn: &mut SqliteConnection, actor: Option<&User>, node: &crate::tree::Node) -> AppResult<()> {
    if wanting(conn, "delete").await?.is_empty() {
        return Ok(());
    }
    let restricted = crate::tree::saved_scopes(conn, Some(&node.id)).await? != "[]";
    queue(conn, actor, node, None, "delete", restricted).await
}

// ───────────── The page ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct Delivery {
    pub id: i64,
    pub event: String,
    pub created_at: i64,
    pub attempts: i64,
    pub done_at: Option<i64>,
    pub failed: bool,
    pub error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    url: String,
    events: String,
    drives: String,
    allow_private: bool,
    enabled: bool,
}

#[derive(Serialize)]
pub struct Webhook {
    pub id: String,
    pub url: String,
    pub events: Vec<String>,
    /// Only these spaces; empty: every space but personal ones
    pub drives: Vec<String>,
    pub allow_private: bool,
    pub enabled: bool,
    /// Messages waiting to be sent
    pub pending: i64,
    /// The latest messages, newest first
    pub recent: Vec<Delivery>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Space {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Serialize)]
pub struct Info {
    pub webhooks: Vec<Webhook>,
    /// The events that can be chosen
    pub events: &'static [&'static str],
    /// The spaces that can be chosen
    pub spaces: Vec<Space>,
}

async fn info(st: &AppState) -> AppResult<Info> {
    let rows: Vec<Row> =
        sqlx::query_as("SELECT id, url, events, drives, allow_private, enabled FROM file_webhooks ORDER BY created_at, rowid").fetch_all(&st.db).await?;
    let mut webhooks = Vec::with_capacity(rows.len());
    for r in rows {
        let recent: Vec<Delivery> = sqlx::query_as(
            "SELECT id, event, created_at, attempts, done_at, failed, error FROM file_webhook_deliveries WHERE webhook_id = ? ORDER BY id DESC LIMIT 20",
        )
        .bind(&r.id)
        .fetch_all(&st.db)
        .await?;
        let (pending,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM file_webhook_deliveries WHERE webhook_id = ? AND done_at IS NULL AND failed = 0")
            .bind(&r.id)
            .fetch_one(&st.db)
            .await?;
        webhooks.push(Webhook {
            events: serde_json::from_str(&r.events).unwrap_or_default(),
            drives: serde_json::from_str(&r.drives).unwrap_or_default(),
            id: r.id,
            url: r.url,
            allow_private: r.allow_private,
            enabled: r.enabled,
            pending,
            recent,
        });
    }
    let spaces = sqlx::query_as("SELECT id, name, kind FROM drives WHERE kind <> 'personal' ORDER BY CASE kind WHEN 'company' THEN 0 ELSE 1 END, name")
        .fetch_all(&st.db)
        .await?;
    Ok(Info { webhooks, events: &EVENTS, spaces })
}

pub async fn list(State(st): State<AppState>, _: Admin) -> AppResult<Json<Info>> {
    Ok(Json(info(&st).await?))
}

#[derive(Deserialize)]
pub struct Change {
    pub url: String,
    /// A new secret; left out when changing a webhook, the one saved stays
    #[serde(default)]
    pub secret: Option<String>,
    pub events: Vec<String>,
    #[serde(default)]
    pub drives: Vec<String>,
    #[serde(default)]
    pub allow_private: bool,
    pub enabled: bool,
}

/// Checks what was entered, and tidies it: each event and space once, events in their usual order
fn check(c: &mut Change) -> AppResult<()> {
    c.url = c.url.trim().to_string();
    if c.url.is_empty() {
        return Err(AppError::bad_request("Enter the webhook address as a full URL, such as https://example.com/hook"));
    }
    crate::alerts::check_address(&c.url)?;
    if c.secret.as_ref().is_some_and(|s| s.chars().count() < crate::alerts::MIN_SECRET) {
        return Err(AppError::bad_request(format!("The shared secret needs at least {} characters", crate::alerts::MIN_SECRET)));
    }
    if c.events.iter().any(|e| !EVENTS.contains(&e.as_str())) {
        return Err(AppError::bad_request("Unknown event"));
    }
    c.events = EVENTS.iter().filter(|e| c.events.iter().any(|chosen| chosen == *e)).map(|e| e.to_string()).collect();
    if c.events.is_empty() {
        return Err(AppError::bad_request("Choose at least one event"));
    }
    c.drives.sort();
    c.drives.dedup();
    Ok(())
}

/// Saves a webhook: a new one without `id`
async fn store(st: &AppState, admin: &User, id: Option<&str>, mut c: Change) -> AppResult<()> {
    check(&mut c)?;
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    let admin = crate::auth::require_admin(&mut tx, admin).await?;
    let drives = serde_json::to_string(&c.drives).map_err(AppError::internal)?;
    let (known,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM drives WHERE kind <> 'personal' AND id IN (SELECT value FROM json_each(?))").bind(&drives).fetch_one(&mut *tx).await?;
    if known != c.drives.len() as i64 {
        return Err(AppError::bad_request("A chosen space no longer exists"));
    }
    let events = serde_json::to_string(&c.events).map_err(AppError::internal)?;
    let at = now();
    match id {
        None => {
            let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM file_webhooks").fetch_one(&mut *tx).await?;
            if count >= MAX_WEBHOOKS {
                return Err(AppError::bad_request(format!("At most {MAX_WEBHOOKS} webhooks")));
            }
            let Some(secret) = &c.secret else { return Err(AppError::bad_request("Set a shared secret to sign the webhook's messages with")) };
            let id = new_id();
            sqlx::query("INSERT INTO file_webhooks (id, url, secret, events, drives, allow_private, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(&id)
                .bind(&c.url)
                .bind(crate::secrets::seal(&seal_context(&id), secret))
                .bind(events)
                .bind(drives)
                .bind(c.allow_private)
                .bind(c.enabled)
                .bind(at)
                .execute(&mut *tx)
                .await?;
            logs::record_activity(&mut tx, &admin, None, "settings", &format!("Added a file webhook: {}", shown(&c.url))).await?;
        }
        Some(id) => {
            let changed =
                sqlx::query("UPDATE file_webhooks SET url = ?, secret = COALESCE(?, secret), events = ?, drives = ?, allow_private = ?, enabled = ? WHERE id = ?")
                    .bind(&c.url)
                    .bind(c.secret.as_ref().map(|s| crate::secrets::seal(&seal_context(id), s)))
                    .bind(events)
                    .bind(drives)
                    .bind(c.allow_private)
                    .bind(c.enabled)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            if changed.rows_affected() == 0 {
                return Err(not_found());
            }
            // What waits is tried with the new settings at once
            sqlx::query("UPDATE file_webhook_deliveries SET next_attempt_at = ? WHERE webhook_id = ? AND done_at IS NULL AND failed = 0")
                .bind(at)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            logs::record_activity(&mut tx, &admin, None, "settings", &format!("Updated a file webhook: {}", shown(&c.url))).await?;
        }
    }
    tx.commit().await?;
    st.part::<Memory>().wake.notify_one();
    Ok(())
}

fn not_found() -> AppError {
    AppError::not_found("Webhook not found")
}

/// An address as the activity log shows it: without its query, where some services put a key
fn shown(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

pub async fn create(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<Change>) -> AppResult<Json<Info>> {
    store(&st, &user, None, req).await?;
    Ok(Json(info(&st).await?))
}

pub async fn update(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>, Json(req): Json<Change>) -> AppResult<Json<Info>> {
    store(&st, &user, Some(&id), req).await?;
    Ok(Json(info(&st).await?))
}

pub async fn remove(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>) -> AppResult<Json<Info>> {
    {
        let _w = st.write_lock.lock().await;
        let mut tx = db::begin_write(&st.db).await?;
        let user = crate::auth::require_admin(&mut tx, &user).await?;
        let gone: Option<(String,)> = sqlx::query_as("DELETE FROM file_webhooks WHERE id = ? RETURNING url").bind(&id).fetch_optional(&mut *tx).await?;
        let (url,) = gone.ok_or_else(not_found)?;
        logs::record_activity(&mut tx, &user, None, "settings", &format!("Removed a file webhook: {}", shown(&url))).await?;
        tx.commit().await?;
    }
    Ok(Json(info(&st).await?))
}

/// Sends a test message now, and says why when it can't be sent
pub async fn test(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let target = delivery::target(&st, &id).await?.ok_or_else(not_found)?;
    let at = now();
    let body = json!({ "id": new_id(), "event": "test", "at": at });
    delivery::post(&st, &target, body).await.map_err(AppError::bad_request)?;
    Ok(Json(json!({ "ok": true })))
}
