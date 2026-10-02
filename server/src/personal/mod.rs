//! Personal spaces ("My files").
//!
//! Whether a new user gets one, and on which storage location, follows the policy in Control panel › General
//! (`personal_spaces`, `personal_location`). The new user dialog and single sign-on domain rules can choose
//! otherwise, and an administrator can add one to an existing user later, or remove it: its files then move into
//! another space or are deleted, as when the user is deleted (admin/).
//!
//! A user without a personal space has `users.root_id` NULL: the "root" alias answers 404, and the web interface
//! starts in the first space they can use.
//!
//! When the chosen location's folder isn't available (a disk or share that isn't mounted), creating the user, or
//! their first sign-in with single sign-on, still succeeds: the user waits for their space on that location
//! (`users.personal_pending`), and it is created at their next sign-in, or by the retry that runs every minute, once
//! the folder is back.

mod removal;

pub use removal::*;

use std::{path::Path as FsPath, time::Duration};

use sqlx::SqliteConnection;

use crate::{
    db::{add_grant, create_drive},
    error::{AppError, AppResult},
    state::AppState,
};

/// The name of every personal space (shown in the interface's language)
pub const NAME: &str = "My files";

/// How often personal spaces waiting for their storage location are tried again
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// The location of new personal spaces by the policy: the one chosen in the settings while it exists, else the
/// default location
pub async fn policy_location(st: &AppState, conn: &mut SqliteConnection) -> AppResult<String> {
    let chosen = st.system.read().unwrap().personal_location.clone();
    if !chosen.is_empty() && crate::db::location_exists(conn, &chosen).await? {
        return Ok(chosen);
    }
    Ok(crate::locations::default_location(conn).await?)
}

/// Whether a new user gets a personal space, and on which location: `create` and `location` as the administrator or
/// a domain rule chose them, the policy where they are None. A location that doesn't exist is refused when `strict`
/// (chosen just now), else it gives way to the policy (a domain rule saved before the location was deleted).
/// Returns the location, or None for no personal space.
pub async fn choose(st: &AppState, conn: &mut SqliteConnection, create: Option<bool>, location: Option<&str>, strict: bool) -> AppResult<Option<String>> {
    if !create.unwrap_or_else(|| st.system.read().unwrap().personal_spaces) {
        return Ok(None);
    }
    match location.filter(|l| !l.is_empty()) {
        Some(l) if crate::db::location_exists(conn, l).await? => Ok(Some(l.to_string())),
        Some(_) if strict => Err(AppError::not_found("Storage location not found")),
        _ => Ok(Some(policy_location(st, conn).await?)),
    }
}

/// Before taking the write lock to create a personal space: asks the disk of the location `choose` picks whether its
/// folder is available (`space_folders::check`), so the write lock doesn't wait for a disk that doesn't answer
pub async fn check_ahead(st: &AppState, create: Option<bool>, location: Option<&str>) {
    let Ok(mut c) = st.db.acquire().await else { return };
    let chosen = choose(st, &mut c, create, location, false).await;
    drop(c);
    if let Ok(Some(location)) = chosen {
        crate::space_folders::check(st, &location).await;
    }
}

/// Creates the personal space of user `user_id` on `location`, in the caller's transaction (under the write lock), and
/// returns its root folder. Fails when the location's folder isn't available (space_folders.rs); the caller then
/// rolls back. `space_folders`: see `AppState::space_folders`. The caller calls `folders::spaces_changed` after
/// committing.
pub async fn create(conn: &mut SqliteConnection, space_folders: Option<&FsPath>, user_id: i64, location: &str) -> AppResult<String> {
    let (drive_id, root_id) = create_drive(conn, NAME, "personal", user_id, 0, location).await?;
    crate::space_folders::make_folder_space(conn, space_folders, &drive_id).await?;
    add_grant(conn, &root_id, "user", user_id, "owner", Some(user_id), None).await?;
    sqlx::query("UPDATE users SET root_id = ?, personal_pending = NULL WHERE id = ?").bind(&root_id).bind(user_id).execute(&mut *conn).await?;
    Ok(root_id)
}

