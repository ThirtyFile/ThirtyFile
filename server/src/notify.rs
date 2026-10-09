//! Notifications (#64): things people are told about under the bell in the app and, when an administrator set up an
//! email server (mail.rs), by email.
//!
//! - `shared`: a folder, file or whole space was shared with the person (directly or with a group they are in; not
//!   access given to everyone)
//! - `space_full`: a space they own or manage (their personal space; for team spaces owners and managers; for "All
//!   files" administrators) is almost full. Told once, and again only after the space had room again
//! - `access_expiring`: access given to them ends within three days. Not told when the access was given with that
//!   short an expiry in the first place: the `shared` notification already said when it ends
//! - `app_password`: an app password was made for their account (so one made by someone else doesn't go unnoticed)
//! - `sign_in_method`: a Microsoft, Google, GitHub or other account was linked to theirs, and can now sign in to it
//! - `link_upload`: files arrived through a share link they made that accepts files. Files through the same link
//!   within an hour count up the unread notification instead of adding one (and send no further email)
//! - `backup` (administrators): a backup policy's snapshots fail, wait for a location that can't be reached, or are
//!   overdue; and when it is fine again. Told once per trouble (backups/policy.rs)
//! - `replica` (administrators): a replica policy doesn't keep its copies (a target not working, damaged copies, too
//!   few targets, or behind for too long); and when it does again (replicas/policy.rs)
//!
//! Each person can turn each kind off, in the app and by email separately. Emails are sent in the person's language
//! (`i18n::recipient`) and the time zone they last used the app with, after the change that caused them is saved.
//! Space and expiry checks run with the hourly maintenance.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use crate::{
    auth::{User, get_cookie},
    error::{AppError, AppResult},
    i18n::{Lang, Text, tr},
    logs::format_time,
    state::AppState,
    tree::{PrincipalType, Role},
    util::{format_bytes, now},
};

pub const KINDS: [&str; 8] = ["shared", "space_full", "access_expiring", "app_password", "sign_in_method", "link_upload", "backup", "replica"];
/// Files through one link within this long are told in one notification
const LINK_UPLOAD_BATCH: i64 = 3600;
/// A space is almost full from this share of its size (percent)…
const FULL_PERCENT: i64 = 90;
/// …and has room again below this one, after which filling it up is told again
const ROOM_PERCENT: i64 = 80;
/// Access ending within this is announced
pub const EXPIRY_NOTICE: i64 = 3 * 86400;
/// Notifications older than this are deleted
const KEEP: i64 = 90 * 86400;
/// The most notifications the bell lists
const LIST_LIMIT: i64 = 100;

#[derive(Debug, Clone)]
pub struct Notice {
    pub kind: &'static str,
    /// What opening it shows
    pub node_id: Option<String>,
    pub data: Value,
}

/// An email to send once the change that caused it is saved
#[derive(Debug)]
pub struct Outgoing {
    to: String,
    /// The recipient's saved language and the one they last used (`i18n::recipient`)
    chosen_lang: String,
    lang: String,
    tz_offset: i64,
    notice: Notice,
}

/// Records `notice` for each of `users` who wants it in the app (disabled accounts are skipped); returns the emails
/// to send to those who want it by email, for [`send_later`] after the transaction is committed
pub async fn add(conn: &mut SqliteConnection, users: &[i64], notice: &Notice) -> AppResult<Vec<Outgoing>> {
    if users.is_empty() {
        return Ok(Vec::new());
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        email: String,
        chosen_lang: String,
        lang: String,
        tz_offset: i64,
        in_app: bool,
        by_email: bool,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT u.id, u.email, u.chosen_lang, u.lang, u.tz_offset, COALESCE(p.in_app, 1) AS in_app, COALESCE(p.email, 1) AS by_email
         FROM users u LEFT JOIN notification_prefs p ON p.user_id = u.id AND p.kind = ?2
         WHERE u.id IN (SELECT value FROM json_each(?1)) AND u.disabled = 0",
    )
    .bind(serde_json::to_string(users).unwrap())
    .bind(notice.kind)
    .fetch_all(&mut *conn)
    .await?;
    let data = notice.data.to_string();
    let mut emails = Vec::new();
    for r in rows {
        if r.in_app {
            sqlx::query("INSERT INTO notifications (user_id, kind, data, node_id, created_at) VALUES (?, ?, ?, ?, ?)")
                .bind(r.id)
                .bind(notice.kind)
                .bind(&data)
                .bind(&notice.node_id)
                .bind(now())
                .execute(&mut *conn)
                .await?;
        }
        if r.by_email && !r.email.is_empty() {
            emails.push(Outgoing { to: r.email, chosen_lang: r.chosen_lang, lang: r.lang, tz_offset: r.tz_offset, notice: notice.clone() });
        }
    }
    Ok(emails)
}

/// The people a grant gives access to: the user, or the members of the group (access for everyone isn't announced)
pub async fn grantees(conn: &mut SqliteConnection, principal_type: PrincipalType, principal_id: i64) -> AppResult<Vec<i64>> {
    Ok(match principal_type {
        PrincipalType::User => vec![principal_id],
        PrincipalType::Group => sqlx::query_as::<_, (i64,)>("SELECT user_id FROM group_members WHERE group_id = ?")
            .bind(principal_id)
            .fetch_all(conn)
            .await?
            .into_iter()
            .map(|(id,)| id)
            .collect(),
        PrincipalType::Everyone => Vec::new(),
    })
}

fn mark_key_expiry(grant_id: i64, expires_at: i64) -> String {
    format!("access_expiring:{grant_id}:{expires_at}")
}

async fn set_mark(conn: &mut SqliteConnection, key: &str) -> AppResult<()> {
    sqlx::query("INSERT INTO notification_marks (key, at) VALUES (?, ?) ON CONFLICT (key) DO NOTHING").bind(key).bind(now()).execute(conn).await?;
    Ok(())
}

