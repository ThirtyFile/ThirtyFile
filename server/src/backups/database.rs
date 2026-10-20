//! Database backups (Control panel › Backups › Database): a copy of the database on a schedule, on a storage location,
//! encrypted with a passphrase the administrator chooses.
//!
//! File backups (the rest of backups/) copy the spaces' content, not the database: the folder tree, accounts, groups
//! and access, share links, tags, buckets and receive folders, versions and the activity log. A copy here brings those
//! back: `thirtyfile restore-database <file>` into a new data folder, then the file backups (site/docs/backup.html).
//!
//! - A copy is made with `VACUUM INTO` (`db::backup_to`), consistent while the server keeps working, then encrypted and
//!   stored as `.thirtyfile-backups/database/thirtyfile-<UTC time>.tfdb` on the location.
//! - Encrypted with a key derived from the passphrase (Argon2id), never with `secret.key`, which isn't in the copy: the
//!   copy holds password hashes, IP addresses and secrets that only `secret.key` opens. AES-256-GCM in chunks of 1 MiB
//!   (`MAGIC`), each with its number in its nonce and the last one marked, so none can be dropped, reordered or cut
//!   off unnoticed.
//! - Kept: the newest `keep_last`, and the newest of each of the last `keep_daily` days, `keep_weekly` weeks and
//!   `keep_monthly` months (in the schedule's time zone). Copies are only ever deleted after a new one is stored.
//! - Settings in `database_backup` (JSON), the passphrase sealed with `secret.key` in `database_backup_key`, and what
//!   happened in `database_backup_status`. A failure, or no copy for `alert_hours`, is told to administrators as the
//!   file backups' are (`scheduler::alert`), and when it works again.

