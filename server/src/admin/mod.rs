//! Administration: user accounts, roles, permissions and quotas.

pub mod personal;
mod settings;

pub use settings::*;

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};

use crate::{
    auth::{Admin, UserRole, hash_password, min_password, validate_password},
    db::{add_grant, set_setting},
    error::{AppError, AppResult},
    logs,
    personal::{DeleteQuery, Moved, Removed, move_personal_first, remove_personal_in},
    state::AppState,
    users::{NewUser, create_user, validate_display_name, validate_username},
};

#[derive(Serialize, sqlx::FromRow)]
pub struct UserRow {
    pub id: i64,
    pub username: String,
    display_name: String,
    role: UserRole,
    can_write: bool,
    can_delete: bool,
    can_share: bool,
    quota_bytes: i64,
    disabled: bool,
    created_at: i64,
    last_login_at: Option<i64>,
    /// Linked third-party sign-ins (comma-separated)
    sso: String,
    /// Two-factor sign-in is set up
    two_factor: bool,
    /// 'password' (created by an administrator) or the provider that created the account automatically
    source: String,
    /// Email of the most recently used linked sign-in (to spot accounts whose username no longer matches)
    sso_email: String,
    used_bytes: i64,
    /// The user has a personal space ("My files")
    pub personal_space: bool,
    /// The storage location of their personal space (None without one)
    pub personal_location: Option<String>,
    /// The storage location their personal space is waiting for, when it couldn't be created yet (personal.rs)
    pub personal_pending: Option<String>,
    /// The folder their files go into when their personal space is removed and the files are kept ("Files of amy", in
    /// the system default language); a number is added when the name is taken
    #[sqlx(skip)]
    files_folder: String,
}

impl UserRow {
    fn named(mut self, lang: crate::i18n::Lang) -> UserRow {
        self.files_folder = crate::i18n::tr(lang, crate::i18n::Text::FilesOf, &[("username", &self.username)]);
        self
    }
}

const USER_ROW_SQL: &str =
    "SELECT u.id, u.username, u.display_name, u.role, u.can_write, u.can_delete, u.can_share, u.quota_bytes, u.disabled, u.created_at, u.last_login_at, u.source,
       (SELECT COALESCE(GROUP_CONCAT(provider), '') FROM user_identities WHERE user_id = u.id) AS sso,
       u.totp_secret IS NOT NULL AS two_factor,
       (SELECT COALESCE(email, '') FROM user_identities WHERE user_id = u.id ORDER BY last_login_at DESC LIMIT 1) AS sso_email,
       (SELECT COALESCE(SUM(used_bytes), 0) FROM drives WHERE kind = 'personal' AND owner_id = u.id) AS used_bytes,
       u.root_id IS NOT NULL AS personal_space,
       (SELECT location_id FROM drives WHERE kind = 'personal' AND owner_id = u.id) AS personal_location,
       u.personal_pending
     FROM users u";

#[derive(Deserialize)]
pub struct ListQuery {
    /// Paging: only accounts with an id above this one
    after: Option<i64>,
    /// Page size; without it every account is returned (the group editor picks members from all of them)
    limit: Option<i64>,
    /// Only accounts whose username, display name or an email address (their own, or a linked sign-in's) contains this
    /// (letter case ignored)
    #[serde(default)]
    q: Option<String>,
}

pub async fn list(State(st): State<AppState>, _: Admin, Query(q): Query<ListQuery>) -> AppResult<Json<Vec<UserRow>>> {
    let sql = format!(
        r"{USER_ROW_SQL} WHERE u.id > ?1 AND (?3 IS NULL OR u.username LIKE ?3 ESCAPE '\' OR u.display_name LIKE ?3 ESCAPE '\' OR u.email LIKE ?3 ESCAPE '\'
           OR EXISTS (SELECT 1 FROM user_identities WHERE user_id = u.id AND email LIKE ?3 ESCAPE '\'))
         ORDER BY u.id LIMIT ?2"
    );
    let limit = q.limit.map_or(-1, |l| l.clamp(1, 1000));
    let term = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("%{}%", crate::util::like_escape(s)));
    let query = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(q.after.unwrap_or(0)).bind(limit).bind(term);
    let lang = crate::i18n::names(&st);
    Ok(Json(query.fetch_all(&st.db).await?.into_iter().map(|r: UserRow| r.named(lang)).collect()))
}

#[derive(Deserialize)]
pub struct CreateReq {
    username: String,
    password: String,
    #[serde(default)]
    display_name: String,
    #[serde(default = "default_role")]
    role: String,
    #[serde(default = "yes")]
    can_write: bool,
    #[serde(default = "yes")]
    can_delete: bool,
    #[serde(default = "yes")]
    can_share: bool,
    /// When omitted, the system setting "default space size for new users" is used
    #[serde(default)]
    quota_bytes: Option<i64>,
    /// Give the user a personal space ("My files"); when omitted, the system setting decides
    #[serde(default)]
    personal_space: Option<bool>,
    /// The storage location of their personal space; when omitted, the system setting's
    #[serde(default)]
    personal_location: Option<String>,
}

fn default_role() -> String {
    "user".into()
}
fn yes() -> bool {
    true
}

fn role_label(role: UserRole) -> &'static str {
    match role {
        UserRole::Admin => "administrator",
        UserRole::User => "standard user",
    }
}

pub async fn get_row(st: &AppState, id: i64) -> AppResult<UserRow> {
    let sql = format!("{USER_ROW_SQL} WHERE u.id = ?");
    let row: Option<UserRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(id).fetch_optional(&st.db).await?;
    Ok(row.ok_or_else(|| AppError::not_found("User not found"))?.named(crate::i18n::names(st)))
}

