//! Control panel › Replicas: replica policies, their targets and jobs, removing copies no longer needed, and promoting a
//! target to be the spaces' location. Administrators only.

use std::sync::atomic::Ordering;

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use super::{Policy, SCOPE_HASHES, Target};
use crate::{
    auth::Admin,
    backups::runner::{ACTIVE, Failure},
    error::{AppError, AppResult},
    locations::Relation,
    replicas::Memory,
    state::AppState,
    util::{new_id, now},
};

/// Jobs listed (those not over, then the newest)
const HISTORY: i64 = 300;

#[derive(Serialize)]
pub struct TargetView {
    #[serde(flatten)]
    target: Target,
    name: String,
    schedule: Value,
}

#[derive(Serialize)]
pub struct PolicyView {
    #[serde(flatten)]
    policy: Policy,
    source_name: String,
    targets: Vec<TargetView>,
    /// The spaces chosen (when it doesn't take every space of its location)
    spaces: Vec<String>,
    /// Spaces replicated now
    replicated: i64,
    health: super::policy::Health,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct JobInfo {
    id: String,
    kind: String,
    policy_id: String,
    location_id: Option<String>,
    state: String,
    label: String,
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
}

#[derive(Serialize)]
pub struct Overview {
    policies: Vec<PolicyView>,
    jobs: Vec<JobInfo>,
    /// Copies kept on locations that no policy wants any more (a target removed, fewer copies wanted, a policy deleted):
    /// location, name, how many and their bytes
    unneeded: Vec<(String, String, i64, i64)>,
}

async fn location_name(conn: &mut SqliteConnection, id: &str) -> AppResult<String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?").bind(id).fetch_optional(conn).await?;
    Ok(row.map(|(n,)| n).unwrap_or_default())
}

/// How spaces are named in lists: a personal space as "My files · owner"
async fn space_labels(conn: &mut SqliteConnection, ids: &[String]) -> AppResult<Vec<String>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT d.name, d.kind, COALESCE(u.username, '') FROM drives d LEFT JOIN users u ON u.id = d.owner_id
         WHERE d.id IN (SELECT value FROM json_each(?)) ORDER BY CASE d.kind WHEN 'company' THEN 0 WHEN 'team' THEN 1 ELSE 2 END, d.name",
    )
    .bind(serde_json::to_string(ids).unwrap())
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(n, k, o)| if k == "personal" && !o.is_empty() { format!("{n} · {o}") } else { n }).collect())
}

/// The replica policies, how each is doing, and the jobs
pub async fn list(State(st): State<AppState>, _: Admin) -> AppResult<Json<Overview>> {
    let policies: Vec<Policy> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM replica_policies ORDER BY created_at DESC", super::POLICY_COLS))).fetch_all(&st.db).await?;
    let mut out = Vec::new();
    for policy in policies {
        let mut c = st.db.acquire().await?;
        let targets = super::targets(&mut c, &policy.id).await?;
        let health = super::policy::health(&st, &policy, &targets, now()).await?;
        let mut views = Vec::new();
        for target in targets {
            let name = location_name(&mut c, &target.location_id).await?;
            let schedule = serde_json::from_str(&target.schedule).unwrap_or_default();
            views.push(TargetView { target, name, schedule });
        }
        let spaces: Vec<String> = sqlx::query_as::<_, (String,)>("SELECT drive_id FROM replica_policy_spaces WHERE policy_id = ?")
            .bind(&policy.id)
            .fetch_all(&mut *c)
            .await?
            .into_iter()
            .map(|(d,)| d)
            .collect();
        let replicated = super::scope(&mut c, &policy.id).await?.len() as i64;
        let source_name = location_name(&mut c, &policy.source_location).await?;
        out.push(PolicyView { policy, source_name, targets: views, spaces, replicated, health });
    }
    let mut jobs: Vec<JobInfo> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT id, kind, policy_id, location_id, state, label, files_total, bytes_total, files_done, bytes_done, failed_items, failures, error, note,
                created_by_name, created_at, started_at, finished_at
         FROM replica_jobs ORDER BY state IN {ACTIVE} DESC, created_at DESC, rowid DESC LIMIT {HISTORY}"
    )))
    .fetch_all(&st.db)
    .await?;
    for j in &mut jobs {
        j.failure_list = serde_json::from_str(&j.failures).unwrap_or_default();
        if let Some((fd, bd, ft, bt, rate)) = crate::backups::runner::live(&st.part::<Memory>().queue, &j.id) {
            (j.files_done, j.bytes_done, j.files_total, j.bytes_total, j.speed) = (fd, bd, ft, bt, Some(rate));
        }
    }
    // As last worked out (policy.rs): working it out reads every content of the spaces
    let unneeded: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT u.location_id, COALESCE(l.name, ''), u.copies, u.bytes FROM replica_unneeded u LEFT JOIN storage_locations l ON l.id = u.location_id
         WHERE u.copies > 0 ORDER BY l.name",
    )
    .fetch_all(&st.db)
    .await?;
    Ok(Json(Overview { policies: out, jobs, unneeded }))
}

