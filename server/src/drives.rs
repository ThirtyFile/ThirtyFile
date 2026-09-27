//! Spaces, members and folder sharing (grants), groups, activity log.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use crate::{
    auth::{Admin, User},
    db::{add_grant, create_drive},
    error::{AppError, AppResult},
    state::AppState,
    tree::{self, DRIVE_COLS, Drive, Node, Role},
    util::{now, validate_name},
};

// ───────────── Spaces ─────────────

#[derive(Serialize)]
pub struct DriveInfo {
    id: String,
    name: String,
    kind: String,
    root_id: String,
    role: Option<Role>,
    used_bytes: i64,
    quota_bytes: i64,
    owner_name: String,
    member_count: i64,
    disabled: bool,
    /// Storage location for new files (the default location when not set)
    location_id: String,
    location_name: String,
    location_is_default: bool,
    /// Reason the storage location is offline (e.g. S3 disconnected); browsing works, but opening, downloading and uploading don't
    offline: Option<String>,
    /// "store" or "folder" (a folder on the server)
    mode: String,
    /// Browse, download and share only
    read_only: bool,
    /// Folder spaces, for administrators: the folder, when it was last scanned, and what the scan found
    #[serde(skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_scan_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scan_report: Option<Value>,
}

async fn drive_info(st: &AppState, conn: &mut SqliteConnection, d: Drive, role: Option<Role>) -> AppResult<DriveInfo> {
    Ok(drive_infos(st, conn, vec![(d, role)], false).await?.pop().expect("one space in, one out"))
}

/// What the space cards show, for many spaces in one query (the lists don't run a query per space). With
/// `scan_details`, folder spaces also report their folder and last scan (administrators).
async fn drive_infos(
    st: &AppState,
    conn: &mut SqliteConnection,
    drives: Vec<(Drive, Option<Role>)>,
    scan_details: bool,
) -> AppResult<Vec<DriveInfo>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        location_id: Option<String>,
        location_name: String,
        quota_bytes: i64,
        owner_name: String,
        member_count: i64,
        last_scan_at: Option<i64>,
        scan_report: Option<String>,
    }
    let default_location = st.default_location.read().unwrap().clone();
    let ids = serde_json::to_string(&drives.iter().map(|(d, _)| d.id.as_str()).collect::<Vec<_>>()).unwrap();
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT d.id, d.location_id, COALESCE(l.name, '') AS location_name,
                CASE WHEN d.kind = 'personal' THEN COALESCE(u.quota_bytes, 0) ELSE d.quota_bytes END AS quota_bytes,
                COALESCE(u.username, '') AS owner_name, COALESCE(g.n, 0) AS member_count, d.last_scan_at, d.scan_report
         FROM drives d
         LEFT JOIN storage_locations l ON l.id = COALESCE(d.location_id, ?2)
         LEFT JOIN users u ON u.id = d.owner_id
         LEFT JOIN (SELECT node_id, COUNT(*) AS n FROM grants GROUP BY node_id) g ON g.node_id = d.root_id
         WHERE d.id IN (SELECT value FROM json_each(?1))",
    )
    .bind(&ids)
    .bind(&default_location)
    .fetch_all(&mut *conn)
    .await?;
    let mut rows: HashMap<String, Row> = rows.into_iter().map(|r| (r.id.clone(), r)).collect();
    let mut out = Vec::with_capacity(drives.len());
    for (d, role) in drives {
        let r = rows.remove(&d.id).ok_or_else(|| AppError::not_found("Space not found"))?;
        let location_is_default = r.location_id.is_none();
        let location_id = r.location_id.unwrap_or_else(|| default_location.clone());
        // A folder space is on the server itself: nothing to be offline
        let offline = if d.is_folder() { None } else { st.location_offline(&location_id) };
        let details = scan_details && d.is_folder();
        out.push(DriveInfo {
            mode: d.mode.clone(),
            read_only: d.read_only,
            source_path: d.source_path.clone().filter(|_| details),
            last_scan_at: r.last_scan_at.filter(|_| details),
            scan_report: r.scan_report.filter(|_| details).and_then(|r| serde_json::from_str(&r).ok()),
            used_bytes: d.used_bytes,
            id: d.id,
            name: d.name,
            kind: d.kind,
            root_id: d.root_id,
            role,
            quota_bytes: r.quota_bytes,
            owner_name: r.owner_name,
            member_count: r.member_count,
            disabled: d.disabled,
            location_id,
            location_name: r.location_name,
            location_is_default,
            offline,
        });
    }
    Ok(out)
}

