use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    SqliteConnection, SqlitePool,
    sqlite::{SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

use crate::{
    error::AppResult,
    util::{new_id, now},
};

/// Opens the database; `cache_mb` is the page cache of each connection (`THIRTYFILE_DB_CACHE_MB`)
pub async fn connect(path: &Path, cache_mb: u32) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    open(path, cache_mb, &sqlx::migrate!("./migrations")).await
}

/// `connect` with the given migrations: the tests add one to see an upgrade through
async fn open(path: &Path, cache_mb: u32, migrator: &sqlx::migrate::Migrator) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?.create_if_missing(true);
    // A new database keeps track of the pages it no longer uses, so that `shrink` can give them back to the disk. It is
    // set once, by the connection that creates the file (on a connection to a database that has it, the setting is a
    // write, which would make each new connection wait for the write lock). A database made before can't change it
    // without being rewritten: `thirtyfile compact` (`compact`) does that.
    if !std::fs::metadata(path).is_ok_and(|m| m.len() > 0) {
        let first = opts.clone().auto_vacuum(SqliteAutoVacuum::Incremental).journal_mode(SqliteJournalMode::Wal);
        sqlx::Connection::close(sqlx::ConnectOptions::connect(&first).await?).await?;
    }
    let opts = opts
        .journal_mode(SqliteJournalMode::Wal)
        // The WAL file grows while a long read (a backup writing its manifest) keeps it from being checkpointed: once
        // it can start over, it is cut back to 64 MB instead of keeping its largest size for good
        .pragma("journal_size_limit", JOURNAL_SIZE_LIMIT.to_string())
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(15))
        .foreign_keys(true)
        // Page cache per connection (16 MB by default, 8 connections): the file tree, grants and blobs of a small
        // server stay in memory, and the operating system caches the rest of the file
        .pragma("cache_size", format!("-{}", u64::from(cache_mb) * 1024))
        // Temporary tables (sorting, recursive CTEs) in memory instead of on disk
        .pragma("temp_store", "MEMORY")
        // Memory-mapped reads (up to 256 MB): read queries skip the extra copy through the page cache. The mapped
        // pages are the operating system's file cache, which it can drop when memory is short
        .pragma("mmap_size", "268435456")
        // Sorting names the way File Explorer does ("File 2" before "File 10", letter case ignored in every language)
        .collation("natural_name", crate::util::natural_cmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .after_connect(|conn, _| {
            Box::pin(async move {
                register_functions(conn).await?;
                count_writes(conn).await
            })
        })
        .connect_with(opts)
        .await?;
    // Said plainly, before the database is copied or changed (`check_existing` usually said it already)
    if let Ok(applied) = sqlx::query_as::<_, (i64, Vec<u8>)>("SELECT version, checksum FROM _sqlx_migrations WHERE success = 1").fetch_all(&pool).await
        && let Some(message) = unusable(&applied, migrator)
    {
        return Err(message.into());
    }
    backup_before_migrations(&pool, migrator, path).await?;
    let (listed_before,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'nodes_listed')").fetch_one(&pool).await?;
    migrator.run(&pool).await?;
    keep_natural_order(&pool, listed_before).await?;
    optimize(&pool).await;
    Ok(pool)
}

/// The order of `util::natural_cmp`, which the index `nodes_listed` keeps names in (migrations/0015_indexes.sql):
/// raised whenever natural_cmp orders names differently
const NATURAL_ORDER: u32 = 1;

/// Rebuilds the indexes in the order of `natural_cmp` when it changed since they were built: in another version of
/// ThirtyFile (`NATURAL_ORDER`), or with other letter-case rules (the Unicode version Rust's `to_lowercase` follows). An
/// index out of order would list a folder's items in the wrong order, or leave some out. `built`: whether the index was
/// there before this start (else it was just built, in the current order).
async fn keep_natural_order(pool: &SqlitePool, built: bool) -> Result<(), sqlx::Error> {
    let (major, minor, update) = char::UNICODE_VERSION;
    let order = format!("{NATURAL_ORDER} (Unicode {major}.{minor}.{update})");
    if get_setting(pool, "natural_order").await?.as_deref() == Some(order.as_str()) {
        return Ok(());
    }
    let mut tx = begin_write(pool).await?;
    if built {
        tracing::info!("Sorting names differently than before: rebuilding the index of folder listings");
        sqlx::query("REINDEX natural_name").execute(&mut *tx).await?;
    }
    set_setting(&mut tx, "natural_order", &order).await?;
    tx.commit().await
}

/// The upgrade guide, which says what to do with data from a version that can't be upgraded
pub const UPGRADE_GUIDE: &str = "https://thirtyfile.github.io/ThirtyFile/docs/backup.html#upgrade";

/// Plain messages for a database this version can't use: its migrations (`_sqlx_migrations`: version and checksum)
/// compared with this version's. None when it can be used (new, current, or with migrations still to apply).
fn unusable(applied: &[(i64, Vec<u8>)], migrator: &sqlx::migrate::Migrator) -> Option<String> {
    let known = |v: i64| migrator.iter().find(|m| m.version == v && !m.migration_type.is_down_migration());
    let first = applied.iter().find(|(v, _)| *v == 1);
    let same_start = first.is_some_and(|(_, sum)| known(1).is_some_and(|m| m.checksum.as_ref() == sum.as_slice()));
    let newest = applied.iter().map(|(v, _)| *v).max()?;
    if newest > 1 && !same_start {
        // 0.3 and older kept one migration per change; the database was started over after 0.3
        return Some(format!(
            "This database is from ThirtyFile 0.3 or older, which can't be upgraded to this version. Nothing was changed. See how to move your files over in the upgrade guide: {UPGRADE_GUIDE}"
        ));
    }
    if let Some((v, _)) = applied.iter().find(|(v, _)| known(*v).is_none()) {
        return Some(format!(
            "This database was changed by a newer version of ThirtyFile (it has change {v}, which this version doesn't know). Start the newer version again, or restore the copy saved before the upgrade: {UPGRADE_GUIDE}"
        ));
    }
    if let Some((v, _)) = applied.iter().find(|(v, sum)| known(*v).is_some_and(|m| m.checksum.as_ref() != sum.as_slice())) {
        return Some(format!(
            "This database was made by a different build of ThirtyFile (its change {v} isn't the one this version has): an older release or a development build. It can't be used by this version. Nothing was changed. See the upgrade guide: {UPGRADE_GUIDE}"
        ));
    }
    None
}

