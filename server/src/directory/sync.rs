//! Keeping accounts and groups in step with the directory: at each sign-in (what that person's groups are) and in the
//! background every 15 minutes (everyone's groups; people who are gone or turned off in the directory have their account
//! turned off and their sessions ended, never deleted).

use std::{collections::HashMap, sync::atomic::Ordering, time::Duration};

use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use super::{GroupMode, PROVIDER, Settings};
use crate::{
    error::AppResult,
    ldap::{Entry, Scope, Search, filter},
    state::AppState,
    util::now,
};

const STATE: &str = "directory_state";
const EVERY: Duration = Duration::from_secs(15 * 60);

/// What the last background run did, shown on the settings page
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Report {
    pub at: i64,
    /// People of the directory who have an account here, and were looked at
    pub checked: usize,
    pub turned_off: usize,
    pub turned_on: usize,
    /// Memberships added or taken away
    pub groups_changed: usize,
    pub error: Option<String>,
}

pub async fn last(db: &sqlx::SqlitePool) -> Option<Report> {
    crate::db::get_setting(db, STATE).await.ok().flatten().and_then(|v| serde_json::from_str(&v).ok())
}

/// The name and email a person has in the directory, kept on their account: a name an administrator changed is not
/// written over unless the directory's changed too
pub(super) async fn profile(conn: &mut SqliteConnection, user_id: i64, name: &str, email: &str) -> AppResult<()> {
    if let Ok(name) = crate::users::validate_display_name(name)
        && !name.is_empty()
    {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ? AND display_name <> ?").bind(name).bind(user_id).bind(name).execute(&mut *conn).await?;
    }
    if crate::mail::valid_address(email) {
        sqlx::query("UPDATE users SET email = ? WHERE id = ? AND email = ''").bind(email).bind(user_id).execute(&mut *conn).await?;
    }
    Ok(())
}

/// Puts the person in the groups of the directory they are in, and takes them out of those they are not (only the
/// groups that follow a directory group): the number of memberships changed
pub(super) async fn follow_groups(conn: &mut SqliteConnection, settings: &Settings, user_id: i64, dns: &std::collections::HashSet<String>) -> AppResult<usize> {
    let mut changed = 0;
    let mut seen = std::collections::HashSet::new();
    for m in &settings.mappings {
        let wanted = dns.contains(&Settings::normalize_dn(&m.dn));
        // Two directory groups into one here: in it if either is
        if !seen.insert(m.group_id) {
            if wanted {
                changed += sqlx::query("INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?, ?)")
                    .bind(m.group_id)
                    .bind(user_id)
                    .execute(&mut *conn)
                    .await?
                    .rows_affected() as usize;
            }
            continue;
        }
        let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM groups WHERE id = ?").bind(m.group_id).fetch_optional(&mut *conn).await?;
        if exists.is_none() {
            continue;
        }
        // The group is the directory's: marked, so the pages don't let its members be changed by hand
        sqlx::query("UPDATE groups SET source = ? WHERE id = ? AND source <> ?").bind(PROVIDER).bind(m.group_id).bind(PROVIDER).execute(&mut *conn).await?;
        let res = if wanted {
            sqlx::query("INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?, ?)").bind(m.group_id).bind(user_id).execute(&mut *conn).await?
        } else {
            sqlx::query("DELETE FROM group_members WHERE group_id = ? AND user_id = ?").bind(m.group_id).bind(user_id).execute(&mut *conn).await?
        };
        changed += res.rows_affected() as usize;
    }
    Ok(changed)
}

/// Everyone linked to the directory, looked at once. `None` when one is already under way.
pub async fn run(st: &AppState) -> Option<Report> {
    if st.part::<super::Memory>().running.swap(true, Ordering::SeqCst) {
        return None;
    }
    /// Lets the next sync run when this one ends, however it ends
    struct Running(AppState);
    impl Drop for Running {
        fn drop(&mut self) {
            self.0.part::<super::Memory>().running.store(false, Ordering::SeqCst);
        }
    }
    let _running = Running(st.clone());
    let mut report = Report { at: now(), ..Default::default() };
    if let Err(why) = run_inner(st, &mut report).await {
        report.error = Some(why);
    }
    if let (Ok(json), Ok(mut conn)) = (serde_json::to_string(&report), st.db.acquire().await) {
        let _ = crate::db::set_setting(&mut conn, STATE, &json).await;
    }
    Some(report)
}