// ───────────── Policies ─────────────

#[derive(Deserialize, Clone)]
pub struct TargetReq {
    location: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    schedule: Option<Value>,
    #[serde(default)]
    tz: Option<String>,
}

#[derive(Deserialize)]
pub struct PolicyReq {
    #[serde(default)]
    name: Option<String>,
    /// The location whose spaces are replicated (set when the policy is made)
    #[serde(default)]
    source: Option<String>,
    /// The targets, in order of priority (the whole list: targets left out are removed, their copies stay)
    #[serde(default)]
    targets: Option<Vec<TargetReq>>,
    #[serde(default)]
    copies: Option<i64>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    all_spaces: Option<bool>,
    #[serde(default)]
    spaces: Option<Vec<String>>,
    #[serde(default)]
    read_fallback: Option<bool>,
    #[serde(default)]
    verify_days: Option<i64>,
    #[serde(default)]
    alert_hours: Option<i64>,
    #[serde(default)]
    rate_limit: Option<i64>,
}

/// Checks the targets: other locations than the source, apart from it and each other, each once
async fn check_targets(st: &AppState, source: &str, targets: &[TargetReq]) -> AppResult<()> {
    if targets.is_empty() {
        return Err(AppError::bad_request("Choose where the copies go"));
    }
    let mut seen = std::collections::HashSet::new();
    for t in targets {
        if !seen.insert(t.location.clone()) {
            return Err(AppError::bad_request("A location is chosen twice"));
        }
        if t.location == source {
            return Err(AppError::bad_request("The copies go to other locations than the spaces' own"));
        }
        let exists: Option<(String,)> = sqlx::query_as("SELECT kind FROM storage_locations WHERE id = ?").bind(&t.location).fetch_optional(&st.db).await?;
        if exists.is_none() {
            return Err(AppError::not_found("Storage location not found"));
        }
        for other in std::iter::once(source).chain(targets.iter().map(|x| x.location.as_str()).filter(|l| *l != t.location)) {
            if crate::locations::relation(st, &t.location, other).await? == Relation::Nested {
                return Err(AppError::bad_request("These locations are in the same place, or one is inside the other: choose a location somewhere else"));
            }
        }
        let mode = t.mode.as_deref().unwrap_or("realtime");
        if !matches!(mode, "realtime" | "scheduled") {
            return Err(AppError::bad_request("Choose when copies are made"));
        }
        if mode == "scheduled" {
            crate::backups::policy::Schedule::parse(t.schedule.as_ref().unwrap_or(&json!({ "daily": "03:00" })))?;
        }
        crate::backups::policy::time_zone(t.tz.as_deref().unwrap_or("UTC"))?;
    }
    Ok(())
}