/// A share: tells the people a new grant gives access to (except the person who shared). Called in the grant's
/// transaction; the caller sends the returned emails after committing.
pub async fn shared(conn: &mut SqliteConnection, by: &User, node: &crate::tree::Node, drive: &crate::tree::Drive, grant_id: i64) -> AppResult<Vec<Outgoing>> {
    let (principal_type, principal_id, role, expires_at): (PrincipalType, i64, Role, Option<i64>) =
        sqlx::query_as("SELECT principal_type, principal_id, role, expires_at FROM grants WHERE id = ?").bind(grant_id).fetch_one(&mut *conn).await?;
    let mut users = grantees(conn, principal_type, principal_id).await?;
    users.retain(|id| *id != by.id);
    // Access that ends soon: the share itself says when, so the hourly check needn't say it again
    if let Some(t) = expires_at.filter(|t| *t <= now() + EXPIRY_NOTICE) {
        set_mark(conn, &mark_key_expiry(grant_id, t)).await?;
    }
    let is_root = node.parent_id.is_none();
    let data = json!({
        "by": if by.display_name.is_empty() { &by.username } else { &by.display_name },
        "name": if is_root { &drive.name } else { &node.name },
        "item": if is_root { "space" } else { node.kind.as_str() },
        "drive_kind": drive.kind,
        "role": role,
        "expires_at": expires_at,
    });
    add(conn, &users, &Notice { kind: "shared", node_id: Some(node.id.clone()), data }).await
}

// ───────────── Hourly checks ─────────────

/// Spaces that are almost full and access that ends soon; old notifications are removed. Emails go out afterwards.
pub async fn check(st: &AppState) -> AppResult<usize> {
    let mut emails = Vec::new();
    let mut told = 0;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        told += check_spaces(&mut tx, &mut emails).await?;
        told += check_expiring(&mut tx, &mut emails).await?;
        let ts = now();
        sqlx::query("DELETE FROM notifications WHERE created_at < ?").bind(ts - KEEP).execute(&mut *tx).await?;
        // The access these were about has ended by now
        sqlx::query("DELETE FROM notification_marks WHERE key LIKE 'access_expiring:%' AND at < ?").bind(ts - 30 * 86400).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM notification_marks WHERE key LIKE 'link_upload:%' AND at < ?").bind(ts - LINK_UPLOAD_BATCH).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    send_later(st, emails);
    Ok(told)
}

async fn check_spaces(conn: &mut SqliteConnection, emails: &mut Vec<Outgoing>) -> AppResult<usize> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        name: String,
        kind: String,
        root_id: String,
        used_bytes: i64,
        quota: i64,
        marked: bool,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT d.id, d.name, d.kind, d.root_id, d.used_bytes,
                CASE WHEN d.kind = 'personal' THEN COALESCE(u.quota_bytes, 0) ELSE d.quota_bytes END AS quota,
                EXISTS (SELECT 1 FROM notification_marks m WHERE m.key = 'space_full:' || d.id) AS marked
         FROM drives d LEFT JOIN users u ON u.id = d.owner_id
         WHERE d.disabled = 0",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut told = 0;
    for d in rows {
        let key = format!("space_full:{}", d.id);
        // Spaces without a size limit, or with room again: told again next time they fill up
        if d.quota <= 0 || (d.used_bytes as i128) * 100 < (d.quota as i128) * ROOM_PERCENT as i128 {
            if d.marked {
                sqlx::query("DELETE FROM notification_marks WHERE key = ?").bind(&key).execute(&mut *conn).await?;
            }
            continue;
        }
        if d.marked || (d.used_bytes as i128) * 100 < (d.quota as i128) * FULL_PERCENT as i128 {
            continue;
        }
        let users: Vec<i64> = sqlx::query_as::<_, (i64,)>(
            "SELECT u.id FROM grants g JOIN users u
               ON (g.principal_type = 'user' AND u.id = g.principal_id)
               OR (g.principal_type = 'group' AND u.id IN (SELECT user_id FROM group_members WHERE group_id = g.principal_id))
             WHERE g.node_id = ?1 AND g.role IN ('manager', 'owner') AND (g.expires_at IS NULL OR g.expires_at > ?2)
             UNION SELECT id FROM users WHERE ?3 = 'company' AND role = 'admin'",
        )
        .bind(&d.root_id)
        .bind(now())
        .bind(&d.kind)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect();
        let percent = ((d.used_bytes as i128) * 100 / d.quota as i128).min(100) as i64;
        let data = json!({ "name": d.name, "drive_kind": d.kind, "used": d.used_bytes, "quota": d.quota, "percent": percent });
        emails.extend(add(conn, &users, &Notice { kind: "space_full", node_id: Some(d.root_id.clone()), data }).await?);
        set_mark(conn, &key).await?;
        told += 1;
    }
    Ok(told)
}

async fn check_expiring(conn: &mut SqliteConnection, emails: &mut Vec<Outgoing>) -> AppResult<usize> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        node_id: String,
        principal_type: PrincipalType,
        principal_id: i64,
        role: Role,
        expires_at: i64,
        name: String,
        kind: String,
        is_root: bool,
        drive_name: String,
        drive_kind: String,
    }
    let ts = now();
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT g.id, g.node_id, g.principal_type, g.principal_id, g.role, g.expires_at, n.name, n.kind,
                n.parent_id IS NULL AS is_root, d.name AS drive_name, d.kind AS drive_kind
         FROM grants g JOIN nodes n ON n.id = g.node_id JOIN drives d ON d.id = n.drive_id
         WHERE g.expires_at > ?1 AND g.expires_at <= ?2 AND g.principal_type IN ('user', 'group')
           AND n.trashed_at IS NULL AND d.disabled = 0
           AND NOT EXISTS (SELECT 1 FROM notification_marks m WHERE m.key = 'access_expiring:' || g.id || ':' || g.expires_at)",
    )
    .bind(ts)
    .bind(ts + EXPIRY_NOTICE)
    .fetch_all(&mut *conn)
    .await?;
    let told = rows.len();
    for g in rows {
        let users = grantees(conn, g.principal_type, g.principal_id).await?;
        let data = json!({
            "name": if g.is_root { &g.drive_name } else { &g.name },
            "item": if g.is_root { "space" } else { g.kind.as_str() },
            "drive_kind": g.drive_kind,
            "role": g.role,
            "expires_at": g.expires_at,
        });
        emails.extend(add(conn, &users, &Notice { kind: "access_expiring", node_id: Some(g.node_id.clone()), data }).await?);
        set_mark(conn, &mark_key_expiry(g.id, g.expires_at)).await?;
    }
    Ok(told)
}