/// Before anything is written into the data or storage folder: stops with a plain message when the database in
/// `path` is one this version can't use (from 0.3 or older, from a newer version, or from another build). A database
/// that isn't there, or can't be read here, is left to `connect`.
pub async fn check_existing(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let Ok(opts) = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display())) else { return Ok(()) };
    let opts = opts.read_only(true).create_if_missing(false);
    let Ok(mut conn) = sqlx::ConnectOptions::connect(&opts).await else { return Ok(()) };
    let applied: Result<Vec<(i64, Vec<u8>)>, _> = sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations WHERE success = 1").fetch_all(&mut conn).await;
    let _ = sqlx::Connection::close(conn).await;
    match applied {
        Ok(applied) => unusable(&applied, &sqlx::migrate!("./migrations")).map_or(Ok(()), Err),
        // No migrations table: a new database
        Err(_) => Ok(()),
    }
}

/// Brings the query planner's statistics up to date where they are missing or old (after connecting, and daily): without
/// them SQLite can pick an index that reads a whole space instead of the one that finds a single path
pub async fn optimize(pool: &SqlitePool) {
    if let Err(e) = sqlx::query("PRAGMA optimize=0x10002").execute(pool).await {
        tracing::warn!("Couldn't update the database statistics: {e}");
    }
}

/// The size the WAL file is cut back to once it can start over (`journal_size_limit`)
const JOURNAL_SIZE_LIMIT: u64 = 64 << 20;
/// Free pages `shrink` leaves in the database file for the writes to come (4 MB with the 4 KB pages of a database)
const FREE_PAGES_KEPT: i64 = 1000;
/// Free pages `shrink` gives back at most in one go (400 MB), so that the write it is doesn't hold the database long
const FREE_PAGES_GIVEN_BACK: i64 = 100_000;

/// Once a day: gives back to the disk what the database no longer uses. A database that keeps track of its free pages
/// (incremental auto-vacuum: new ones, and others after `compact`) gives back those beyond FREE_PAGES_KEPT, a write
/// to take the server's write lock for (`write_lock`). Then the WAL file is copied into the database and cut to nothing,
/// unless a read still needs it (it is cut back to JOURNAL_SIZE_LIMIT later).
pub async fn shrink(pool: &SqlitePool, write_lock: &tokio::sync::Mutex<()>) {
    let free = async {
        let (mode, free) = free_pages(&mut *pool.acquire().await?).await?;
        if mode == 2 && free > FREE_PAGES_KEPT {
            let _w = write_lock.lock().await;
            let pages = (free - FREE_PAGES_KEPT).min(FREE_PAGES_GIVEN_BACK);
            sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA incremental_vacuum({pages})"))).execute(pool).await?;
        }
        Ok::<_, sqlx::Error>(())
    };
    if let Err(e) = free.await {
        tracing::warn!("Couldn't give the unused space of the database back: {e}");
    }
    if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(pool).await {
        tracing::warn!("Couldn't empty the WAL file of the database: {e}");
    }
}

/// The database's auto-vacuum mode (2: incremental) and its free pages. Read on one connection, the free pages first:
/// that read looks at the database file, which the mode alone doesn't, so a connection that last looked before the
/// mode changed (`compact`) would still say the old one.
async fn free_pages(c: &mut SqliteConnection) -> Result<(i64, i64), sqlx::Error> {
    let (free,): (i64,) = sqlx::query_as("PRAGMA freelist_count").fetch_one(&mut *c).await?;
    let (mode,): (i64,) = sqlx::query_as("PRAGMA auto_vacuum").fetch_one(&mut *c).await?;
    Ok((mode, free))
}

/// `thirtyfile compact`, with ThirtyFile stopped: rewrites the database to the size of what it holds, keeping track
/// of its free pages from then on so that `shrink` gives them back daily (a database made by an earlier version doesn't).
/// Returns its size before and after, in bytes.
pub async fn compact(pool: &SqlitePool) -> Result<(i64, i64), sqlx::Error> {
    let size = async || -> Result<i64, sqlx::Error> {
        let (pages,): (i64,) = sqlx::query_as("PRAGMA page_count").fetch_one(pool).await?;
        let (page,): (i64,) = sqlx::query_as("PRAGMA page_size").fetch_one(pool).await?;
        Ok(pages * page)
    };
    let before = size().await?;
    let mut c = pool.acquire().await?;
    // (it takes effect with VACUUM, which rewrites the database)
    sqlx::query("PRAGMA auto_vacuum = INCREMENTAL").execute(&mut *c).await?;
    sqlx::query("VACUUM").execute(&mut *c).await?;
    drop(c);
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(pool).await?;
    Ok((before, size().await?))
}

/// Automatic backups kept before upgrades (the oldest are removed)
const UPGRADE_BACKUPS: usize = 3;