/// Writes a policy's targets: new ones added, ones left out removed (their copies stay until removed as not needed),
/// the order kept as priority
async fn save_targets(conn: &mut SqliteConnection, policy: &str, targets: &[TargetReq]) -> AppResult<()> {
    let list: Vec<&str> = targets.iter().map(|t| t.location.as_str()).collect();
    sqlx::query("DELETE FROM replica_targets WHERE policy_id = ?1 AND location_id NOT IN (SELECT value FROM json_each(?2))")
        .bind(policy)
        .bind(serde_json::to_string(&list).unwrap())
        .execute(&mut *conn)
        .await?;
    for (i, t) in targets.iter().enumerate() {
        let schedule = t.schedule.clone().unwrap_or_else(|| json!({ "daily": "03:00" })).to_string();
        sqlx::query(
            "INSERT INTO replica_targets (policy_id, location_id, priority, mode, schedule, tz) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (policy_id, location_id) DO UPDATE SET priority = ?3, mode = ?4, schedule = ?5, tz = ?6, next_run_at = NULL",
        )
        .bind(policy)
        .bind(&t.location)
        .bind(i as i64)
        .bind(t.mode.as_deref().unwrap_or("realtime"))
        .bind(schedule)
        .bind(t.tz.as_deref().unwrap_or("UTC"))
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

fn number(v: Option<i64>, current: Option<i64>, default: i64, range: std::ops::RangeInclusive<i64>, what: &str) -> AppResult<i64> {
    let n = v.or(current).unwrap_or(default);
    if !range.contains(&n) {
        return Err(AppError::bad_request(format!("{what} must be between {} and {}", range.start(), range.end())));
    }
    Ok(n)
}

/// Makes a replica policy: its targets start copying right away
pub async fn create(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<PolicyReq>) -> AppResult<Json<Value>> {
    let source = req.source.clone().unwrap_or_default();
    let mut c = st.db.acquire().await?;
    let source_name = {
        let row: Option<(String,)> = sqlx::query_as("SELECT name FROM storage_locations WHERE id = ?").bind(&source).fetch_optional(&mut *c).await?;
        row.ok_or_else(|| AppError::not_found("Storage location not found"))?.0
    };
    drop(c);
    let targets = req.targets.clone().unwrap_or_default();
    check_targets(&st, &source, &targets).await?;
    let copies = number(req.copies, None, 1, 1..=16, "Copies")?;
    let name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if n.chars().count() > 200 => return Err(AppError::bad_request("Name is too long")),
        Some(n) => n.to_string(),
        None => format!("Replicas of {source_name}"),
    };
    let all_spaces = req.all_spaces.unwrap_or(true);
    let spaces = req.spaces.clone().unwrap_or_default();
    if !all_spaces && spaces.is_empty() {
        return Err(AppError::bad_request("Choose the spaces to replicate"));
    }
    let id = new_id();
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query(
                "INSERT INTO replica_policies (id, name, source_location, enabled, all_spaces, copies, read_fallback, verify_days, alert_hours, rate_limit,
                                               created_by, created_by_name, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&name)
            .bind(&source)
            .bind(req.enabled.unwrap_or(true))
            .bind(all_spaces)
            .bind(copies)
            .bind(req.read_fallback.unwrap_or(true))
            .bind(number(req.verify_days, None, 1, 0..=365, "Days between checks")?)
            .bind(number(req.alert_hours, None, 24, 0..=8760, "Hours before administrators are told")?)
            .bind(number(req.rate_limit, None, 0, 0..=1 << 40, "The speed limit")?)
            .bind(user.id)
            .bind(&user.username)
            .bind(now())
            .bind(now())
            .execute(&mut *tx)
            .await?;
            save_targets(&mut tx, &id, &targets).await?;
            sqlx::query("INSERT INTO replica_policy_spaces (policy_id, drive_id) SELECT ?, value FROM json_each(?)")
                .bind(&id)
                .bind(serde_json::to_string(&if all_spaces { Vec::new() } else { spaces.clone() }).unwrap())
                .execute(&mut *tx)
                .await?;
            super::forget_counts(&mut tx, Some(&id)).await?;
            crate::logs::record_activity(&mut tx, &user, None, "replica_create", &name).await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    st.part::<Memory>().queue.policies.notify_one();
    Ok(Json(json!({ "id": id })))
}

/// Changes a policy. Targets left out of `targets` are removed: their copies stay until removed as not needed.
pub async fn update(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>, Json(req): Json<PolicyReq>) -> AppResult<Json<Value>> {
    let current = super::load(&mut *st.db.acquire().await?, &id).await?.ok_or_else(|| AppError::not_found("This replica policy no longer exists"))?;
    if let Some(targets) = &req.targets {
        check_targets(&st, &current.source_location, targets).await?;
    }
    {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            let name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                Some(n) if n.chars().count() > 200 => return Err(AppError::bad_request("Name is too long")),
                Some(n) => n.to_string(),
                None => current.name.clone(),
            };
            let all_spaces = req.all_spaces.unwrap_or(current.all_spaces);
            sqlx::query(
                "UPDATE replica_policies SET name = ?, enabled = ?, all_spaces = ?, copies = ?, read_fallback = ?, verify_days = ?, alert_hours = ?, rate_limit = ?,
                                             updated_at = ?
                 WHERE id = ?",
            )
            .bind(&name)
            .bind(req.enabled.unwrap_or(current.enabled))
            .bind(all_spaces)
            .bind(number(req.copies, Some(current.copies), 1, 1..=16, "Copies")?)
            .bind(req.read_fallback.unwrap_or(current.read_fallback))
            .bind(number(req.verify_days, Some(current.verify_days), 1, 0..=365, "Days between checks")?)
            .bind(number(req.alert_hours, Some(current.alert_hours), 24, 0..=8760, "Hours before administrators are told")?)
            .bind(number(req.rate_limit, Some(current.rate_limit), 0, 0..=1 << 40, "The speed limit")?)
            .bind(now())
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            if let Some(spaces) = &req.spaces {
                if !all_spaces && spaces.is_empty() {
                    return Err(AppError::bad_request("Choose the spaces to replicate"));
                }
                sqlx::query("DELETE FROM replica_policy_spaces WHERE policy_id = ?").bind(&id).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO replica_policy_spaces (policy_id, drive_id) SELECT ?, value FROM json_each(?)")
                    .bind(&id)
                    .bind(serde_json::to_string(spaces).unwrap())
                    .execute(&mut *tx)
                    .await?;
            }
            if let Some(targets) = &req.targets {
                save_targets(&mut tx, &id, targets).await?;
            }
            // What the targets should hold may be other now
            if req.copies.is_some() || req.targets.is_some() || req.spaces.is_some() || req.all_spaces.is_some() {
                super::forget_counts(&mut tx, Some(&id)).await?;
            }
            // Paused: syncs waiting for their turn wait until it is resumed
            if req.enabled == Some(false) {
                sqlx::query("UPDATE replica_jobs SET state = 'paused' WHERE policy_id = ? AND state IN ('queued', 'waiting')").bind(&id).execute(&mut *tx).await?;
            } else if req.enabled == Some(true) && !current.enabled {
                sqlx::query("UPDATE replica_jobs SET state = 'queued' WHERE policy_id = ? AND state = 'paused'").bind(&id).execute(&mut *tx).await?;
            }
            crate::logs::record_activity(&mut tx, &user, None, "replica_update", &name).await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    if req.enabled == Some(false) {
        pause_running(&st, &id);
    }
    st.part::<Memory>().queue.wake.notify_one();
    st.part::<Memory>().queue.policies.notify_one();
    Ok(Json(json!({ "ok": true })))
}

/// The policy's job running now stops after the item it is copying
fn pause_running(st: &AppState, policy: &str) {
    let running: Vec<(String, std::sync::Arc<crate::backups::runner::Control>)> =
        st.part::<Memory>().queue.running.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let st = st.clone();
    let policy = policy.to_string();
    tokio::spawn(async move {
        for (id, ctl) in running {
            let mine: Option<(i64,)> =
                sqlx::query_as("SELECT 1 FROM replica_jobs WHERE id = ? AND policy_id = ?").bind(&id).bind(&policy).fetch_optional(&st.db).await.unwrap_or(None);
            if mine.is_some() {
                ctl.pause.store(true, Ordering::SeqCst);
            }
        }
    });
}

#[derive(Deserialize)]
pub struct TargetQuery {
    #[serde(default)]
    location: Option<String>,
}

/// Syncs the policy's targets (or one) now, or tries a failed sync again
pub async fn sync(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>, Json(q): Json<TargetQuery>) -> AppResult<Json<Value>> {
    let targets = super::targets(&mut *st.db.acquire().await?, &id).await?;
    let mut queued = Vec::new();
    for t in targets.iter().filter(|t| q.location.as_ref().is_none_or(|l| *l == t.location_id)) {
        if let Some(job) = super::policy::trigger(&st, &id, &t.location_id, "manual", Some((user.id, user.username.clone()))).await? {
            queued.push(job);
        }
    }
    Ok(Json(json!({ "jobs": queued })))
}

/// Reads the copies of the policy's targets (or one) back and checks them
pub async fn verify(State(st): State<AppState>, _: Admin, Path(id): Path<String>, Json(q): Json<TargetQuery>) -> AppResult<Json<Value>> {
    let p = super::load(&mut *st.db.acquire().await?, &id).await?.ok_or_else(|| AppError::not_found("This replica policy no longer exists"))?;
    let targets = super::targets(&mut *st.db.acquire().await?, &id).await?;
    let mut queued = Vec::new();
    for t in targets.iter().filter(|t| q.location.as_ref().is_none_or(|l| *l == t.location_id)) {
        if let Some(job) = super::policy::queue_verify(&st, &p, &t.location_id, now()).await? {
            queued.push(job);
        }
    }
    Ok(Json(json!({ "jobs": queued })))
}

/// Deletes a policy. Its copies stay, kept from deletion, until removed as not needed.
pub async fn delete(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let p = super::load(&mut *st.db.acquire().await?, &id).await?.ok_or_else(|| AppError::not_found("This replica policy no longer exists"))?;
    pause_running(&st, &id);
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    let res = async {
        let running: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM replica_jobs WHERE policy_id = ? AND state = 'running'").bind(&id).fetch_optional(&mut *tx).await?;
        if running.is_some() {
            return Err(AppError::conflict("A job of this policy is running: it stops after the item it is copying. Try again in a moment."));
        }
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE replica_jobs SET state = 'cancelled', finished_at = ? WHERE policy_id = ? AND state IN {ACTIVE}")))
            .bind(now())
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM replica_policies WHERE id = ?").bind(&id).execute(&mut *tx).await?;
        super::forget_counts(&mut tx, Some(&id)).await?;
        crate::logs::record_activity(&mut tx, &user, None, "replica_delete", &p.name).await?;
        AppResult::Ok(())
    }
    .await;
    crate::db::settle(tx, res).await?;
    Ok(Json(json!({ "ok": true })))
}

/// How many copies on a location no policy wants (`unneeded_on`), and their bytes
pub(super) async fn unneeded_count(st: &AppState, location: &str) -> AppResult<(i64, i64)> {
    let extra = unneeded_on(st, location).await?;
    let (bytes,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(size), 0) FROM replica_copies WHERE location_id = ?1 AND hash IN (SELECT value FROM json_each(?2))")
        .bind(location)
        .bind(serde_json::to_string(&extra).unwrap())
        .fetch_one(&st.db)
        .await?;
    Ok((extra.len() as i64, bytes))
}

/// Copies on a location that no policy wants: of content no attached policy uses, or not among the copies a policy
/// wants there
async fn unneeded_on(st: &AppState, location: &str) -> AppResult<Vec<String>> {
    let mut wanted = std::collections::HashSet::new();
    let policies: Vec<(String,)> = sqlx::query_as("SELECT policy_id FROM replica_targets WHERE location_id = ?").bind(location).fetch_all(&st.db).await?;
    for (pid,) in policies {
        let mut c = st.db.acquire().await?;
        let Some(p) = super::load(&mut c, &pid).await? else { continue };
        let targets = super::targets(&mut c, &pid).await?;
        let spaces = super::store_scope(&mut c, &pid).await?;
        let rows: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT b.hash, b.location_id FROM blobs b WHERE b.hash IN ({SCOPE_HASHES})")))
            .bind(serde_json::to_string(&spaces).unwrap())
            .fetch_all(&mut *c)
            .await?;
        // An old primary being checked keeps what it has until it counts again
        let stale = targets.iter().any(|t| t.location_id == location && t.state == "stale");
        for (hash, primary) in rows {
            if stale || super::required(&targets, p.copies, &primary).contains(&location) {
                wanted.insert(hash);
            }
        }
        // Folder spaces' current files
        if stale || super::required(&targets, p.copies, &p.source_location).contains(&location) {
            let folders = super::folder_scope(&mut c, &pid).await?;
            let rows: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(super::folders::current_hashes(Some("?1"))))
                .bind(serde_json::to_string(&folders).unwrap())
                .fetch_all(&mut *c)
                .await?;
            wanted.extend(rows.into_iter().map(|(h,)| h));
        }
    }
    // And whatever a folder space whose location can't be reached now has: a copy may be all there is of it now
    let offline: Vec<String> = sqlx::query_as::<_, (String, Option<String>)>("SELECT id, location_id FROM drives WHERE mode = 'folder'")
        .fetch_all(&st.db)
        .await?
        .into_iter()
        .filter(|(_, l)| l.as_deref().is_none_or(|l| st.location_offline(l).is_some()))
        .map(|(d, _)| d)
        .collect();
    if !offline.is_empty() {
        let rows: Vec<(String,)> =
            sqlx::query_as(sqlx::AssertSqlSafe(super::folders::current_hashes(Some("?1")))).bind(serde_json::to_string(&offline).unwrap()).fetch_all(&st.db).await?;
        wanted.extend(rows.into_iter().map(|(h,)| h));
    }
    let held: Vec<(String,)> = sqlx::query_as("SELECT hash FROM replica_copies WHERE location_id = ?").bind(location).fetch_all(&st.db).await?;
    Ok(held.into_iter().map(|(h,)| h).filter(|h| !wanted.contains(h)).collect())
}