// ───────────── Emails ─────────────

/// A file arrived through a share link that accepts files: its creator (`owner`) is told, in the folder it went to.
/// Within an hour, further files through the same link count up the unread notification.
pub async fn link_upload(st: &AppState, owner: i64, share_id: &str, file: &crate::tree::Node) -> AppResult<()> {
    let Some(folder_id) = file.parent_id.clone() else { return Ok(()) };
    let emails = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let ts = now();
        let counted = sqlx::query(
            "UPDATE notifications SET data = json_set(data, '$.count', COALESCE(json_extract(data, '$.count'), 1) + 1, '$.file', ?1), created_at = ?2
             WHERE user_id = ?3 AND kind = 'link_upload' AND read_at IS NULL AND json_extract(data, '$.share') = ?4 AND created_at > ?5",
        )
        .bind(&file.name)
        .bind(ts)
        .bind(owner)
        .bind(share_id)
        .bind(ts - LINK_UPLOAD_BATCH)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let emails = if counted > 0 {
            Vec::new()
        } else {
            let folder = crate::tree::get_node(&mut tx, &folder_id).await?;
            let name = match &folder {
                Some(f) if f.name.is_empty() => crate::tree::get_drive(&mut tx, f.drive()).await?.map(|d| d.name).unwrap_or_default(),
                Some(f) => f.name.clone(),
                None => String::new(),
            };
            let notice = Notice {
                kind: "link_upload",
                node_id: Some(folder_id),
                data: json!({ "share": share_id, "name": name, "item": "folder", "file": file.name, "count": 1 }),
            };
            let mut emails = add(&mut tx, &[owner], &notice).await?;
            // By email, too, the files of an hour are told once: also when the notice isn't wanted in the app (where
            // a notification of the hour counts them up), or was read meanwhile
            let key = format!("link_upload:{share_id}");
            let (recent,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM notification_marks WHERE key = ? AND at > ?)")
                .bind(&key)
                .bind(ts - LINK_UPLOAD_BATCH)
                .fetch_one(&mut *tx)
                .await?;
            if recent {
                emails.clear();
            } else if !emails.is_empty() {
                sqlx::query("INSERT INTO notification_marks (key, at) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET at = ?2")
                    .bind(&key)
                    .bind(ts)
                    .execute(&mut *tx)
                    .await?;
            }
            emails
        };
        tx.commit().await?;
        emails
    };
    send_later(st, emails);
    Ok(())
}

/// Emails of one kind an address gets within the sign-in limits' window (15 minutes): access taken away and given
/// again, over and over, can't turn into a stream of emails
const MAX_EMAILS_PER_KIND: usize = 5;

/// Sends the emails in the background, one after another; nothing happens when no email server is set up
pub fn send_later(st: &AppState, mut emails: Vec<Outgoing>) {
    emails.retain(|e| crate::auth::begin_attempt(st, &format!("mail:{}:{}", e.notice.kind, e.to.to_lowercase()), MAX_EMAILS_PER_KIND));
    if emails.is_empty() {
        return;
    }
    let st = st.clone();
    tokio::spawn(async move {
        let cfg = crate::mail::load(&st.db).await;
        if !cfg.ready() {
            return;
        }
        let site = st.part::<crate::branding::Memory>().settings.read().site_name.clone();
        let base = st.system.read().public_url.clone();
        let default = crate::i18n::system_default(&st);
        for e in emails {
            let lang = crate::i18n::recipient(&e.chosen_lang, &e.lang, default);
            let (subject, body) = render(&e.notice, lang, e.tz_offset, &site, &base);
            if let Err(err) = crate::mail::send(&cfg, &site, &crate::mail::Message { to: &e.to, subject: &subject, body: &body }).await {
                tracing::warn!("Couldn't send a notification email to {}: {err}", e.to);
            }
        }
    });
}

/// The test message of Control panel › Email
pub fn test_message(lang: Lang, site: &str) -> (String, String) {
    (tr(lang, Text::TestSubject, &[("site", site)]), tr(lang, Text::TestBody, &[("site", site)]))
}

/// A time in the person's time zone, e.g. "2026-10-01 14:00 (UTC+8)"
fn local_time(t: i64, tz_offset: i64) -> String {
    let minutes = -tz_offset.clamp(-14 * 60, 14 * 60);
    let zone = match (minutes / 60, (minutes % 60).abs()) {
        (0, 0) => "UTC".to_string(),
        (h, 0) => format!("UTC{h:+}"),
        (h, m) => format!("UTC{}{}:{m:02}", if minutes < 0 { "-" } else { "+" }, h.abs()),
    };
    format!("{} ({zone})", &format_time(t, minutes * 60)[..16])
}

/// A name as the app shows it: the spaces the system names itself are translated
fn shown_name(data: &Value, lang: Lang) -> String {
    let name = data["name"].as_str().unwrap_or_default();
    let item = data["item"].as_str();
    let kind = data["drive_kind"].as_str();
    let is_space = item.is_none() || item == Some("space");
    match (is_space, kind, name) {
        (true, Some("personal"), "My files") => tr(lang, Text::MyFiles, &[]),
        (true, Some("company"), "All files") => tr(lang, Text::AllFiles, &[]),
        _ => name.to_string(),
    }
}

fn role_name(role: &str, lang: Lang) -> String {
    let text = match role {
        "owner" => Text::RoleOwner,
        "manager" => Text::RoleManager,
        "editor" => Text::RoleEditor,
        _ => Text::RoleViewer,
    };
    tr(lang, text, &[])
}