use std::{
    collections::HashSet,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use axum::{Json, extract::State};
use jiff::{Timestamp, tz::TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{policy, scheduler};
use crate::{
    auth::Admin,
    db,
    error::{AppError, AppResult},
    state::AppState,
    util::now,
};

/// Where copies are kept on their location
pub const DIR: &str = ".thirtyfile-backups/database";
/// The start of every copy: the format and its version
const MAGIC: &[u8; 8] = b"TFDBENC1";
const CHUNK: usize = 1 << 20;
const TAG: usize = 16;
/// Argon2id: 64 MiB, 3 passes, one lane
const ARGON: (u32, u32, u32) = (64 * 1024, 3, 1);
/// The least a passphrase has
pub const MIN_PASSPHRASE: usize = 12;

const CONFIG: &str = "database_backup";
const KEY: &str = "database_backup_key";
const STATUS: &str = "database_backup_status";
/// What the passphrase is sealed with (`secrets::seal`)
const SEAL: &str = "settings:database_backup";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// The storage location copies are kept on
    pub location: String,
    /// As a file backup's (`policy::Schedule`): `{"daily": "02:30"}`, `{"weekly": …, "days": […]}`, `{"every": minutes}`
    pub schedule: Value,
    pub tz: String,
    pub keep_last: u32,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
    /// Tell administrators when there is no new copy for this long (0: never)
    pub alert_hours: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            location: String::new(),
            schedule: json!({ "daily": "02:30" }),
            tz: "UTC".into(),
            keep_last: 7,
            keep_daily: 7,
            keep_weekly: 4,
            keep_monthly: 12,
            alert_hours: 48,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Status {
    pub next_run_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    /// The newest copy: its name on the location, its size, and how long making it took
    pub last_file: Option<String>,
    pub last_size: Option<u64>,
    pub last_duration_ms: Option<u64>,
    /// What went wrong the last time, None when it worked
    pub error: Option<String>,
    /// What administrators were last told (`scheduler::Alert`)
    pub alerted: String,
}

/// Database backups' part of `AppState`
#[derive(Default)]
pub struct Memory {
    /// Wakes the scheduler (settings saved, a copy asked for)
    pub wake: tokio::sync::Notify,
    running: AtomicBool,
    /// "Back up now" asked for
    asked: AtomicBool,
}

async fn status(conn: &mut sqlx::SqliteConnection) -> AppResult<Status> {
    Ok(db::json_setting(conn, STATUS).await?)
}

/// Changes what happened, read anew so a change from elsewhere (what administrators were told) isn't lost
async fn change_status(st: &AppState, change: impl FnOnce(&mut Status)) -> AppResult<Status> {
    let _w = st.write_lock.lock().await;
    let mut tx = db::begin_write(&st.db).await?;
    let mut s = status(&mut tx).await?;
    change(&mut s);
    db::set_json_setting(&mut tx, STATUS, &s).await?;
    tx.commit().await?;
    Ok(s)
}

fn schedule_of(c: &Config) -> AppResult<(policy::Schedule, TimeZone)> {
    Ok((policy::Schedule::parse(&c.schedule)?, policy::time_zone(&c.tz)?))
}

// ───────────── Settings ─────────────

#[derive(Serialize)]
pub struct Info {
    #[serde(flatten)]
    pub config: Config,
    pub has_passphrase: bool,
    pub running: bool,
    pub status: Status,
}

async fn info(st: &AppState) -> AppResult<Info> {
    let mut conn = st.db.acquire().await?;
    let config = db::json_setting(&mut conn, CONFIG).await?;
    let has_passphrase = db::get_setting(&st.db, KEY).await?.is_some_and(|v| !v.is_empty());
    Ok(Info { config, has_passphrase, running: st.part::<Memory>().running.load(Ordering::Acquire), status: status(&mut conn).await? })
}

pub async fn get(State(st): State<AppState>, _: Admin) -> AppResult<Json<Info>> {
    Ok(Json(info(&st).await?))
}

#[derive(Deserialize)]
pub struct Change {
    #[serde(flatten)]
    pub config: Config,
    /// A new passphrase; kept as it is when left out
    #[serde(default)]
    pub passphrase: Option<String>,
}

pub async fn save(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<Change>) -> AppResult<Json<Info>> {
    let c = req.config;
    let (schedule, tz) = schedule_of(&c)?;
    let keeps = [c.keep_last, c.keep_daily, c.keep_weekly, c.keep_monthly];
    if c.keep_last == 0 || keeps.iter().any(|k| *k > 1000) || c.alert_hours > 24 * 365 {
        return Err(AppError::bad_request("Keep at least the newest copy, and at most 1,000 of each"));
    }
    if let Some(p) = &req.passphrase
        && p.chars().count() < MIN_PASSPHRASE
    {
        return Err(AppError::bad_request(format!("The passphrase needs at least {MIN_PASSPHRASE} characters")));
    }
    {
        let _w = st.write_lock.lock().await;
        let mut tx = db::begin_write(&st.db).await?;
        let user = crate::auth::require_admin(&mut tx, &user).await?;
        if c.enabled || !c.location.is_empty() {
            let (known,): (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM storage_locations WHERE id = ?)").bind(&c.location).fetch_one(&mut *tx).await?;
            if !known {
                return Err(AppError::bad_request("Choose the storage location to keep the copies on"));
            }
        }
        if let Some(p) = &req.passphrase {
            db::set_setting(&mut tx, KEY, &crate::secrets::seal(SEAL, p)).await?;
        }
        let (has,): (bool,) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM settings WHERE key = ? AND value <> '')").bind(KEY).fetch_one(&mut *tx).await?;
        if c.enabled && !has {
            return Err(AppError::bad_request("Set a passphrase to encrypt the copies with"));
        }
        db::set_json_setting(&mut tx, CONFIG, &c).await?;
        let mut s = status(&mut tx).await?;
        s.next_run_at = if c.enabled { policy::next_after(&schedule, &tz, now()) } else { None };
        db::set_json_setting(&mut tx, STATUS, &s).await?;
        let detail = if c.enabled { "Database backups are on" } else { "Database backups are off" };
        crate::logs::record_activity(&mut tx, &user, None, "settings", detail).await?;
        tx.commit().await?;
    }
    st.part::<Memory>().wake.notify_one();
    Ok(Json(info(&st).await?))
}

/// "Back up now": made in the background; the page follows `running` and `status`
pub async fn run_now(State(st): State<AppState>, _: Admin) -> AppResult<Json<Info>> {
    let c: Config = db::json_setting(&mut *st.db.acquire().await?, CONFIG).await?;
    if c.location.is_empty() || db::get_setting(&st.db, KEY).await?.is_none_or(|v| v.is_empty()) {
        return Err(AppError::conflict("Choose a location and a passphrase first"));
    }
    st.part::<Memory>().asked.store(true, Ordering::Release);
    st.part::<Memory>().wake.notify_one();
    let mut i = info(&st).await?;
    i.running = true;
    Ok(Json(i))
}

#[derive(Serialize)]
pub struct Copy {
    pub name: String,
    pub size: u64,
    pub at: i64,
}

/// The copies on the location, newest first
pub async fn copies(State(st): State<AppState>, _: Admin) -> AppResult<Json<Vec<Copy>>> {
    let c: Config = db::json_setting(&mut *st.db.acquire().await?, CONFIG).await?;
    if c.location.is_empty() {
        return Ok(Json(vec![]));
    }
    let dst = st.storage(&c.location)?;
    Ok(Json(list(&*dst).await.map_err(AppError::internal)?))
}

async fn list(dst: &dyn crate::storage::Storage) -> std::io::Result<Vec<Copy>> {
    let entries = match dst.list_dir(DIR).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut out: Vec<Copy> = entries.into_iter().filter_map(|e| Some(Copy { at: copy_time(&e.name)?, size: e.size, name: e.name })).collect();
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.name.cmp(&a.name)));
    Ok(out)
}

