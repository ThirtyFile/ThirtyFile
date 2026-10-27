//! Control panel › Directory sign-in: the settings, a test that says where it goes wrong, and a sync by hand

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    GroupMode, Lookup, Settings,
    settings::{SEAL, check_plain_target, store},
    sync,
};
use crate::{
    auth::Admin,
    error::{AppError, AppResult},
    ldap::{self, Connection},
    logs,
    state::AppState,
};

/// Directory groups followed at most
const MAX_MAPPINGS: usize = 200;

fn view(s: &Settings, report: Option<sync::Report>) -> Value {
    let mut shown = serde_json::to_value(s).unwrap_or_default();
    shown["bind_password"] = json!("");
    json!({ "settings": shown, "has_bind_password": !s.bind_password.is_empty(), "last_sync": report })
}

pub async fn get(State(st): State<AppState>, _: Admin) -> Json<Value> {
    Json(view(&super::load(&st.db).await, sync::last(&st.db).await))
}

#[derive(Deserialize)]
pub struct SaveReq {
    #[serde(flatten)]
    settings: Settings,
    #[serde(flatten)]
    me: crate::tokens::Confirmation,
}

/// What is typed, tidied: no spaces around the texts
fn tidy(mut s: Settings) -> Settings {
    for text in [
        &mut s.url,
        &mut s.bind_dn,
        &mut s.user_base,
        &mut s.user_filter,
        &mut s.attr_id,
        &mut s.attr_username,
        &mut s.attr_name,
        &mut s.attr_email,
        &mut s.require_group,
        &mut s.group_base,
        &mut s.group_filter,
    ] {
        *text = text.trim().to_string();
    }
    s.ca_pem = s.ca_pem.trim().to_string();
    s.mappings.retain(|m| !m.dn.trim().is_empty());
    for m in &mut s.mappings {
        m.dn = m.dn.trim().to_string();
    }
    s
}

pub async fn save(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<SaveReq>) -> AppResult<Json<Value>> {
    let SaveReq { settings, me } = req;
    // Who may sign in, and as whom, depends on it: whoever changes it says who they are
    me.check(&st, &user).await?;
    let mut new = tidy(settings);
    let current = super::load(&st.db).await;
    // A password left blank is the one saved
    new.bind_password = if new.bind_password.is_empty() { current.bind_password.clone() } else { crate::secrets::seal(SEAL, &new.bind_password) };
    if new.mappings.len() > MAX_MAPPINGS {
        return Err(AppError::bad_request(format!("At most {MAX_MAPPINGS} directory groups can be followed")));
    }
    if new.enabled {
        new.check().map_err(AppError::bad_request)?;
        if new.bind_password.is_empty() {
            return Err(AppError::bad_request("Enter the service account's password"));
        }
        check_plain_target(&new).await?;
    } else if !new.url.is_empty() {
        new.target().map_err(AppError::bad_request)?;
        new.roots().map_err(AppError::bad_request)?;
    }
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let me = crate::auth::require_admin(&mut tx, &user).await?;
    for m in &new.mappings {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM groups WHERE id = ?").bind(m.group_id).fetch_one(&mut *tx).await?;
        if n == 0 {
            return Err(AppError::bad_request("A group chosen for a directory group doesn't exist"));
        }
    }
    store(&mut tx, &new).await?;
    // The groups that follow the directory are marked; those that no longer do are the interface's again
    let ids = serde_json::to_string(&new.mappings.iter().map(|m| m.group_id).collect::<Vec<_>>()).unwrap();
    sqlx::query("UPDATE groups SET source = ? WHERE id IN (SELECT value FROM json_each(?)) AND source <> ?")
        .bind(super::PROVIDER)
        .bind(&ids)
        .bind(super::PROVIDER)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE groups SET source = 'local' WHERE source = ? AND id NOT IN (SELECT value FROM json_each(?))")
        .bind(super::PROVIDER)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    let detail = format!("{} ({})", if new.enabled { "turned on" } else { "turned off" }, new.url);
    logs::record_activity(&mut tx, &me, None, "directory_update", &detail).await?;
    tx.commit().await?;
    Ok(Json(view(&new, sync::last(&st.db).await)))
}

#[derive(Deserialize)]
pub struct TestReq {
    #[serde(flatten)]
    settings: Settings,
    /// A user name to look for, and whose groups to list
    #[serde(default)]
    username: String,
}

#[derive(Serialize)]
struct Step {
    step: &'static str,
    ok: bool,
    detail: String,
}

fn step(step: &'static str, ok: bool, detail: impl Into<String>) -> Step {
    Step { step, ok, detail: detail.into() }
}

