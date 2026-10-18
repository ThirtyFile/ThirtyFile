//! Receive-folder credentials are issued only through a browser session, never through FTP.

use super::credentials::{Folder, publisher};
use crate::{
    auth::{Admin, User},
    error::{AppError, AppResult},
    state::AppState,
    util::{new_id, now, random_token},
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize)]
pub struct ListQuery {
    pub node_id: Option<String>,
}
#[derive(Serialize)]
pub struct FolderInfo {
    #[serde(flatten)]
    pub folder: Folder,
    pub creator: String,
    pub folder_name: String,
    pub active: bool,
}
#[derive(Serialize)]
pub struct List {
    pub can_publish: bool,
    pub folders: Vec<FolderInfo>,
}

pub async fn list(State(st): State<AppState>, user: User, Query(q): Query<ListQuery>) -> AppResult<Json<List>> {
    let cfg = super::config::load(&st).await?;
    let mut conn = st.db.acquire().await?;
    if q.node_id.is_none() && !user.is_admin() {
        return Err(AppError::forbidden("Administrator permission required"));
    }
    let allowed = match &q.node_id {
        Some(id) => publisher(&mut conn, &user, id, cfg.managers_publish).await.is_ok(),
        None => true,
    };
    let mut out = List { can_publish: allowed && cfg.enabled && q.node_id.is_some(), folders: vec![] };
    if !allowed {
        return Ok(Json(out));
    }
    let folders: Vec<Folder> = sqlx::query_as("SELECT * FROM ftp_receive_folders WHERE (?1 IS NULL OR node_id=?1) ORDER BY created_at DESC LIMIT 1000")
        .bind(q.node_id)
        .fetch_all(&mut *conn)
        .await?;
    for folder in folders {
        let creator = crate::auth::user_by_id(&st, &mut conn, folder.creator_id).await?;
        let active = cfg.enabled
            && folder.enabled
            && folder.expires_at.is_none_or(|t| t > now())
            && match &creator {
                Some(u) => publisher(&mut conn, u, &folder.node_id, cfg.managers_publish).await.is_ok(),
                None => false,
            };
        let folder_name = crate::tree::get_node(&mut conn, &folder.node_id).await?.map(|n| n.name).unwrap_or_default();
        out.folders.push(FolderInfo { folder, creator: creator.map(|u| u.username).unwrap_or_default(), folder_name, active });
    }
    Ok(Json(out))
}
pub async fn admin_list(st: State<AppState>, Admin(user): Admin, q: Query<ListQuery>) -> AppResult<Json<List>> {
    list(st, user, q).await
}

#[derive(Deserialize)]
pub struct Change {
    pub name: String,
    pub enabled: bool,
    pub allow_list: bool,
    pub allow_read: bool,
    /// An upload replaces a file of the same name; without it, the new file gets a number
    #[serde(default = "replaces")]
    pub replace_existing: bool,
    pub expires_at: Option<i64>,
}

fn replaces() -> bool {
    true
}

impl Change {
    fn validate(&self) -> AppResult<()> {
        if self.name.trim().is_empty() || self.name.chars().count() > 80 || self.name.chars().any(char::is_control) {
            return Err(AppError::bad_request("Enter a receive folder name (up to 80 characters)"));
        }
        if self.expires_at.is_some_and(|t| t <= now()) {
            return Err(AppError::bad_request("The receive folder's end date must be in the future"));
        }
        Ok(())
    }
}
#[derive(Deserialize)]
pub struct Create {
    pub node_id: String,
    #[serde(flatten)]
    pub change: Change,
    pub password: Option<String>,
    pub code: Option<String>,
}
#[derive(Serialize)]
pub struct Created {
    #[serde(flatten)]
    pub folder: Folder,
    pub password: String,
}