/// Like `create` for a new user, except that when the space can't be created the user waits for it (see the module)
/// instead of the whole transaction failing. Returns whether it was created.
pub async fn create_or_wait(conn: &mut SqliteConnection, space_folders: Option<&FsPath>, user_id: i64, username: &str, location: &str) -> AppResult<bool> {
    // A savepoint: a failed attempt leaves nothing behind, and the caller's transaction goes on
    #[allow(clippy::disallowed_methods, reason = "a savepoint in the caller's write transaction (db::begin_write)")]
    let mut attempt = sqlx::Connection::begin(&mut *conn).await?;
    match create(&mut attempt, space_folders, user_id, location).await {
        Ok(_) => {
            attempt.commit().await?;
            Ok(true)
        }
        Err(e) => {
            attempt.rollback().await?;
            tracing::warn!(
                "The personal space of {username} couldn't be created on storage location {location} ({}); it is created once the location is available",
                e.message
            );
            sqlx::query("UPDATE users SET personal_pending = ? WHERE id = ?").bind(location).bind(user_id).execute(&mut *conn).await?;
            Ok(false)
        }
    }
}

/// Tries again to create the personal spaces waiting for their location: every user's, or only `user`'s (when they
/// sign in). Failures are only logged. Returns how many were created.
pub async fn retry_pending(st: &AppState, user: Option<i64>) -> usize {
    let rows: Vec<(i64, String, String)> =
        match sqlx::query_as("SELECT id, username, personal_pending FROM users WHERE personal_pending IS NOT NULL AND (?1 IS NULL OR id = ?1)")
            .bind(user)
            .fetch_all(&st.db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("Couldn't look for personal spaces waiting to be created: {e}");
                return 0;
            }
        };
    let mut created = 0;
    for (id, username, location) in rows {
        // Asked before taking the write lock: a location that is offline, or whose disk doesn't answer, is left for later
        if !crate::space_folders::check(st, &location).await {
            tracing::debug!("The personal space of {username} still can't be created: storage location {location} isn't available");
            continue;
        }
        match retry_one(st, id).await {
            Ok(Some(location)) => {
                created += 1;
                tracing::info!("Created the personal space of {username} on storage location {location}, which is available again");
            }
            Ok(None) => {}
            // Every minute until the location is back: not worth a warning each time
            Err(e) => tracing::debug!("The personal space of {username} still can't be created: {}", e.message),
        }
    }
    if created > 0 {
        crate::folders::spaces_changed(st);
    }
    created
}

/// Creates one waiting personal space; returns the location it was created on, or None when nothing was waiting
async fn retry_one(st: &AppState, id: i64) -> AppResult<Option<String>> {
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = retry_in(st, &mut tx, id).await;
    // A failed attempt may have written already: rolled back before the lock goes (`db::settle`)
    crate::db::settle(tx, res).await
}

async fn retry_in(st: &AppState, tx: &mut SqliteConnection, id: i64) -> AppResult<Option<String>> {
    // Checked again under the lock: another retry or an administrator may have been quicker
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT root_id, personal_pending FROM users WHERE id = ?").bind(id).fetch_optional(&mut *tx).await?;
    let location = match row {
        Some((None, Some(location))) => location,
        Some((Some(_), Some(_))) => {
            sqlx::query("UPDATE users SET personal_pending = NULL WHERE id = ?").bind(id).execute(&mut *tx).await?;
            return Ok(None);
        }
        _ => return Ok(None),
    };
    // A location deleted meanwhile: the default one
    let location = if crate::db::location_exists(tx, &location).await? { location } else { crate::locations::default_location(tx).await? };
    create(tx, st.space_folders.as_deref(), id, &location).await?;
    Ok(Some(location))
}