/// A copy's name: `thirtyfile-20261019-023000Z.tfdb`, the UTC time it was made
fn copy_name(t: i64) -> String {
    let at = Timestamp::from_second(t).unwrap_or(Timestamp::UNIX_EPOCH);
    format!("thirtyfile-{}.tfdb", at.strftime("%Y%m%d-%H%M%SZ"))
}

fn copy_time(name: &str) -> Option<i64> {
    let stamp = name.strip_prefix("thirtyfile-")?.strip_suffix("Z.tfdb")?;
    let t = jiff::civil::DateTime::strptime("%Y%m%d-%H%M%S", stamp).ok()?;
    Some(t.to_zoned(TimeZone::UTC).ok()?.timestamp().as_second())
}

// ───────────── Making a copy ─────────────

pub fn spawn_scheduler(st: AppState) {
    scheduler::spawn(st, "Database backups", |st| &st.part::<Memory>().wake, |st, t| async move { tick(&st, t).await });
}

/// One look: a copy when one is due or asked for, and administrators told when something changed
pub async fn tick(st: &AppState, t: i64) -> AppResult<()> {
    let mem = st.part::<Memory>();
    let c: Config = db::json_setting(&mut *st.db.acquire().await?, CONFIG).await?;
    let s = status(&mut *st.db.acquire().await?).await?;
    let asked = mem.asked.swap(false, Ordering::AcqRel);
    let due = c.enabled && s.next_run_at.is_some_and(|n| n <= t);
    if (asked || due) && !mem.running.swap(true, Ordering::AcqRel) {
        let st = st.clone();
        tokio::spawn(async move {
            let result = run(&st, &c).await;
            st.part::<Memory>().running.store(false, Ordering::Release);
            if let Err(e) = &result {
                tracing::warn!("Database backup: {}", e.message);
            }
            if let Err(e) = after(&st, &c, result.err().map(|e| e.message)).await {
                tracing::warn!("Database backup: {}", e.message);
            }
        });
        return Ok(());
    }
    if c.enabled { alert(st, &c, &s, t).await } else { Ok(()) }
}