/// When this version brings migrations the database doesn't have yet, copies the database to
/// `backups/drive-before-<version>.db` next to it first: going back to the older version means restoring it, since
/// an older version refuses to start on a database changed by a newer one.
async fn backup_before_migrations(pool: &SqlitePool, migrator: &sqlx::migrate::Migrator, path: &Path) -> Result<(), sqlx::Error> {
    // A new database has no migrations table yet: nothing to keep
    let Ok(applied) = sqlx::query_as::<_, (i64,)>("SELECT version FROM _sqlx_migrations WHERE success = 1").fetch_all(pool).await else {
        return Ok(());
    };
    let applied: std::collections::HashSet<i64> = applied.into_iter().map(|(v,)| v).collect();
    if applied.is_empty() || migrator.iter().all(|m| applied.contains(&m.version)) {
        return Ok(());
    }
    let dir = path.parent().unwrap_or(Path::new(".")).join("backups");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("drive-before-{}.db", crate::VERSION));
    if file.exists() {
        // An earlier start of this version failed after the copy: keep that one, it has the old schema
        return Ok(());
    }
    tracing::info!("Upgrading the database: saving a copy of it first in {}", file.display());
    backup_to(pool, &file).await?;
    // Keep the newest few
    let mut old: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("drive-before-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    old.sort();
    for (_, p) in old.iter().rev().skip(UPGRADE_BACKUPS) {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// A consistent copy of the running database in one file (`VACUUM INTO`, safe while the server writes, unlike copying
/// drive.db with its WAL file). Refuses to overwrite an existing file.
pub async fn backup_to(pool: &SqlitePool, file: &Path) -> Result<(), sqlx::Error> {
    if file.exists() {
        return Err(sqlx::Error::Protocol(format!("{} already exists", file.display())));
    }
    sqlx::query("VACUUM INTO ?").bind(file.to_string_lossy().into_owned()).execute(pool).await?;
    Ok(())
}

/// Registers `unicode_lower(text)` on a connection: lower case in every language, where SQLite's `lower()` and
/// `NOCASE` only fold A–Z. Names in the content store are unique by this key (the `name_key` column), so it must be
/// the same rule as Rust's `to_lowercase()`, which the server uses to compare names in memory.
///
/// The column's index calls the function, so the database can only be changed by ThirtyFile itself (reading it with
/// the `sqlite3` tool works, as long as `name_key` isn't selected).
async fn register_functions(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    use libsqlite3_sys as ffi;
    use std::ffi::{c_int, c_void};

    unsafe extern "C" fn unicode_lower(ctx: *mut ffi::sqlite3_context, argc: c_int, argv: *mut *mut ffi::sqlite3_value) {
        // SAFETY: SQLite passes `argc` valid values, and the text pointer is valid for `sqlite3_value_bytes` bytes
        // until the next call on this value. The result is copied by SQLite (SQLITE_TRANSIENT).
        unsafe {
            if argc != 1 {
                ffi::sqlite3_result_null(ctx);
                return;
            }
            let value = *argv;
            if ffi::sqlite3_value_type(value) == ffi::SQLITE_NULL {
                ffi::sqlite3_result_null(ctx);
                return;
            }
            let text = ffi::sqlite3_value_text(value);
            let len = ffi::sqlite3_value_bytes(value);
            let bytes = if text.is_null() { &[][..] } else { std::slice::from_raw_parts(text, len.max(0) as usize) };
            let lower = String::from_utf8_lossy(bytes).to_lowercase();
            ffi::sqlite3_result_text(ctx, lower.as_ptr().cast(), lower.len() as c_int, ffi::SQLITE_TRANSIENT());
        }
    }

    let mut handle = conn.lock_handle().await?;
    let db = handle.as_raw_handle().as_ptr();
    let flags = ffi::SQLITE_UTF8 | ffi::SQLITE_DETERMINISTIC | ffi::SQLITE_INNOCUOUS;
    // SAFETY: `db` is the open connection, held locked for the call; the function has no user data or destructor.
    let rc =
        unsafe { ffi::sqlite3_create_function_v2(db, c"unicode_lower".as_ptr(), 1, flags, std::ptr::null_mut::<c_void>(), Some(unicode_lower), None, None, None) };
    if rc != ffi::SQLITE_OK {
        return Err(sqlx::Error::Protocol(format!("Couldn't register unicode_lower() (SQLite error {rc})")));
    }
    Ok(())
}

/// Counts each write saved on a connection (`WRITES`), whoever made it: SQLite calls the WAL hook once a transaction
/// that changed the database has committed, before the statement that committed it returns, so anything read after
/// the count was taken sees that write.
///
/// The hook replaces SQLite's own, which checkpoints the WAL file once it holds 1000 pages (`wal_autocheckpoint`,
/// which would replace this hook in turn, so it isn't set): this one checkpoints the same way.
async fn count_writes(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    use libsqlite3_sys as ffi;
    use std::ffi::{c_char, c_int, c_void};

    const CHECKPOINT_PAGES: c_int = 1000;

    unsafe extern "C" fn committed(_: *mut c_void, db: *mut ffi::sqlite3, name: *const c_char, pages: c_int) -> c_int {
        WRITES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if pages >= CHECKPOINT_PAGES {
            // SAFETY: SQLite passes the connection and the name of the database that was written, valid for the call.
            // A passive checkpoint, like SQLite's own: a reader in the way only leaves part of the WAL for later.
            unsafe { ffi::sqlite3_wal_checkpoint(db, name) };
        }
        ffi::SQLITE_OK
    }

    let mut handle = conn.lock_handle().await?;
    let db = handle.as_raw_handle().as_ptr();
    // SAFETY: `db` is the open connection, held locked for the call; the hook has no user data.
    unsafe { ffi::sqlite3_wal_hook(db, Some(committed), std::ptr::null_mut()) };
    Ok(())
}

pub async fn get_setting(db: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    Ok(sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(db).await?.map(|(v,)| v))
}

pub async fn set_setting(conn: &mut SqliteConnection, key: &str, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(conn)
        .await?;
    Ok(())
}

/// Starts a transaction that writes (`BEGIN IMMEDIATE`): every transaction that may write starts here, never with
/// `begin()` (clippy.toml refuses it). It takes SQLite's write lock at once, waiting for it up to the busy timeout
/// (`connect`), and keeps it until it ends: start it after taking `st.write_lock`, once slow work (hashing, other
/// storage) is done.
///
/// A deferred transaction (`BEGIN`) only asks for the lock at its first write, usually after reading. If another
/// connection holds the lock then, SQLite answers "database is locked" at once instead of waiting, as waiting could
/// deadlock. That happens when a transaction is dropped after writing (a handler returning an error with `?`): sqlx
/// rolls it back later, in the background, so its connection may still hold the lock when the next writer, which has
/// the server's write lock by then, starts writing.
///
/// Read-only transactions (a consistent view over several queries) may still use a deferred `begin()`, with
/// `#[allow(clippy::disallowed_methods)]` and a reason.
pub async fn begin_write(pool: &SqlitePool) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, sqlx::Error> {
    pool.begin_with("BEGIN IMMEDIATE").await
}

/// Writes saved so far, by any connection and any code (`count_writes`): what was read after taking this count is out
/// of date only once it changes (shares/public.rs keeps a link's details for a moment, as long as nothing was changed
/// meanwhile).
///
/// Counted when a write is saved, not when its transaction begins: a read between the two would see the database as
/// it was, yet be kept as if it came after the change.
static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn writes() -> u64 {
    WRITES.load(std::sync::atomic::Ordering::SeqCst)
}

/// Ends a write transaction by what the work in it returned: committed when it succeeded, rolled back when it failed,
/// before the write lock is released. A transaction that is only dropped rolls back later, in the background, and keeps
/// SQLite's write lock meanwhile: harmless, as the next writer (`begin_write`) waits for it, but settling it releases
/// the lock straight away.
pub async fn settle<T>(tx: sqlx::Transaction<'_, sqlx::Sqlite>, res: AppResult<T>) -> AppResult<T> {
    match res {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}

pub async fn location_exists(conn: &mut SqliteConnection, id: &str) -> Result<bool, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM storage_locations WHERE id = ?").bind(id).fetch_one(&mut *conn).await?;
    Ok(n > 0)
}

/// Checks a storage location an administrator chose for a new space
pub async fn check_location(conn: &mut SqliteConnection, id: &str) -> AppResult<()> {
    if location_exists(conn, id).await? { Ok(()) } else { Err(crate::error::AppError::not_found("Storage location not found")) }
}

/// Creates a space and its root folder on the storage location `location_id`, returning (space id, root folder id).
/// The space records the location for good (only a move changes it, moves/): its files go there, or its folder
/// once `space_folders::make_folder_space` makes it a folder space. The caller creates the access grants separately.
pub async fn create_drive(
    conn: &mut SqliteConnection,
    name: &str,
    kind: &str,
    owner_id: i64,
    quota_bytes: i64,
    location_id: &str,
) -> Result<(String, String), sqlx::Error> {
    let drive_id = new_id();
    let root_id = new_id();
    let ts = now();
    sqlx::query(
        "INSERT INTO nodes (id, owner_id, parent_id, kind, name, drive_id, created_at, updated_at)
         VALUES (?, ?, NULL, 'folder', '', ?, ?, ?)",
    )
    .bind(&root_id)
    .bind(owner_id)
    .bind(&drive_id)
    .bind(ts)
    .bind(ts)
    .execute(&mut *conn)
    .await?;
    sqlx::query("INSERT INTO drives (id, name, kind, root_id, owner_id, quota_bytes, created_by, created_at, location_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&drive_id)
        .bind(name)
        .bind(kind)
        .bind(&root_id)
        .bind(owner_id)
        .bind(quota_bytes)
        .bind(owner_id)
        .bind(ts)
        .bind(location_id)
        .execute(&mut *conn)
        .await?;
    Ok((drive_id, root_id))
}

pub async fn add_grant(
    conn: &mut SqliteConnection,
    node_id: &str,
    principal_type: &str,
    principal_id: i64,
    role: &str,
    granted_by: Option<i64>,
    expires_at: Option<i64>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO grants (node_id, principal_type, principal_id, role, granted_by, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (node_id, principal_type, principal_id)
         DO UPDATE SET role = excluded.role, granted_by = excluded.granted_by, expires_at = excluded.expires_at",
    )
    .bind(node_id)
    .bind(principal_type)
    .bind(principal_id)
    .bind(role)
    .bind(granted_by)
    .bind(now())
    .bind(expires_at)
    .execute(conn)
    .await?;
    Ok(())
}

/// Tables whose ids are never given out twice
pub enum Counted {
    Users,
    Groups,
}

/// The next id for a user or group: one higher than any id given out before, even when that row was deleted since, so
/// a new account or group can't pick up log entries or sign-in settings still pointing at a deleted one
pub async fn next_id(conn: &mut SqliteConnection, table: Counted) -> Result<i64, sqlx::Error> {
    let (name, sql) = match table {
        Counted::Users => (
            "users",
            "INSERT INTO id_counters (name, last) VALUES (?1, (SELECT COALESCE(MAX(id), 0) FROM users) + 1)
             ON CONFLICT (name) DO UPDATE SET last = MAX(id_counters.last, (SELECT COALESCE(MAX(id), 0) FROM users)) + 1
             RETURNING last",
        ),
        Counted::Groups => (
            "groups",
            "INSERT INTO id_counters (name, last) VALUES (?1, (SELECT COALESCE(MAX(id), 0) FROM groups) + 1)
             ON CONFLICT (name) DO UPDATE SET last = MAX(id_counters.last, (SELECT COALESCE(MAX(id), 0) FROM groups)) + 1
             RETURNING last",
        ),
    };
    let (id,): (i64,) = sqlx::query_as(sql).bind(name).fetch_one(&mut *conn).await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database file whose migrations table holds `applied` (version, checksum), as an earlier version left it
    async fn database_with(dir: &Path, applied: &[(i64, Vec<u8>)]) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("drive.db");
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display())).unwrap().create_if_missing(true);
        let mut c = sqlx::ConnectOptions::connect(&opts).await.unwrap();
        sqlx::query(
            "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                                            success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)",
        )
        .execute(&mut c)
        .await
        .unwrap();
        for (v, sum) in applied {
            sqlx::query("INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, 'old', 1, ?, 0)")
                .bind(v)
                .bind(sum)
                .execute(&mut c)
                .await
                .unwrap();
        }
        sqlx::Connection::close(c).await.unwrap();
        path
    }

    #[tokio::test]
    async fn databases_this_version_cant_use_are_refused_plainly_and_left_alone() {
        let base = std::env::temp_dir().join(format!("thirtyfile-old-{}", crate::util::new_id()));
        let current = sqlx::migrate!("./migrations").iter().find(|m| m.version == 1).unwrap().checksum.to_vec();
        let cases = [
            // 0.3 kept a migration per change
            ("0.3", (1..=24).map(|v| (v, vec![v as u8; 48])).collect::<Vec<_>>(), "ThirtyFile 0.3 or older"),
            ("newer", vec![(1, current.clone()), (9999, vec![2; 48])], "a newer version"),
            ("other build", vec![(1, vec![9; 48])], "a different build"),
        ];
        for (what, applied, says) in cases {
            let dir = base.join(what.replace(' ', "-"));
            let path = database_with(&dir, &applied).await;
            let e = check_existing(&path).await.unwrap_err();
            assert!(e.contains(says) && e.contains(UPGRADE_GUIDE), "{what}: {e}");
            let e = connect(&path, 16).await.unwrap_err().to_string();
            assert!(e.contains(says), "{what}: {e}");
            assert!(!dir.join("backups").exists(), "{what}: nothing copied");
        }
        // A new database, and one from this version, are fine
        check_existing(&base.join("none").join("drive.db")).await.unwrap();
        let db = connect(&base.join("drive.db"), 16).await.unwrap();
        db.close().await;
        check_existing(&base.join("drive.db")).await.unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn items_of_folder_spaces_from_0_4_0_are_kept_without_a_birth_time() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        // As 0.4.0 left it: its one migration, and an item of a folder space
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at) VALUES (1, 'amy', 'x', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO nodes (id, owner_id, kind, name, created_at, updated_at, fs_path, fs_ino) VALUES ('n', 1, 'folder', 'Docs', 0, 0, 'Docs', 7)")
            .execute(&db)
            .await
            .unwrap();
        db.close().await;
        // Until the next scan records when it was created (folders.rs)
        let db = connect(&path, 16).await.unwrap();
        let (ino, birth): (Option<i64>, Option<i64>) = sqlx::query_as("SELECT fs_ino, fs_birth_ns FROM nodes WHERE id = 'n'").fetch_one(&db).await.unwrap();
        assert_eq!((ino, birth), (Some(7), None));
        db.close().await;
        assert!(dir.join("backups").join(format!("drive-before-{}.db", crate::VERSION)).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_from_0_4_0_gets_an_empty_error_log_and_keeps_its_logs() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-errors-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO activity (at, username, node_name, action) VALUES (1, 'amy', 'Plan.docx', 'upload')").execute(&db).await.unwrap();
        assert!(sqlx::query("SELECT id FROM error_log").fetch_all(&db).await.is_err(), "0.4.0 has no error log");
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        let (activity,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM activity").fetch_one(&db).await.unwrap();
        assert_eq!(activity, 1);
        sqlx::query("INSERT INTO error_log (at, first_at, source, severity, fingerprint) VALUES (2, 2, 'backend', 'error', 'f')").execute(&db).await.unwrap();
        let (count, message): (i64, String) = sqlx::query_as("SELECT count, message FROM error_log").fetch_one(&db).await.unwrap();
        assert_eq!((count, message.as_str()), (1, ""));
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_from_0_4_0_gets_the_list_of_unfinished_changes() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        // As 0.4.0 left it: its one migration, and a folder in the trash
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at) VALUES (1, 'amy', 'x', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO nodes (id, owner_id, kind, name, created_at, updated_at, trashed_at, trash_id, trash_root) VALUES ('n', 1, 'folder', 'Old', 0, 0, 5, 't', 1)")
            .execute(&db)
            .await
            .unwrap();
        db.close().await;
        let db = connect(&path, 16).await.unwrap();
        // The item is kept; a change to it can be recorded, and goes with it
        sqlx::query("INSERT INTO tree_changes (id, kind, node_id, created_at) VALUES ('c', 'purge', 'n', 0)").execute(&db).await.unwrap();
        assert!(sqlx::query("INSERT INTO tree_changes (id, kind, node_id, created_at) VALUES ('d', 'other', 'n', 0)").execute(&db).await.is_err());
        sqlx::query("DELETE FROM nodes WHERE id = 'n'").execute(&db).await.unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tree_changes").fetch_one(&db).await.unwrap();
        assert_eq!(n, 0);
        db.close().await;
        assert!(dir.join("backups").join(format!("drive-before-{}.db", crate::VERSION)).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_from_0_4_0_gets_empty_lists_of_copies_and_keeps_its_locations() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-copies-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, created_at) VALUES ('nas', 'NAS', 'local', '{}', 0)").execute(&db).await.unwrap();
        assert!(sqlx::query("SELECT id FROM backup_sets").fetch_all(&db).await.is_err(), "0.4.0 has no copies");
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM storage_locations").fetch_one(&db).await.unwrap();
        assert_eq!(n, 2);
        sqlx::query("INSERT INTO backup_sets (id, kind, name, dest_location, created_at) VALUES ('s', 'copy', 'Copy', 'nas', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO backup_jobs (id, kind, set_id, created_at) VALUES ('j', 'snapshot', 's', 0)").execute(&db).await.unwrap();
        let (state,): (String,) = sqlx::query_as("SELECT state FROM backup_jobs WHERE id = 'j'").fetch_one(&db).await.unwrap();
        assert_eq!(state, "queued");
        // A location holding a copy can't go
        assert!(sqlx::query("DELETE FROM storage_locations WHERE id = 'nas'").execute(&db).await.is_err());
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_from_0_4_0_counts_the_changes_of_its_spaces_from_then_on() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-changes-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at) VALUES (1, 'amy', 'x', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO nodes (id, owner_id, kind, name, created_at, updated_at) VALUES ('r', 1, 'folder', '', 0, 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO drives (id, name, kind, root_id, owner_id, created_at, location_id) VALUES ('d', 'My files', 'personal', 'r', 1, 0, 'local')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("UPDATE nodes SET drive_id = 'd' WHERE id = 'r'").execute(&db).await.unwrap();
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        // Nothing counted before the upgrade; every change after it
        let count =
            || async { sqlx::query_as::<_, (i64,)>("SELECT COALESCE((SELECT seq FROM space_changes WHERE drive_id = 'd'), 0)").fetch_one(&db).await.unwrap().0 };
        assert_eq!(count().await, 0);
        sqlx::query("INSERT INTO nodes (id, owner_id, parent_id, kind, name, drive_id, created_at, updated_at) VALUES ('f', 1, 'r', 'folder', 'Docs', 'd', 0, 0)")
            .execute(&db)
            .await
            .unwrap();
        let first = count().await;
        assert!(first > 0);
        sqlx::query("UPDATE nodes SET name = 'Papers' WHERE id = 'f'").execute(&db).await.unwrap();
        assert!(count().await > first);
        sqlx::query("INSERT INTO backup_sets (id, kind, name, dest_location, created_at) VALUES ('s', 'policy', 'Nightly', 'local', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO backup_policies (set_id, mode, created_at, updated_at) VALUES ('s', 'scheduled', 0, 0)").execute(&db).await.unwrap();
        let (schedule, keep): (String, i64) = sqlx::query_as("SELECT schedule, keep_days FROM backup_policies").fetch_one(&db).await.unwrap();
        assert_eq!((schedule.as_str(), keep), (r#"{"daily":"03:00"}"#, 30));
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_from_0_4_0_gets_empty_lists_of_replicas() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-replicas-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, created_at) VALUES ('nas', 'NAS', 'local', '{}', 0)").execute(&db).await.unwrap();
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        sqlx::query("INSERT INTO replica_policies (id, name, source_location, created_at, updated_at) VALUES ('p', 'Mirror', 'local', 0, 0)")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO replica_targets (policy_id, location_id, priority) VALUES ('p', 'nas', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO replica_copies (hash, location_id, size, created_at) VALUES ('h', 'nas', 1, 0)").execute(&db).await.unwrap();
        let (copies, fallback, state): (i64, bool, String) =
            sqlx::query_as("SELECT copies, read_fallback, (SELECT state FROM replica_copies) FROM replica_policies").fetch_one(&db).await.unwrap();
        assert_eq!((copies, fallback, state.as_str()), (1, true, "verified"));
        // Its targets must be locations
        assert!(sqlx::query("INSERT INTO replica_targets (policy_id, location_id, priority) VALUES ('p', 'nowhere', 1)").execute(&db).await.is_err());
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn accounts_from_0_4_0_keep_the_language_they_last_used_and_have_none_chosen() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-languages-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at, lang) VALUES (1, 'amy', 'x', 0, 'zh-TW')").execute(&db).await.unwrap();
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        let (chosen, last): (String, String) = sqlx::query_as("SELECT chosen_lang, lang FROM users WHERE id = 1").fetch_one(&db).await.unwrap();
        assert_eq!((chosen.as_str(), last.as_str()), ("", "zh-TW"));
        sqlx::query("UPDATE users SET chosen_lang = 'ja' WHERE id = 1").execute(&db).await.unwrap();
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn accounts_from_0_4_0_get_the_interface_style_of_their_operating_system() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-styles-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at) VALUES (1, 'amy', 'x', 0)").execute(&db).await.unwrap();
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        let (style,): (String,) = sqlx::query_as("SELECT ui_style FROM users WHERE id = 1").fetch_one(&db).await.unwrap();
        assert_eq!(style, "auto");
        sqlx::query("UPDATE users SET ui_style = 'mac' WHERE id = 1").execute(&db).await.unwrap();
        assert!(sqlx::query("UPDATE users SET ui_style = 'linux' WHERE id = 1").execute(&db).await.is_err());
        // Accounts made after the upgrade start with it too
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at) VALUES (2, 'ben', 'x', 0)").execute(&db).await.unwrap();
        let (style,): (String,) = sqlx::query_as("SELECT ui_style FROM users WHERE id = 2").fetch_one(&db).await.unwrap();
        assert_eq!(style, "auto");
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_from_0_4_0_keeps_what_the_replicas_page_shows() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-replica-counts-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, created_at) VALUES ('nas', 'NAS', 'local', '{}', 0)").execute(&db).await.unwrap();
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        sqlx::query("INSERT INTO replica_policies (id, name, source_location, created_at, updated_at) VALUES ('p', 'Mirror', 'local', 0, 0)")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO replica_targets (policy_id, location_id, priority) VALUES ('p', 'nas', 0)").execute(&db).await.unwrap();
        // A target's counts aren't worked out until the scheduler or a sync does it
        let (held, wanted): (Option<i64>, Option<i64>) = sqlx::query_as("SELECT held, wanted FROM replica_targets").fetch_one(&db).await.unwrap();
        assert_eq!((held, wanted), (None, None));
        sqlx::query("UPDATE replica_targets SET held = 3, wanted = 4").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO replica_unneeded (location_id, copies, bytes) VALUES ('nas', 2, 100)").execute(&db).await.unwrap();
        let (held, copies): (i64, i64) =
            sqlx::query_as("SELECT t.held, u.copies FROM replica_targets t JOIN replica_unneeded u ON u.location_id = t.location_id").fetch_one(&db).await.unwrap();
        assert_eq!((held, copies), (3, 2));
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn items_from_0_4_0_keep_their_uploader() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-found-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at) VALUES (1, 'amy', 'x', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO nodes (id, owner_id, kind, name, created_at, updated_at, fs_path) VALUES ('n', 1, 'file', 'a.txt', 0, 0, 'a.txt')")
            .execute(&db)
            .await
            .unwrap();
        db.close().await;
        // Which of them a folder check found can't be told any more: they stay as they were
        let db = connect(&path, 16).await.unwrap();
        let sql = format!("SELECT found, owner_name FROM (SELECT n.found AS found, {} FROM nodes n)", crate::tree::NODE_COLS);
        let read = async || -> (i64, String) { sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).fetch_one(&db).await.unwrap() };
        assert_eq!(read().await, (0, "amy".to_string()));
        // Found from now on
        sqlx::query("UPDATE nodes SET found = 1").execute(&db).await.unwrap();
        assert_eq!(read().await, (1, String::new()));
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_from_0_4_0_finds_the_jobs_of_a_policy_by_their_state() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-jobs-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO storage_locations (id, name, kind, config, created_at) VALUES ('nas', 'NAS', 'local', '{}', 0)").execute(&db).await.unwrap();
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        sqlx::query("INSERT INTO backup_sets (id, kind, name, dest_location, created_at) VALUES ('s', 'policy', 'Nightly', 'nas', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO backup_jobs (id, kind, set_id, created_at) VALUES ('j', 'snapshot', 's', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO replica_jobs (id, kind, policy_id, location_id, created_at) VALUES ('r', 'sync', 'p', 'nas', 0)").execute(&db).await.unwrap();
        // The schedulers' questions are answered from an index, not by reading every job
        let plan = |sql: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query_as::<_, (i64, i64, i64, String)>(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}")))
                    .fetch_all(&db)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|r| r.3)
                    .collect::<Vec<_>>()
                    .join(" | ")
            }
        };
        let backup =
            plan("SELECT id FROM backup_jobs WHERE set_id = 's' AND kind = 'snapshot' AND state IN ('queued', 'failed') ORDER BY created_at DESC LIMIT 1").await;
        assert!(backup.contains("USING INDEX backup_jobs_set (set_id=? AND state=?)"), "{backup}");
        let replica =
            plan("SELECT id FROM replica_jobs WHERE policy_id = 'p' AND location_id = 'nas' AND state IN ('queued', 'failed') ORDER BY created_at DESC LIMIT 1")
                .await;
        assert!(replica.contains("USING INDEX replica_jobs_target (policy_id=? AND location_id=? AND state=?)"), "{replica}");
        let (state,): (String,) = sqlx::query_as("SELECT state FROM backup_jobs WHERE id = 'j'").fetch_one(&db).await.unwrap();
        assert_eq!(state, "queued");
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_from_0_4_0_counts_changes_with_an_index_and_only_when_kept_columns_change() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-seq-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at) VALUES (1, 'amy', 'x', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO nodes (id, owner_id, kind, name, created_at, updated_at) VALUES ('r', 1, 'folder', '', 0, 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO drives (id, name, kind, root_id, owner_id, created_at, location_id) VALUES ('d', 'Disk', 'team', 'r', 1, 0, 'local')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("UPDATE nodes SET drive_id = 'd', fs_path = '' WHERE id = 'r'").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO nodes (id, owner_id, parent_id, kind, name, drive_id, fs_path, created_at, updated_at) VALUES ('f', 1, 'r', 'file', 'a.txt', 'd', 'a.txt', 0, 0)")
            .execute(&db)
            .await
            .unwrap();
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        let seq = async || sqlx::query_as::<_, (i64,)>("SELECT COALESCE((SELECT seq FROM space_changes WHERE drive_id = 'd'), 0)").fetch_one(&db).await.unwrap().0;
        let first = seq().await;
        // Bookkeeping, and writes that leave the item as it was, don't count; a new name does
        for sql in [
            "UPDATE nodes SET fs_birth_ns = 5, fs_ino = 7, fs_dev = 1, found = 1, content_by = 1 WHERE id = 'f'",
            "UPDATE nodes SET name = 'a.txt', size = 0 WHERE id = 'f'",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&db).await.unwrap();
            assert_eq!(seq().await, first, "{sql}");
        }
        sqlx::query("UPDATE nodes SET name = 'b.txt', fs_path = 'b.txt' WHERE id = 'f'").execute(&db).await.unwrap();
        assert!(seq().await > first);
        // The next number is read from an index, not from every space
        let plan: Vec<(i64, i64, i64, String)> =
            sqlx::query_as("EXPLAIN QUERY PLAN SELECT COALESCE(MAX(seq), 0) + 1 FROM space_changes").fetch_all(&db).await.unwrap();
        assert!(plan.iter().any(|p| p.3.contains("space_changes_seq")), "{plan:?}");
        let (unused,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'error_log_user'").fetch_one(&db).await.unwrap();
        assert_eq!(unused, 0);
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_database_and_its_wal_file_give_back_the_space_they_no_longer_use() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-shrink-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drive.db");
        let db = connect(&path, 16).await.unwrap();
        let (mode, _) = free_pages(&mut db.acquire().await.unwrap()).await.unwrap();
        let (limit,): (i64,) = sqlx::query_as("PRAGMA journal_size_limit").fetch_one(&db).await.unwrap();
        assert_eq!((mode, limit as u64), (2, JOURNAL_SIZE_LIMIT), "a new database keeps track of its free pages");
        // 40 MB written, then deleted
        sqlx::query("CREATE TABLE scratch (data BLOB)").execute(&db).await.unwrap();
        for _ in 0..40 {
            sqlx::query("INSERT INTO scratch (data) VALUES (zeroblob(1024 * 1024))").execute(&db).await.unwrap();
        }
        sqlx::query("DELETE FROM scratch").execute(&db).await.unwrap();
        let size = |f: &str| std::fs::metadata(dir.join(f)).map_or(0, |m| m.len());
        let lock = tokio::sync::Mutex::new(());
        shrink(&db, &lock).await;
        let (free,): (i64,) = sqlx::query_as("PRAGMA freelist_count").fetch_one(&db).await.unwrap();
        assert!(free <= FREE_PAGES_KEPT, "{free}");
        assert!(size("drive.db") < 20 << 20, "{}", size("drive.db"));
        assert_eq!(size("drive.db-wal"), 0);
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_made_without_auto_vacuum_is_compacted_on_request() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-compact-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drive.db");
        // As earlier versions made it: no track of free pages, which can't be changed without rewriting it
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display())).unwrap().create_if_missing(true).journal_mode(SqliteJournalMode::Wal);
        let mut c = sqlx::ConnectOptions::connect(&opts).await.unwrap();
        sqlx::query("CREATE TABLE scratch (data BLOB)").execute(&mut c).await.unwrap();
        sqlx::query("INSERT INTO scratch (data) VALUES (zeroblob(8 * 1024 * 1024))").execute(&mut c).await.unwrap();
        sqlx::query("DELETE FROM scratch").execute(&mut c).await.unwrap();
        sqlx::Connection::close(c).await.unwrap();

        let db = connect(&path, 16).await.unwrap();
        // Asked on several connections at once: also on others than the one that rewrote the database
        let modes = async || {
            let mut held = Vec::new();
            for _ in 0..3 {
                held.push(db.acquire().await.unwrap());
            }
            let mut modes = Vec::new();
            for c in &mut held {
                modes.push(free_pages(c).await.unwrap().0);
            }
            modes
        };
        assert_eq!(modes().await, [0, 0, 0]);
        let lock = tokio::sync::Mutex::new(());
        shrink(&db, &lock).await;
        let (free,): (i64,) = sqlx::query_as("PRAGMA freelist_count").fetch_one(&db).await.unwrap();
        assert!(free > FREE_PAGES_KEPT, "left as it is: {free}");
        let (before, after) = compact(&db).await.unwrap();
        assert!(before > 8 << 20 && after < 4 << 20, "{before} → {after}");
        assert_eq!(modes().await, [2, 2, 2]);
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn listings_are_reindexed_when_names_sort_differently() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-order-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drive.db");
        let db = connect(&path, 16).await.unwrap();
        let current = get_setting(&db, "natural_order").await.unwrap().unwrap();
        assert!(current.starts_with(&format!("{NATURAL_ORDER} ")), "{current}");
        // As an older version, or a build with other letter-case rules, left it
        set_setting(&mut db.acquire().await.unwrap(), "natural_order", "0").await.unwrap();
        db.close().await;
        let db = connect(&path, 16).await.unwrap();
        assert_eq!(get_setting(&db, "natural_order").await.unwrap(), Some(current));
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_database_from_0_4_0_reads_a_space_a_page_at_a_time() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-040-pages-{}", crate::util::new_id()));
        let path = dir.join("drive.db");
        let v040 = migrations_in(&dir.join("v0.4.0"), &[Path::new("migrations").join("0001_init.sql")], None).await;
        let db = open(&path, 16, &v040).await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, created_at) VALUES (1, 'amy', 'x', 0)").execute(&db).await.unwrap();
        sqlx::query("INSERT INTO nodes (id, owner_id, kind, name, created_at, updated_at, drive_id) VALUES ('f', 1, 'file', 'a.txt', 0, 0, 'd')")
            .execute(&db)
            .await
            .unwrap();
        db.close().await;

        let db = connect(&path, 16).await.unwrap();
        let plan = |sql: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query_as::<_, (i64, i64, i64, String)>(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}")))
                    .fetch_all(&db)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|r| r.3)
                    .collect::<Vec<_>>()
                    .join(" | ")
            }
        };
        // A page of a space's files starts where the last one ended, in id order, without sorting
        let page = plan("SELECT id FROM nodes WHERE drive_id = 'd' AND kind = 'file' AND id > 'a' ORDER BY id LIMIT 200").await;
        assert!(page.contains("USING COVERING INDEX nodes_drive_kind_id (drive_id=? AND kind=? AND id>?)") && !page.contains("TEMP B-TREE"), "{page}");
        // Copies to check again are found without reading every copy of the location
        let recheck = plan("SELECT hash FROM replica_copies WHERE location_id = 'nas' AND state IN ('stale', 'corrupt') ORDER BY hash LIMIT 200").await;
        assert!(recheck.contains("replica_copies_recheck"), "{recheck}");
        // Whether some spaces use a content is answered from the index alone
        let used = plan("SELECT 1 FROM nodes WHERE blob_hash = 'h' AND drive_id IN ('d', 'e')").await;
        assert!(used.contains("COVERING INDEX nodes_blob (blob_hash=? AND drive_id=?)"), "{used}");
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE drive_id = 'd'").fetch_one(&db).await.unwrap();
        assert_eq!(n, 1);
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_database_is_only_copied_on_request_when_it_is_up_to_date() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drive.db");
        let db = connect(&path, 16).await.unwrap();
        backup_before_migrations(&db, &sqlx::migrate!("./migrations"), &path).await.unwrap();
        assert!(!dir.join("backups").exists());
        // On request, never over an existing file
        backup_to(&db, &dir.join("manual.db")).await.unwrap();
        assert!(backup_to(&db, &dir.join("manual.db")).await.is_err());
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn every_saved_write_is_counted_and_the_wal_file_is_still_checkpointed() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-test-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = connect(&dir.join("drive.db"), 16).await.unwrap();
        sqlx::query("CREATE TABLE scratch (data BLOB)").execute(&db).await.unwrap();
        // Counted once saved, also when written outside `begin_write`
        let mut tx = begin_write(&db).await.unwrap();
        sqlx::query("INSERT INTO scratch (data) VALUES (x'00')").execute(&mut *tx).await.unwrap();
        let before = writes();
        tx.commit().await.unwrap();
        assert!(writes() > before);
        let before = writes();
        sqlx::query("DELETE FROM scratch").execute(&db).await.unwrap();
        assert!(writes() > before);
        // 20 MB written in 40 transactions: the WAL file is checkpointed and reused once it holds 1000 pages (4 MB),
        // instead of growing with every write
        for _ in 0..40 {
            sqlx::query("INSERT INTO scratch (data) VALUES (zeroblob(512 * 1024))").execute(&db).await.unwrap();
        }
        let wal = std::fs::metadata(dir.join("drive.db-wal")).unwrap().len();
        assert!(wal < 10 << 20, "{wal}");
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Migrations read from a folder holding `files` from server/migrations, plus `extra` (name, SQL)
    async fn migrations_in(dir: &Path, files: &[std::path::PathBuf], extra: Option<(&str, &str)>) -> sqlx::migrate::Migrator {
        std::fs::create_dir_all(dir).unwrap();
        for f in files {
            std::fs::copy(f, dir.join(f.file_name().unwrap())).unwrap();
        }
        if let Some((name, sql)) = extra {
            std::fs::write(dir.join(name), sql).unwrap();
        }
        sqlx::migrate::Migrator::new(dir).await.unwrap()
    }

    /// The structure is frozen at 0.4.0 (CI checks that 0001_init.sql is the file of v0.4.0): later changes are new
    /// migration files. A data folder made by 0.4.0 is copied before they run, then upgraded with its data kept.
    #[tokio::test]
    async fn a_database_from_0_4_0_is_copied_then_upgraded_when_a_migration_file_is_added() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-upgrade-{}", crate::util::new_id()));
        let path = dir.join("data").join("drive.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut ours: Vec<_> = std::fs::read_dir("migrations").unwrap().map(|e| e.unwrap().path()).collect();
        ours.sort();

        // 0.4.0: 0001_init.sql only, with an account and a setting in it
        let v040 = migrations_in(&dir.join("v0.4.0"), &ours[..1], None).await;
        assert_eq!(v040.iter().map(|m| m.version).collect::<Vec<_>>(), [1]);
        let db = open(&path, 16, &v040).await.unwrap();
        crate::settings::bootstrap_admin(&db, Some("upgrade-test"), None).await.unwrap();
        set_setting(&mut db.acquire().await.unwrap(), "upgrade-test", "kept").await.unwrap();
        db.close().await;

        // This version and one more migration file after it (test-only)
        let next = sqlx::migrate!("./migrations").iter().map(|m| m.version).max().unwrap() + 1;
        let newer = migrations_in(&dir.join("newer"), &ours, Some((&format!("{next:04}_upgrade_test.sql"), "CREATE TABLE upgrade_test (id INTEGER);"))).await;
        check_existing(&path).await.unwrap();
        let db = open(&path, 16, &newer).await.unwrap();
        let applied: Vec<(i64,)> = sqlx::query_as("SELECT version FROM _sqlx_migrations WHERE success = 1 ORDER BY version").fetch_all(&db).await.unwrap();
        assert_eq!(applied.into_iter().map(|(v,)| v).collect::<Vec<_>>(), newer.iter().map(|m| m.version).collect::<Vec<_>>());
        sqlx::query("SELECT id FROM upgrade_test").fetch_all(&db).await.unwrap();
        assert_eq!(get_setting(&db, "upgrade-test").await.unwrap().as_deref(), Some("kept"));
        let (users,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE username = 'admin'").fetch_one(&db).await.unwrap();
        assert_eq!(users, 1);
        db.close().await;

        // The copy is the database as 0.4.0 left it, and 0.4.0 can start on it again
        let copy = dir.join("data").join("backups").join(format!("drive-before-{}.db", crate::VERSION));
        assert!(copy.is_file(), "no copy in {}", copy.display());
        let old = open(&copy, 16, &v040).await.unwrap();
        let applied: Vec<(i64,)> = sqlx::query_as("SELECT version FROM _sqlx_migrations").fetch_all(&old).await.unwrap();
        assert_eq!(applied, [(1,)]);
        assert_eq!(get_setting(&old, "upgrade-test").await.unwrap().as_deref(), Some("kept"));
        old.close().await;

        // Starting again changes nothing and copies nothing more
        let db = open(&path, 16, &newer).await.unwrap();
        db.close().await;
        let copies = std::fs::read_dir(copy.parent().unwrap()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".db")).count();
        assert_eq!(copies, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_schema_has_the_indexes_and_constraints_it_needs_and_nothing_unused() {
        let env = crate::testutil::env().await;
        let db = &env.st.db;
        // Versions in a folder space are found by their path without reading every version
        let plan: Vec<(i64, i64, i64, String)> =
            sqlx::query_as("EXPLAIN QUERY PLAN SELECT id FROM node_versions WHERE drive_id = 'd' AND fs_path = 'p'").fetch_all(db).await.unwrap();
        assert!(plan.iter().any(|p| p.3.contains("USING INDEX node_versions_fs_path (drive_id=? AND fs_path=?)")), "{plan:?}");
        // nodes_owner_recent covers what nodes_owner did
        let (owner,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'nodes_owner'").fetch_one(db).await.unwrap();
        assert_eq!(owner, 0);
        // Every session has its public id
        let columns: Vec<(i64, String, String, bool)> =
            sqlx::query_as("SELECT cid, name, type, \"notnull\" FROM pragma_table_info('sessions')").fetch_all(db).await.unwrap();
        assert!(columns.iter().any(|c| c.1 == "id" && c.3), "{columns:?}");
        // The company space's root is read from the space itself, not kept as a setting too
        assert_eq!(get_setting(db, "shared_root_id").await.unwrap(), None);
        assert!(env.st.shared_root().is_some());
    }
}