/// Tries the waiting personal spaces again every minute (a location that is back is used within a minute). First makes
/// personal spaces left read-only by a removal that was interrupted writable again (`release_interrupted`).
pub fn spawn_retry(st: AppState) {
    tokio::spawn(async move {
        release_interrupted(&st).await;
        let mut tick = tokio::time::interval(RETRY_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            retry_pending(&st, None).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use axum::{
        Json,
        extract::{Path, Query, State},
    };

    use super::*;
    use crate::{
        admin::{
            UserRow,
            personal::{AddReq, add, remove},
        },
        auth::Admin,
        testutil,
    };
    use serde_json::{Value, json};

    async fn set_policy(env: &testutil::TestEnv, v: Value) {
        let req = serde_json::from_value(v).unwrap();
        let _ = crate::admin::update_settings(State(env.st.clone()), Admin(env.admin().await), Json(req)).await.unwrap();
    }

    async fn new_user(env: &testutil::TestEnv, name: &str, extra: Value) -> AppResult<UserRow> {
        let mut v = json!({ "username": name, "password": testutil::password() });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let Json(row) = crate::admin::create(State(env.st.clone()), Admin(env.admin().await), Json(serde_json::from_value(v).unwrap())).await?;
        Ok(row)
    }

    /// (root folder, location of the personal space, the location it waits for)
    async fn personal(env: &testutil::TestEnv, id: i64) -> (Option<String>, Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT u.root_id, (SELECT location_id FROM drives WHERE kind = 'personal' AND owner_id = u.id), u.personal_pending FROM users u WHERE u.id = ?",
        )
        .bind(id)
        .fetch_one(&env.st.db)
        .await
        .unwrap()
    }

    /// A Local folder location in a new folder (not the default)
    async fn local_location(env: &testutil::TestEnv, id: &str) -> std::path::PathBuf {
        let dir = env.dir.join(id);
        // Added as an administrator adds it: the folder holds the location's marker
        crate::storage::claim_folder(&dir, id).unwrap();
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, ?, 'local', ?, 0, 0)")
            .bind(id)
            .bind(id.to_uppercase())
            .bind(json!({ "path": dir.to_string_lossy() }).to_string())
            .execute(&env.st.db)
            .await
            .unwrap();
        dir
    }

    #[tokio::test]
    async fn with_the_policy_off_new_users_get_no_personal_space() {
        let env = testutil::folders_env().await;
        set_policy(&env, json!({ "personal_spaces": false })).await;
        assert!(!crate::settings::load_system_settings(&env.st.db).await.unwrap().personal_spaces, "the setting is saved");
        let amy = new_user(&env, "amy", json!({})).await.unwrap();
        assert!(!amy.personal_space);
        assert_eq!(personal(&env, amy.id).await, (None, None, None));
        assert!(!env.dir.join("blobs/users/amy").exists());
        // The administrator can still choose to give one
        let ben = new_user(&env, "ben", json!({ "personal_space": true })).await.unwrap();
        assert!(ben.personal_space);
        assert_eq!(personal(&env, ben.id).await.1.as_deref(), Some(crate::locations::BUILTIN));
    }

    #[tokio::test]
    async fn a_personal_space_goes_on_the_location_chosen() {
        let env = testutil::folders_env().await;
        let nas = local_location(&env, "nas").await;
        // Chosen when creating the user
        let amy = new_user(&env, "amy", json!({ "personal_location": "nas" })).await.unwrap();
        assert_eq!(personal(&env, amy.id).await.1.as_deref(), Some("nas"));
        assert!(nas.join("users/amy").is_dir());
        // By the policy
        set_policy(&env, json!({ "personal_location": "nas" })).await;
        let ben = new_user(&env, "ben", json!({})).await.unwrap();
        assert_eq!(personal(&env, ben.id).await.1.as_deref(), Some("nas"));
        // A location that doesn't exist is refused, in the settings and when creating a user
        let bad = serde_json::from_value(json!({ "personal_location": "nope" })).unwrap();
        assert!(crate::admin::update_settings(State(env.st.clone()), Admin(env.admin().await), Json(bad)).await.is_err());
        let err = new_user(&env, "cat", json!({ "personal_location": "nope" })).await.map(|_| ()).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
        // The policy's location deleted since: the default location
        sqlx::query("DELETE FROM drives WHERE location_id = 'nas'").execute(&env.st.db).await.unwrap();
        sqlx::query("UPDATE users SET root_id = NULL").execute(&env.st.db).await.unwrap();
        sqlx::query("DELETE FROM storage_locations WHERE id = 'nas'").execute(&env.st.db).await.unwrap();
        let dan = new_user(&env, "dan", json!({})).await.unwrap();
        assert_eq!(personal(&env, dan.id).await.1.as_deref(), Some(crate::locations::BUILTIN));
    }

    #[tokio::test]
    async fn the_root_alias_answers_clearly_without_a_personal_space() {
        let env = testutil::env().await;
        set_policy(&env, json!({ "personal_spaces": false })).await;
        let row = new_user(&env, "amy", json!({})).await.unwrap();
        let mut c = env.st.db.acquire().await.unwrap();
        let amy = crate::auth::user_by_id(&env.st, &mut c, row.id).await.unwrap().unwrap();
        drop(c);
        assert_eq!(amy.root_id, None);
        let err = crate::nodes::get(State(env.st.clone()), amy.clone(), Path("root".into())).await.map(|_| ()).unwrap_err();
        assert_eq!((err.status, err.message.as_str()), (axum::http::StatusCode::NOT_FOUND, "You don't have a personal space"));
        // The page says so, and she still reaches the company space; her usage is 0
        let Json(me) = crate::signin::me(State(env.st.clone()), amy.clone()).await.unwrap();
        let me = serde_json::to_value(&me).unwrap();
        assert_eq!((me["root_id"].clone(), me["personal_pending"].clone(), me["used_bytes"].clone()), (Value::Null, json!(false), json!(0)));
        let Json(drives) = crate::drives::list(State(env.st.clone()), amy.clone()).await.unwrap();
        let kinds: Vec<String> = serde_json::to_value(&drives).unwrap().as_array().unwrap().iter().map(|d| d["kind"].as_str().unwrap().to_string()).collect();
        assert_eq!(kinds, ["company"]);
        // An upload without a folder (which means "root") is refused the same way
        let mut h = axum::http::HeaderMap::new();
        h.insert("upload-length", "1".parse().unwrap());
        h.insert("upload-metadata", "filename YS50eHQ=".parse().unwrap());
        let err = crate::upload::create(State(env.st.clone()), amy, h).await.map(|_| ()).unwrap_err();
        assert_eq!((err.status, err.message.as_str()), (axum::http::StatusCode::NOT_FOUND, "You don't have a personal space"));
    }

    #[tokio::test]
    async fn an_administrator_adds_and_removes_a_personal_space() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let nas = local_location(&env, "nas").await;
        set_policy(&env, json!({ "personal_spaces": false })).await;
        let row = new_user(&env, "amy", json!({})).await.unwrap();
        let add = |loc: Option<&str>| {
            let (st, admin) = (env.st.clone(), admin.clone());
            let req = AddReq { location_id: loc.map(str::to_string) };
            async move { add(State(st), Admin(admin), Path(row.id), Json(req)).await }
        };
        let Json(after) = add(Some("nas")).await.unwrap();
        assert!(after.personal_space);
        assert!(nas.join("users/amy").is_dir());
        // Only one
        assert_eq!(add(None).await.map(|_| ()).unwrap_err().status, axum::http::StatusCode::CONFLICT);

        // Her files, moved into the company space when it is removed
        let mut c = env.st.db.acquire().await.unwrap();
        let amy = crate::auth::user_by_id(&env.st, &mut c, row.id).await.unwrap().unwrap();
        drop(c);
        let a = env.upload(&amy, amy.root(), "a.txt", b"amy's").await;
        let company = env.drive_of(&env.st.shared_root().unwrap()).await;
        let remove = |q: Value| {
            let (st, admin) = (env.st.clone(), admin.clone());
            async move {
                let Json(job) = remove(State(st.clone()), Admin(admin), Path(row.id), Query(serde_json::from_value(q).unwrap())).await?;
                assert_eq!(job.state, "done");
                crate::admin::get_row(&st, row.id).await.map(Json)
            }
        };
        // A choice is needed while it holds files
        assert_eq!(remove(json!({})).await.map(|_| ()).unwrap_err().status, axum::http::StatusCode::BAD_REQUEST);
        let Json(after) = remove(json!({ "move_to": company })).await.unwrap();
        assert!(!after.personal_space && after.personal_location.is_none());
        assert_eq!(personal(&env, row.id).await, (None, None, None));
        assert_eq!(std::fs::read(env.dir.join("blobs/company/Files of amy/a.txt")).unwrap(), b"amy's");
        assert_eq!(env.node_at(&company, "Files of amy/a.txt").await.unwrap().0, a);
        let (detail,): (String,) =
            sqlx::query_as("SELECT detail FROM activity WHERE action = 'user_update' ORDER BY id DESC LIMIT 1").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(detail, "amy: removed My files, files moved to All files › Files of amy");
        // Nothing left to remove
        assert_eq!(remove(json!({ "delete_files": true })).await.map(|_| ()).unwrap_err().status, axum::http::StatusCode::BAD_REQUEST);

        // Added again (by the policy's location, the built-in one), then removed with its files
        let _ = add(None).await.unwrap();
        let mut c = env.st.db.acquire().await.unwrap();
        let amy = crate::auth::user_by_id(&env.st, &mut c, row.id).await.unwrap().unwrap();
        drop(c);
        assert_eq!(personal(&env, row.id).await.1.as_deref(), Some(crate::locations::BUILTIN));
        let b = env.upload(&amy, amy.root(), "b.txt", b"gone").await;
        let Json(after) = remove(json!({ "delete_files": true })).await.unwrap();
        assert!(!after.personal_space);
        let mut gone = false;
        for _ in 0..200 {
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE id = ?").bind(&b).fetch_one(&env.st.db).await.unwrap();
            gone = n == 0;
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(gone, "the files were deleted with the space");
        // The user signs in without it
        let (session, _) = env.sign_in(&amy, "test").await;
        assert_eq!(session.root_id, None);
    }

    #[tokio::test]
    async fn a_personal_space_waits_for_a_missing_folder_and_is_created_later() {
        let env = testutil::folders_env().await;
        let nas = local_location(&env, "nas").await;
        // Not mounted: the folder (with its marker) is elsewhere for now
        let away = env.dir.join("nas-away");
        std::fs::rename(&nas, &away).unwrap();
        // Creating the user still works
        let row = new_user(&env, "amy", json!({ "personal_location": "nas" })).await.unwrap();
        assert!(!row.personal_space);
        assert_eq!(row.personal_pending.as_deref(), Some("nas"));
        assert_eq!(personal(&env, row.id).await, (None, None, Some("nas".into())));
        assert!(!nas.exists(), "nothing created below the missing folder");
        // Still missing: nothing happens
        assert_eq!(retry_pending(&env.st, None).await, 0);
        let mut c = env.st.db.acquire().await.unwrap();
        let amy = crate::auth::user_by_id(&env.st, &mut c, row.id).await.unwrap().unwrap();
        drop(c);
        let Json(me) = crate::signin::me(State(env.st.clone()), amy.clone()).await.unwrap();
        assert!(me.personal_pending);
        // An empty mount point isn't the location's folder either
        std::fs::create_dir(&nas).unwrap();
        assert_eq!(retry_pending(&env.st, None).await, 0);
        assert_eq!(std::fs::read_dir(&nas).unwrap().count(), 0, "nothing created in the empty mount point");
        std::fs::remove_dir(&nas).unwrap();
        // Back: signing in creates it
        std::fs::rename(&away, &nas).unwrap();
        let (session, _) = env.sign_in(&amy, "test").await;
        assert!(session.root_id.is_some());
        assert_eq!(personal(&env, row.id).await.1.as_deref(), Some("nas"));
        assert!(nas.join("users/amy").is_dir());

        // The periodic retry does too; a location deleted meanwhile gives way to the default one
        let ben = new_user(&env, "ben", json!({ "personal_location": "nas" })).await.unwrap();
        assert!(ben.personal_space);
        sqlx::query("UPDATE users SET root_id = NULL, personal_pending = 'gone' WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        sqlx::query("DELETE FROM drives WHERE kind = 'personal' AND owner_id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        assert_eq!(retry_pending(&env.st, None).await, 1);
        let (root, location, pending) = personal(&env, ben.id).await;
        assert!(root.is_some());
        assert_eq!((location.as_deref(), pending), (Some(crate::locations::BUILTIN), None));

        // An administrator can cancel the wait
        std::fs::rename(&nas, &away).unwrap();
        let cat = new_user(&env, "cat", json!({ "personal_location": "nas" })).await.unwrap();
        assert_eq!(cat.personal_pending.as_deref(), Some("nas"));
        let q = Query(serde_json::from_value(json!({})).unwrap());
        let _ = remove(State(env.st.clone()), Admin(env.admin().await), Path(cat.id), q).await.unwrap();
        let after = crate::admin::get_row(&env.st, cat.id).await.unwrap();
        assert_eq!((after.personal_space, after.personal_pending), (false, None));
    }
}