/// Makes a copy now, stores it and deletes the ones no longer kept; returns the copy's name
pub async fn run(st: &AppState, c: &Config) -> AppResult<String> {
    let started = std::time::Instant::now();
    let t = now();
    change_status(st, |s| s.last_attempt_at = Some(t)).await?;
    let sealed = db::get_setting(&st.db, KEY).await?.filter(|v| !v.is_empty()).ok_or_else(|| AppError::conflict("Set a passphrase to encrypt the copies with"))?;
    let passphrase = crate::secrets::open(SEAL, &sealed).map_err(AppError::internal)?;
    let dst = st.storage(&c.location)?;
    let id = crate::util::new_id();
    let plain = st.tmp_dir().join(format!("database-{id}.db"));
    let sealed_file = st.tmp_dir().join(format!("database-{id}.tfdb"));
    let made = async {
        db::backup_to(&st.db, &plain).await?;
        let (from, to) = (plain.clone(), sealed_file.clone());
        tokio::task::spawn_blocking(move || encrypt_file(&from, &to, &passphrase)).await.map_err(AppError::internal)??;
        let size = tokio::fs::metadata(&sealed_file).await?.len();
        let name = copy_name(t);
        let key = format!("{DIR}/{name}");
        dst.put_at(&key, &sealed_file).await?;
        match dst.stat(&key).await? {
            Some(e) if e.size == size => {}
            _ => return Err(AppError::internal("The copy stored on the location isn't complete")),
        }
        Ok::<_, AppError>((name, size))
    }
    .await;
    let _ = tokio::fs::remove_file(&plain).await;
    let _ = tokio::fs::remove_file(&sealed_file).await;
    let (name, size) = made?;
    tidy(&*dst, c).await.map_err(AppError::internal)?;
    let ms = started.elapsed().as_millis() as u64;
    let file = name.clone();
    change_status(st, |s| {
        s.last_success_at = Some(t);
        s.last_file = Some(file);
        s.last_size = Some(size);
        s.last_duration_ms = Some(ms);
    })
    .await?;
    Ok(name)
}

/// After a run: what went wrong, the next time on the schedule, and administrators told
async fn after(st: &AppState, c: &Config, error: Option<String>) -> AppResult<()> {
    let t = now();
    let next = if c.enabled { schedule_of(c).ok().and_then(|(s, tz)| policy::next_after(&s, &tz, t)) } else { None };
    let s = change_status(st, |s| {
        s.error = error;
        s.next_run_at = next;
    })
    .await?;
    alert(st, c, &s, t).await
}

async fn alert(st: &AppState, c: &Config, s: &Status, t: i64) -> AppResult<()> {
    let overdue = c.alert_hours > 0 && s.last_success_at.or(s.last_attempt_at).is_some_and(|l| t - l > i64::from(c.alert_hours) * 3600);
    let state = if s.error.is_some() {
        "failing"
    } else if overdue {
        "overdue"
    } else {
        ""
    };
    // Nothing to say before the first copy was tried
    if s.last_attempt_at.is_none() && state.is_empty() {
        return Ok(());
    }
    let a = scheduler::Alert {
        state,
        alerted: &s.alerted,
        record: "UPDATE settings SET value = json_set(value, '$.alerted', ?1) WHERE key = ?2",
        id: STATUS,
        notice: "backup",
        data: json!({ "name": "Database", "database": true, "error": s.error, "since": s.last_success_at }),
        action: "backup_alert",
        name: "Database",
    };
    scheduler::alert(st, a, t).await
}

// ───────────── Keeping ─────────────

/// The copies of `copies` (name, time) that are kept, in the time zone `tz`
fn kept(copies: &[(String, i64)], c: &Config, tz: &TimeZone) -> HashSet<String> {
    let mut sorted: Vec<&(String, i64)> = copies.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    let mut keep: HashSet<String> = sorted.iter().take(c.keep_last.max(1) as usize).map(|(n, _)| n.clone()).collect();
    // The newest of each day, week and month, for so many of them
    let mut periods = |count: u32, period: &dyn Fn(&jiff::Zoned) -> (i32, i32)| {
        let mut seen = HashSet::new();
        for (name, t) in &sorted {
            let Ok(at) = Timestamp::from_second(*t) else { continue };
            let p = period(&at.to_zoned(tz.clone()));
            if seen.len() >= count as usize && !seen.contains(&p) {
                break;
            }
            if seen.insert(p) {
                keep.insert(name.clone());
            }
        }
    };
    periods(c.keep_daily, &|z| (i32::from(z.year()), i32::from(z.day_of_year())));
    periods(c.keep_weekly, &|z| {
        let w = z.date().iso_week_date();
        (i32::from(w.year()), i32::from(w.week()))
    });
    periods(c.keep_monthly, &|z| (i32::from(z.year()), i32::from(z.month())));
    keep
}