async fn confirm(st: &AppState, user: &User, password: Option<String>, code: Option<&str>) -> AppResult<()> {
    crate::tokens::confirm_identity(st, user, password, code, "Sign out and sign in again, then manage FTP credentials within 10 minutes").await
}
pub async fn create(State(st): State<AppState>, user: User, Json(req): Json<Create>) -> AppResult<Json<Created>> {
    req.change.validate()?;
    confirm(&st, &user, req.password, req.code.as_deref()).await?;
    let password = random_token(32);
    let hash = crate::auth::hash_password(password.clone()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let current = crate::auth::current_user(&mut tx, &user).await?;
    let cfg = super::config::from_connection(&mut tx).await?;
    if !cfg.enabled {
        return Err(AppError::forbidden("FTP access is disabled"));
    }
    publisher(&mut tx, &current, &req.node_id, cfg.managers_publish).await?;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ftp_receive_folders WHERE node_id=?").bind(&req.node_id).fetch_one(&mut *tx).await?;
    if count >= 20 {
        return Err(AppError::bad_request("This folder already has 20 FTP receive folders"));
    }
    let folder = Folder {
        id: new_id(),
        node_id: req.node_id,
        creator_id: user.id,
        name: req.change.name.trim().into(),
        username: format!("ftp-{}", random_token(12)),
        password_hash: hash,
        generation: 1,
        enabled: req.change.enabled,
        allow_list: req.change.allow_list,
        allow_read: req.change.allow_read,
        replace_existing: req.change.replace_existing,
        expires_at: req.change.expires_at,
        created_at: now(),
        last_used_at: None,
    };
    sqlx::query("INSERT INTO ftp_receive_folders(id,node_id,creator_id,name,username,password_hash,enabled,allow_list,allow_read,replace_existing,expires_at,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&folder.id).bind(&folder.node_id).bind(folder.creator_id).bind(&folder.name).bind(&folder.username).bind(&folder.password_hash).bind(folder.enabled).bind(folder.allow_list).bind(folder.allow_read).bind(folder.replace_existing).bind(folder.expires_at).bind(folder.created_at).execute(&mut *tx).await?;
    crate::logs::record_activity(&mut tx, &user, None, "settings", &format!("Created FTP receive folder {}", folder.name)).await?;
    tx.commit().await?;
    Ok(Json(Created { folder, password }))
}

async fn managed(conn: &mut sqlx::SqliteConnection, user: &User, id: &str) -> AppResult<Folder> {
    let current = crate::auth::current_user(conn, user).await?;
    if current.guest {
        return Err(AppError::forbidden("Guest accounts can't publish receive folders"));
    }
    let folder: Folder = sqlx::query_as("SELECT * FROM ftp_receive_folders WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::not_found("Receive folder not found"))?;
    if !current.is_admin() {
        let cfg = super::config::from_connection(conn).await?;
        publisher(conn, &current, &folder.node_id, cfg.managers_publish).await?;
    }
    Ok(folder)
}

#[derive(Deserialize)]
pub struct Edit {
    #[serde(flatten)]
    pub change: Change,
    pub password: Option<String>,
    pub code: Option<String>,
}
pub async fn update(State(st): State<AppState>, user: User, Path(id): Path<String>, Json(req): Json<Edit>) -> AppResult<Json<serde_json::Value>> {
    req.change.validate()?;
    confirm(&st, &user, req.password, req.code.as_deref()).await?;
    let req = req.change;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let folder = managed(&mut tx, &user, &id).await?;
    // Turning it on, letting the device see, read or replace more, or giving it longer: only who published it, or an
    // administrator
    let widens = (req.enabled && !folder.enabled)
        || (req.allow_list && !folder.allow_list)
        || (req.allow_read && !folder.allow_read)
        || (req.replace_existing && !folder.replace_existing)
        || folder.expires_at.is_some_and(|was| req.expires_at.is_none_or(|now| now > was));
    if widens {
        crate::tokens::may_hand_out(&crate::auth::current_user(&mut tx, &user).await?, folder.creator_id)?;
    }
    sqlx::query("UPDATE ftp_receive_folders SET name=?,enabled=?,allow_list=?,allow_read=?,replace_existing=?,expires_at=?,generation=generation+1 WHERE id=?")
        .bind(req.name.trim())
        .bind(req.enabled)
        .bind(req.allow_list)
        .bind(req.allow_read)
        .bind(req.replace_existing)
        .bind(req.expires_at)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    crate::logs::record_activity(&mut tx, &user, None, "settings", &format!("Updated FTP receive folder {}", req.name.trim())).await?;
    tx.commit().await?;
    Ok(Json(serde_json::json!({"ok":true})))
}

#[derive(Deserialize)]
pub struct Confirm {
    pub password: Option<String>,
    pub code: Option<String>,
}
pub async fn rotate(State(st): State<AppState>, user: User, Path(id): Path<String>, Json(req): Json<Confirm>) -> AppResult<Json<Created>> {
    confirm(&st, &user, req.password, req.code.as_deref()).await?;
    let password = random_token(32);
    let hash = crate::auth::hash_password(password.clone()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let mut folder = managed(&mut tx, &user, &id).await?;
    crate::tokens::may_hand_out(&crate::auth::current_user(&mut tx, &user).await?, folder.creator_id)?;
    sqlx::query("UPDATE ftp_receive_folders SET password_hash=?,generation=generation+1 WHERE id=?").bind(&hash).bind(id).execute(&mut *tx).await?;
    folder.password_hash = hash;
    folder.generation += 1;
    crate::logs::record_activity(&mut tx, &user, None, "settings", &format!("Rotated the password of FTP receive folder {}", folder.name)).await?;
    tx.commit().await?;
    Ok(Json(Created { folder, password }))
}
pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<String>, Json(req): Json<Confirm>) -> AppResult<Json<serde_json::Value>> {
    confirm(&st, &user, req.password, req.code.as_deref()).await?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let folder = managed(&mut tx, &user, &id).await?;
    sqlx::query("DELETE FROM ftp_receive_folders WHERE id=?").bind(id).execute(&mut *tx).await?;
    crate::logs::record_activity(&mut tx, &user, None, "settings", &format!("Removed FTP receive folder {}", folder.name)).await?;
    tx.commit().await?;
    Ok(Json(serde_json::json!({"ok":true})))
}