/// Subject and text of a notification email
pub fn render(n: &Notice, lang: Lang, tz_offset: i64, site: &str, base_url: &str) -> (String, String) {
    let d = &n.data;
    let name = shown_name(d, lang);
    let role = role_name(d["role"].as_str().unwrap_or_default(), lang);
    let ends = d["expires_at"].as_i64().map(|t| local_time(t, tz_offset));
    let (subject, mut body) = match n.kind {
        "shared" => shared_text(d, &name, &role, ends.as_deref(), lang),
        "space_full" => space_full_text(d, &name, lang),
        "backup" => backup_text(d, tz_offset, lang),
        "replica" => replica_text(d, lang),
        "link_upload" => link_upload_text(d, &name, lang),
        "app_password" => app_password_text(d, lang),
        "sign_in_method" => sign_in_method_text(d, lang),
        _ => expiring_text(&name, &role, &ends.unwrap_or_default(), lang),
    };
    if let (Some(id), false) = (&n.node_id, base_url.is_empty()) {
        let path = if d["item"].as_str() == Some("file") { "view" } else { "files" };
        body.push_str(&tr(lang, Text::MailOpen, &[("link", &format!("{base_url}/{path}/{id}"))]));
    }
    body.push_str(&tr(lang, Text::MailFooter, &[("site", site)]));
    (subject, body)
}

/// Someone was given access to an item or a space
fn shared_text(d: &Value, name: &str, role: &str, ends: Option<&str>, lang: Lang) -> (String, String) {
    let by = d["by"].as_str().unwrap_or_default();
    let space = d["item"].as_str() == Some("space");
    let subject = tr(lang, if space { Text::SharedSpaceSubject } else { Text::SharedItemSubject }, &[("by", by), ("name", name)]);
    let mut body = tr(lang, Text::SharedBody, &[("subject", &subject), ("role", role)]);
    if let Some(ends) = ends {
        body.push_str(&tr(lang, Text::SharedEnds, &[("ends", ends)]));
    }
    (subject, body)
}

/// A space is almost full
fn space_full_text(d: &Value, name: &str, lang: Lang) -> (String, String) {
    let used = format_bytes(d["used"].as_i64().unwrap_or_default());
    let quota = format_bytes(d["quota"].as_i64().unwrap_or_default());
    let percent = d["percent"].as_i64().unwrap_or_default().to_string();
    let vars = [("name", name), ("used", &used), ("quota", &quota), ("percent", &percent)];
    (tr(lang, Text::SpaceFullSubject, &vars), tr(lang, Text::SpaceFullBody, &vars))
}

/// A backup policy fails, waits for its location or is overdue, or works again
fn backup_text(d: &Value, tz_offset: i64, lang: Lang) -> (String, String) {
    let backup = d["name"].as_str().unwrap_or_default();
    let error = d["error"].as_str().unwrap_or_default();
    let since = d["since"].as_i64().map(|t| local_time(t, tz_offset));
    let (subject, text) = match d["state"].as_str().unwrap_or_default() {
        "failing" => (Text::BackupFailingSubject, Text::BackupFailingBody),
        "waiting" => (Text::BackupWaitingSubject, Text::BackupWaitingBody),
        "overdue" => (Text::BackupOverdueSubject, Text::BackupOverdueBody),
        _ => (Text::BackupOkSubject, Text::BackupOkBody),
    };
    let vars = [("backup", backup), ("error", error)];
    let mut body = tr(lang, text, &vars);
    if let Some(since) = since {
        body.push_str(&tr(lang, Text::BackupNewest, &[("since", &since)]));
    }
    body.push_str(&tr(lang, Text::BackupSee, &[]));
    (tr(lang, subject, &vars), body)
}

/// A replica policy doesn't keep all its copies, or does again
fn replica_text(d: &Value, lang: Lang) -> (String, String) {
    let policy = d["name"].as_str().unwrap_or_default();
    let error = d["error"].as_str().filter(|e| !e.is_empty());
    let (current, wanted) = (d["current"].as_i64().unwrap_or_default().to_string(), d["wanted"].as_i64().unwrap_or_default().to_string());
    let (subject, text) = if d["state"].as_str() == Some("degraded") {
        (Text::ReplicaDegradedSubject, Text::ReplicaDegradedBody)
    } else {
        (Text::ReplicaOkSubject, Text::ReplicaOkBody)
    };
    let vars = [("policy", policy), ("current", &current), ("wanted", &wanted)];
    let mut body = tr(lang, text, &vars);
    if let Some(error) = error {
        body.push_str(&format!("\n{error}\n"));
    }
    body.push_str(&tr(lang, Text::ReplicaSee, &[]));
    (tr(lang, subject, &vars), body)
}

/// A file arrived through a link that accepts files
fn link_upload_text(d: &Value, name: &str, lang: Lang) -> (String, String) {
    let vars = [("file", d["file"].as_str().unwrap_or_default()), ("name", name)];
    (tr(lang, Text::LinkUploadSubject, &vars), tr(lang, Text::LinkUploadBody, &vars))
}

/// An app password was created for the account
fn app_password_text(d: &Value, lang: Lang) -> (String, String) {
    let read_only = d["scope"].as_str() == Some("read");
    let access = tr(lang, if read_only { Text::AppPasswordReadOnly } else { Text::AppPasswordReadWrite }, &[]);
    let vars = [("name", d["name"].as_str().unwrap_or_default()), ("access", &access), ("ip", d["ip"].as_str().unwrap_or_default())];
    (tr(lang, Text::AppPasswordSubject, &vars), tr(lang, Text::AppPasswordBody, &vars))
}

/// A sign-in method was linked to the account
fn sign_in_method_text(d: &Value, lang: Lang) -> (String, String) {
    let account = d["account"].as_str().filter(|a| !a.is_empty()).map(|a| tr(lang, Text::SignInMethodAccount, &[("account", a)])).unwrap_or_default();
    let vars = [("provider", d["label"].as_str().unwrap_or_default()), ("account", &account), ("ip", d["ip"].as_str().unwrap_or_default())];
    (tr(lang, Text::SignInMethodSubject, &vars), tr(lang, Text::SignInMethodBody, &vars))
}