/// Tries the settings as they are typed (a blank password is the saved one): connect, bind, find the person, list their
/// groups, and stops at the first thing that goes wrong, with the exact reason
pub async fn test(State(st): State<AppState>, Admin(_): Admin, Json(req): Json<TestReq>) -> AppResult<Json<Value>> {
    let mut s = tidy(req.settings);
    let saved = super::load(&st.db).await;
    let mut steps = Vec::new();
    if s.bind_password.is_empty() {
        // The saved password goes only where it was saved for: to try another server or account, it is typed again
        if s.url != saved.url || s.bind_dn != saved.bind_dn {
            steps.push(step(
                "Settings",
                false,
                "To try another server or service account, enter the service account's password: the saved one is only sent where it was saved for",
            ));
            return Ok(Json(json!({ "steps": steps })));
        }
        s.bind_password = saved.bind_password;
    } else {
        s.bind_password = crate::secrets::seal(SEAL, &s.bind_password);
    }
    run_test(&mut s, req.username.trim(), &mut steps).await;
    Ok(Json(json!({ "steps": steps })))
}

async fn run_test(s: &mut Settings, username: &str, steps: &mut Vec<Step>) {
    if let Err(e) = s.check() {
        steps.push(step("Settings", false, e));
        return;
    }
    if let Err(e) = check_plain_target(s).await {
        steps.push(step("Settings", false, e.message));
        return;
    }
    let (config, password) = match (s.connection(), s.password()) {
        (Ok(c), Ok(p)) => (c, p),
        (Err(e), _) | (_, Err(e)) => {
            steps.push(step("Settings", false, e));
            return;
        }
    };
    let mut conn = match Connection::connect(&config).await {
        Ok(c) => {
            let how = match config.security {
                ldap::Security::Ldaps => "over TLS",
                ldap::Security::StartTls => "with StartTLS",
                ldap::Security::None => "without encryption",
            };
            steps.push(step("Connect", true, format!("Connected to {}:{} {how}", config.host, config.port)));
            c
        }
        Err(e) => {
            steps.push(step("Connect", false, e.to_string()));
            return;
        }
    };
    if let Err(e) = conn.bind(&s.bind_dn, &password).await {
        steps.push(step("Service account", false, e.to_string()));
        return;
    }
    steps.push(step("Service account", true, format!("Signed in as {}", s.bind_dn)));
    if username.is_empty() {
        return;
    }
    let entry = match super::find_user(&mut conn, s, username).await {
        Ok(Lookup::One(e)) => *e,
        Ok(Lookup::Nobody) => {
            steps.push(step("Find the person", false, "No entry answers to this name: the filter found nothing under the search base"));
            return;
        }
        Ok(Lookup::Many) => {
            steps.push(step("Find the person", false, "More than one entry answers to this name: the filter has to find exactly one"));
            return;
        }
        Err(e) => {
            steps.push(step("Find the person", false, e.to_string()));
            return;
        }
    };
    let id = super::stable_id(&entry, &s.attr_id);
    let mut found = vec![format!("DN: {}", entry.dn)];
    for (label, attr) in [("Stable id", &s.attr_id), ("User name", &s.attr_username), ("Name", &s.attr_name), ("Email", &s.attr_email)] {
        found.push(format!(
            "{label} ({attr}): {}",
            if attr == &s.attr_id { id.clone() } else { entry.text(attr) }.unwrap_or_else(|| "— (the entry has none)".into())
        ));
    }
    if !super::entry_enabled(&entry) {
        found.push("The directory has this account turned off: it can't sign in".into());
    }
    steps.push(step("Find the person", id.is_some(), found.join("\n")));
    if s.group_mode == GroupMode::None {
        return;
    }
    match super::memberships(&mut conn, s, &entry).await {
        Ok(groups) => {
            let mut list: Vec<String> = groups.into_iter().collect();
            list.sort();
            let followed = s.mappings.iter().filter(|m| list.contains(&Settings::normalize_dn(&m.dn))).count();
            let required = (!s.require_group.is_empty()).then(|| list.contains(&Settings::normalize_dn(&s.require_group)));
            let mut detail = if list.is_empty() { "The person is in no group".to_string() } else { list.iter().take(50).cloned().collect::<Vec<_>>().join("\n") };
            if let Some(ok) = required {
                detail.push_str(if ok { "\nIn the group that is required" } else { "\nNot in the group that is required: this person can't sign in" });
            }
            detail.push_str(&format!("\n{followed} of these are followed"));
            steps.push(step("Groups", required != Some(false), detail));
        }
        Err(e) => steps.push(step("Groups", false, e.to_string())),
    }
    conn.unbind().await;
}

/// Syncs everyone now, rather than at the next quarter of an hour
pub async fn sync_now(State(st): State<AppState>, Admin(_): Admin) -> AppResult<Json<Value>> {
    if !super::load(&st.db).await.enabled {
        return Err(AppError::bad_request("Turn on directory sign-in first"));
    }
    match sync::run(&st).await {
        Some(report) => Ok(Json(json!(report))),
        None => Err(AppError::conflict("A sync is already under way")),
    }
}