#[derive(Deserialize)]
pub struct PurgeReq {
    location: String,
}

/// Removes the copies on a location that no policy wants any more (after a target was removed, fewer copies are
/// wanted, or a policy was deleted). Each is checked again before it is deleted: content that is the primary there
/// stays.
pub async fn purge(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<PurgeReq>) -> AppResult<Json<Value>> {
    let extra = unneeded_on(&st, &req.location).await?;
    let n = extra.len();
    super::sync::drop_copies(&st, &req.location, extra).await?;
    {
        let name = location_name(&mut *st.db.acquire().await?, &req.location).await?;
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query("DELETE FROM replica_unneeded WHERE location_id = ?").bind(&req.location).execute(&mut *tx).await?;
            crate::logs::record_activity(&mut tx, &user, None, "replica_purge", &format!("{name}: {n}")).await
        }
        .await;
        crate::db::settle(tx, res).await?;
    }
    Ok(Json(json!({ "removed": n })))
}

// ───────────── Jobs ─────────────

pub async fn pause_job(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    if let Some(ctl) = st.part::<Memory>().queue.running.lock().unwrap().get(&id) {
        ctl.pause.store(true, Ordering::SeqCst);
        return Ok(Json(json!({ "ok": true })));
    }
    let _w = st.write_lock.lock().await;
    let n = sqlx::query("UPDATE replica_jobs SET state = 'paused' WHERE id = ? AND state IN ('queued', 'waiting')").bind(&id).execute(&st.db).await?.rows_affected();
    if n == 0 {
        return Err(AppError::conflict("This job can't be paused now"));
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn resume_job(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    {
        let _w = st.write_lock.lock().await;
        // Resumed with the policy as it is now
        let n = sqlx::query(
            "UPDATE replica_jobs SET state = 'queued', error = NULL, params = json_set(params, '$.epoch', (SELECT epoch FROM replica_policies p WHERE p.id = replica_jobs.policy_id))
             WHERE id = ? AND state IN ('paused', 'failed', 'waiting')",
        )
        .bind(&id)
        .execute(&st.db)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(AppError::conflict("This job can't be resumed now"));
        }
    }
    st.part::<Memory>().queue.wake.notify_one();
    Ok(Json(json!({ "ok": true })))
}

pub async fn cancel_job(State(st): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<Json<Value>> {
    if let Some(ctl) = st.part::<Memory>().queue.running.lock().unwrap().get(&id) {
        ctl.cancel.store(true, Ordering::SeqCst);
        return Ok(Json(json!({ "ok": true })));
    }
    let _w = st.write_lock.lock().await;
    let n = sqlx::query("UPDATE replica_jobs SET state = 'cancelled', finished_at = ? WHERE id = ? AND state IN ('queued', 'paused', 'failed', 'waiting')")
        .bind(now())
        .bind(&id)
        .execute(&st.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::conflict("This job can't be cancelled now"));
    }
    Ok(Json(json!({ "ok": true })))
}

// ───────────── Promoting a target ─────────────

#[derive(Deserialize)]
pub struct PromoteQuery {
    target: String,
}

/// What promoting a target would do, before it is done
#[derive(Serialize)]
pub struct Preflight {
    source: String,
    source_name: String,
    target_name: String,
    /// Whether each can be reached now
    source_reachable: bool,
    target_reachable: bool,
    /// How the target is doing (current, behind…)
    target_state: String,
    behind_since: Option<i64>,
    /// Every space whose files are read from the target afterwards: the policy's spaces, and other spaces sharing their
    /// content (identical content is stored once)
    spaces: Vec<String>,
    /// Content that goes over (a checked copy is there), and content that doesn't (no checked copy there): the files
    /// using it stay on the old location, and can't be read while it can't
    moved: i64,
    moved_bytes: i64,
    missing: i64,
    missing_bytes: i64,
    /// Folder spaces not wholly on the target: they stay where they are, as they are
    folder_spaces: Vec<String>,
    /// Content is missing and the old location can't be reached: promoting loses access to it until that location is
    /// back, and has to be accepted
    needs_accept: bool,
    /// Why it can't be promoted now, None when it can
    problem: Option<String>,
}

async fn preflight(st: &AppState, policy: &Policy, target: &str) -> AppResult<Preflight> {
    let mut c = st.db.acquire().await?;
    let targets = super::targets(&mut c, &policy.id).await?;
    let spaces = super::store_scope(&mut c, &policy.id).await?;
    let source = policy.source_location.clone();
    let source_name = location_name(&mut c, &source).await?;
    let target_name = location_name(&mut c, target).await?;
    let (moved, moved_bytes, missing, missing_bytes): (i64, i64, i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT COALESCE(SUM(k), 0), COALESCE(SUM(CASE WHEN k = 1 THEN size END), 0), COALESCE(SUM(1 - k), 0), COALESCE(SUM(CASE WHEN k = 0 THEN size END), 0)
         FROM (SELECT b.size, EXISTS (SELECT 1 FROM replica_copies c WHERE c.hash = b.hash AND c.location_id = ?2 AND c.state = 'verified') AS k
               FROM blobs b WHERE b.location_id = ?3 AND b.hash IN ({SCOPE_HASHES}))"
    )))
    .bind(serde_json::to_string(&spaces).unwrap())
    .bind(target)
    .bind(&source)
    .fetch_one(&mut *c)
    .await?;
    // Every space using content that goes over
    let affected: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "WITH moved AS (SELECT b.hash FROM blobs b WHERE b.location_id = ?3 AND b.hash IN ({SCOPE_HASHES})
                          AND EXISTS (SELECT 1 FROM replica_copies c WHERE c.hash = b.hash AND c.location_id = ?2 AND c.state = 'verified'))
         SELECT value FROM json_each(?1)
         UNION SELECT drive_id FROM nodes WHERE blob_hash IN (SELECT hash FROM moved) AND drive_id IS NOT NULL
         UNION SELECT n.drive_id FROM node_versions v JOIN nodes n ON n.id = v.node_id WHERE v.blob_hash IN (SELECT hash FROM moved) AND n.drive_id IS NOT NULL"
    )))
    .bind(serde_json::to_string(&spaces).unwrap())
    .bind(target)
    .bind(&source)
    .fetch_all(&mut *c)
    .await?;
    let mut affected: Vec<String> = affected.into_iter().map(|(d,)| d).collect();
    // Folder spaces: those wholly on the target go over; the others stay
    let (mut moved, mut moved_bytes, mut missing) = (moved, moved_bytes, missing);
    let mut staying = Vec::new();
    for space in super::folder_scope(&mut c, &policy.id).await? {
        let (items, covered, bytes) = super::folders::readiness(&mut c, &space, target).await?;
        if items == covered {
            (moved, moved_bytes) = (moved + covered, moved_bytes + bytes);
            affected.push(space);
        } else {
            missing += items - covered;
            staying.push(space);
        }
    }
    let labels = space_labels(&mut c, &affected).await?;
    let folder_spaces = space_labels(&mut c, &staying).await?;
    let busy = crate::moves::location_busy(&mut c, &source).await? || crate::moves::location_busy(&mut c, target).await?;
    drop(c);
    let source_reachable = crate::locations::probe(st, &source).await.is_ok();
    let target_reachable = crate::locations::probe(st, target).await.is_ok();
    let health = super::policy::health(st, policy, &targets, now()).await?;
    let th = health.targets.iter().find(|h| h.location_id == target);
    let t = targets.iter().find(|t| t.location_id == target);
    let problem = if t.is_none_or(|t| t.state != "active") {
        Some("Choose a target of the policy that holds its copies".to_string())
    } else if !target_reachable {
        Some(format!("{target_name} can't be reached now"))
    } else if busy {
        Some("A space is being moved to or from these locations. Wait until the move finishes, or cancel it.".to_string())
    } else if missing > 0 && source_reachable {
        Some(format!(
            "{missing} contents aren't on {target_name} yet. Sync it first; a target that is behind is promoted only while {source_name} can't be reached."
        ))
    } else if moved == 0 && missing == 0 && affected.is_empty() {
        Some("There is nothing to promote".to_string())
    } else {
        None
    };
    Ok(Preflight {
        source,
        source_name,
        target_name,
        source_reachable,
        target_reachable,
        target_state: th.map_or("unknown", |h| h.state).to_string(),
        behind_since: th.and_then(|h| h.behind_since),
        spaces: labels,
        moved,
        moved_bytes,
        missing,
        missing_bytes,
        folder_spaces,
        needs_accept: missing > 0 && !source_reachable,
        problem,
    })
}