pub async fn create(State(st): State<AppState>, Admin(me): Admin, Json(req): Json<CreateReq>) -> AppResult<Json<UserRow>> {
    let username = req.username.trim();
    validate_username(username)?;
    validate_password(&req.password, min_password(&st))?;
    let role = UserRole::parse(&req.role)?;
    let display_name = validate_display_name(&req.display_name)?.to_string();
    let password_hash = hash_password(req.password.clone()).await?;
    crate::personal::check_ahead(&st, req.personal_space, req.personal_location.as_deref()).await;
    let id = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let personal = crate::personal::choose(&st, &mut tx, req.personal_space, req.personal_location.as_deref(), true).await?;
        let id = create_user(
            &mut tx,
            NewUser {
                username,
                password_hash: &password_hash,
                role,
                can_write: req.can_write,
                can_delete: req.can_delete,
                can_share: req.can_share,
                quota_bytes: req.quota_bytes.unwrap_or_else(|| st.system.read().unwrap().default_user_quota).max(0),
                source: "password",
                provisioned_by: None,
                personal_space: personal.as_deref(),
                space_folders: st.space_folders.as_deref(),
            },
        )
        .await
        .map_err(|e| if e.status == axum::http::StatusCode::CONFLICT { AppError::conflict("Username already exists") } else { e })?;
        if !display_name.is_empty() {
            sqlx::query("UPDATE users SET display_name = ? WHERE id = ?").bind(&display_name).bind(id).execute(&mut *tx).await?;
        }
        // The first password is the administrator's: the person chooses their own when signing in
        sqlx::query("UPDATE users SET must_change_password = 1 WHERE id = ?").bind(id).execute(&mut *tx).await?;
        logs::record_activity(&mut tx, &me, None, "user_create", &format!("{username} ({})", role_label(role))).await?;
        tx.commit().await?;
        crate::folders::spaces_changed(&st);
        id
    };
    Ok(Json(get_row(&st, id).await?))
}

#[derive(Deserialize)]
pub struct UpdateReq {
    password: Option<String>,
    display_name: Option<String>,
    role: Option<String>,
    can_write: Option<bool>,
    can_delete: Option<bool>,
    can_share: Option<bool>,
    quota_bytes: Option<i64>,
    disabled: Option<bool>,
}

