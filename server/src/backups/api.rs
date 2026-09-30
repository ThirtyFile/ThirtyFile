//! Control panel › Backups: copies of storage locations, their jobs, and restoring from them. Administrators only.

use std::sync::atomic::Ordering;

use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use super::{
    SpaceInfo,
    runner::{self, ACTIVE, Failure},
};
use crate::{
    auth::{Admin, User},
    error::{AppError, AppResult},
    locations::Relation,
    state::AppState,
    util::{new_id, now},
};

/// Jobs listed (those not over, then the newest)
const HISTORY: i64 = 300;

#[derive(Serialize, sqlx::FromRow)]
pub struct SetInfo {
    id: String,
    kind: String,
    name: String,
    source_location: Option<String>,
    source_name: String,
    dest_location: String,
    dest_name: String,
    created_by_name: String,
    created_at: i64,
    removing: bool,
    /// Content held on the destination: how many and their bytes
    objects: i64,
    bytes: i64,
    #[sqlx(skip)]
    snapshots: Vec<SnapshotInfo>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct SnapshotInfo {
    id: String,
    #[serde(skip)]
    set_id: String,
    state: String,
    cutoff: Option<i64>,
    #[serde(skip)]
    space_list: String,
    #[sqlx(skip)]
    spaces: Vec<SpaceInfo>,
    folders: i64,
    files: i64,
    versions: i64,
    logical_bytes: i64,
    created_at: i64,
    completed_at: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct JobInfo {
    id: String,
    kind: String,
    set_id: String,
    snapshot_id: Option<String>,
    state: String,
    label: String,
    #[serde(skip)]
    params: String,
    files_total: i64,
    bytes_total: i64,
    files_done: i64,
    bytes_done: i64,
    failed_items: i64,
    #[serde(skip)]
    failures: String,
    error: Option<String>,
    note: Option<String>,
    created_by_name: String,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    #[sqlx(skip)]
    speed: Option<f64>,
    #[sqlx(skip)]
    #[serde(rename = "failures")]
    failure_list: Vec<Failure>,
    /// Restores: the space restored into, as its list names it
    #[sqlx(skip)]
    target: Option<String>,
}

#[derive(Serialize)]
pub struct Overview {
    sets: Vec<SetInfo>,
    jobs: Vec<JobInfo>,
}

/// How a space is named in lists: a personal space as "My files · owner"
fn space_label(name: &str, kind: &str, owner: &str) -> String {
    if kind == "personal" && !owner.is_empty() { format!("{name} · {owner}") } else { name.to_string() }
}

/// The copies, with their snapshots, and the jobs
pub async fn list(State(st): State<AppState>, _: Admin) -> AppResult<Json<Overview>> {
    let mut sets: Vec<SetInfo> = sqlx::query_as(
        "SELECT s.id, s.kind, s.name, s.source_location, s.source_name, s.dest_location, COALESCE(l.name, '') AS dest_name, s.created_by_name,
                s.created_at, s.removing,
                (SELECT COUNT(*) FROM backup_objects o WHERE o.set_id = s.id) AS objects,
                (SELECT COALESCE(SUM(size), 0) FROM backup_objects o WHERE o.set_id = s.id) AS bytes
         FROM backup_sets s LEFT JOIN storage_locations l ON l.id = s.dest_location ORDER BY s.created_at DESC",
    )
    .fetch_all(&st.db)
    .await?;
    let mut snapshots: Vec<SnapshotInfo> = sqlx::query_as(
        "SELECT id, set_id, state, cutoff, space_list, folders, files, versions, logical_bytes, created_at, completed_at FROM backup_snapshots ORDER BY created_at DESC",
    )
    .fetch_all(&st.db)
    .await?;
    for s in &mut snapshots {
        s.spaces = serde_json::from_str(&s.space_list).unwrap_or_default();
    }
    for set in &mut sets {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut snapshots).into_iter().partition(|s| s.set_id == set.id);
        set.snapshots = mine;
        snapshots = rest;
    }
    let mut jobs: Vec<JobInfo> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT id, kind, set_id, snapshot_id, state, label, params, files_total, bytes_total, files_done, bytes_done, failed_items, failures, error, note,
                created_by_name, created_at, started_at, finished_at
         FROM backup_jobs ORDER BY state IN {ACTIVE} DESC, created_at DESC, rowid DESC LIMIT {HISTORY}"
    )))
    .fetch_all(&st.db)
    .await?;
    for j in &mut jobs {
        j.failure_list = serde_json::from_str(&j.failures).unwrap_or_default();
        if let Some((fd, bd, ft, bt, rate)) = runner::live(&st, &j.id) {
            (j.files_done, j.bytes_done, j.files_total, j.bytes_total, j.speed) = (fd, bd, ft, bt, Some(rate));
        }
        if j.kind == "restore" {
            let target = serde_json::from_str::<Value>(&j.params).ok().and_then(|p| p["target_drive"].as_str().map(str::to_string));
            if let Some(drive) = target {
                let row: Option<(String, String, String)> = sqlx::query_as(
                    "SELECT d.name, d.kind, COALESCE(u.username, '') FROM drives d LEFT JOIN users u ON u.id = d.owner_id WHERE d.id = ?",
                )
                .bind(&drive)
                .fetch_optional(&st.db)
                .await?;
                j.target = row.map(|(n, k, o)| space_label(&n, &k, &o));
            }
        }
    }
    Ok(Json(Overview { sets, jobs }))
}