/// Access someone was given ends soon
fn expiring_text(name: &str, role: &str, ends: &str, lang: Lang) -> (String, String) {
    let vars = [("name", name), ("role", role), ("ends", ends)];
    (tr(lang, Text::ExpiringSubject, &vars), tr(lang, Text::ExpiringBody, &vars))
}

// ───────────── The bell ─────────────

#[derive(sqlx::FromRow)]
pub struct Item {
    id: i64,
    kind: String,
    /// JSON text
    data: String,
    node_id: Option<String>,
    created_at: i64,
    read: bool,
}

#[derive(Deserialize)]
pub struct ListQuery {
    /// The browser's time zone (minutes, as `Date.getTimezoneOffset()`), remembered for the times in emails
    tz: Option<i64>,
}

/// The newest notifications and how many are unread. Also remembers the language and time zone the person uses, for emails
pub async fn list(State(st): State<AppState>, user: User, headers: HeaderMap, Query(q): Query<ListQuery>) -> AppResult<Json<Value>> {
    let mut c = st.db.acquire().await?;
    let items: Vec<Item> =
        sqlx::query_as("SELECT id, kind, data, node_id, created_at, read_at IS NOT NULL AS read FROM notifications WHERE user_id = ? ORDER BY id DESC LIMIT ?")
            .bind(user.id)
            .bind(LIST_LIMIT)
            .fetch_all(&mut *c)
            .await?;
    let (unread, lang, tz_offset): (i64, String, i64) =
        sqlx::query_as("SELECT (SELECT COUNT(*) FROM notifications WHERE user_id = ?1 AND read_at IS NULL), lang, tz_offset FROM users WHERE id = ?1")
            .bind(user.id)
            .fetch_one(&mut *c)
            .await?;
    drop(c);
    let used_lang = get_cookie(&headers, "tf_lang").filter(|l| Lang::parse(l).is_some()).unwrap_or(&lang).to_string();
    let used_tz = q.tz.map_or(tz_offset, |t| t.clamp(-14 * 60, 14 * 60));
    if used_lang != lang || used_tz != tz_offset {
        let _w = st.write_lock.lock().await;
        sqlx::query("UPDATE users SET lang = ?, tz_offset = ? WHERE id = ?").bind(&used_lang).bind(used_tz).bind(user.id).execute(&st.db).await?;
    }
    let items: Vec<Value> = items
        .into_iter()
        .map(|n| {
            let data: Value = serde_json::from_str(&n.data).unwrap_or_default();
            json!({ "id": n.id, "kind": n.kind, "data": data, "node_id": n.node_id, "created_at": n.created_at, "read": n.read })
        })
        .collect();
    Ok(Json(json!({ "items": items, "unread": unread })))
}

#[derive(Deserialize)]
pub struct ReadReq {
    /// None = all of them
    ids: Option<Vec<i64>>,
}

pub async fn mark_read(State(st): State<AppState>, user: User, Json(req): Json<ReadReq>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE notifications SET read_at = ?1 WHERE user_id = ?2 AND read_at IS NULL AND (?3 IS NULL OR id IN (SELECT value FROM json_each(?3)))")
        .bind(now())
        .bind(user.id)
        .bind(req.ids.map(|ids| serde_json::to_string(&ids).unwrap()))
        .execute(&st.db)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<i64>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    sqlx::query("DELETE FROM notifications WHERE id = ? AND user_id = ?").bind(id).bind(user.id).execute(&st.db).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn clear(State(st): State<AppState>, user: User) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    sqlx::query("DELETE FROM notifications WHERE user_id = ?").bind(user.id).execute(&st.db).await?;
    Ok(Json(json!({ "ok": true })))
}

// ───────────── Notification settings ─────────────

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct KindPrefs {
    in_app: bool,
    email: bool,
}

async fn settings_of(st: &AppState, user_id: i64) -> AppResult<Value> {
    let mut c = st.db.acquire().await?;
    let (email,): (String,) = sqlx::query_as("SELECT email FROM users WHERE id = ?").bind(user_id).fetch_one(&mut *c).await?;
    let rows: Vec<(String, bool, bool)> =
        sqlx::query_as("SELECT kind, in_app, email FROM notification_prefs WHERE user_id = ?").bind(user_id).fetch_all(&mut *c).await?;
    let kinds: serde_json::Map<String, Value> = KINDS
        .iter()
        .map(|k| {
            let p = rows.iter().find(|r| r.0 == *k).map_or(KindPrefs { in_app: true, email: true }, |r| KindPrefs { in_app: r.1, email: r.2 });
            (k.to_string(), json!(p))
        })
        .collect();
    drop(c);
    let email_ready = crate::mail::load(&st.db).await.ready();
    Ok(json!({ "email": email, "email_ready": email_ready, "kinds": kinds }))
}

pub async fn get_settings(State(st): State<AppState>, user: User) -> AppResult<Json<Value>> {
    Ok(Json(settings_of(&st, user.id).await?))
}

