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
    /// A policy's settings and how it is doing (sets made by a policy)
    #[sqlx(skip)]
    policy: Option<PolicyView>,
}

/// A backup policy as Control panel › Backups shows it
#[derive(Serialize)]
pub struct PolicyView {
    #[serde(flatten)]
    settings: super::policy::Policy,
    schedule: Value,
    /// The spaces chosen (when it doesn't take every space of its location)
    spaces: Vec<String>,
    health: super::policy::Health,
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
    let mut c = st.db.acquire().await?;
    for set in &mut sets {
        if let Some(settings) = super::policy::load(&mut c, &set.id).await? {
            let spaces = sqlx::query_as::<_, (String,)>("SELECT drive_id FROM backup_policy_spaces WHERE set_id = ?")
                .bind(&set.id)
                .fetch_all(&mut *c)
                .await?
                .into_iter()
                .map(|(d,)| d)
                .collect();
            let health = super::policy::health(&mut c, &settings, now()).await?;
            let schedule = serde_json::from_str(&settings.schedule).unwrap_or_default();
            set.policy = Some(PolicyView { settings, schedule, spaces, health });
        }
    }
    drop(c);
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
        if let Some((fd, bd, ft, bt, rate)) = runner::live(&st.backups, &j.id) {
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

// ───────────── Policies ─────────────

#[derive(Deserialize)]
pub struct PolicyReq {
    #[serde(default)]
    name: Option<String>,
    /// The location whose spaces are backed up, and where the backup goes (set when the policy is made)
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    dest: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    schedule: Option<Value>,
    #[serde(default)]
    tz: Option<String>,
    #[serde(default)]
    all_spaces: Option<bool>,
    #[serde(default)]
    spaces: Option<Vec<String>>,
    #[serde(default)]
    versions: Option<bool>,
    #[serde(default)]
    trash: Option<bool>,
    #[serde(default)]
    keep_days: Option<i64>,
    #[serde(default)]
    keep_min: Option<i64>,
    #[serde(default)]
    rate_limit: Option<i64>,
    #[serde(default)]
    alert_hours: Option<i64>,
    #[serde(default)]
    verify_days: Option<i64>,
}

/// A policy's settings, checked: those not given are taken from `current` (or the defaults)
struct Settings {
    enabled: bool,
    mode: String,
    schedule: Value,
    tz: String,
    all_spaces: bool,
    spaces: Vec<String>,
    versions: bool,
    trash: bool,
    keep_days: i64,
    keep_min: i64,
    rate_limit: i64,
    alert_hours: i64,
    verify_days: i64,
}

async fn settings(conn: &mut SqliteConnection, req: &PolicyReq, current: Option<(&super::policy::Policy, Vec<String>)>) -> AppResult<Settings> {
    let cur = current.as_ref().map(|c| c.0);
    let mode = req.mode.clone().or_else(|| cur.map(|c| c.mode.clone())).unwrap_or_else(|| "scheduled".into());
    if !matches!(mode.as_str(), "realtime" | "scheduled" | "both") {
        return Err(AppError::bad_request("Choose when backups are made"));
    }
    let schedule = req
        .schedule
        .clone()
        .or_else(|| cur.and_then(|c| serde_json::from_str(&c.schedule).ok()))
        .unwrap_or_else(|| json!({ "daily": "03:00" }));
    let tz = req.tz.clone().or_else(|| cur.map(|c| c.tz.clone())).unwrap_or_else(|| "UTC".into());
    super::policy::time_zone(&tz)?;
    let schedule = if mode == "realtime" { schedule } else { super::policy::Schedule::parse(&schedule)?.to_json() };
    let all_spaces = req.all_spaces.or(cur.map(|c| c.all_spaces)).unwrap_or(true);
    let spaces = req.spaces.clone().or_else(|| current.as_ref().map(|c| c.1.clone())).unwrap_or_default();
    if !all_spaces {
        let (known,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM drives WHERE id IN (SELECT value FROM json_each(?))")
            .bind(serde_json::to_string(&spaces).unwrap())
            .fetch_one(&mut *conn)
            .await?;
        if spaces.is_empty() || known != spaces.len() as i64 {
            return Err(AppError::bad_request("Choose the spaces to back up"));
        }
    }
    let num = |v: Option<i64>, c: Option<i64>, default: i64, range: std::ops::RangeInclusive<i64>, what: &str| -> AppResult<i64> {
        let n = v.or(c).unwrap_or(default);
        if !range.contains(&n) {
            return Err(AppError::bad_request(format!("{what} must be between {} and {}", range.start(), range.end())));
        }
        Ok(n)
    };
    Ok(Settings {
        enabled: req.enabled.or(cur.map(|c| c.enabled)).unwrap_or(true),
        mode,
        schedule,
        tz,
        all_spaces,
        spaces,
        versions: req.versions.or(cur.map(|c| c.versions)).unwrap_or(true),
        trash: req.trash.or(cur.map(|c| c.trash)).unwrap_or(true),
        keep_days: num(req.keep_days, cur.map(|c| c.keep_days), 30, 1..=3650, "Days snapshots are kept")?,
        keep_min: num(req.keep_min, cur.map(|c| c.keep_min), 1, 1..=1000, "Snapshots always kept")?,
        rate_limit: num(req.rate_limit, cur.map(|c| c.rate_limit), 0, 0..=1 << 40, "The speed limit")?,
        alert_hours: num(req.alert_hours, cur.map(|c| c.alert_hours), 48, 0..=8760, "Hours before a backup is overdue")?,
        verify_days: num(req.verify_days, cur.map(|c| c.verify_days), 7, 0..=365, "Days between checks")?,
    })
}

/// Writes a policy's settings (the row exists)
async fn save_settings(conn: &mut SqliteConnection, set: &str, s: &Settings) -> AppResult<()> {
    sqlx::query(
        "UPDATE backup_policies SET enabled = ?, mode = ?, schedule = ?, tz = ?, all_spaces = ?, versions = ?, trash = ?, keep_days = ?, keep_min = ?,
                                    rate_limit = ?, alert_hours = ?, verify_days = ?, next_run_at = NULL, updated_at = ?
         WHERE set_id = ?",
    )
    .bind(s.enabled)
    .bind(&s.mode)
    .bind(s.schedule.to_string())
    .bind(&s.tz)
    .bind(s.all_spaces)
    .bind(s.versions)
    .bind(s.trash)
    .bind(s.keep_days)
    .bind(s.keep_min)
    .bind(s.rate_limit)
    .bind(s.alert_hours)
    .bind(s.verify_days)
    .bind(now())
    .bind(set)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM backup_policy_spaces WHERE set_id = ?").bind(set).execute(&mut *conn).await?;
    if !s.all_spaces {
        sqlx::query("INSERT INTO backup_policy_spaces (set_id, drive_id) SELECT ?, value FROM json_each(?)")
            .bind(set)
            .bind(serde_json::to_string(&s.spaces).unwrap())
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// The next times a schedule makes snapshots, in its time zone (for the page to show)
#[derive(Deserialize)]
pub struct NextRunsReq {
    schedule: Value,
    tz: String,
}

pub async fn next_runs(_: Admin, Json(req): Json<NextRunsReq>) -> AppResult<Json<Vec<i64>>> {
    let s = super::policy::Schedule::parse(&req.schedule)?;
    let tz = super::policy::time_zone(&req.tz)?;
    let mut out = Vec::new();
    let mut at = now();
    for _ in 0..3 {
        let Some(next) = super::policy::next_after(&s, &tz, at) else { break };
        out.push(next);
        at = next;
    }
    Ok(Json(out))
}

/// Makes a backup policy: a set on `dest` whose snapshots of the spaces of `source` are made by the scheduler (the
/// first one right away)
pub async fn create_policy(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<PolicyReq>) -> AppResult<Json<Value>> {
    let (source, dest) = (req.source.clone().unwrap_or_default(), req.dest.clone().unwrap_or_default());
    let (source_name, dest_name, _, _) = check_pair(&st, &source, &dest).await?;
    crate::locations::probe(&st, &dest).await.map_err(|e| AppError::bad_request(format!("The location to copy to can't be reached: {e}")))?;
    let name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if n.chars().count() > 200 => return Err(AppError::bad_request("Name is too long")),
        Some(n) => n.to_string(),
        None => format!("Backup of {source_name}"),
    };
    let set_id = new_id();
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            let s = settings(&mut tx, &req, None).await?;
            sqlx::query(
                "INSERT INTO backup_sets (id, kind, name, source_location, source_name, dest_location, created_by, created_by_name, created_at)
                 VALUES (?, 'policy', ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&set_id)
            .bind(&name)
            .bind(&source)
            .bind(&source_name)
            .bind(&dest)
            .bind(user.id)
            .bind(&user.username)
            .bind(now())
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO backup_policies (set_id, mode, created_at, updated_at) VALUES (?, ?, ?, ?)")
                .bind(&set_id)
                .bind(&s.mode)
                .bind(now())
                .bind(now())
                .execute(&mut *tx)
                .await?;
            save_settings(&mut tx, &set_id, &s).await?;
            crate::logs::record_activity(&mut tx, &user, None, "backup_policy_create", &format!("{name}: {source_name} → {dest_name}")).await?;
            AppResult::Ok(s.enabled)
        }
        .await;
        let enabled = crate::db::settle(tx, res).await?;
        if !enabled {
            return Ok(Json(json!({ "set_id": set_id })));
        }
    }
    // The first snapshot is made now: the schedule protects the spaces from then on
    super::policy::trigger(&st, &set_id, "manual", Some((user.id, user.username.clone()))).await?;
    st.backups.policies.notify_one();
    Ok(Json(json!({ "set_id": set_id })))
}

/// Changes a policy's settings; pausing it pauses the snapshot it is making, resuming it resumes that
pub async fn update_policy(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>, Json(req): Json<PolicyReq>) -> AppResult<Json<Value>> {
    let set = super::load_set(&st.db, &id).await?;
    if set.removing {
        return Err(AppError::conflict("This backup is being deleted"));
    }
    let (enabled, was) = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            let current = super::policy::load(&mut tx, &id).await?.ok_or_else(|| AppError::not_found("This backup policy no longer exists"))?;
            let spaces: Vec<String> = sqlx::query_as::<_, (String,)>("SELECT drive_id FROM backup_policy_spaces WHERE set_id = ?")
                .bind(&id)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|(d,)| d)
                .collect();
            let s = settings(&mut tx, &req, Some((&current, spaces))).await?;
            save_settings(&mut tx, &id, &s).await?;
            if let Some(name) = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                if name.chars().count() > 200 {
                    return Err(AppError::bad_request("Name is too long"));
                }
                sqlx::query("UPDATE backup_sets SET name = ? WHERE id = ?").bind(name).bind(&id).execute(&mut *tx).await?;
            }
            // Paused: a snapshot waiting for its turn waits until the policy is resumed
            if !s.enabled {
                sqlx::query("UPDATE backup_jobs SET state = 'paused' WHERE set_id = ? AND kind = 'snapshot' AND state IN ('queued', 'waiting')")
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
            } else if !current.enabled {
                sqlx::query("UPDATE backup_jobs SET state = 'queued', error = NULL WHERE set_id = ? AND kind = 'snapshot' AND state = 'paused'")
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
            }
            let what = if s.enabled == current.enabled { "settings changed" } else if s.enabled { "resumed" } else { "paused" };
            crate::logs::record_activity(&mut tx, &user, None, "backup_policy_update", &format!("{}: {what}", set.name)).await?;
            AppResult::Ok((s.enabled, current.enabled))
        }
        .await;
        crate::db::settle(tx, res).await?
    };
    if !enabled && was {
        // The policy's snapshot being made now stops after the item it is copying
        let running: Vec<String> = st.backups.running.lock().unwrap().keys().cloned().collect();
        let mine: Option<(String,)> = sqlx::query_as("SELECT id FROM backup_jobs WHERE set_id = ? AND kind = 'snapshot' AND id IN (SELECT value FROM json_each(?))")
            .bind(&id)
            .bind(serde_json::to_string(&running).unwrap())
            .fetch_optional(&st.db)
            .await?;
        if let Some((job,)) = mine
            && let Some(ctl) = st.backups.running.lock().unwrap().get(&job)
        {
            ctl.pause.store(true, Ordering::SeqCst);
        }
    }
    st.backups.wake.notify_one();
    st.backups.policies.notify_one();
    Ok(Json(json!({ "ok": true })))
}