// ───────────── Copy everything ─────────────

#[derive(Deserialize)]
pub struct CopyReq {
    /// The location whose spaces are copied, and the location the copy goes to
    source: String,
    dest: String,
    /// The copy's name (default: "Copy of <source>")
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct CopySpace {
    id: String,
    name: String,
    kind: String,
    owner_name: String,
    mode: String,
    used_bytes: i64,
}

#[derive(Serialize)]
pub struct CopyPreview {
    source_name: String,
    dest_name: String,
    dest_kind: String,
    /// The spaces copied, with their trash and earlier versions
    spaces: Vec<CopySpace>,
    files: i64,
    trash_files: i64,
    versions: i64,
    /// Bytes of the files and versions, identical content counted every time
    bytes: i64,
    /// What goes to the destination: each content once
    content_bytes: i64,
    /// On a disk of this server: its free space; None when it can't be told (S3, SFTP, FTP)
    free_bytes: Option<u64>,
    /// On the same disk or storage service as the source: the copy doesn't survive its failure
    shared: bool,
    /// FTP without TLS: the files travel unencrypted
    unencrypted: bool,
    /// Why the destination can't be used now (unreachable), None when it can
    problem: Option<String>,
}

/// The locations' names and kinds, checked for a copy from `source` to `dest`
async fn check_pair(st: &AppState, source: &str, dest: &str) -> AppResult<(String, String, String, Relation)> {
    let name_of = |id: &str| {
        let id = id.to_string();
        async move {
            sqlx::query_as::<_, (String, String, String)>("SELECT name, kind, config FROM storage_locations WHERE id = ?")
                .bind(&id)
                .fetch_optional(&st.db)
                .await?
                .ok_or_else(|| AppError::not_found("Storage location not found"))
        }
    };
    let (source_name, ..) = name_of(source).await?;
    let (dest_name, dest_kind, _) = name_of(dest).await?;
    if source == dest {
        return Err(AppError::bad_request("Choose another location to copy to"));
    }
    let relation = crate::locations::relation(st, source, dest).await?;
    if relation == Relation::Nested {
        return Err(AppError::bad_request("These locations are in the same place, or one is inside the other: choose a location somewhere else"));
    }
    Ok((source_name, dest_name, dest_kind, relation))
}

/// The spaces on a location, as a copy of it takes them
async fn spaces_on(conn: &mut SqliteConnection, location: &str) -> AppResult<Vec<CopySpace>> {
    Ok(sqlx::query_as(
        "SELECT d.id, d.name, d.kind, CASE WHEN d.kind = 'personal' THEN COALESCE(u.username, '') ELSE '' END AS owner_name, d.mode, d.used_bytes
         FROM drives d LEFT JOIN users u ON u.id = d.owner_id WHERE d.location_id = ?
         ORDER BY CASE d.kind WHEN 'company' THEN 0 WHEN 'team' THEN 1 ELSE 2 END, d.name, owner_name",
    )
    .bind(location)
    .fetch_all(conn)
    .await?)
}

/// Free space on the disk holding a location of this server's disks; None for other kinds, or when it can't be told
async fn free_on(st: &AppState, location: &str) -> AppResult<Option<u64>> {
    let (kind, config): (String, String) = sqlx::query_as("SELECT kind, config FROM storage_locations WHERE id = ?").bind(location).fetch_one(&st.db).await?;
    if kind != "local" {
        return Ok(None);
    }
    let Ok(root) = crate::storage::local_root(location, &crate::locations::config_json(location, &config), &st.storage_dir) else { return Ok(None) };
    Ok(crate::util::disk_space_soon(&root).await.map(|(free, _)| free))
}

/// What copying everything on `source` to `dest` would copy, and whether the destination can take it
pub async fn copy_preview(State(st): State<AppState>, _: Admin, Json(req): Json<CopyReq>) -> AppResult<Json<CopyPreview>> {
    let (source_name, dest_name, dest_kind, relation) = check_pair(&st, &req.source, &req.dest).await?;
    let spaces = spaces_on(&mut *st.db.acquire().await?, &req.source).await?;
    let ids = serde_json::to_string(&spaces.iter().map(|s| &s.id).collect::<Vec<_>>()).unwrap();
    let (files, trash_files, file_bytes): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(trashed_at IS NOT NULL), 0), COALESCE(SUM(size), 0) FROM nodes
         WHERE kind = 'file' AND drive_id IN (SELECT value FROM json_each(?))",
    )
    .bind(&ids)
    .fetch_one(&st.db)
    .await?;
    let (versions, version_bytes): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(v.size), 0) FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE n.drive_id IN (SELECT value FROM json_each(?))",
    )
    .bind(&ids)
    .fetch_one(&st.db)
    .await?;
    // Content-store content once; folder files and versions as they are
    let (store_bytes,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(size), 0) FROM blobs WHERE hash IN (
           SELECT blob_hash FROM nodes WHERE drive_id IN (SELECT value FROM json_each(?1)) AND blob_hash IS NOT NULL
           UNION SELECT v.blob_hash FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE n.drive_id IN (SELECT value FROM json_each(?1)) AND v.blob_hash IS NOT NULL)",
    )
    .bind(&ids)
    .fetch_one(&st.db)
    .await?;
    let (folder_bytes,): (i64,) = sqlx::query_as(
        "SELECT (SELECT COALESCE(SUM(size), 0) FROM nodes WHERE kind = 'file' AND fs_path IS NOT NULL AND drive_id IN (SELECT value FROM json_each(?1)))
              + (SELECT COALESCE(SUM(size), 0) FROM node_versions WHERE fs_path IS NOT NULL AND drive_id IN (SELECT value FROM json_each(?1)))",
    )
    .bind(&ids)
    .fetch_one(&st.db)
    .await?;
    let (config,): (String,) = sqlx::query_as("SELECT config FROM storage_locations WHERE id = ?").bind(&req.dest).fetch_one(&st.db).await?;
    let unencrypted = dest_kind == "ftp" && !crate::locations::config_json(&req.dest, &config)["tls"].as_bool().unwrap_or(false);
    let problem = crate::locations::probe(&st, &req.dest).await.err();
    Ok(Json(CopyPreview {
        source_name,
        dest_name,
        dest_kind,
        spaces,
        files,
        trash_files,
        versions,
        bytes: file_bytes + version_bytes,
        content_bytes: store_bytes + folder_bytes,
        free_bytes: free_on(&st, &req.dest).await?,
        shared: relation == Relation::Shared,
        unencrypted,
        problem,
    }))
}