#[derive(Deserialize)]
pub struct SettingsReq {
    email: Option<String>,
    #[serde(default)]
    kinds: std::collections::HashMap<String, KindPrefs>,
    /// The current password (and a two-factor code), needed to change the email address
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

pub async fn update_settings(State(st): State<AppState>, user: User, Json(req): Json<SettingsReq>) -> AppResult<Json<Value>> {
    let email = req.email.map(|e| e.trim().to_string());
    if let Some(e) = &email
        && !e.is_empty()
        && !crate::mail::valid_address(e)
    {
        return Err(AppError::bad_request("Enter a valid email address"));
    }
    if req.kinds.keys().any(|k| !KINDS.contains(&k.as_str())) {
        return Err(AppError::bad_request("Unknown kind of notification"));
    }
    let (old, username, chosen_lang, last_lang): (String, String, String, String) =
        sqlx::query_as("SELECT email, username, chosen_lang, lang FROM users WHERE id = ?").bind(user.id).fetch_one(&st.db).await?;
    let lang = crate::i18n::recipient(&chosen_lang, &last_lang, crate::i18n::system_default(&st));
    let email = email.filter(|e| *e != old);
    // Password reset links and every notice go to this address, so changing it asks who it is again, like the
    // password itself
    if email.is_some() {
        crate::tokens::confirm_identity(
            &st,
            &user,
            req.password,
            req.code.as_deref(),
            "Sign out and sign in again, then change your email address within 10 minutes",
        )
        .await?;
    }
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        if let Some(e) = &email {
            sqlx::query("UPDATE users SET email = ? WHERE id = ?").bind(e).bind(user.id).execute(&mut *tx).await?;
        }
        for (kind, p) in &req.kinds {
            sqlx::query(
                "INSERT INTO notification_prefs (user_id, kind, in_app, email) VALUES (?, ?, ?, ?)
                 ON CONFLICT (user_id, kind) DO UPDATE SET in_app = excluded.in_app, email = excluded.email",
            )
            .bind(user.id)
            .bind(kind)
            .bind(p.in_app)
            .bind(p.email)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    if let Some(new) = email.filter(|_| !old.is_empty()) {
        tell_old_address(&st, old, &username, lang, &new);
    }
    Ok(Json(settings_of(&st, user.id).await?))
}

/// Tells the address an account used to have that it was replaced (whatever the notification settings say), so a
/// change the owner didn't make doesn't go unnoticed
fn tell_old_address(st: &AppState, old: String, username: &str, lang: Lang, new: &str) {
    let st = st.clone();
    let (username, new) = (username.to_string(), new.to_string());
    tokio::spawn(async move {
        let cfg = crate::mail::load(&st.db).await;
        if !cfg.ready() {
            return;
        }
        let site = st.part::<crate::branding::Memory>().settings.read().site_name.clone();
        let (subject, body) = email_changed(lang, &site, &username, &new);
        if let Err(e) = crate::mail::send(&cfg, &site, &crate::mail::Message { to: &old, subject: &subject, body: &body }).await {
            tracing::warn!("Couldn't tell {old} that the email address of {username} changed: {e}");
        }
    });
}

fn email_changed(lang: Lang, site: &str, username: &str, new: &str) -> (String, String) {
    let new = if new.is_empty() { tr(lang, Text::EmailChangedNone, &[]) } else { new.to_string() };
    let vars = [("site", site), ("username", username), ("new", &new)];
    (tr(lang, Text::EmailChangedSubject, &vars), tr(lang, Text::EmailChangedBody, &vars))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use axum::extract::{Path, Query, State};

    async fn bell(env: &testutil::TestEnv, user: &User) -> Value {
        list(State(env.st.clone()), user.clone(), HeaderMap::new(), Query(ListQuery { tz: None })).await.unwrap().0
    }

    async fn share(env: &testutil::TestEnv, by: &User, node: &str, req: Value) {
        let req = serde_json::from_value(req).unwrap();
        let _ = crate::drives::grant(State(env.st.clone()), by.clone(), Path(node.to_string()), Json(req)).await.unwrap();
    }

    #[tokio::test]
    async fn sharing_tells_the_people_it_is_shared_with_once() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let cat = env.user("cat", true).await;
        let folder = env.folder(&amy, amy.root(), "Plans").await;
        share(&env, &amy, &folder, json!({ "principal_type": "user", "principal_id": ben.id, "role": "editor" })).await;
        let b = bell(&env, &ben).await;
        assert_eq!(b["unread"], 1);
        let n = &b["items"][0];
        assert_eq!((n["kind"].as_str(), n["data"]["name"].as_str(), n["data"]["by"].as_str()), (Some("shared"), Some("Plans"), Some("amy")));
        assert_eq!((n["data"]["item"].as_str(), n["data"]["role"].as_str(), n["node_id"].as_str()), (Some("folder"), Some("editor"), Some(folder.as_str())));
        assert_eq!(bell(&env, &amy).await["unread"], 0, "not the person who shared");
        // Changing the role of someone who already has access isn't news
        share(&env, &amy, &folder, json!({ "principal_type": "user", "principal_id": ben.id, "role": "viewer" })).await;
        assert_eq!(bell(&env, &ben).await["unread"], 1);

        // A group: each member, except the person sharing, and not someone who turned it off
        sqlx::query("INSERT INTO groups (id, name, created_at) VALUES (1, 'Team', 0)").execute(&env.st.db).await.unwrap();
        for u in [&amy, &ben, &cat] {
            sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES (1, ?)").bind(u.id).execute(&env.st.db).await.unwrap();
        }
        let prefs = json!({ "kinds": { "shared": { "in_app": false, "email": true } } });
        let _ = update_settings(State(env.st.clone()), ben.clone(), Json(serde_json::from_value(prefs).unwrap())).await.unwrap();
        let other = env.folder(&amy, amy.root(), "Budget").await;
        share(&env, &amy, &other, json!({ "principal_type": "group", "principal_id": 1, "role": "viewer" })).await;
        assert_eq!(bell(&env, &cat).await["unread"], 1);
        assert_eq!(bell(&env, &ben).await["unread"], 1, "turned off in the app");
        assert_eq!(bell(&env, &amy).await["unread"], 0);
        // Everyone: nobody is told
        let third = env.folder(&amy, amy.root(), "Open").await;
        share(&env, &amy, &third, json!({ "principal_type": "everyone", "role": "viewer" })).await;
        assert_eq!(bell(&env, &cat).await["unread"], 1);

        // Reading and removing
        let id = bell(&env, &cat).await["items"][0]["id"].as_i64().unwrap();
        let _ = mark_read(State(env.st.clone()), cat.clone(), Json(ReadReq { ids: Some(vec![id]) })).await.unwrap();
        assert_eq!(bell(&env, &cat).await["unread"], 0);
        let _ = mark_read(State(env.st.clone()), ben.clone(), Json(ReadReq { ids: None })).await.unwrap();
        assert_eq!(bell(&env, &ben).await["unread"], 0);
        let _ = delete(State(env.st.clone()), ben.clone(), Path(id)).await.unwrap();
        assert_eq!(bell(&env, &cat).await["items"].as_array().unwrap().len(), 1, "someone else's notification stays");
        let _ = clear(State(env.st.clone()), cat.clone()).await.unwrap();
        assert!(bell(&env, &cat).await["items"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn full_spaces_and_ending_access_are_told_once() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        sqlx::query("UPDATE users SET quota_bytes = 1000 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        sqlx::query("UPDATE drives SET used_bytes = 950 WHERE owner_id = ? AND kind = 'personal'").bind(amy.id).execute(&env.st.db).await.unwrap();
        let folder = env.folder(&amy, amy.root(), "Plans").await;
        let soon = now() + 86400;
        crate::db::add_grant(&mut env.st.db.acquire().await.unwrap(), &folder, "user", ben.id, "viewer", Some(amy.id), Some(soon)).await.unwrap();

        assert_eq!(check(&env.st).await.unwrap(), 2);
        let b = bell(&env, &amy).await;
        assert_eq!((b["items"][0]["kind"].as_str(), b["items"][0]["data"]["percent"].as_i64()), (Some("space_full"), Some(95)));
        let b = bell(&env, &ben).await;
        assert_eq!((b["items"][0]["kind"].as_str(), b["items"][0]["data"]["expires_at"].as_i64()), (Some("access_expiring"), Some(soon)));
        assert_eq!(check(&env.st).await.unwrap(), 0, "not again");

        // Room again, then full again: told again
        sqlx::query("UPDATE drives SET used_bytes = 100 WHERE owner_id = ? AND kind = 'personal'").bind(amy.id).execute(&env.st.db).await.unwrap();
        assert_eq!(check(&env.st).await.unwrap(), 0);
        sqlx::query("UPDATE drives SET used_bytes = 990 WHERE owner_id = ? AND kind = 'personal'").bind(amy.id).execute(&env.st.db).await.unwrap();
        assert_eq!(check(&env.st).await.unwrap(), 1);
        assert_eq!(bell(&env, &amy).await["unread"], 2);

        // Shared with a short expiry: the share already says when it ends
        let cat = env.user("cat", true).await;
        let other = env.folder(&amy, amy.root(), "Budget").await;
        share(&env, &amy, &other, json!({ "principal_type": "user", "principal_id": cat.id, "role": "viewer", "expires_at": soon })).await;
        assert_eq!(check(&env.st).await.unwrap(), 0);
        assert_eq!(bell(&env, &cat).await["items"][0]["data"]["expires_at"].as_i64(), Some(soon));
    }

    #[tokio::test]
    async fn emails_go_to_people_who_want_them_in_their_language() {
        let env = testutil::env().await;
        let (port, mut rx) = crate::mail::tests::fake_server(true).await;
        let mut c = env.st.db.acquire().await.unwrap();
        crate::mail::store(&mut c, &crate::mail::tests::settings(port)).await.unwrap();
        drop(c);
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let cat = env.user("cat", true).await;
        let prefs = |email: &str, by_email: bool| {
            serde_json::from_value(json!({ "email": email, "password": testutil::password(), "kinds": { "shared": { "in_app": true, "email": by_email } } }))
                .unwrap()
        };
        let _ = update_settings(State(env.st.clone()), ben.clone(), Json(prefs("ben@example.com", true))).await.unwrap();
        let _ = update_settings(State(env.st.clone()), cat.clone(), Json(prefs("cat@example.com", false))).await.unwrap();
        assert!(update_settings(State(env.st.clone()), cat.clone(), Json(prefs("not an address", true))).await.is_err());
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::COOKIE, "tf_lang=zh-TW".parse().unwrap());
        let _ = list(State(env.st.clone()), ben.clone(), headers, Query(ListQuery { tz: Some(-480) })).await.unwrap();

        let folder = env.folder(&amy, amy.root(), "Plans").await;
        for u in [&ben, &cat] {
            share(&env, &amy, &folder, json!({ "principal_type": "user", "principal_id": u.id, "role": "editor" })).await;
        }
        let (to, text) = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
        assert_eq!(to, "ben@example.com");
        assert!(text.contains("amy 與你分享了「Plans」"), "{text}");
        assert!(text.contains("編輯者"), "{text}");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(rx.try_recv().is_err(), "cat turned emails off");
        let s = get_settings(State(env.st.clone()), ben.clone()).await.unwrap().0;
        assert_eq!((s["email"].as_str(), s["email_ready"].as_bool()), (Some("ben@example.com"), Some(true)));
    }

    #[tokio::test]
    async fn emails_are_in_the_recipients_language_not_in_the_one_of_whoever_shared() {
        let env = testutil::env().await;
        let (port, mut rx) = crate::mail::tests::fake_server(true).await;
        let mut c = env.st.db.acquire().await.unwrap();
        crate::mail::store(&mut c, &crate::mail::tests::settings(port)).await.unwrap();
        drop(c);
        env.st.system.write().default_lang = "en".into();
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let cat = env.user("cat", true).await;
        for (u, address) in [(&ben, "ben@example.com"), (&cat, "cat@example.com")] {
            let prefs = json!({ "email": address, "password": testutil::password() });
            let _ = update_settings(State(env.st.clone()), u.clone(), Json(serde_json::from_value(prefs).unwrap())).await.unwrap();
        }
        // Amy shares from a page in Traditional Chinese; Ben chose it for himself; Cat only last used it, and the
        // system default comes first
        sqlx::query("UPDATE users SET chosen_lang = 'zh-TW' WHERE id IN (?, ?)").bind(amy.id).bind(ben.id).execute(&env.st.db).await.unwrap();
        sqlx::query("UPDATE users SET lang = 'zh-TW' WHERE id = ?").bind(cat.id).execute(&env.st.db).await.unwrap();
        let folder = env.folder(&amy, amy.root(), "Plans").await;
        for u in [&ben, &cat] {
            share(&env, &amy, &folder, json!({ "principal_type": "user", "principal_id": u.id, "role": "viewer" })).await;
        }
        let mut got = std::collections::HashMap::new();
        for _ in 0..2 {
            let (to, text) = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
            got.insert(to, text);
        }
        let subject = |lang| tr(lang, Text::SharedItemSubject, &[("by", "amy"), ("name", "Plans")]);
        assert!(got["ben@example.com"].contains(&subject(Lang::ZhTw)), "{}", got["ben@example.com"]);
        assert!(got["cat@example.com"].contains(&subject(Lang::En)), "{}", got["cat@example.com"]);
    }

    #[tokio::test]
    async fn repeated_notices_send_a_few_emails_not_one_each() {
        let env = testutil::env().await;
        let (port, mut rx) = crate::mail::tests::fake_server(true).await;
        let mut c = env.st.db.acquire().await.unwrap();
        crate::mail::store(&mut c, &crate::mail::tests::settings(port)).await.unwrap();
        drop(c);
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let prefs = json!({ "email": "ben@example.com", "password": testutil::password(), "kinds": { "shared": { "in_app": true, "email": true } } });
        let _ = update_settings(State(env.st.clone()), ben.clone(), Json(serde_json::from_value(prefs).unwrap())).await.unwrap();
        let prefs = json!({ "email": "amy@example.com", "password": testutil::password(), "kinds": { "link_upload": { "in_app": false, "email": true } } });
        let _ = update_settings(State(env.st.clone()), amy.clone(), Json(serde_json::from_value(prefs).unwrap())).await.unwrap();

        // Access taken away and given again, over and over
        let folder = env.folder(&amy, amy.root(), "Plans").await;
        for _ in 0..20 {
            share(&env, &amy, &folder, json!({ "principal_type": "user", "principal_id": ben.id, "role": "viewer" })).await;
            env.revoke(&folder, &ben).await;
        }
        // Files through a link whose notices are only wanted by email: told once for the hour, like in the app
        let inbox = env.folder(&amy, amy.root(), "Inbox").await;
        let file = crate::tree::get_node(&mut env.st.db.acquire().await.unwrap(), &env.file(&amy, &inbox, "a.txt").await).await.unwrap().unwrap();
        for _ in 0..10 {
            link_upload(&env.st, amy.id, "link1", &file).await.unwrap();
        }
        let mut to = std::collections::HashMap::<String, usize>::new();
        while let Ok(Some((addr, _))) = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            *to.entry(addr).or_default() += 1;
        }
        assert_eq!(to.get("amy@example.com"), Some(&1), "{to:?}");
        let ben_got = to.get("ben@example.com").copied().unwrap_or(0);
        assert!((1..=MAX_EMAILS_PER_KIND).contains(&ben_got), "{to:?}");
    }

    #[test]
    fn email_text() {
        let n = Notice {
            kind: "access_expiring",
            node_id: Some("abc".into()),
            data: json!({ "name": "My files", "item": "space", "drive_kind": "personal", "role": "viewer", "expires_at": 86400 }),
        };
        let (subject, body) = render(&n, Lang::ZhTw, -480, "Drive", "https://drive.example.com");
        assert_eq!(subject, "你對「我的檔案」的存取權即將結束");
        assert!(body.contains("1970-01-02 08:00 (UTC+8)"), "the expiry uses the recipient's time zone");
        assert!(body.contains("https://drive.example.com/files/abc"), "the space link uses the configured site URL");
        let (_, body) = render(&n, Lang::En, 330, "Drive", "");
        assert!(body.contains("1970-01-01 18:30 (UTC-5:30)"), "the expiry uses the recipient's time zone");
        assert!(!body.contains("http"), "no site URL, no link");
        let file = Notice { kind: "shared", node_id: Some("f1".into()), data: json!({ "by": "Amy", "name": "a.txt", "item": "file", "role": "editor" }) };
        let (subject, body) = render(&file, Lang::En, 0, "Drive", "https://d.example");
        assert_eq!(subject, "Amy shared “a.txt” with you");
        assert!(body.contains("https://d.example/view/f1"), "the file link uses the configured site URL");
    }

    #[tokio::test]
    async fn changing_the_email_address_asks_for_the_password_and_tells_the_old_one() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let (port, mut mails) = crate::mail::tests::fake_server(true).await;
        {
            let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
            crate::mail::store(&mut tx, &crate::mail::tests::settings(port)).await.unwrap();
            tx.commit().await.unwrap();
        }
        let set = |v: Value| update_settings(State(env.st.clone()), amy.clone(), Json(serde_json::from_value(v).unwrap()));
        let email = || async { sqlx::query_as::<_, (String,)>("SELECT email FROM users WHERE id = ?").bind(amy.id).fetch_one(&env.st.db).await.unwrap().0 };

        // A session alone can't point the account's emails (and password resets) elsewhere
        assert!(set(json!({ "email": "amy@example.com" })).await.is_err());
        assert!(set(json!({ "email": "amy@example.com", "password": testutil::wrong_password() })).await.is_err());
        assert_eq!(email().await, "");
        assert!(set(json!({ "email": "amy@example.com", "password": testutil::password() })).await.is_ok());
        assert_eq!(email().await, "amy@example.com");
        // What she is told about, and the same address again, need nothing more
        assert!(set(json!({ "email": " amy@example.com ", "kinds": { "shared": { "in_app": true, "email": false } } })).await.is_ok());

        // Changed: the old address is told
        assert!(set(json!({ "email": "amy@elsewhere.example", "password": testutil::password() })).await.is_ok());
        assert_eq!(email().await, "amy@elsewhere.example");
        let (to, text) = tokio::time::timeout(std::time::Duration::from_secs(10), mails.recv()).await.unwrap().unwrap();
        assert_eq!(to, "amy@example.com");
        assert!(text.contains("amy@elsewhere.example"), "{text}");
    }
}