/// Deletes the copies no longer kept (after a new one was stored)
async fn tidy(dst: &dyn crate::storage::Storage, c: &Config) -> std::io::Result<()> {
    let tz = policy::time_zone(&c.tz).unwrap_or(TimeZone::UTC);
    let copies: Vec<(String, i64)> = list(dst).await?.into_iter().map(|c| (c.name, c.at)).collect();
    let keep = kept(&copies, c, &tz);
    for (name, _) in copies.iter().filter(|(n, _)| !keep.contains(n)) {
        dst.delete_at(&format!("{DIR}/{name}")).await?;
    }
    Ok(())
}

// ───────────── Encryption ─────────────

fn key_of(passphrase: &str, salt: &[u8], (m, t, p): (u32, u32, u32)) -> std::io::Result<[u8; 32]> {
    let params = argon2::Params::new(m, t, p, Some(32)).map_err(std::io::Error::other)?;
    let mut key = [u8::default(); 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(std::io::Error::other)?;
    Ok(key)
}

fn nonce(prefix: &[u8; 7], n: u32, last: bool) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[..7].copy_from_slice(prefix);
    out[7..11].copy_from_slice(&n.to_be_bytes());
    out[11] = u8::from(last);
    out
}

/// Reads as much as fills `buf` (less only at the end); the number of bytes read
fn fill(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Encrypts the file `src` into `dst` (blocking)
pub fn encrypt_file(src: &Path, dst: &Path, passphrase: &str) -> std::io::Result<()> {
    let salt: [u8; 16] = rand::random();
    let prefix: [u8; 7] = rand::random();
    let mut header = MAGIC.to_vec();
    for v in [ARGON.0, ARGON.1, ARGON.2] {
        header.extend_from_slice(&v.to_le_bytes());
    }
    header.extend_from_slice(&salt);
    header.extend_from_slice(&prefix);
    let cipher = Aes256Gcm::new(&key_of(passphrase, &salt, ARGON)?.into());
    let mut input = std::io::BufReader::new(std::fs::File::open(src)?);
    let mut out = std::io::BufWriter::new(std::fs::File::create(dst)?);
    out.write_all(&header)?;
    let (mut this, mut next) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let mut len = fill(&mut input, &mut this)?;
    for n in 0u32.. {
        // A chunk is the last when nothing comes after it
        let next_len = if len == CHUNK { fill(&mut input, &mut next)? } else { 0 };
        let last = next_len == 0;
        let sealed = cipher
            .encrypt(&Nonce::try_from(&nonce(&prefix, n, last)[..]).map_err(std::io::Error::other)?, Payload { msg: &this[..len], aad: &header })
            .map_err(|_| std::io::Error::other("couldn't encrypt the copy"))?;
        out.write_all(&sealed)?;
        if last {
            break;
        }
        std::mem::swap(&mut this, &mut next);
        len = next_len;
    }
    out.into_inner().map_err(|e| e.into_error())?.sync_all()
}

/// The message for a copy that can't be decrypted
pub const UNREADABLE: &str = "The passphrase is wrong, or the file isn't a complete ThirtyFile database backup";

/// Decrypts the copy `src` into `dst` (blocking); `UNREADABLE` when the passphrase or the copy is wrong
pub fn decrypt_file(src: &Path, dst: &Path, passphrase: &str) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let mut input = std::io::BufReader::new(std::fs::File::open(src).map_err(io)?);
    let mut header = [u8::default(); 8 + 12 + 16 + 7];
    if fill(&mut input, &mut header).map_err(io)? != header.len() || &header[..8] != MAGIC {
        return Err(UNREADABLE.into());
    }
    let word = |i: usize| u32::from_le_bytes(header[8 + 4 * i..12 + 4 * i].try_into().unwrap_or_default());
    let params = (word(0), word(1), word(2));
    // Within what this version makes, so a damaged header can't ask for gigabytes of memory
    if params.0 > 1024 * 1024 || params.1 > 16 || params.2 > 8 {
        return Err(UNREADABLE.into());
    }
    let salt = &header[20..36];
    let prefix: [u8; 7] = header[36..43].try_into().map_err(|_| UNREADABLE.to_string())?;
    let cipher = Aes256Gcm::new(&key_of(passphrase, salt, params).map_err(io)?.into());
    let mut out = std::io::BufWriter::new(std::fs::File::create(dst).map_err(io)?);
    let (mut this, mut next) = (vec![0u8; CHUNK + TAG], vec![0u8; CHUNK + TAG]);
    let mut len = fill(&mut input, &mut this).map_err(io)?;
    for n in 0u32.. {
        let next_len = if len == CHUNK + TAG { fill(&mut input, &mut next).map_err(io)? } else { 0 };
        let last = next_len == 0;
        let nonce = Nonce::try_from(&nonce(&prefix, n, last)[..]).map_err(|_| UNREADABLE.to_string())?;
        let plain = cipher.decrypt(&nonce, Payload { msg: &this[..len], aad: &header }).map_err(|_| UNREADABLE.to_string())?;
        out.write_all(&plain).map_err(io)?;
        if last {
            break;
        }
        std::mem::swap(&mut this, &mut next);
        len = next_len;
    }
    out.into_inner().map_err(|e| e.into_error().to_string())?.sync_all().map_err(io)
}