/// Starts copying everything on a location to another: a new copy, made by a job in the background
pub async fn copy(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<CopyReq>) -> AppResult<Json<Value>> {
    let (source_name, dest_name, _, _) = check_pair(&st, &req.source, &req.dest).await?;
    crate::locations::probe(&st, &req.dest).await.map_err(|e| AppError::bad_request(format!("The location to copy to can't be reached: {e}")))?;
    let name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if n.chars().count() > 200 => return Err(AppError::bad_request("Name is too long")),
        Some(n) => n.to_string(),
        None => format!("Copy of {source_name}"),
    };
    let spaces = spaces_on(&mut *st.db.acquire().await?, &req.source).await?;
    if spaces.is_empty() {
        return Err(AppError::bad_request("There are no spaces on this location to copy"));
    }
    // On a disk of this server: room for the content
    if let Some(free) = free_on(&st, &req.dest).await? {
        let Json(preview) = copy_preview(State(st.clone()), Admin(user.clone()), Json(CopyReq { source: req.source.clone(), dest: req.dest.clone(), name: None })).await?;
        if (free as i64) < preview.content_bytes {
            return Err(AppError::bad_request(format!(
                "There isn't enough free space on {dest_name}: {needed} is needed, {free} is free",
                needed = crate::util::format_bytes_u64(preview.content_bytes.max(0) as u64),
                free = crate::util::format_bytes_u64(free)
            )));
        }
    }
    let (set_id, job_id) = (new_id(), new_id());
    let params = json!({ "spaces": spaces.iter().map(|s| &s.id).collect::<Vec<_>>(), "versions": true, "trash": true });
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query(
                "INSERT INTO backup_sets (id, kind, name, source_location, source_name, dest_location, created_by, created_by_name, created_at)
                 VALUES (?, 'copy', ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&set_id)
            .bind(&name)
            .bind(&req.source)
            .bind(&source_name)
            .bind(&req.dest)
            .bind(user.id)
            .bind(&user.username)
            .bind(now())
            .execute(&mut *tx)
            .await?;
            insert_job(&mut tx, &user, &job_id, "snapshot", &set_id, None, &params, &name).await?;
            crate::logs::record_activity(&mut tx, &user, None, "backup_copy", &format!("{name}: {source_name} → {dest_name}")).await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    st.backups.wake.notify_one();
    Ok(Json(json!({ "set_id": set_id, "job_id": job_id })))
}