async fn run_inner(st: &AppState, report: &mut Report) -> Result<(), String> {
    let settings = super::load(&st.db).await;
    if !settings.enabled {
        return Ok(());
    }
    settings.check()?;
    let mut conn = super::open(&settings).await.map_err(|e| e.to_string())?;
    // Every person the sign-in filter could find: the filter with `*` where the name goes
    let everyone = filter::parse(&settings.user_filter.replace("{username}", "*")).map_err(|e| format!("The filter that finds a person: {}", e.0))?;
    let found = conn
        .search(&Search {
            base: settings.user_base.clone(),
            scope: Scope::Subtree,
            filter: everyone,
            attributes: super::attributes(&settings),
            size_limit: 0,
            page_size: 500,
            max_entries: 200_000,
        })
        .await
        .map_err(|e| e.to_string())?;
    let entries: HashMap<String, &Entry> = found.entries.iter().filter_map(|e| super::stable_id(e, &settings.attr_id).map(|id| (id, e))).collect();
    let linked: Vec<(String, i64, String, bool, bool)> = sqlx::query_as(
        "SELECT i.subject, u.id, u.username, u.disabled, u.directory_disabled FROM user_identities i JOIN users u ON u.id = i.user_id WHERE i.provider = ?",
    )
    .bind(PROVIDER)
    .fetch_all(&st.db)
    .await
    .map_err(|e| e.to_string())?;
    // A search that finds no one while accounts are linked is a wrong base or a service account that sees nothing:
    // nobody is turned off for it
    let blind = entries.is_empty() && !linked.is_empty();
    if blind {
        report.error = Some("The search found no one, so no account was turned off. Check the search base and what the service account may see.".into());
    }
    for (subject, user_id, username, disabled, by_directory) in linked {
        report.checked += 1;
        let entry = entries.get(&subject).copied().filter(|e| super::entry_enabled(e));
        let groups = match entry {
            Some(e) => super::memberships(&mut conn, &settings, e).await.map_err(|e| e.to_string())?,
            None => Default::default(),
        };
        // Taken out of the group that is required: no longer someone who may sign in, as if gone from the directory
        let entry = entry.filter(|_| settings.require_group.trim().is_empty() || groups.contains(&Settings::normalize_dn(&settings.require_group)));
        match entry {
            Some(entry) => {
                let _w = st.write_lock.lock().await;
                let mut tx = crate::db::begin_write(&st.db).await.map_err(|e| e.to_string())?;
                if disabled && by_directory {
                    sqlx::query("UPDATE users SET disabled = 0, directory_disabled = 0 WHERE id = ?")
                        .bind(user_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.turned_on += 1;
                }
                profile(&mut tx, user_id, &entry.text(&settings.attr_name).unwrap_or_default(), &entry.text(&settings.attr_email).unwrap_or_default())
                    .await
                    .map_err(|e| e.message)?;
                if settings.group_mode != GroupMode::None {
                    report.groups_changed += follow_groups(&mut tx, &settings, user_id, &groups).await.map_err(|e| e.message)?;
                }
                tx.commit().await.map_err(|e| e.to_string())?;
            }
            None if !blind && !disabled => {
                let _w = st.write_lock.lock().await;
                let mut tx = crate::db::begin_write(&st.db).await.map_err(|e| e.to_string())?;
                sqlx::query("UPDATE users SET disabled = 1, directory_disabled = 1 WHERE id = ?")
                    .bind(user_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
                crate::auth::sign_out_everywhere(&mut tx, user_id, None).await.map_err(|e| e.message)?;
                tx.commit().await.map_err(|e| e.to_string())?;
                crate::logs::record_login_via(st, Some(user_id), &username, "disabled", PROVIDER, "directory", &axum::http::HeaderMap::new());
                tracing::info!("Directory sync: the account {username} is gone or turned off in the directory, and is turned off here");
                report.turned_off += 1;
            }
            None => {}
        }
    }
    conn.unbind().await;
    Ok(())
}

/// The background run: every 15 minutes while directory sign-in is on
pub fn spawn(st: AppState) {
    crate::util::supervise("directory", move |_| {
        let st = st.clone();
        async move {
            // After a start, a minute for everything else to settle
            tokio::time::sleep(Duration::from_secs(60)).await;
            loop {
                if super::load(&st.db).await.enabled
                    && let Some(report) = run(&st).await
                    && let Some(why) = report.error
                {
                    tracing::warn!("Directory sync: {why}");
                }
                tokio::time::sleep(EVERY).await;
            }
        }
    });
}