pub async fn update(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<i64>, Json(req): Json<UpdateReq>) -> AppResult<Json<UserRow>> {
    let role = req.role.as_deref().map(UserRole::parse).transpose()?;
    if id == me.id && (role.is_some_and(|r| r != UserRole::Admin) || req.disabled == Some(true)) {
        return Err(AppError::bad_request("You can't disable your own account or remove your own administrator rights"));
    }
    let password_hash = match &req.password {
        Some(p) => {
            validate_password(p, min_password(&st))?;
            Some(hash_password(p.clone()).await?)
        }
        None => None,
    };
    let target = get_row(&st, id).await?;
    let display_name = req.display_name.as_deref().map(validate_display_name).transpose()?.map(str::to_string);
    let mut changes = Vec::new();
    if password_hash.is_some() {
        changes.push("reset password".to_string());
    }
    if let Some(n) = display_name.as_deref().filter(|n| *n != target.display_name) {
        changes.push(if n.is_empty() { "cleared display name".to_string() } else { format!("display name {n}") });
    }
    if let Some(r) = role.filter(|r| *r != target.role) {
        changes.push(
            match r {
                UserRole::Admin => "made administrator",
                UserRole::User => "changed to standard user",
            }
            .to_string(),
        );
    }
    for (label, v, old) in [("edit", req.can_write, target.can_write), ("delete", req.can_delete, target.can_delete), ("share", req.can_share, target.can_share)] {
        if let Some(v) = v.filter(|v| *v != old) {
            changes.push(format!("{} {label} permission", if v { "granted" } else { "removed" }));
        }
    }
    if let Some(q) = req.quota_bytes.filter(|q| *q != target.quota_bytes) {
        changes.push(format!("space size {}", if q <= 0 { "unlimited".to_string() } else { crate::util::format_bytes(q) }));
    }
    if let Some(d) = req.disabled.filter(|d| *d != target.disabled) {
        changes.push(if d { "disabled account" } else { "enabled account" }.to_string());
    }
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        still_admin(&mut tx, me.id).await?;
        if !changes.is_empty() {
            logs::record_activity(&mut tx, &me, None, "user_update", &format!("{}: {}", target.username, changes.join(", "))).await?;
        }
        sqlx::query(
            "UPDATE users SET
               password_hash = COALESCE(?, password_hash),
               display_name = COALESCE(?, display_name),
               role = COALESCE(?, role),
               can_write = COALESCE(?, can_write),
               can_delete = COALESCE(?, can_delete),
               can_share = COALESCE(?, can_share),
               quota_bytes = COALESCE(?, quota_bytes),
               disabled = COALESCE(?, disabled)
             WHERE id = ?",
        )
        .bind(&password_hash)
        .bind(&display_name)
        .bind(role)
        .bind(req.can_write)
        .bind(req.can_delete)
        .bind(req.can_share)
        .bind(req.quota_bytes.map(|q| q.max(0)))
        .bind(req.disabled)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        // When the password is reset, sign the user out on all devices and remove their app passwords; a disabled
        // account is signed out (its app passwords stop working while it is disabled)
        if password_hash.is_some() {
            crate::auth::sign_out_everywhere(&mut tx, id, None).await?;
            // So do linked single sign-on accounts, which sign in without the password: whoever may have
            // linked one while in control of the account is locked out too (the person links theirs again)
            sqlx::query("DELETE FROM user_identities WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
            sqlx::query("UPDATE users SET must_change_password = 1 WHERE id = ?").bind(id).execute(&mut *tx).await?;
        } else if req.disabled == Some(true) {
            sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    Ok(Json(get_row(&st, id).await?))
}

/// Deletes a user. Their personal space is moved to another space (`move_to`) or permanently deleted
/// (`delete_files`); files they uploaded to other spaces and team spaces they own are transferred to the administrator
/// performing the deletion; their share links are deleted. A personal space that is a folder space leaves its folder
/// on the disk (space_folders.rs): after a move it holds only its trash and earlier versions; "deleted" files are
/// removed from ThirtyFile but stay in the folder, for the administrator to remove or keep.
///
/// Moving the files to or from a folder space copies them, which can take long: the removal runs as a job (jobs.rs),
/// which the page follows.
pub async fn delete(State(st): State<AppState>, Admin(me): Admin, Path(id): Path<i64>, Query(q): Query<DeleteQuery>) -> AppResult<Json<crate::jobs::Job>> {
    if id == me.id {
        return Err(AppError::bad_request("You can't delete your own account"));
    }
    let row = get_row(&st, id).await?;
    let pending = crate::jobs::reserve(&st, me.id, "delete_user", crate::jobs::Limit::Changes)?;
    let job = pending.run(crate::jobs::wait(), move |t| async move { delete_user(&st, &me, id, &row.username, &q, &t).await.map(|()| Default::default()) }).await?;
    Ok(Json(job))
}

async fn delete_user(st: &AppState, me: &crate::auth::User, id: i64, username: &str, q: &DeleteQuery, progress: &crate::jobs::Tracker) -> AppResult<()> {
    let moved = move_personal_first(st, me, id, username, q, progress).await?;
    let res = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = delete_in(&mut tx, me, id, username, q, moved.as_ref()).await;
        // Moving the files can fail after writing (the target space is full): rolled back before the lock goes
        crate::db::settle(tx, res).await
    };
    let (removed, uploads) = Moved::kept_on_error(moved.as_ref(), st, res).await?;
    if let Some(removed) = removed {
        removed.finish(st).await;
    }
    for (u,) in uploads {
        let _ = tokio::fs::remove_file(st.tmp_dir().join(format!("upload-{u}"))).await;
    }
    Ok(())
}

/// Refuses unless the account making a change is still an enabled administrator, read inside the change's transaction.
/// Two administrators demoting, disabling or deleting each other at the same moment were both allowed when signing in
/// was all that was checked, leaving no administrator; now the second change finds it no longer may.
async fn still_admin(tx: &mut sqlx::SqliteConnection, me: i64) -> AppResult<()> {
    let row: Option<(UserRole, bool)> = sqlx::query_as("SELECT role, disabled FROM users WHERE id = ?").bind(me).fetch_optional(&mut *tx).await?;
    if row != Some((UserRole::Admin, false)) {
        return Err(AppError::forbidden("Administrator permission required"));
    }
    Ok(())
}

/// Deletes the user in the transaction: returns their removed personal space and the uploads that were cancelled
async fn delete_in(
    tx: &mut sqlx::SqliteConnection,
    me: &crate::auth::User,
    id: i64,
    username: &str,
    q: &DeleteQuery,
    moved: Option<&Moved>,
) -> AppResult<(Option<Removed>, Vec<(String,)>)> {
    still_admin(tx, me.id).await?;
    let removed = remove_personal_in(tx, me, id, username, q, moved).await?;
    let detail = match removed.as_ref().and_then(|r| r.detail.as_deref()) {
        Some(d) => format!("{username}: {d}"),
        None => username.to_string(),
    };
    let uploads: Vec<(String,)> = sqlx::query_as("SELECT id FROM uploads WHERE owner_id = ?").bind(id).fetch_all(&mut *tx).await?;
    sqlx::query("DELETE FROM uploads WHERE owner_id = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM shares WHERE owner_id = ?").bind(id).execute(&mut *tx).await?;
    // Transfer team space ownership to the administrator
    let owned: Vec<(String,)> =
        sqlx::query_as("SELECT node_id FROM grants WHERE principal_type = 'user' AND principal_id = ? AND role = 'owner'").bind(id).fetch_all(&mut *tx).await?;
    for (node_id,) in owned {
        add_grant(tx, &node_id, "user", me.id, "owner", Some(me.id), None).await?;
    }
    sqlx::query("DELETE FROM grants WHERE principal_type = 'user' AND principal_id = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE drives SET owner_id = ? WHERE owner_id = ?").bind(me.id).bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE nodes SET owner_id = ? WHERE owner_id = ?").bind(me.id).bind(id).execute(&mut *tx).await?;
    // The trash shows "—" for items deleted by a removed account (a later account could get the same id)
    sqlx::query("UPDATE nodes SET trashed_by = NULL WHERE trashed_by = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM users WHERE id = ?").bind(id).execute(&mut *tx).await?;
    logs::record_activity(tx, me, None, "user_delete", &detail).await?;
    Ok((removed, uploads))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::Admin, personal::release_interrupted, testutil, tree};
    use serde_json::json;

    #[tokio::test]
    async fn the_user_list_comes_in_pages() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        for name in ["amy", "ben", "cat"] {
            env.user(name, false).await;
        }
        let page = |after, limit| {
            let (st, admin) = (env.st.clone(), admin.clone());
            async move {
                let Json(rows) = list(State(st), Admin(admin), Query(ListQuery { after, limit, q: None })).await.unwrap();
                rows.into_iter().map(|r| r.id).collect::<Vec<_>>()
            }
        };
        let all = page(None, None).await;
        assert_eq!(all.len(), 4);
        let first = page(None, Some(2)).await;
        assert_eq!(first, all[..2]);
        assert_eq!(page(Some(first[1]), Some(2)).await, all[2..]);
    }

    #[tokio::test]
    async fn storage_used_counts_folder_spaces_and_the_content_store() {
        let env = testutil::folders_env().await;
        let admin = env.admin().await;
        let stored = || async { system_info(&env.st).await.unwrap().stats.stored_bytes };
        assert_eq!(stored().await, 0);
        // A folder space (the default of a new installation): its files are on the disk as they are
        let (company,): (String,) = sqlx::query_as("SELECT root_id FROM drives WHERE kind = 'company'").fetch_one(&env.st.db).await.unwrap();
        env.upload(&admin, &company, "a.txt", b"12345").await;
        assert_eq!(stored().await, 5);
        // Content store: identical content is stored once
        let store = testutil::env().await;
        let admin = store.admin().await;
        let root = admin.root_id.clone().unwrap();
        store.stored_file(&admin, &root, "b.txt", b"abc").await;
        store.stored_file(&admin, &root, "c.txt", b"abc").await;
        assert_eq!(system_info(&store.st).await.unwrap().stats.stored_bytes, 3);
    }

    #[tokio::test]
    async fn the_user_list_can_be_searched() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        for name in ["amy", "ben", "cat_1", "cat21"] {
            env.user(name, false).await;
        }
        sqlx::query("UPDATE users SET display_name = 'Benjamin Smith' WHERE username = 'ben'").execute(&env.st.db).await.unwrap();
        let find = |q: &str| {
            let (st, admin, q) = (env.st.clone(), admin.clone(), Some(q.to_string()));
            async move {
                let Json(rows) = list(State(st), Admin(admin), Query(ListQuery { after: None, limit: Some(200), q })).await.unwrap();
                rows.into_iter().map(|r| r.username).collect::<Vec<_>>()
            }
        };
        assert_eq!(find("AMY").await, ["amy"]);
        assert_eq!(find("smith").await, ["ben"]);
        // _ is a letter here, not "any character"
        assert_eq!(find("cat_").await, ["cat_1"]);
        assert_eq!(find("  ").await.len(), 5);
    }

    fn req(name: &str, quota: Option<i64>) -> Json<CreateReq> {
        Json(CreateReq {
            username: name.into(),
            password: testutil::password().into(),
            display_name: String::new(),
            role: "user".into(),
            can_write: true,
            can_delete: true,
            can_share: true,
            quota_bytes: quota,
            personal_space: None,
            personal_location: None,
        })
    }

    #[tokio::test]
    async fn a_new_account_never_gets_the_id_of_a_deleted_one() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let Json(first) = create(State(env.st.clone()), Admin(admin.clone()), req("first", None)).await.unwrap();
        // The newest account is deleted: its id would be handed out again without the counter
        let _ = delete(State(env.st.clone()), Admin(admin.clone()), Path(first.id), Query(DeleteQuery::default())).await.unwrap();
        let Json(next) = create(State(env.st.clone()), Admin(admin.clone()), req("next", None)).await.unwrap();
        assert!(next.id > first.id, "the deleted account's id was given out again");
    }

    #[tokio::test]
    async fn two_administrators_removing_each_other_leave_one() {
        let env = testutil::env().await;
        let first = env.admin().await;
        let Json(row) = create(State(env.st.clone()), Admin(first.clone()), Json(CreateReq { role: "admin".into(), ..req("second", None).0 })).await.unwrap();
        let second = crate::auth::user_by_id(&env.st, &mut env.st.db.acquire().await.unwrap(), row.id).await.unwrap().unwrap();
        let demote = || Json(serde_json::from_value::<UpdateReq>(json!({ "role": "user" })).unwrap());
        // Both signed in as administrators; the first one's change lands first
        let _ = update(State(env.st.clone()), Admin(first.clone()), Path(second.id), demote()).await.unwrap();
        let res = update(State(env.st.clone()), Admin(second.clone()), Path(first.id), demote()).await;
        assert!(matches!(res, Err(e) if e.status == axum::http::StatusCode::FORBIDDEN));
        let disable = Json(serde_json::from_value::<UpdateReq>(json!({ "disabled": true })).unwrap());
        assert!(update(State(env.st.clone()), Admin(second.clone()), Path(first.id), disable).await.is_err());
        assert!(delete(State(env.st.clone()), Admin(second.clone()), Path(first.id), Query(DeleteQuery::default())).await.is_err());
        let (admins,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(admins, 1);
    }

    #[tokio::test]
    async fn new_users_get_the_default_quota() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let gb = 1024 * 1024 * 1024;
        let settings = SettingsReq { default_user_quota: Some(10 * gb), ..Default::default() };
        let _ = update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.unwrap();

        // Not specified: apply the default; specified (including 0 = unlimited): use the given value
        let Json(a) = create(State(env.st.clone()), Admin(admin.clone()), req("carol", None)).await.unwrap();
        assert_eq!(a.quota_bytes, 10 * gb);
        let Json(b) = create(State(env.st.clone()), Admin(admin.clone()), req("dave", Some(2 * gb))).await.unwrap();
        assert_eq!(b.quota_bytes, 2 * gb);
        let Json(c) = create(State(env.st.clone()), Admin(admin.clone()), req("erin", Some(0))).await.unwrap();
        assert_eq!(c.quota_bytes, 0);

        // The setting is saved and survives restarts
        assert_eq!(crate::settings::load_system_settings(&env.st.db).await.unwrap().default_user_quota, 10 * gb);
        let bad = SettingsReq { default_user_quota: Some(-1), ..Default::default() };
        assert!(update_settings(State(env.st.clone()), Admin(admin), Json(bad)).await.is_err());
    }

    #[tokio::test]
    async fn default_language_is_validated_and_saved() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        assert_eq!(env.st.system.read().unwrap().default_lang, "auto");
        let settings = SettingsReq { default_lang: Some("zh-TW".into()), ..Default::default() };
        let Json(info) = update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.unwrap();
        assert_eq!(info.default_lang, "zh-TW");
        assert_eq!(crate::settings::load_system_settings(&env.st.db).await.unwrap().default_lang, "zh-TW");
        let bad = SettingsReq { default_lang: Some("fr".into()), ..Default::default() };
        assert!(update_settings(State(env.st.clone()), Admin(admin.clone()), Json(bad)).await.is_err());
        assert_eq!(env.st.system.read().unwrap().default_lang, "zh-TW");
        // Every language the server knows, also those the Control panel doesn't offer yet
        for lang in ["zh-CN", "ja", "en", "auto"] {
            let settings = SettingsReq { default_lang: Some(lang.into()), ..Default::default() };
            let Json(info) = update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.unwrap();
            assert_eq!(info.default_lang, lang);
        }
        for l in crate::i18n::Lang::ALL {
            assert!(super::LANGS.contains(&l.code()), "{}", l.code());
        }
    }

    #[tokio::test]
    async fn minimum_password_length_applies_to_new_passwords() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        for bad in [5, 65] {
            let settings = SettingsReq { min_password_length: Some(bad), ..Default::default() };
            assert!(update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.is_err());
        }
        let settings = SettingsReq { min_password_length: Some(12), require_two_factor: Some(true), ..Default::default() };
        let Json(info) = update_settings(State(env.st.clone()), Admin(admin.clone()), Json(settings)).await.unwrap();
        assert!(info.min_password_length == 12 && info.require_two_factor);
        let saved = crate::settings::load_system_settings(&env.st.db).await.unwrap();
        assert!(saved.min_password_length == 12 && saved.require_two_factor);

        let mut short = req("carol", None);
        short.password = "elevenchars".into();
        let err = create(State(env.st.clone()), Admin(admin.clone()), short).await.map(|_| ()).unwrap_err();
        assert_eq!(err.message, "Password must be at least 12 characters");
        let amy = env.user("amy", true).await;
        let update = UpdateReq {
            password: Some("elevenchars".into()),
            display_name: None,
            role: None,
            can_write: None,
            can_delete: None,
            can_share: None,
            quota_bytes: None,
            disabled: None,
        };
        assert!(super::update(State(env.st.clone()), Admin(admin), Path(amy.id), Json(update)).await.is_err());
        let Json(me) = crate::signin::me(State(env.st.clone()), amy, axum::http::HeaderMap::new()).await.unwrap();
        assert_eq!(me.min_password_length, 12);
    }

    #[test]
    fn public_url_is_validated_and_normalized() {
        assert_eq!(normalize_public_url(" https://Drive.Example.com/ ").unwrap(), "https://drive.example.com");
        assert_eq!(normalize_public_url("http://192.168.1.10:8080").unwrap(), "http://192.168.1.10:8080");
        assert_eq!(normalize_public_url("").unwrap(), "");
        for bad in ["drive.example.com", "ftp://x.com", "https://x.com/drive", "https://", "https://x.com?a=1", "javascript://x", "https://a b.com"] {
            assert!(normalize_public_url(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn public_url_is_saved_and_given_to_users() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let settings = SettingsReq { public_url: Some("https://drive.example.com/".into()), ..Default::default() };
        let Json(info) = update_settings(State(env.st.clone()), Admin(admin), Json(settings)).await.unwrap();
        assert_eq!(info.public_url, "https://drive.example.com");
        assert_eq!(crate::settings::load_system_settings(&env.st.db).await.unwrap().public_url, "https://drive.example.com");
        let amy = env.user("amy", true).await;
        let Json(me) = crate::signin::me(State(env.st.clone()), amy, axum::http::HeaderMap::new()).await.unwrap();
        assert_eq!(me.public_url, "https://drive.example.com");
    }

    /// A file of `size` bytes (no content) in a folder; space usage is recomputed
    async fn sized_file(env: &testutil::TestEnv, owner: &crate::auth::User, parent: &str, name: &str, size: i64) -> String {
        let id = env.file(owner, parent, name).await;
        sqlx::query("UPDATE nodes SET size = ? WHERE id = ?").bind(size).bind(&id).execute(&env.st.db).await.unwrap();
        tree::recompute_usage(&env.st).await.unwrap();
        id
    }

    async fn delete_user(env: &testutil::TestEnv, id: i64, q: serde_json::Value) -> AppResult<crate::jobs::Job> {
        let admin = env.admin().await;
        let Json(job) = delete(State(env.st.clone()), Admin(admin), Path(id), Query(serde_json::from_value(q).unwrap())).await?;
        Ok(crate::jobs::wait_for(&env.st, &job.id).await)
    }

    async fn drive_row(env: &testutil::TestEnv, node: &str) -> (String, String) {
        sqlx::query_as("SELECT n.drive_id, n.parent_id FROM nodes n WHERE n.id = ?").bind(node).fetch_one(&env.st.db).await.unwrap()
    }

    #[tokio::test]
    async fn the_folder_of_a_removed_users_files_is_named_in_the_system_default_language() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        sized_file(&env, &amy, amy.root(), "notes.txt", 10).await;
        let company = env.st.shared_root().unwrap();
        let company_drive = env.drive_of(&company).await;
        assert_eq!(get_row(&env.st, amy.id).await.unwrap().files_folder, "Files of amy");
        // Whoever removes it, and whatever language their page is in
        let settings = SettingsReq { default_lang: Some("zh-TW".into()), ..Default::default() };
        let _ = update_settings(State(env.st.clone()), Admin(admin), Json(settings)).await.unwrap();
        let name = crate::i18n::tr(crate::i18n::Lang::ZhTw, crate::i18n::Text::FilesOf, &[("username", "amy")]);
        assert_ne!(name, "Files of amy");
        assert_eq!(get_row(&env.st, amy.id).await.unwrap().files_folder, name);

        let _ = delete_user(&env, amy.id, json!({ "move_to": company_drive })).await.unwrap();
        let (n,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE parent_id = ? AND name = ?").bind(&company).bind(&name).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 1);
    }

    async fn used(env: &testutil::TestEnv, drive: &str) -> i64 {
        let (n,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(drive).fetch_one(&env.st.db).await.unwrap();
        n
    }

    #[tokio::test]
    async fn deleting_a_user_moves_their_files_into_a_folder_named_after_them() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let deep = env.folder(&amy, &docs, "2026").await;
        let report = sized_file(&env, &amy, &deep, "report.pdf", 300).await;
        let top = sized_file(&env, &amy, amy.root(), "notes.txt", 100).await;
        let binned = sized_file(&env, &amy, amy.root(), "old.txt", 50).await;
        sqlx::query("UPDATE nodes SET trashed_at = 1, trash_id = 't1', trash_root = 1 WHERE id = ?").bind(&binned).execute(&env.st.db).await.unwrap();
        let company = env.st.shared_root().unwrap();
        let company_drive = env.drive_of(&company).await;
        // The name is taken already: the folder gets a number
        env.folder(&amy, &company, "Files of amy").await;

        // Without a choice nothing happens
        let err = delete_user(&env, amy.id, json!({})).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(get_row(&env.st, amy.id).await.is_ok());

        let _ = delete_user(&env, amy.id, json!({ "move_to": company_drive })).await.unwrap();
        assert!(get_row(&env.st, amy.id).await.is_err());
        let (folder,): (String,) =
            sqlx::query_as("SELECT id FROM nodes WHERE parent_id = ? AND name = 'Files of amy (1)'").bind(&company).fetch_one(&env.st.db).await.unwrap();
        // The whole tree is in the company space now, as it was
        assert_eq!(drive_row(&env, &docs).await, (company_drive.clone(), folder.clone()));
        assert_eq!(drive_row(&env, &top).await, (company_drive.clone(), folder.clone()));
        assert_eq!(drive_row(&env, &report).await, (company_drive.clone(), deep.clone()));
        // The trash wasn't moved: it goes with the personal space, and with the root folder is deleted in the background
        assert_eq!(used(&env, &company_drive).await, 400);
        let gone = || async {
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE id IN (?, ?)").bind(&binned).bind(amy.root()).fetch_one(&env.st.db).await.unwrap();
            n == 0
        };
        for _ in 0..200 {
            if gone().await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(gone().await, "the trash and the root folder were left behind");
        let (moved,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ?").bind(&company_drive).fetch_one(&env.st.db).await.unwrap();
        assert!(moved >= 6, "the moved files were purged with the space: {moved}");
        let (personal,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM drives WHERE kind = 'personal' AND name = 'My files' AND root_id = ?")
            .bind(amy.root())
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        assert_eq!(personal, 0);
        let (detail,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action = 'user_delete'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(detail, "amy: files moved to All files › Files of amy (1)");
        tree::recompute_usage(&env.st).await.unwrap();
        assert_eq!(used(&env, &company_drive).await, 400, "the counter matches the files");
    }

    #[tokio::test]
    async fn moving_a_deleted_users_files_respects_the_target_space() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        sized_file(&env, &amy, amy.root(), "big.bin", 1000).await;
        let bens = env.drive_of(ben.root()).await;
        let amys = env.drive_of(amy.root()).await;

        // Another person's personal space counts against their quota: refused with a clear message, nothing changes
        sqlx::query("UPDATE users SET quota_bytes = 500 WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        let err = delete_user(&env, amy.id, json!({ "move_to": bens })).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
        assert!(err.message.contains("doesn't have room for"), "{}", err.message);
        assert!(get_row(&env.st, amy.id).await.is_ok());
        // Not into the user's own space, both choices at once, or a space that doesn't exist
        assert!(delete_user(&env, amy.id, json!({ "move_to": amys })).await.is_err());
        assert!(delete_user(&env, amy.id, json!({ "move_to": bens, "delete_files": true })).await.is_err());
        assert!(delete_user(&env, amy.id, json!({ "move_to": "nope" })).await.is_err());

        sqlx::query("UPDATE users SET quota_bytes = 0 WHERE id = ?").bind(ben.id).execute(&env.st.db).await.unwrap();
        let _ = delete_user(&env, amy.id, json!({ "move_to": bens })).await.unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = ? AND name = 'big.bin'").bind(&bens).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(used(&env, &bens).await, 1000);
    }

    #[tokio::test]
    async fn files_added_while_a_personal_space_is_removed_are_kept() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        let a = env.stored_file(&amy, amy.root(), "a.txt", b"first").await;
        let space = env.folder_space("Scans").await;
        let amys = env.drive_of(amy.root()).await;
        let q: DeleteQuery = serde_json::from_value(json!({ "move_to": space.drive })).unwrap();
        let moving = || async {
            let (m,): (bool,) = sqlx::query_as("SELECT moving FROM drives WHERE id = ?").bind(&amys).fetch_one(&env.st.db).await.unwrap();
            m
        };

        // Deleting Amy moves her files on the disk first; until her space is gone, nothing can be added to it
        let moved = move_personal_first(&env.st, &admin, amy.id, "amy", &q, &Default::default()).await.unwrap();
        assert!(moved.is_some());
        assert_eq!(env.drive_of(&a).await, space.drive);
        assert!(moving().await, "read-only while it is removed");
        let err = env.try_upload(&amy, amy.root(), "late.txt", b"late").await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);

        // Something that got in all the same (indexed by a scan, say) stops the removal, checked under the write lock
        let late = env.stored_file(&amy, amy.root(), "late.txt", b"late").await;
        let res = {
            let _w = env.st.write_lock.lock().await;
            let mut tx = crate::db::begin_write(&env.st.db).await.unwrap();
            let res = delete_in(&mut tx, &admin, amy.id, "amy", &q, moved.as_ref()).await;
            crate::db::settle(tx, res).await
        };
        let err = Moved::kept_on_error(moved.as_ref(), &env.st, res).await.map(|_| ()).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        // Amy, her space and the late file are still there, and the space is writable again
        assert!(get_row(&env.st, amy.id).await.is_ok());
        assert_eq!(env.drive_of(&late).await, amys);
        assert!(!moving().await);

        // Trying again moves the rest
        let _ = delete_user(&env, amy.id, json!({ "move_to": space.drive })).await.unwrap();
        assert!(get_row(&env.st, amy.id).await.is_err());
        assert_eq!(env.drive_of(&late).await, space.drive);
        assert_eq!(std::fs::read(space.dir.join("Files of amy (1)/late.txt")).unwrap(), b"late");
    }

    #[tokio::test]
    async fn deleting_a_user_whose_files_take_long_to_move_goes_on_when_the_page_gives_up() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let a = env.stored_file(&amy, amy.root(), "a.txt", b"amy's").await;
        let space = env.folder_space("Scans").await;
        let go = std::sync::Arc::new(tokio::sync::Notify::new());
        let _hook = crate::fsops::hook_after_place(crate::fsops::wait_for(&go));
        let asked = delete(State(env.st.clone()), Admin(env.admin().await), Path(amy.id), Query(serde_json::from_value(json!({ "move_to": space.drive })).unwrap()));
        // The page (or a proxy) stops waiting while the files are copied: the removal isn't cut off in the middle, which
        // left the space read-only until a restart
        let _ = tokio::time::timeout(std::time::Duration::from_millis(300), asked).await;
        go.notify_one();
        let mut gone = false;
        for _ in 0..200 {
            if get_row(&env.st, amy.id).await.is_err() {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(gone, "the user is deleted");
        assert_eq!(env.node_at(&space.drive, "Files of amy/a.txt").await.unwrap().0, a);
        // A long one answers with the job, for the page to follow
        let _short = crate::jobs::short_wait();
        let ben = env.user("ben", true).await;
        env.stored_file(&ben, ben.root(), "b.txt", b"ben's").await;
        let Json(job) =
            delete(State(env.st.clone()), Admin(env.admin().await), Path(ben.id), Query(serde_json::from_value(json!({ "move_to": space.drive })).unwrap()))
                .await
                .unwrap();
        assert_eq!((job.kind, job.state), ("delete_user", "running"));
        go.notify_one();
        assert_eq!(crate::jobs::wait_for(&env.st, &job.id).await.state, "done");
        assert!(get_row(&env.st, ben.id).await.is_err());
    }

    #[tokio::test]
    async fn a_removal_stopped_midway_leaves_the_space_writable_after_a_restart() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let amys = env.drive_of(amy.root()).await;
        sqlx::query("UPDATE drives SET moving = 1 WHERE id = ?").bind(&amys).execute(&env.st.db).await.unwrap();
        release_interrupted(&env.st).await;
        let (m,): (bool,) = sqlx::query_as("SELECT moving FROM drives WHERE id = ?").bind(&amys).fetch_one(&env.st.db).await.unwrap();
        assert!(!m);
        env.upload(&amy, amy.root(), "a.txt", b"a").await;
    }

    /// Waits for the background purge of a deleted space to remove a node
    async fn purged(env: &testutil::TestEnv, node: &str) -> bool {
        for _ in 0..200 {
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE id = ?").bind(node).fetch_one(&env.st.db).await.unwrap();
            if n == 0 {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_deleted_users_files_can_go_into_a_folder_space_and_deleting_them_stays_possible() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        let a = env.stored_file(&amy, &docs, "a.txt", b"from the store").await;
        let space = env.folder_space("Scans").await;
        let _ = delete_user(&env, amy.id, json!({ "move_to": space.drive })).await.unwrap();
        // Written to the folder, with the same ids
        assert_eq!(std::fs::read(space.dir.join("Files of amy/Docs/a.txt")).unwrap(), b"from the store");
        assert_eq!(env.node_at(&space.drive, "Files of amy/Docs/a.txt").await.unwrap().0, a);
        assert_eq!(env.drive_of(&docs).await, space.drive);
        assert!(purged(&env, amy.root()).await);

        let ben = env.user("ben", true).await;
        let file = sized_file(&env, &ben, ben.root(), "b.txt", 10).await;
        let _ = delete_user(&env, ben.id, json!({ "delete_files": true })).await.unwrap();
        assert!(purged(&env, &file).await);
        // An empty personal space needs no choice
        let cat = env.user("cat", true).await;
        let _ = delete_user(&env, cat.id, json!({})).await.unwrap();
    }

    #[tokio::test]
    async fn a_deleted_users_folder_is_moved_on_the_disk_or_kept() {
        let env = testutil::folders_env().await;
        let storage = env.dir.join("blobs");
        let amy = env.user("amy", true).await;
        let docs = env.folder(&amy, amy.root(), "Docs").await;
        assert!(storage.join("users/amy/Docs").is_dir());
        let a = env.upload(&amy, &docs, "a.txt", b"amy's").await;
        let company = env.drive_of(&env.st.shared_root().unwrap()).await;

        let _ = delete_user(&env, amy.id, json!({ "move_to": company })).await.unwrap();
        assert_eq!(std::fs::read(storage.join("company/Files of amy/Docs/a.txt")).unwrap(), b"amy's");
        assert!(!storage.join("users/amy/Docs").exists(), "moved, not copied");
        assert_eq!(env.node_at(&company, "Files of amy/Docs/a.txt").await.unwrap().0, a);
        let (detail,): (String,) = sqlx::query_as("SELECT detail FROM activity WHERE action = 'user_delete'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(detail, "amy: files moved to All files › Files of amy");
        assert!(purged(&env, amy.root()).await);
        assert!(storage.join("users/amy").is_dir(), "the emptied folder stays");

        // Deleting the files removes them from ThirtyFile; the folder and the files stay on the disk
        let ben = env.user("ben", true).await;
        let b = env.upload(&ben, ben.root(), "b.txt", b"ben's").await;
        let _ = delete_user(&env, ben.id, json!({ "delete_files": true })).await.unwrap();
        assert!(purged(&env, &b).await);
        assert_eq!(std::fs::read(storage.join("users/ben/b.txt")).unwrap(), b"ben's");
        let (detail,): (String,) =
            sqlx::query_as("SELECT detail FROM activity WHERE action = 'user_delete' AND detail LIKE 'ben%'").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(detail, format!("ben: files removed, their folder on the server is kept: {}", storage.join("users").join("ben").display()));
        // A new account with the same name doesn't get the old files
        let ben = env.user("ben", true).await;
        let (folder,): (String,) = sqlx::query_as("SELECT source_path FROM drives WHERE root_id = ?").bind(ben.root()).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(std::path::PathBuf::from(folder), storage.join("users/ben (2)"));
    }

    #[tokio::test]
    async fn what_a_deleted_user_had_elsewhere_stays_and_passes_to_the_administrator() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let amy = env.user("amy", true).await;
        let company = env.st.shared_root().unwrap();
        let company_drive = env.drive_of(&company).await;
        // The same content in her own space and in the company space; a team space she owns; an upload under way
        env.stored_file(&amy, amy.root(), "mine.txt", b"report").await;
        let theirs = env.stored_file(&amy, &company, "report.txt", b"report").await;
        let team_root = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0, "local").await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            root
        };
        let team_drive = env.drive_of(&team_root).await;
        let ts = crate::util::now();
        sqlx::query("INSERT INTO uploads (id, owner_id, parent_id, rel_path, name, size, offset, created_at, expires_at, drive_id) VALUES ('u1', ?, ?, '', 'big.bin', 900, 0, ?, ?, ?)")
            .bind(amy.id)
            .bind(&company)
            .bind(ts)
            .bind(ts + crate::upload::UPLOAD_TTL)
            .bind(&company_drive)
            .execute(&env.st.db)
            .await
            .unwrap();
        let part = env.st.tmp_dir().join("upload-u1");
        std::fs::write(&part, b"partial").unwrap();
        tree::recompute_usage(&env.st).await.unwrap();

        let _ = delete_user(&env, amy.id, json!({ "delete_files": true })).await.unwrap();
        // Her file in the company space stays, now the administrator's, and still counts there
        let (owner,): (i64,) = sqlx::query_as("SELECT owner_id FROM nodes WHERE id = ?").bind(&theirs).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(owner, admin.id);
        assert_eq!(used(&env, &company_drive).await, 6);
        // The team space too, with its owner access
        let (drive_owner,): (i64,) = sqlx::query_as("SELECT owner_id FROM drives WHERE id = ?").bind(&team_drive).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(drive_owner, admin.id);
        let (grants,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM grants WHERE node_id = ? AND principal_id = ? AND role = 'owner'")
            .bind(&team_root)
            .bind(admin.id)
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        assert_eq!(grants, 1);
        // The upload is gone with what it had received
        let (uploads,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM uploads").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(uploads, 0);
        assert!(!part.exists());
        // Her own files are deleted in the background; the content they shared with the company file stays
        let hash = crate::util::sha256_hex(b"report");
        let mut refs = 0;
        for _ in 0..200 {
            (refs,) = sqlx::query_as("SELECT refcount FROM blobs WHERE hash = ?").bind(&hash).fetch_one(&env.st.db).await.unwrap();
            if refs == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(refs, 1);
        let (pending,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pending_blob_deletes WHERE hash = ?").bind(&hash).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(pending, 0);
    }
}