#[allow(clippy::too_many_arguments)]
async fn insert_job(conn: &mut SqliteConnection, user: &User, id: &str, kind: &str, set: &str, snapshot: Option<&str>, params: &Value, label: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO backup_jobs (id, kind, set_id, snapshot_id, params, label, created_by, created_by_name, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(kind)
    .bind(set)
    .bind(snapshot)
    .bind(params.to_string())
    .bind(label)
    .bind(user.id)
    .bind(&user.username)
    .bind(now())
    .execute(conn)
    .await?;
    Ok(())
}

/// Queues the removal of a set from its destination (once: a removal already queued stays)
pub(super) async fn queue_remove(conn: &mut SqliteConnection, by: Option<i64>, by_name: &str, set: &str) -> AppResult<()> {
    let queued: Option<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT 1 FROM backup_jobs WHERE set_id = ? AND kind = 'remove' AND state IN {ACTIVE}")))
        .bind(set)
        .fetch_optional(&mut *conn)
        .await?;
    if queued.is_some() {
        return Ok(());
    }
    let (name,): (String,) = sqlx::query_as("SELECT name FROM backup_sets WHERE id = ?").bind(set).fetch_one(&mut *conn).await?;
    sqlx::query("UPDATE backup_sets SET removing = 1 WHERE id = ?").bind(set).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO backup_jobs (id, kind, set_id, label, created_by, created_by_name, created_at) VALUES (?, 'remove', ?, ?, ?, ?, ?)")
        .bind(new_id())
        .bind(set)
        .bind(name)
        .bind(by)
        .bind(by_name)
        .bind(now())
        .execute(conn)
        .await?;
    Ok(())
}

// ───────────── Jobs ─────────────

async fn job_state(st: &AppState, id: &str) -> AppResult<(runner::Job, String)> {
    let job = runner::job(&mut *st.db.acquire().await?, id).await?.ok_or_else(|| AppError::not_found("This job no longer exists"))?;
    let (state,): (String,) = sqlx::query_as("SELECT state FROM backup_jobs WHERE id = ?").bind(id).fetch_one(&st.db).await?;
    Ok((job, state))
}

/// Pauses a job: a running one stops after the item it is working on, keeping what it did
pub async fn pause(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    if let Some(ctl) = st.backups.running.lock().unwrap().get(&id) {
        ctl.pause.store(true, Ordering::SeqCst);
        return Ok(Json(json!({ "ok": true })));
    }
    let _w = st.write_lock.lock().await;
    let n = sqlx::query("UPDATE backup_jobs SET state = 'paused' WHERE id = ? AND state IN ('queued', 'waiting')").bind(&id).execute(&st.db).await?.rows_affected();
    if n == 0 {
        return Err(AppError::conflict("This job can't be paused now"));
    }
    Ok(Json(json!({ "ok": true })))
}

/// Resumes a paused or failed job (or tries one again): it waits for its turn, then continues where it stopped
pub async fn resume(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    {
        let _w = st.write_lock.lock().await;
        let n = sqlx::query("UPDATE backup_jobs SET state = 'queued', error = NULL WHERE id = ? AND state IN ('paused', 'failed', 'waiting')")
            .bind(&id)
            .execute(&st.db)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(AppError::conflict("This job can't be resumed now"));
        }
    }
    st.backups.wake.notify_one();
    Ok(Json(json!({ "ok": true })))
}