/// Spaces I can access
pub async fn list(State(st): State<AppState>, user: User) -> AppResult<Json<Vec<DriveInfo>>> {
    let mut c = st.db.acquire().await?;
    let drives = tree::user_drives(&mut c, &user).await?.into_iter().map(|(d, role)| (d, Some(role))).collect();
    Ok(Json(drive_infos(&st, &mut c, drives, false).await?))
}

fn can_create_drive(st: &AppState, user: &User) -> bool {
    user.is_admin() || st.system.read().unwrap().allow_user_drives
}

#[derive(Deserialize)]
pub struct CreateDriveReq {
    name: String,
    #[serde(default)]
    quota_bytes: i64,
    /// Administrators: show this folder on the server as the space (a folder space) instead of storing files
    #[serde(default)]
    source_path: Option<String>,
    /// Folder spaces: browse, download and share only
    #[serde(default)]
    read_only: bool,
}

/// Team spaces a standard user may create
const MAX_OWN_SPACES: i64 = 20;

pub async fn create(State(st): State<AppState>, user: User, Json(req): Json<CreateDriveReq>) -> AppResult<Json<DriveInfo>> {
    if !can_create_drive(&st, &user) {
        return Err(AppError::forbidden("Only administrators can create spaces"));
    }
    let name = validate_name(&req.name)?;
    let source = match req.source_path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(_) if !user.is_admin() => return Err(AppError::forbidden("Only administrators can show a folder on the server as a space")),
        Some(p) => {
            let p = crate::folders::check_source(&st, p)?;
            let (taken,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM drives WHERE mode = 'folder' AND source_path = ?").bind(&p).fetch_one(&st.db).await?;
            if taken > 0 {
                return Err(AppError::conflict("Another space already shows this folder"));
            }
            Some(p)
        }
        None => None,
    };
    // A space created by a standard user gets that user's own quota (0 = unlimited only when the user is unlimited)
    // rather than being unlimited; administrators adjust it later. Each space has its own quota: whether users may
    // create spaces at all is the administrator's setting
    let quota = if user.is_admin() { req.quota_bytes.max(0) } else { user.quota_bytes.max(0) };
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    if !user.is_admin() {
        // Each space brings its own quota, so the number a user can create is limited
        let (own,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM drives WHERE kind = 'team' AND owner_id = ?").bind(user.id).fetch_one(&mut *tx).await?;
        if own >= MAX_OWN_SPACES {
            return Err(AppError::bad_request("You can create at most 20 spaces. Ask an administrator for more."));
        }
    }
    let (drive_id, root_id) = create_drive(&mut tx, &name, "team", user.id, quota).await?;
    add_grant(&mut tx, &root_id, "user", user.id, "owner", Some(user.id), None).await?;
    if let Some(source) = &source {
        crate::folders::set_up(&mut tx, &drive_id, &root_id, source).await?;
        sqlx::query("UPDATE drives SET read_only = ? WHERE id = ?").bind(req.read_only).bind(&drive_id).execute(&mut *tx).await?;
    }
    let root = tree::get_node(&mut tx, &root_id).await?.unwrap();
    tree::log(&mut tx, &user, Some(&root), "drive_create", source.as_deref().unwrap_or_default()).await?;
    let drive = tree::get_drive(&mut tx, &drive_id).await?.unwrap();
    let info = drive_info(&st, &mut tx, drive, Some(Role::Owner)).await?;
    tx.commit().await?;
    drop(_w);
    if source.is_some() {
        // Index the folder right away (in the background: a large folder takes a while)
        crate::folders::scan_later(&st, &drive_id);
    }
    Ok(Json(info))
}

/// Scans a folder space now (Control panel › Spaces › Scan now)
pub async fn scan(State(st): State<AppState>, Admin(_): Admin, Path(id): Path<String>) -> AppResult<Json<crate::folders::ScanReport>> {
    Ok(Json(crate::folders::scan(&st, &id).await?))
}

/// Space management rights: manager or above; administrators can manage all non-personal spaces
pub async fn manageable_drive(conn: &mut SqliteConnection, user: &User, id: &str) -> AppResult<(Drive, Option<Role>)> {
    let drive = tree::get_drive(conn, id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    let root = tree::get_node(conn, &drive.root_id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    let role = tree::role_on(conn, user, &root).await?;
    let admin_override = user.is_admin() && drive.kind != "personal";
    if !(admin_override || role.is_some_and(|r| r >= Role::Manager)) {
        return Err(AppError::forbidden("You don't have permission to manage this space"));
    }
    Ok((drive, role))
}

#[derive(Deserialize)]
pub struct UpdateDriveReq {
    name: Option<String>,
    quota_bytes: Option<i64>,
    /// Folder spaces, administrators: browse, download and share only
    read_only: Option<bool>,
}

pub async fn update(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<UpdateDriveReq>,
) -> AppResult<Json<DriveInfo>> {
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let (drive, role) = if user.is_admin() {
        // Administrators can change the quota of any space (including personal spaces) without gaining access to its content
        let d = tree::get_drive(&mut tx, &id).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
        let root = tree::get_node(&mut tx, &d.root_id).await?.unwrap();
        let r = tree::role_on(&mut tx, &user, &root).await?;
        (d, r)
    } else {
        manageable_drive(&mut tx, &user, &id).await?
    };
    if let Some(name) = &req.name {
        if drive.kind == "personal" {
            return Err(AppError::bad_request("Personal spaces can't be renamed"));
        }
        let name = validate_name(name)?;
        sqlx::query("UPDATE drives SET name = ? WHERE id = ?").bind(&name).bind(&drive.id).execute(&mut *tx).await?;
    }
    if let Some(q) = req.quota_bytes {
        if !user.is_admin() {
            return Err(AppError::forbidden("Only administrators can change quotas"));
        }
        if drive.kind == "personal" {
            sqlx::query("UPDATE users SET quota_bytes = ? WHERE id = ?").bind(q.max(0)).bind(drive.owner_id).execute(&mut *tx).await?;
        } else {
            sqlx::query("UPDATE drives SET quota_bytes = ? WHERE id = ?").bind(q.max(0)).bind(&drive.id).execute(&mut *tx).await?;
        }
    }
    if let Some(read_only) = req.read_only {
        if !user.is_admin() {
            return Err(AppError::forbidden("Only administrators can make a space read-only"));
        }
        if !drive.is_folder() {
            return Err(AppError::bad_request("Only spaces that show a folder on the server can be read-only"));
        }
        sqlx::query("UPDATE drives SET read_only = ? WHERE id = ?").bind(read_only).bind(&drive.id).execute(&mut *tx).await?;
    }
    let root = tree::get_node(&mut tx, &drive.root_id).await?.unwrap();
    tree::log(&mut tx, &user, Some(&root), "drive_update", "").await?;
    let drive = tree::get_drive(&mut tx, &drive.id).await?.unwrap();
    let info = drive_info(&st, &mut tx, drive, role).await?;
    tx.commit().await?;
    Ok(Json(info))
}

/// Permanently deletes a team space (owner or administrator)
pub async fn delete(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let (drive, role) = manageable_drive(&mut tx, &user, &id).await?;
    if drive.kind != "team" {
        return Err(AppError::bad_request("Only team spaces can be deleted"));
    }
    if !(user.is_admin() || role == Some(Role::Owner)) {
        return Err(AppError::forbidden("Only the space owner or an administrator can delete a space"));
    }
    // The space disappears now; its files are deleted in the background, a batch at a time
    sqlx::query("DELETE FROM drives WHERE id = ?").bind(&drive.id).execute(&mut *tx).await?;
    tree::log(&mut tx, &user, None, "drive_delete", &drive.name).await?;
    tx.commit().await?;
    tree::purge_detached_later(&st);
    Ok(Json(json!({ "ok": true })))
}

/// Administrators: all spaces (without content)
pub async fn admin_list(State(st): State<AppState>, Admin(user): Admin) -> AppResult<Json<Vec<DriveInfo>>> {
    let mut c = st.db.acquire().await?;
    let sql = format!("SELECT {DRIVE_COLS} FROM drives d ORDER BY CASE d.kind WHEN 'company' THEN 0 WHEN 'team' THEN 1 ELSE 2 END, d.name");
    let drives: Vec<Drive> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).fetch_all(&mut *c).await?;
    // The administrator's own role (same as role_on for each root: grants, and managing the company space)
    let roles: HashMap<String, Role> = tree::user_drives(&mut c, &user).await?.into_iter().map(|(d, r)| (d.id, r)).collect();
    let drives = drives.into_iter().map(|d| {
        let role = roles.get(&d.id).copied();
        (d, role)
    });
    Ok(Json(drive_infos(&st, &mut c, drives.collect(), true).await?))
}

// ───────────── Access (space members / folder sharing) ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct GrantInfo {
    id: i64,
    node_id: String,
    principal_type: String,
    principal_id: i64,
    principal_name: String,
    role: String,
    expires_at: Option<i64>,
    granted_by_name: String,
    created_at: i64,
    /// Inherited from a parent folder (its name); null for direct grants
    #[sqlx(default)]
    inherited_from: Option<String>,
}

const GRANT_COLS: &str = "g.id, g.node_id, g.principal_type, g.principal_id,
    CASE g.principal_type
      WHEN 'everyone' THEN 'Everyone'
      WHEN 'user' THEN COALESCE((SELECT username FROM users WHERE id = g.principal_id), '(deleted)')
      ELSE COALESCE((SELECT name FROM groups WHERE id = g.principal_id), '(deleted)')
    END AS principal_name,
    g.role, g.expires_at, COALESCE((SELECT username FROM users WHERE id = g.granted_by), '') AS granted_by_name, g.created_at";

#[derive(Serialize)]
pub struct AccessInfo {
    node: Node,
    drive: Drive,
    is_drive_root: bool,
    my_role: Option<Role>,
    can_manage: bool,
    direct: Vec<GrantInfo>,
    inherited: Vec<GrantInfo>,
}

/// Whether the user can manage access to a node
/// Refuses when no owner other than the given principal would remain (owners whose access has expired don't count)
async fn require_other_owner(conn: &mut SqliteConnection, node_id: &str, principal_type: &str, principal_id: i64) -> AppResult<()> {
    let (others,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM grants WHERE node_id = ? AND role = 'owner' AND NOT (principal_type = ? AND principal_id = ?)
         AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(node_id)
    .bind(principal_type)
    .bind(principal_id)
    .bind(now())
    .fetch_one(&mut *conn)
    .await?;
    if others == 0 {
        return Err(AppError::bad_request("A space must have at least one owner"));
    }
    Ok(())
}

async fn can_manage_node(conn: &mut SqliteConnection, user: &User, node: &Node, drive: &Drive) -> AppResult<(bool, Option<Role>)> {
    let role = tree::role_on(conn, user, node).await?;
    let admin_override = user.is_admin() && drive.kind != "personal";
    Ok((admin_override || role.is_some_and(|r| r >= Role::Manager), role))
}

pub async fn access(State(st): State<AppState>, user: User, Path(id): Path<String>) -> AppResult<Json<AccessInfo>> {
    let mut c = st.db.acquire().await?;
    let id = tree::resolve_alias(&user, &id)?;
    let node = tree::get_node(&mut c, id).await?.filter(|n| n.trashed_at.is_none()).ok_or_else(|| AppError::not_found("Item not found"))?;
    let drive = tree::get_drive(&mut c, node.drive()).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    let (can_manage, my_role) = can_manage_node(&mut c, &user, &node, &drive).await?;
    if my_role.is_none() && !can_manage {
        return Err(AppError::not_found("Item not found"));
    }
    let sql = format!("SELECT {GRANT_COLS} FROM grants g WHERE g.node_id = ? ORDER BY g.created_at");
    let direct: Vec<GrantInfo> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&node.id).fetch_all(&mut *c).await?;
    let sql = format!(
        "WITH RECURSIVE up(id, parent_id, name, depth) AS (
           SELECT id, parent_id, name, 0 FROM nodes WHERE id = (SELECT parent_id FROM nodes WHERE id = ?1)
           UNION ALL SELECT n.id, n.parent_id, n.name, up.depth + 1 FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT {GRANT_COLS}, CASE WHEN up.parent_id IS NULL THEN ?2 ELSE up.name END AS inherited_from
         FROM grants g JOIN up ON g.node_id = up.id ORDER BY up.depth DESC, g.created_at"
    );
    let mut inherited: Vec<GrantInfo> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(&node.id).bind(&drive.name).fetch_all(&mut *c).await?;
    // Someone who only has the folder shared with them (not a member of the space) sees the grants from the shared
    // folder down, like the path they see: the folders above it and who else can access them are not theirs to know
    if !can_manage {
        let member_of: Vec<String> = tree::user_drives(&mut c, &user).await?.into_iter().map(|(d, _)| d.id).collect();
        if !member_of.iter().any(|d| d == node.drive()) {
            let shared: std::collections::HashSet<String> =
                tree::shared_with_me_outside(&mut c, &user, &member_of).await?.into_iter().map(|(n, _, _)| n.id).collect();
            let path = tree::path_of(&mut c, &node.id).await?;
            let visible: std::collections::HashSet<&str> = match path.iter().position(|c| shared.contains(&c.id)) {
                Some(start) => path[start..].iter().map(|c| c.id.as_str()).collect(),
                None => Default::default(),
            };
            inherited.retain(|g| visible.contains(g.node_id.as_str()));
        }
    }
    let is_drive_root = node.parent_id.is_none();
    Ok(Json(AccessInfo { node, drive, is_drive_root, my_role, can_manage, direct, inherited }))
}

#[derive(Deserialize)]
pub struct GrantReq {
    principal_type: String,
    #[serde(default)]
    principal_id: i64,
    role: String,
    expires_at: Option<i64>,
}

pub async fn grant(
    State(st): State<AppState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<GrantReq>,
) -> AppResult<Json<Value>> {
    let role = Role::parse(&req.role).ok_or_else(|| AppError::bad_request("Invalid role"))?;
    if !matches!(req.principal_type.as_str(), "user" | "group" | "everyone") {
        return Err(AppError::bad_request("Invalid user or group"));
    }
    if matches!(req.expires_at, Some(t) if t <= now()) {
        return Err(AppError::bad_request("The expiration time must be in the future"));
    }
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let id = tree::resolve_alias(&user, &id)?;
    let node = tree::get_node(&mut tx, id).await?.filter(|n| n.trashed_at.is_none()).ok_or_else(|| AppError::not_found("Item not found"))?;
    let drive = tree::get_drive(&mut tx, node.drive()).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    let (can_manage, my_role) = can_manage_node(&mut tx, &user, &node, &drive).await?;
    if !can_manage {
        return Err(AppError::forbidden("You don't have permission to manage access"));
    }
    let is_root = node.parent_id.is_none();
    if drive.kind == "personal" && is_root {
        return Err(AppError::bad_request("A personal space can't be shared as a whole. Share a folder in it instead."));
    }
    if role == Role::Owner && !(is_root && drive.kind == "team") {
        return Err(AppError::bad_request("The \"Owner\" role can only be assigned on team spaces"));
    }
    // Can't grant a role higher than your own (except administrators managing company / team spaces)
    if !user.is_admin() && my_role.is_some_and(|r| role > r) {
        return Err(AppError::forbidden("You can't grant a role higher than your own"));
    }
    let principal_id = if req.principal_type == "everyone" { 0 } else { req.principal_id };
    let name = principal_name(&mut tx, &req.principal_type, principal_id).await?.ok_or_else(|| AppError::bad_request("User or group not found"))?;
    // The grant replaced by this one: only someone with at least that role may change it, and an owner may only be
    // lowered or given an expiry while another owner remains
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT role FROM grants WHERE node_id = ? AND principal_type = ? AND principal_id = ?")
            .bind(&node.id)
            .bind(&req.principal_type)
            .bind(principal_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(old) = existing.and_then(|(r,)| Role::parse(&r)) {
        if !user.is_admin() && my_role.is_some_and(|r| old > r) {
            return Err(AppError::forbidden("You can't change the access of someone whose role is higher than yours"));
        }
        if old == Role::Owner && (role != Role::Owner || req.expires_at.is_some()) {
            require_other_owner(&mut tx, &node.id, &req.principal_type, principal_id).await?;
        }
    }
    add_grant(&mut tx, &node.id, &req.principal_type, principal_id, role.as_str(), Some(user.id), req.expires_at).await?;
    tree::log(&mut tx, &user, Some(&node), "grant", &format!("{name} → {}", role_label(role))).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn revoke(State(st): State<AppState>, user: User, Path(grant_id): Path<i64>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let (node_id, principal_type, principal_id, role): (String, String, i64, String) =
        sqlx::query_as("SELECT node_id, principal_type, principal_id, role FROM grants WHERE id = ?")
            .bind(grant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::not_found("Access entry not found"))?;
    let node = tree::get_node(&mut tx, &node_id).await?.ok_or_else(|| AppError::not_found("Item not found"))?;
    let drive = tree::get_drive(&mut tx, node.drive()).await?.ok_or_else(|| AppError::not_found("Space not found"))?;
    // Users can leave items others shared with them
    let leaving = principal_type == "user" && principal_id == user.id && role != "owner";
    if !leaving {
        let (can_manage, my_role) = can_manage_node(&mut tx, &user, &node, &drive).await?;
        if !can_manage {
            return Err(AppError::forbidden("You don't have permission to manage access"));
        }
        if !user.is_admin() && my_role.is_some_and(|mine| Role::parse(&role).is_some_and(|r| r > mine)) {
            return Err(AppError::forbidden("You can't change the access of someone whose role is higher than yours"));
        }
    }
    if drive.kind == "personal" && node.parent_id.is_none() {
        return Err(AppError::bad_request("The owner of a personal space can't be removed"));
    }
    if role == "owner" {
        require_other_owner(&mut tx, &node_id, &principal_type, principal_id).await?;
    }
    let name = principal_name(&mut tx, &principal_type, principal_id).await?.unwrap_or_default();
    sqlx::query("DELETE FROM grants WHERE id = ?").bind(grant_id).execute(&mut *tx).await?;
    tree::log(&mut tx, &user, Some(&node), "revoke", &name).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn principal_name(conn: &mut SqliteConnection, kind: &str, id: i64) -> AppResult<Option<String>> {
    Ok(match kind {
        "user" => sqlx::query_as::<_, (String,)>("SELECT username FROM users WHERE id = ?").bind(id).fetch_optional(conn).await?.map(|r| r.0),
        "group" => sqlx::query_as::<_, (String,)>("SELECT name FROM groups WHERE id = ?").bind(id).fetch_optional(conn).await?.map(|r| r.0),
        _ => Some("Everyone".into()),
    })
}

fn role_label(r: Role) -> &'static str {
    match r {
        Role::Viewer => "Viewer",
        Role::Editor => "Editor",
        Role::Manager => "Manager",
        Role::Owner => "Owner",
    }
}

// ───────────── Directory (choosing who to grant access) ─────────────

#[derive(Serialize, sqlx::FromRow)]
pub struct Principal {
    principal_type: String,
    principal_id: i64,
    name: String,
    detail: String,
}

#[derive(Deserialize)]
pub struct DirectoryQuery {
    #[serde(default)]
    q: String,
}

pub async fn directory(State(st): State<AppState>, _: User, Query(q): Query<DirectoryQuery>) -> AppResult<Json<Vec<Principal>>> {
    let term = format!("%{}%", q.q.trim().replace(['%', '_'], ""));
    let rows: Vec<Principal> = sqlx::query_as(
        "SELECT 'group' AS principal_type, g.id AS principal_id, g.name,
                (SELECT CASE COUNT(*) WHEN 1 THEN '1 member' ELSE COUNT(*) || ' members' END FROM group_members WHERE group_id = g.id) AS detail
         FROM groups g WHERE g.name LIKE ?1
         UNION ALL
         SELECT 'user', u.id, u.username, CASE u.role WHEN 'admin' THEN 'Administrator' ELSE 'User' END
         FROM users u WHERE u.disabled = 0 AND u.username LIKE ?1
         ORDER BY 1 DESC, 3 LIMIT 30",
    )
    .bind(term)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(rows))
}

// ───────────── Groups (administrators) ─────────────

#[derive(Serialize)]
pub struct GroupInfo {
    id: i64,
    name: String,
    description: String,
    created_at: i64,
    members: Vec<GroupMember>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct GroupMember {
    id: i64,
    username: String,
}

pub async fn list_groups(State(st): State<AppState>, _: Admin) -> AppResult<Json<Vec<GroupInfo>>> {
    let groups: Vec<(i64, String, String, i64)> =
        sqlx::query_as("SELECT id, name, description, created_at FROM groups ORDER BY name").fetch_all(&st.db).await?;
    // Every group's members in one query
    let members: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT m.group_id, u.id, u.username FROM group_members m JOIN users u ON u.id = m.user_id ORDER BY u.username",
    )
    .fetch_all(&st.db)
    .await?;
    let mut by_group: HashMap<i64, Vec<GroupMember>> = HashMap::new();
    for (group_id, id, username) in members {
        by_group.entry(group_id).or_default().push(GroupMember { id, username });
    }
    let out = groups
        .into_iter()
        .map(|(id, name, description, created_at)| GroupInfo { id, name, description, created_at, members: by_group.remove(&id).unwrap_or_default() })
        .collect();
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct GroupReq {
    name: Option<String>,
    description: Option<String>,
    members: Option<Vec<i64>>,
}

async fn set_members(conn: &mut SqliteConnection, group_id: i64, members: &[i64]) -> AppResult<()> {
    sqlx::query("DELETE FROM group_members WHERE group_id = ?").bind(group_id).execute(&mut *conn).await?;
    for uid in members {
        sqlx::query("INSERT OR IGNORE INTO group_members (group_id, user_id) SELECT ?, id FROM users WHERE id = ?")
            .bind(group_id)
            .bind(uid)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

pub async fn create_group(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<GroupReq>) -> AppResult<Json<Value>> {
    let name = validate_name(req.name.as_deref().unwrap_or_default())?;
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let id = crate::db::next_id(&mut tx, crate::db::Counted::Groups).await?;
    sqlx::query("INSERT INTO groups (id, name, description, created_at) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(&name)
        .bind(req.description.unwrap_or_default())
        .bind(now())
        .execute(&mut *tx)
        .await
        .map_err(|e| if matches!(&e, sqlx::Error::Database(d) if d.is_unique_violation()) { AppError::conflict("A group with this name already exists") } else { e.into() })?;
    set_members(&mut tx, id, req.members.as_deref().unwrap_or_default()).await?;
    tree::log(&mut tx, &user, None, "group_create", &name).await?;
    tx.commit().await?;
    Ok(Json(json!({ "id": id })))
}

pub async fn update_group(
    State(st): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<i64>,
    Json(req): Json<GroupReq>,
) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    if let Some(name) = &req.name {
        let name = validate_name(name)?;
        sqlx::query("UPDATE groups SET name = ? WHERE id = ?").bind(&name).bind(id).execute(&mut *tx).await?;
    }
    if let Some(d) = &req.description {
        sqlx::query("UPDATE groups SET description = ? WHERE id = ?").bind(d).bind(id).execute(&mut *tx).await?;
    }
    if let Some(m) = &req.members {
        set_members(&mut tx, id, m).await?;
    }
    let name = principal_name(&mut tx, "group", id).await?.unwrap_or_default();
    tree::log(&mut tx, &user, None, "group_update", &name).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_group(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<i64>) -> AppResult<Json<Value>> {
    let _w = st.write_lock.lock().await;
    let mut tx = st.db.begin().await?;
    let name = principal_name(&mut tx, "group", id).await?.unwrap_or_default();
    sqlx::query("DELETE FROM grants WHERE principal_type = 'group' AND principal_id = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM groups WHERE id = ?").bind(id).execute(&mut *tx).await?;
    // Accounts created by single sign-on must no longer be added to it
    let mut sso = st.sso.read().unwrap().clone();
    let changed = sso.forget_group(id);
    if changed {
        crate::sso::store(&mut tx, &sso).await?;
    }
    tree::log(&mut tx, &user, None, "group_delete", &name).await?;
    tx.commit().await?;
    if changed {
        *st.sso.write().unwrap() = sso;
    }
    Ok(Json(json!({ "ok": true })))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[tokio::test]
    async fn a_deleted_group_leaves_the_sign_in_settings_and_its_id_isnt_reused() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let group = |name: &str| Json(GroupReq { name: Some(name.into()), description: None, members: None });
        let Json(g) = create_group(State(env.st.clone()), Admin(admin.clone()), group("Sales")).await.unwrap();
        let id = g["id"].as_i64().unwrap();
        {
            let mut sso = env.st.sso.write().unwrap();
            sso.google.groups = vec![id];
            sso.domain_rules = vec![crate::sso::DomainRule { domain: "example.com".into(), groups: vec![id, 99], ..Default::default() }];
        }
        let _ = delete_group(State(env.st.clone()), Admin(admin.clone()), Path(id)).await.unwrap();
        let sso = env.st.sso.read().unwrap().clone();
        assert!(sso.google.groups.is_empty() && sso.domain_rules[0].groups == vec![99]);
        let Json(again) = create_group(State(env.st.clone()), Admin(admin), group("Support")).await.unwrap();
        assert!(again["id"].as_i64().unwrap() > id);
    }

    #[tokio::test]
    async fn standard_users_can_create_a_limited_number_of_spaces() {
        let env = testutil::env().await;
        env.st.system.write().unwrap().allow_user_drives = true;
        let amy = env.user("amy", true).await;
        for i in 0..MAX_OWN_SPACES {
            let _ = create(State(env.st.clone()), amy.clone(), Json(CreateDriveReq { name: format!("Team {i}"), quota_bytes: 0, source_path: None, read_only: false })).await.unwrap();
        }
        let res = create(State(env.st.clone()), amy.clone(), Json(CreateDriveReq { name: "One more".into(), quota_bytes: 0, source_path: None, read_only: false })).await;
        assert!(matches!(res, Err(e) if e.status == axum::http::StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn shared_access_only_shows_grants_from_the_shared_folder_down() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let carol = env.user("carol", true).await;
        let private = env.folder(&amy, &amy.root_id, "Private").await;
        let shared = env.folder(&amy, &private, "Shared").await;
        let inner = env.folder(&amy, &shared, "Inner").await;
        env.grant(&private, &carol, "viewer").await;
        env.grant(&shared, &ben, "editor").await;
        // Ben only has "Shared": he sees the grant on it, not the ones on "Private" or on the space root
        let Json(info) = access(State(env.st.clone()), ben.clone(), Path(inner.clone())).await.unwrap();
        assert!(!info.can_manage);
        assert_eq!(info.inherited.iter().map(|g| g.node_id.as_str()).collect::<Vec<_>>(), vec![shared.as_str()]);
        // Amy, the owner, still sees everything
        let Json(info) = access(State(env.st.clone()), amy.clone(), Path(inner)).await.unwrap();
        assert!(info.inherited.iter().any(|g| g.node_id == private) && info.inherited.iter().any(|g| g.node_id == shared));
    }

    fn grant_req(to: &crate::auth::User, role: &str, expires_at: Option<i64>) -> Json<GrantReq> {
        Json(GrantReq { principal_type: "user".into(), principal_id: to.id, role: role.into(), expires_at })
    }

    async fn grant_id(env: &testutil::TestEnv, node: &str, to: &crate::auth::User) -> i64 {
        let (id,): (i64,) = sqlx::query_as("SELECT id FROM grants WHERE node_id = ? AND principal_type = 'user' AND principal_id = ?")
            .bind(node)
            .bind(to.id)
            .fetch_one(&env.st.db)
            .await
            .unwrap();
        id
    }

    #[tokio::test]
    async fn managers_cannot_change_or_remove_the_access_of_owners() {
        let env = testutil::env().await;
        let amy = env.user("amy", true).await;
        let ben = env.user("ben", true).await;
        let carol = env.user("carol", true).await;
        let root = {
            let mut conn = env.st.db.acquire().await.unwrap();
            let (_, root) = crate::db::create_drive(&mut conn, "Team", "team", amy.id, 0).await.unwrap();
            crate::db::add_grant(&mut conn, &root, "user", amy.id, "owner", Some(amy.id), None).await.unwrap();
            root
        };
        env.grant(&root, &ben, "manager").await;
        env.grant(&root, &carol, "owner").await;
        let st = || State(env.st.clone());

        // Ben, a manager, can neither lower an owner nor give them an expiry, nor remove them
        for req in [grant_req(&amy, "viewer", None), grant_req(&amy, "manager", None), grant_req(&amy, "viewer", Some(now() + 60))] {
            let err = grant(st(), ben.clone(), Path(root.clone()), req).await.unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
        }
        let err = revoke(st(), ben.clone(), Path(grant_id(&env, &root, &carol).await)).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
        // He still manages roles up to his own
        let dan = env.user("dan", true).await;
        let _ = grant(st(), ben.clone(), Path(root.clone()), grant_req(&dan, "manager", None)).await.unwrap();

        // Owners can lower each other, but the last owner stays, also when their access would expire
        let _ = grant(st(), amy.clone(), Path(root.clone()), grant_req(&carol, "manager", None)).await.unwrap();
        let err = grant(st(), amy.clone(), Path(root.clone()), grant_req(&amy, "manager", None)).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        let err = grant(st(), amy.clone(), Path(root.clone()), grant_req(&amy, "owner", Some(now() + 60))).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        let err = revoke(st(), amy.clone(), Path(grant_id(&env, &root, &amy).await)).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

        // An owner whose access has expired doesn't count as the remaining owner
        env.grant(&root, &carol, "owner").await;
        sqlx::query("UPDATE grants SET expires_at = ? WHERE node_id = ? AND principal_id = ?").bind(now() - 1).bind(&root).bind(carol.id).execute(&env.st.db).await.unwrap();
        let err = revoke(st(), amy.clone(), Path(grant_id(&env, &root, &amy).await)).await.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_space_created_by_a_standard_user_carries_their_quota() {
        let env = testutil::env().await;
        env.st.system.write().unwrap().allow_user_drives = true;
        let amy = env.user("amy", true).await;
        sqlx::query("UPDATE users SET quota_bytes = 5000 WHERE id = ?").bind(amy.id).execute(&env.st.db).await.unwrap();
        let mut conn = env.st.db.acquire().await.unwrap();
        let amy = crate::auth::user_by_id(&env.st, &mut conn, amy.id).await.unwrap().unwrap();
        let req = CreateDriveReq { name: "Team".into(), quota_bytes: 0, source_path: None, read_only: false };
        let Json(info) = create(State(env.st.clone()), amy, Json(req)).await.unwrap();
        assert_eq!(info.quota_bytes, 5000, "not unlimited, whatever the request said");
        let admin = env.admin().await;
        let Json(info) = create(State(env.st.clone()), admin, Json(CreateDriveReq { name: "Big".into(), quota_bytes: 0, source_path: None, read_only: false })).await.unwrap();
        assert_eq!(info.quota_bytes, 0, "administrators may create unlimited spaces");
    }
}