/// Makes a snapshot of a policy now (or tries a failed one again)
pub async fn run_policy(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let set = super::load_set(&st.db, &id).await?;
    let p = super::policy::load(&mut *st.db.acquire().await?, &id).await?.ok_or_else(|| AppError::not_found("This backup policy no longer exists"))?;
    if !p.enabled {
        return Err(AppError::conflict("This backup policy is paused: resume it first"));
    }
    let job = super::policy::trigger(&st, &id, "manual", Some((user.id, user.username.clone()))).await?;
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = crate::logs::record_activity(&mut tx, &user, None, "backup_run", &set.name).await;
        crate::db::settle(tx, res).await?;
    }
    Ok(Json(json!({ "job_id": job })))
}

// ───────────── Jobs ─────────────

async fn job_state(st: &AppState, id: &str) -> AppResult<(runner::Job, runner::JobState)> {
    let job = runner::job(&mut *st.db.acquire().await?, id).await?.ok_or_else(|| AppError::not_found("This job no longer exists"))?;
    let (state,): (runner::JobState,) = sqlx::query_as("SELECT state FROM backup_jobs WHERE id = ?").bind(id).fetch_one(&st.db).await?;
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
    if !matches!(state, runner::JobState::Queued | runner::JobState::Paused | runner::JobState::Failed | runner::JobState::Waiting) {
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

// ───────────── Backups found on a location ─────────────

#[derive(Deserialize)]
pub struct ImportReq {
    location: String,
}

/// Looks for copies and backups kept on a location that this server doesn't list (made before its database was lost,
/// or by another installation of ThirtyFile), and lists them: their complete snapshots can be restored from. Each is
/// then read back and checked in the background, which also records what it holds.
pub async fn import(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<ImportReq>) -> AppResult<Json<Value>> {
    let (location_name,): (String,) = sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?")
        .bind(&req.location)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Storage location not found"))?;
    crate::locations::probe(&st, &req.location).await.map_err(|e| AppError::bad_request(format!("The location can't be reached: {e}")))?;
    let dst = st.storage(&req.location)?;
    let read_error = |e: std::io::Error| AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("Couldn't read from the location: {}", crate::locations::describe(&e)));
    let sets = match dst.list_dir(super::layout::ROOT).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(read_error(e)),
    };
    let (mut found, mut added) = (0, Vec::new());
    for entry in sets.into_iter().filter(|e| e.kind == crate::storage::EntryKind::Folder) {
        let set_id = entry.name;
        let Some(body) = super::layout::read_small(dst.as_ref(), &super::layout::set_file(&set_id)).await.map_err(read_error)? else { continue };
        let Ok(info) = serde_json::from_slice::<super::layout::SetFile>(&body) else { continue };
        if info.format != super::layout::FORMAT || info.id != set_id {
            continue;
        }
        found += 1;
        let known: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM backup_sets WHERE id = ?").bind(&set_id).fetch_optional(&st.db).await?;
        if known.is_some() {
            continue;
        }
        // Complete snapshots only: one without its completion marker is never a restore point
        let snapshot_dir = format!("{}/snapshots", super::layout::set_dir(&set_id));
        let mut complete = Vec::new();
        for s in dst.list_dir(&snapshot_dir).await.or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(Vec::new()) } else { Err(e) }).map_err(read_error)? {
            let Some(body) = super::layout::read_small(dst.as_ref(), &super::layout::complete_key(&set_id, &s.name)).await.map_err(read_error)? else { continue };
            if let Ok(c) = serde_json::from_slice::<super::layout::Complete>(&body)
                && c.format == super::layout::FORMAT
                && c.set == set_id
                && c.snapshot == s.name
            {
                complete.push(c);
            }
        }
        if complete.is_empty() {
            continue;
        }
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query(
                "INSERT INTO backup_sets (id, kind, name, source_name, dest_location, created_by, created_by_name, created_at) VALUES (?, 'imported', ?, ?, ?, ?, ?, ?)",
            )
            .bind(&set_id)
            .bind(&info.name)
            .bind(&info.source)
            .bind(&req.location)
            .bind(user.id)
            .bind(&user.username)
            .bind(info.created_at)
            .execute(&mut *tx)
            .await?;
            for c in &complete {
                sqlx::query(
                    "INSERT INTO backup_snapshots (id, set_id, state, cutoff, folders, files, versions, logical_bytes, manifest_sha256, manifest_size, created_at,
                                                   completed_at)
                     VALUES (?, ?, 'complete', ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&c.snapshot)
                .bind(&set_id)
                .bind(c.cutoff)
                .bind(c.folders)
                .bind(c.files)
                .bind(c.versions)
                .bind(c.logical_bytes)
                .bind(&c.manifest_sha256)
                .bind(c.manifest_size as i64)
                .bind(c.cutoff)
                .bind(c.completed_at)
                .execute(&mut *tx)
                .await?;
            }
            insert_job(&mut tx, &user, &new_id(), "verify", &set_id, None, &json!({ "rebuild": true }), &info.name).await?;
            crate::logs::record_activity(&mut tx, &user, None, "backup_import", &format!("{}: {location_name}", info.name)).await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
        added.push(info.name);
    }
    st.backups.wake.notify_one();
    Ok(Json(json!({ "found": found, "added": added })))
}