/// Cancels a job. The source is never changed; a copy that never completed is removed from its destination.
pub async fn cancel(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let (job, state) = job_state(&st, &id).await?;
    if job.kind == "remove" {
        return Err(AppError::conflict("Deleting a copy can't be cancelled: a copy half deleted can't be restored from. Pause it instead."));
    }
    if let Some(ctl) = st.backups.running.lock().unwrap().get(&id) {
        ctl.cancel.store(true, Ordering::SeqCst);
        return Ok(Json(json!({ "ok": true })));
    }
    if !matches!(state.as_str(), "queued" | "paused" | "failed" | "waiting") {
        return Err(AppError::conflict("This job can't be cancelled now"));
    }
    super::cancelled(&st, &job).await?;
    Ok(Json(json!({ "ok": true })))
}

// ───────────── Sets ─────────────

/// No job of the set is under way (except `except`): a set is deleted or checked only then
async fn set_idle(conn: &mut SqliteConnection, set: &str) -> AppResult<()> {
    let busy: Option<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT kind FROM backup_jobs WHERE set_id = ? AND state IN {ACTIVE} LIMIT 1")))
        .bind(set)
        .fetch_optional(conn)
        .await?;
    match busy.as_ref().map(|b| b.0.as_str()) {
        None => Ok(()),
        Some("remove") => Err(AppError::conflict("This copy is being deleted")),
        Some(_) => Err(AppError::conflict("A job of this copy isn't finished: wait until it is, or cancel it first")),
    }
}