// ───────────── Restoring ─────────────

/// `thirtyfile restore-database <file>`: the copy decrypted into the data folder's `drive.db`, which mustn't be there
/// yet (a new data folder). The passphrase comes from `THIRTYFILE_BACKUP_PASSPHRASE`, else from the input. A copy from
/// a newer version, or one this version can't use, is refused; an older one is upgraded when the server starts.
pub async fn restore_cli(file: &Path, data: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if has_database(data) {
        return Err(format!("{} already has a database: restore into a new, empty data folder", data.display()).into());
    }
    let passphrase = match std::env::var("THIRTYFILE_BACKUP_PASSPHRASE") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!("Type the passphrase of the database backups and press Enter:");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim_end_matches(['\r', '\n']).to_string()
        }
    };
    restore(file, data, passphrase).await?;
    println!(
        "The database is restored to {}. Put secret.key back in the data folder (or set THIRTYFILE_SECRET_KEY), start ThirtyFile, then restore the spaces' files from their backups.",
        data.join("drive.db").display()
    );
    Ok(())
}

fn has_database(data: &Path) -> bool {
    ["drive.db", "drive.db-wal", "drive.db-shm"].iter().any(|f| data.join(f).exists())
}

/// The copy `file` decrypted with `passphrase` into the data folder `data` (`restore_cli`)
pub async fn restore(file: &Path, data: &Path, passphrase: String) -> Result<(), Box<dyn std::error::Error>> {
    let target = data.join("drive.db");
    if has_database(data) {
        return Err(format!("{} already has a database: restore into a new, empty data folder", data.display()).into());
    }
    std::fs::create_dir_all(data)?;
    let tmp: PathBuf = data.join(format!("drive.db.restoring-{}", crate::util::new_id()));
    let (from, to) = (file.to_path_buf(), tmp.clone());
    let decrypted = tokio::task::spawn_blocking(move || decrypt_file(&from, &to, &passphrase)).await?;
    let checked = match decrypted {
        Ok(()) => db::check_existing(&tmp).await,
        Err(e) => Err(e),
    };
    if let Err(e) = checked {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

#[cfg(test)]
mod tests;