// ───────────── Restoring ─────────────

#[derive(Deserialize)]
pub struct RestoreReq {
    /// The space of the snapshot to restore
    space: String,
    /// A folder of it (its id in the snapshot) and items chosen in it; neither: the whole space. Never for a personal
    /// space, whose content administrators don't see.
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    items: Option<Vec<String>>,
    /// The space to restore into (default: the space itself, when it is still there)
    #[serde(default)]
    target_drive: Option<String>,
    /// 'new_folder' (default) or 'original' (back where the items were, in the space itself)
    #[serde(default)]
    mode: Option<String>,
    /// Into the original place, for a file whose name is taken: 'skip', 'keep' or 'replace'
    #[serde(default)]
    on_conflict: Option<String>,
    /// Items that were in the trash too
    #[serde(default)]
    trash: bool,
    /// The browser's time zone offset (minutes, as `Date.getTimezoneOffset`), for the new folder's name
    #[serde(default)]
    tz: i64,
    /// The new folder's name, in the administrator's language (default: "Restored <space> <date>")
    #[serde(default)]
    folder_name: Option<String>,
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
    /// Whether it can go back into its original place: the space is still there
    original: bool,
    /// Files it brings back, and their bytes
    files: i64,
    bytes: i64,
    /// Into the original place: how many of the files have an item where they go (of the first `checked`)
    conflicts: Option<i64>,
    checked: i64,
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
/// account; for a backup found on a location, made by another installation, the account of the same user name), else
/// the company space and team spaces
async fn targets_for(st: &AppState, set: &super::Set, space: &SpaceInfo) -> AppResult<Vec<Target>> {
    Ok(if space.kind == "personal" {
        let by_name = set.kind == "imported";
        sqlx::query_as(
            "SELECT d.id, d.name, d.kind FROM drives d JOIN users u ON u.root_id = d.root_id
             WHERE d.kind = 'personal' AND d.disabled = 0 AND CASE WHEN ?3 THEN u.username = ?2 ELSE d.owner_id = ?1 END",
        )
        .bind(space.owner_id)
        .bind(&space.owner)
        .bind(by_name)
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

/// What a restore would do, and the parameters of its job
async fn restore_plan(st: &AppState, snapshot: &str, req: &RestoreReq) -> AppResult<(super::Set, RestorePreview, super::restore::Params)> {
    let (set, space) = snapshot_space(st, snapshot, &req.space).await?;
    if space.kind == "personal" && (req.folder.is_some() || req.items.is_some()) {
        return Err(AppError::forbidden("Administrators don't see into personal spaces: a personal space is restored whole"));
    }
    let mode = req.mode.clone().unwrap_or_else(|| "new_folder".into());
    let on_conflict = req.on_conflict.clone().unwrap_or_else(|| "keep".into());
    if !matches!(mode.as_str(), "new_folder" | "original") || !matches!(on_conflict.as_str(), "skip" | "keep" | "replace") {
        return Err(AppError::bad_request("Choose where the items go, and what happens to items already there"));
    }
    let (cutoff,): (Option<i64>,) = sqlx::query_as("SELECT cutoff FROM backup_snapshots WHERE id = ?").bind(snapshot).fetch_one(&st.db).await?;
    let targets = targets_for(st, &set, &space).await?;
    // Back into its original place: the space itself, when it is still there (a backup another installation made
    // has no spaces here)
    let original = set.kind != "imported" && targets.iter().any(|t| t.id == space.id);
    let wanted = if mode == "original" {
        Some(space.id.clone())
    } else {
        req.target_drive.clone().or_else(|| targets.iter().find(|t| t.id == space.id).map(|t| t.id.clone())).or_else(|| {
            // A personal space goes back into its owner's personal space (made again since, maybe)
            (space.kind == "personal").then(|| targets.first().map(|t| t.id.clone())).flatten()
        })
    };
    let target = wanted.as_ref().and_then(|w| targets.iter().find(|t| &t.id == w));
    let problem = match (&wanted, target) {
        _ if mode == "original" && !original => Some("The space is no longer there: choose a space to restore into".to_string()),
        (Some(_), None) if space.kind == "personal" => Some("A personal space can only be restored into its owner's personal space".to_string()),
        (Some(_), None) => Some("Choose a company or team space to restore into".to_string()),
        (None, _) if space.kind == "personal" => Some("The owner of this personal space no longer has one".to_string()),
        (None, _) => Some("The space is no longer there: choose a space to restore into".to_string()),
        _ => None,
    };
    let folder_name = match req.folder_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => crate::util::validate_name(n)?,
        None => folder_name(&space, cutoff.unwrap_or_default(), req.tz),
    };
    let params = super::restore::Params {
        space: space.id.clone(),
        space_kind: space.kind.clone(),
        folder: req.folder.clone(),
        items: req.items.clone().filter(|i| !i.is_empty()),
        target_drive: target.map(|t| t.id.clone()).unwrap_or_default(),
        target_parent: None,
        mode,
        on_conflict,
        folder_name: folder_name.clone(),
        trash: req.trash,
    };
    // What it brings back, and what is in the way
    let (files, bytes, conflicts, checked) = if problem.is_none() {
        let dst = st.storage(&set.dest_location)?;
        let (sha, size): (String, i64) = sqlx::query_as("SELECT manifest_sha256, manifest_size FROM backup_snapshots WHERE id = ?").bind(snapshot).fetch_one(&st.db).await?;
        let manifest = super::layout::manifest(st, dst.as_ref(), &set.id, snapshot, &sha, size as u64).await?;
        let plan = super::restore::plan(manifest, params.clone()).await?;
        let (conflicts, checked) = if params.mode == "original" {
            let drive = super::restore::target_space(st, &params).await?;
            (Some(super::restore::taken(st, &drive, &plan.paths).await?), plan.paths.len() as i64)
        } else {
            (None, 0)
        };
        (plan.files, plan.bytes, conflicts, checked)
    } else {
        (0, 0, None, 0)
    };
    let preview = RestorePreview {
        folder_name,
        target_drive: target.map(|t| t.id.clone()),
        target_name: target.map(|t| t.name.clone()),
        space,
        targets,
        original,
        files,
        bytes,
        conflicts,
        checked,
        problem,
    };
    Ok((set, preview, params))
}

/// What restoring from a snapshot would do: how many files, where they go, and how many are in the way
pub async fn restore_preview(State(st): State<AppState>, _: Admin, Path(id): Path<String>, Json(req): Json<RestoreReq>) -> AppResult<Json<RestorePreview>> {
    Ok(Json(restore_plan(&st, &id, &req).await?.1))
}

/// Restores from a snapshot, in the background
pub async fn restore(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>, Json(req): Json<RestoreReq>) -> AppResult<Json<Value>> {
    let (set, plan, params) = restore_plan(&st, &id, &req).await?;
    if let Some(problem) = plan.problem {
        return Err(AppError::bad_request(problem));
    }
    if params.target_drive.is_empty() {
        return Err(AppError::bad_request("Choose a space to restore into"));
    }
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

#[derive(Deserialize)]
pub struct BrowseQuery {
    space: String,
    /// A folder of the space (its id in the snapshot); none: its top folder
    #[serde(default)]
    folder: Option<String>,
}

/// An item of a snapshot, as browsing shows it
#[derive(Debug, Serialize)]
pub struct BrowseItem {
    id: String,
    name: String,
    kind: &'static str,
    size: i64,
    modified: i64,
    trashed: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct BrowsePage {
    /// The folders from the top of the space to this one: (id, name)
    path: Vec<(String, String)>,
    items: Vec<BrowseItem>,
}

/// What a folder of a space held in a snapshot, to choose what to restore. Not for personal spaces: administrators
/// don't see into them.
pub async fn browse(State(st): State<AppState>, _: Admin, Path(id): Path<String>, axum::extract::Query(q): axum::extract::Query<BrowseQuery>) -> AppResult<Json<BrowsePage>> {
    let (set, space) = snapshot_space(&st, &id, &q.space).await?;
    if space.kind == "personal" {
        return Err(AppError::forbidden("Administrators don't see into personal spaces: a personal space is restored whole"));
    }
    let dst = st.storage(&set.dest_location)?;
    let (sha, size): (String, i64) = sqlx::query_as("SELECT manifest_sha256, manifest_size FROM backup_snapshots WHERE id = ?").bind(&id).fetch_one(&st.db).await?;
    let manifest = super::layout::manifest(&st, dst.as_ref(), &set.id, &id, &sha, size as u64).await?;
    let page = tokio::task::spawn_blocking(move || -> AppResult<BrowsePage> {
        use super::layout::Line;
        let mut folders: std::collections::HashMap<String, (Option<String>, String)> = std::collections::HashMap::new();
        let mut at: Option<String> = q.folder.clone();
        let mut items = Vec::new();
        for line in super::layout::lines(&manifest)? {
            match line? {
                Line::Folder { space, id, parent, path, modified, trashed } if space == q.space => {
                    let name = path.rsplit('/').next().unwrap_or_default().to_string();
                    if at.is_none() && parent.is_none() {
                        at = Some(id.clone());
                    }
                    if parent.is_some() && parent == at {
                        items.push(BrowseItem { id: id.clone(), name: name.clone(), kind: "folder", size: 0, modified, trashed });
                    }
                    folders.insert(id, (parent, name));
                }
                Line::File { space, id, parent, path, size, modified, trashed, .. } if space == q.space && Some(&parent) == at.as_ref() => {
                    items.push(BrowseItem { id, name: path.rsplit('/').next().unwrap_or_default().to_string(), kind: "file", size, modified, trashed });
                }
                _ => {}
            }
        }
        let at = at.ok_or_else(|| AppError::not_found("This folder isn't in the snapshot"))?;
        if !folders.contains_key(&at) {
            return Err(AppError::not_found("This folder isn't in the snapshot"));
        }
        let mut path = Vec::new();
        let mut cur = Some(at);
        while let Some(c) = cur {
            let (parent, name) = folders.get(&c).cloned().unwrap_or_default();
            path.push((c, name));
            cur = parent;
        }
        path.reverse();
        items.sort_by(|a, b| (a.kind != "folder").cmp(&(b.kind != "folder")).then_with(|| crate::util::natural_cmp(&a.name, &b.name)));
        Ok(BrowsePage { path, items })
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;
    Ok(Json(page))
}