/// Reads a set back from its destination and checks it
pub async fn verify(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let set = super::load_set(&st.db, &id).await?;
    let job_id = new_id();
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            set_idle(&mut tx, &id).await?;
            let (complete,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM backup_snapshots WHERE set_id = ? AND state = 'complete'").bind(&id).fetch_one(&mut *tx).await?;
            if complete == 0 {
                return Err(AppError::conflict("This copy isn't complete: there is nothing to check yet"));
            }
            insert_job(&mut tx, &user, &job_id, "verify", &id, None, &json!({}), &set.name).await
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    st.backups.wake.notify_one();
    Ok(Json(json!({ "job_id": job_id })))
}

/// Deletes a set from its destination (in the background) and from the list
pub async fn delete(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let set = super::load_set(&st.db, &id).await?;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            set_idle(&mut tx, &id).await?;
            queue_remove(&mut tx, Some(user.id), &user.username, &id).await?;
            crate::logs::record_activity(&mut tx, &user, None, "backup_delete_start", &set.name).await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    st.backups.wake.notify_one();
    Ok(Json(json!({ "ok": true })))
}

// ───────────── Restoring ─────────────

#[derive(Deserialize)]
pub struct RestoreReq {
    /// The space of the snapshot to restore
    space: String,
    /// The space to restore into (default: the space itself, when it is still there)
    #[serde(default)]
    target_drive: Option<String>,
    /// Items that were in the trash too
    #[serde(default)]
    trash: bool,
    /// The browser's time zone offset (minutes, as `Date.getTimezoneOffset`), for the new folder's name
    #[serde(default)]
    tz: i64,
}

#[derive(Serialize)]
pub struct RestorePreview {
    /// The space as the snapshot has it
    space: SpaceInfo,
    /// Where it goes: the space and the new folder's name
    target_drive: Option<String>,
    target_name: Option<String>,
    folder_name: String,
    /// Spaces it can go into (a personal space only goes back into its owner's)
    targets: Vec<Target>,
    /// Why it can't be restored now, None when it can
    problem: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Target {
    id: String,
    name: String,
    kind: String,
}

/// A complete snapshot, its set, and one of its spaces
async fn snapshot_space(st: &AppState, snapshot: &str, space: &str) -> AppResult<(super::Set, SpaceInfo)> {
    let (set_id, state, list): (String, String, String) = sqlx::query_as("SELECT set_id, state, space_list FROM backup_snapshots WHERE id = ?")
        .bind(snapshot)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("This snapshot no longer exists"))?;
    if state != "complete" {
        return Err(AppError::conflict("This snapshot isn't complete, so it can't be restored from"));
    }
    let set = super::load_set(&st.db, &set_id).await?;
    if set.removing {
        return Err(AppError::conflict("This copy is being deleted"));
    }
    let spaces: Vec<SpaceInfo> = serde_json::from_str(&list).unwrap_or_default();
    let info = spaces.into_iter().find(|s| s.id == space).ok_or_else(|| AppError::not_found("This space isn't in the snapshot"))?;
    Ok((set, info))
}

/// The spaces a snapshot's space can be restored into: its owner's personal space for a personal space (the same
/// account, by its id), else the company space and team spaces
async fn targets_for(st: &AppState, space: &SpaceInfo) -> AppResult<Vec<Target>> {
    Ok(if space.kind == "personal" {
        sqlx::query_as(
            "SELECT d.id, d.name, d.kind FROM drives d JOIN users u ON u.root_id = d.root_id
             WHERE d.kind = 'personal' AND d.owner_id = ? AND d.disabled = 0",
        )
        .bind(space.owner_id)
        .fetch_all(&st.db)
        .await?
    } else {
        sqlx::query_as("SELECT id, name, kind FROM drives WHERE kind IN ('company', 'team') AND disabled = 0 ORDER BY kind, name").fetch_all(&st.db).await?
    })
}

fn folder_name(space: &SpaceInfo, cutoff: i64, tz: i64) -> String {
    let when = crate::logs::format_time(cutoff, -tz.clamp(-14 * 60, 14 * 60) * 60);
    // "2026-10-01 14:05:00" → "2026-10-01 14.05" (names can't hold a colon)
    let when = when.get(..16).unwrap_or(&when).replace(':', ".");
    format!("Restored {} {when}", space.name)
}

async fn restore_plan(st: &AppState, snapshot: &str, req: &RestoreReq) -> AppResult<(super::Set, RestorePreview)> {
    let (set, space) = snapshot_space(st, snapshot, &req.space).await?;
    let (cutoff,): (Option<i64>,) = sqlx::query_as("SELECT cutoff FROM backup_snapshots WHERE id = ?").bind(snapshot).fetch_one(&st.db).await?;
    let targets = targets_for(st, &space).await?;
    let wanted = req.target_drive.clone().or_else(|| targets.iter().find(|t| t.id == space.id).map(|t| t.id.clone())).or_else(|| {
        // A personal space goes back into its owner's personal space (made again since, maybe)
        (space.kind == "personal").then(|| targets.first().map(|t| t.id.clone())).flatten()
    });
    let target = wanted.as_ref().and_then(|w| targets.iter().find(|t| &t.id == w));
    let problem = match (&wanted, target) {
        (Some(_), None) if space.kind == "personal" => Some("A personal space can only be restored into its owner's personal space".to_string()),
        (Some(_), None) => Some("Choose a company or team space to restore into".to_string()),
        (None, _) if space.kind == "personal" => Some("The owner of this personal space no longer has one".to_string()),
        (None, _) => Some("The space is no longer there: choose a space to restore into".to_string()),
        _ => None,
    };
    let preview = RestorePreview {
        folder_name: folder_name(&space, cutoff.unwrap_or_default(), req.tz),
        target_drive: target.map(|t| t.id.clone()),
        target_name: target.map(|t| t.name.clone()),
        space,
        targets,
        problem,
    };
    Ok((set, preview))
}

/// What restoring a space of a snapshot would do
pub async fn restore_preview(State(st): State<AppState>, _: Admin, Path(id): Path<String>, Json(req): Json<RestoreReq>) -> AppResult<Json<RestorePreview>> {
    Ok(Json(restore_plan(&st, &id, &req).await?.1))
}

/// Restores a space of a snapshot into a new folder, in the background
pub async fn restore(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>, Json(req): Json<RestoreReq>) -> AppResult<Json<Value>> {
    let (set, plan) = restore_plan(&st, &id, &req).await?;
    if let Some(problem) = plan.problem {
        return Err(AppError::bad_request(problem));
    }
    let target = plan.target_drive.clone().ok_or_else(|| AppError::bad_request("Choose a space to restore into"))?;
    let params = super::restore::Params {
        space: plan.space.id.clone(),
        space_kind: plan.space.kind.clone(),
        folder: None,
        target_drive: target,
        target_parent: None,
        folder_name: plan.folder_name.clone(),
        trash: req.trash,
    };
    let label = space_label(&plan.space.name, &plan.space.kind, &plan.space.owner);
    let job_id = new_id();
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            insert_job(&mut tx, &user, &job_id, "restore", &set.id, Some(&id), &serde_json::to_value(&params).unwrap(), &label).await?;
            crate::logs::record_activity(&mut tx, &user, None, "backup_restore", &format!("{label} ({})", set.name)).await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    st.backups.wake.notify_one();
    Ok(Json(json!({ "job_id": job_id })))
}