/// What promoting a target would do
pub async fn promote_preview(State(st): State<AppState>, _: Admin, Path(id): Path<String>, Query(q): Query<PromoteQuery>) -> AppResult<Json<Preflight>> {
    let p = super::load(&mut *st.db.acquire().await?, &id).await?.ok_or_else(|| AppError::not_found("This replica policy no longer exists"))?;
    Ok(Json(preflight(&st, &p, &q.target).await?))
}

#[derive(Deserialize)]
pub struct PromoteReq {
    target: String,
    /// Content without a copy on the target stays on the old location, unreadable while it is: accepted
    #[serde(default)]
    accept_missing: bool,
}

/// Makes a target the location of the policy's spaces, in one transaction: content with a checked copy there is read
/// from there, the spaces store new files there, the policy's jobs asked for before are refused, and the old location
/// becomes a target whose copies are checked before they count again (and on which nothing new is stored meanwhile).
/// Nothing is deleted from the old location. Content without a copy on the target stays where it was: listed, and
/// accepted by the administrator first.
pub async fn promote(State(st): State<AppState>, Admin(user): Admin, Path(id): Path<String>, Json(req): Json<PromoteReq>) -> AppResult<Json<Value>> {
    let p = super::load(&mut *st.db.acquire().await?, &id).await?.ok_or_else(|| AppError::not_found("This replica policy no longer exists"))?;
    let check = preflight(&st, &p, &req.target).await?;
    if let Some(problem) = check.problem {
        return Err(AppError::conflict(problem));
    }
    if check.needs_accept && !req.accept_missing {
        return Err(AppError::conflict(format!(
            "{} contents aren't on {}: their files can't be read until {} is back. Accept that to promote anyway.",
            check.missing, check.target_name, check.source_name
        )));
    }
    let target = req.target.clone();
    // Folder spaces are held still while they go over: no check for changes, no change from the web
    let folder_spaces = super::folder_scope(&mut *st.db.acquire().await?, &id).await?;
    let mut _held = Vec::new();
    for space in &folder_spaces {
        _held.push(crate::folders::hold(&st, space).await);
    }
    let (moved, missing, converted) = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            // The same epoch as the check: nothing promoted it meanwhile
            let (epoch, source): (i64, String) =
                sqlx::query_as("SELECT epoch, source_location FROM replica_policies WHERE id = ?").bind(&id).fetch_one(&mut *tx).await?;
            if epoch != p.epoch || source != p.source_location {
                return Err(AppError::conflict("The replicas were promoted meanwhile"));
            }
            let spaces = super::store_scope(&mut tx, &id).await?;
            let list = serde_json::to_string(&spaces).unwrap();
            let moved: Vec<(String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "UPDATE blobs SET location_id = ?2 WHERE location_id = ?3 AND hash IN ({SCOPE_HASHES})
                   AND EXISTS (SELECT 1 FROM replica_copies c WHERE c.hash = blobs.hash AND c.location_id = ?2 AND c.state = 'verified')
                 RETURNING hash, size"
            )))
            .bind(&list)
            .bind(&target)
            .bind(&source)
            .fetch_all(&mut *tx)
            .await?;
            let moved_list = serde_json::to_string(&moved).unwrap();
            // The copies there are the content now; the old location's become copies, not counted until checked
            sqlx::query("DELETE FROM replica_copies WHERE location_id = ?1 AND hash IN (SELECT json_extract(value, '$[0]') FROM json_each(?2))")
                .bind(&target)
                .bind(&moved_list)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT OR REPLACE INTO replica_copies (hash, location_id, size, state, created_at)
                 SELECT json_extract(value, '$[0]'), ?1, json_extract(value, '$[1]'), 'stale', ?3 FROM json_each(?2)",
            )
            .bind(&source)
            .bind(&moved_list)
            .bind(now())
            .execute(&mut *tx)
            .await?;
            let (missing,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM blobs WHERE location_id = ?2 AND hash IN ({SCOPE_HASHES})")))
                .bind(&list)
                .bind(&source)
                .fetch_one(&mut *tx)
                .await?;
            // New files of the spaces go to the new location
            sqlx::query("UPDATE drives SET location_id = ?1 WHERE id IN (SELECT value FROM json_each(?2))").bind(&target).bind(&list).execute(&mut *tx).await?;
            // Folder spaces wholly there become content-store spaces there; the others stay where they are
            let mut converted = 0;
            let mut staying = 0i64;
            for space in super::folder_scope(&mut tx, &id).await? {
                if super::folders::promote(&mut tx, &space, &target).await? {
                    converted += 1;
                } else {
                    let (items, covered, _) = super::folders::readiness(&mut tx, &space, &target).await?;
                    staying += items - covered;
                }
            }
            let missing = missing + staying;
            // Network checks and waiting for space locks leave time for uploads or replica verification to change
            // the coverage. A new gap must never inherit acceptance of an earlier preflight.
            if missing != check.missing || (missing > 0 && (check.source_reachable || !req.accept_missing)) {
                return Err(AppError::conflict("Something changed at the same time. Try again."));
            }
            // The policy: its location, a new epoch (jobs asked for before are refused), the old location as a target
            // checked before it counts
            sqlx::query("UPDATE replica_policies SET source_location = ?, epoch = epoch + 1, updated_at = ? WHERE id = ?")
                .bind(&target)
                .bind(now())
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM replica_targets WHERE policy_id = ? AND location_id = ?").bind(&id).bind(&target).execute(&mut *tx).await?;
            sqlx::query(
                "INSERT INTO replica_targets (policy_id, location_id, priority, mode, state)
                 VALUES (?1, ?2, (SELECT COALESCE(MAX(priority), 0) + 1 FROM replica_targets WHERE policy_id = ?1), 'realtime', 'stale')
                 ON CONFLICT (policy_id, location_id) DO UPDATE SET state = 'stale'",
            )
            .bind(&id)
            .bind(&source)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM replica_captured WHERE policy_id = ?").bind(&id).execute(&mut *tx).await?;
            // Content of every policy may be kept elsewhere now
            super::forget_counts(&mut tx, None).await?;
            sqlx::query("DELETE FROM replica_dirty WHERE policy_id = ?").bind(&id).execute(&mut *tx).await?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE replica_jobs SET state = 'cancelled', finished_at = ? WHERE policy_id = ? AND state IN {ACTIVE} AND state != 'running'"
            )))
            .bind(now())
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            let detail = format!(
                "{}: {} → {} ({} moved{})",
                p.name,
                check.source_name,
                check.target_name,
                moved.len(),
                if missing > 0 { format!(", {missing} not on {} and still on {}", check.target_name, check.source_name) } else { String::new() }
            );
            crate::logs::record_activity(&mut tx, &user, None, "replica_promote", &detail).await?;
            AppResult::Ok((moved.len(), missing, converted))
        }
        .await;
        crate::db::settle(tx, res).await?
    };
    drop(_held);
    if converted > 0 {
        crate::folders::spaces_changed(&st);
    }
    // A job of the policy running now was asked for before: it stops, and its results are refused anyway
    let running: Vec<(String, std::sync::Arc<crate::backups::runner::Control>)> =
        st.part::<Memory>().queue.running.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (job, ctl) in running {
        let mine: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM replica_jobs WHERE id = ? AND policy_id = ?").bind(&job).bind(&id).fetch_optional(&st.db).await?;
        if mine.is_some() {
            ctl.cancel.store(true, Ordering::SeqCst);
        }
    }
    st.part::<Memory>().queue.policies.notify_one();
    Ok(Json(json!({ "moved": moved, "missing": missing })))
}
