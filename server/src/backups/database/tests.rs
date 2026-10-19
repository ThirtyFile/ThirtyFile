//! Database backups: the encrypted format, what is kept, a copy made and restored, and what administrators are told

use std::sync::Arc;

use axum::{Json, extract::State};
use serde_json::json;

use super::*;
use crate::{
    storage::{LocalStorage, Storage},
    testutil::{self, TestEnv},
};

const PASSPHRASE: &str = "correct horse battery staple";

/// A Local folder location to keep copies on
async fn add_nas(env: &TestEnv, id: &str) -> PathBuf {
    let dir = env.dir.join(id);
    sqlx::query("INSERT INTO storage_locations (id, name, kind, config, is_default, created_at) VALUES (?, ?, 'local', ?, 0, 0)")
        .bind(id)
        .bind(id.to_uppercase())
        .bind(json!({ "path": dir.to_string_lossy() }).to_string())
        .execute(&env.st.db)
        .await
        .unwrap();
    let backend: Arc<dyn Storage> = Arc::new(LocalStorage::create(dir.clone(), id).unwrap());
    env.st.storages.write().insert(id.into(), backend);
    dir
}

async fn configure(env: &TestEnv, location: &str) -> Config {
    let admin = env.admin().await;
    let config = Config { enabled: true, location: location.into(), ..Config::default() };
    let _ = save(State(env.st.clone()), Admin(admin), Json(Change { config: config.clone(), passphrase: Some(PASSPHRASE.into()) })).await.unwrap();
    config
}

/// A folder of a test's own, removed after it
struct Folder(PathBuf);
impl Folder {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("thirtyfile-dbcopy-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        Folder(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn round_trip(dir: &Path, content: &[u8]) {
    let (plain, sealed, back) = (dir.join("plain"), dir.join("sealed"), dir.join("back"));
    std::fs::write(&plain, content).unwrap();
    encrypt_file(&plain, &sealed, PASSPHRASE).unwrap();
    let stored = std::fs::read(&sealed).unwrap();
    if content.len() >= 64 {
        assert!(!stored.windows(64).any(|w| w == &content[..64]), "the content is readable in the copy");
    }
    decrypt_file(&sealed, &back, PASSPHRASE).unwrap();
    assert_eq!(std::fs::read(&back).unwrap(), content);
}

#[test]
fn a_copy_decrypts_to_what_it_was_with_the_passphrase_only() {
    let dir = Folder::new();
    let pattern: Vec<u8> = (0..(2 * CHUNK + 5)).map(|i| (i % 251) as u8).collect();
    // Empty, smaller than a chunk, exactly one chunk, several and a part
    for content in [&b""[..], b"a small database", &pattern[..CHUNK], &pattern[..]] {
        round_trip(dir.path(), content);
    }
    let (sealed, back) = (dir.path().join("sealed"), dir.path().join("back"));
    std::fs::write(dir.path().join("plain"), &pattern).unwrap();
    encrypt_file(&dir.path().join("plain"), &sealed, PASSPHRASE).unwrap();
    assert_eq!(decrypt_file(&sealed, &back, "not the passphrase").unwrap_err(), UNREADABLE);
    let stored = std::fs::read(&sealed).unwrap();
    let header = 8 + 12 + 16 + 7;
    // Cut off after a chunk: the end is missing, which the last chunk's mark shows
    let cut = dir.path().join("cut");
    std::fs::write(&cut, &stored[..header + CHUNK + TAG]).unwrap();
    assert_eq!(decrypt_file(&cut, &back, PASSPHRASE).unwrap_err(), UNREADABLE);
    // Two chunks swapped
    let mut swapped = stored[..header].to_vec();
    swapped.extend_from_slice(&stored[header + CHUNK + TAG..header + 2 * (CHUNK + TAG)]);
    swapped.extend_from_slice(&stored[header..header + CHUNK + TAG]);
    swapped.extend_from_slice(&stored[header + 2 * (CHUNK + TAG)..]);
    std::fs::write(&cut, &swapped).unwrap();
    assert_eq!(decrypt_file(&cut, &back, PASSPHRASE).unwrap_err(), UNREADABLE);
    // A byte changed, in the header or in the content
    for at in [9, header + 100] {
        let mut changed = stored.clone();
        changed[at] ^= 1;
        std::fs::write(&cut, &changed).unwrap();
        assert_eq!(decrypt_file(&cut, &back, PASSPHRASE).unwrap_err(), UNREADABLE, "byte {at}");
    }
    // Not a copy at all
    std::fs::write(&cut, b"SQLite format 3\0").unwrap();
    assert_eq!(decrypt_file(&cut, &back, PASSPHRASE).unwrap_err(), UNREADABLE);
}

#[test]
fn a_copys_name_says_when_it_was_made() {
    let t = 1_792_375_800; // 2026-10-19 02:10:00 UTC
    assert_eq!(copy_name(t), "thirtyfile-20261019-021000Z.tfdb");
    assert_eq!(copy_time(&copy_name(t)), Some(t));
    assert_eq!(copy_time("drive.db"), None);
    assert_eq!(copy_time("thirtyfile-2026-bad.tfdb"), None);
}

#[test]
fn the_newest_copies_and_the_newest_of_recent_days_weeks_and_months_are_kept() {
    let day = 86_400;
    // 2026-10-19 02:30 UTC, then two copies a day going back 120 days
    let newest = 1_792_391_400;
    let copies: Vec<(String, i64)> = (0..240).map(|i| newest - i * day / 2).map(|t| (copy_name(t), t)).collect();
    let c = Config { keep_last: 3, keep_daily: 5, keep_weekly: 3, keep_monthly: 2, ..Config::default() };
    let keep = kept(&copies, &c, &TimeZone::UTC);
    let kept_times: HashSet<i64> = copies.iter().filter(|(n, _)| keep.contains(n)).map(|(_, t)| *t).collect();
    // The newest three
    for i in 0..3 {
        assert!(kept_times.contains(&(newest - i * day / 2)), "newest {i}");
    }
    // A copy of each of the last five days (UTC), the newest of each: today's at 06:30, the others' at 18:30
    assert!(kept_times.contains(&newest));
    for d in 1..5 {
        assert!(kept_times.contains(&(newest - d * day + day / 2)), "day {d}");
    }
    // Far fewer than all, and nothing older than the months kept
    assert!(keep.len() < 15, "{}", keep.len());
    let oldest = *kept_times.iter().min().unwrap();
    assert!(newest - oldest < 62 * day, "kept a copy {} days old", (newest - oldest) / day);
    // Nothing kept but the newest when that is all that is asked
    let none = Config { keep_last: 1, keep_daily: 0, keep_weekly: 0, keep_monthly: 0, ..Config::default() };
    assert_eq!(kept(&copies, &none, &TimeZone::UTC), HashSet::from([copy_name(newest)]));
}

#[tokio::test]
async fn settings_need_a_location_a_passphrase_and_a_valid_schedule() {
    let env = testutil::env().await;
    let admin = env.admin().await;
    add_nas(&env, "nas").await;
    let change = |config: Config, passphrase: Option<&str>| {
        save(State(env.st.clone()), Admin(admin.clone()), Json(Change { config, passphrase: passphrase.map(str::to_string) }))
    };
    let on = Config { enabled: true, location: "nas".into(), ..Config::default() };
    assert!(change(on.clone(), None).await.is_err(), "no passphrase yet");
    assert!(change(on.clone(), Some("too short")).await.is_err());
    assert!(change(Config { location: "nowhere".into(), ..on.clone() }, Some(PASSPHRASE)).await.is_err());
    assert!(change(Config { schedule: json!({ "daily": "25:00" }), ..on.clone() }, Some(PASSPHRASE)).await.is_err());
    assert!(change(Config { keep_last: 0, ..on.clone() }, Some(PASSPHRASE)).await.is_err());
    let Json(info) = change(on.clone(), Some(PASSPHRASE)).await.unwrap();
    assert!(info.has_passphrase && info.status.next_run_at.is_some());
    // The passphrase is kept sealed, never as it is, and never sent back
    let stored = db::get_setting(&env.st.db, KEY).await.unwrap().unwrap();
    assert!(!stored.contains(PASSPHRASE));
    assert!(!serde_json::to_string(&info.config).unwrap().contains(PASSPHRASE));
    // Changed again without a passphrase: the one saved stays
    let _ = change(Config { keep_last: 2, ..on }, None).await.unwrap();
    assert_eq!(db::get_setting(&env.st.db, KEY).await.unwrap().unwrap(), stored);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_made_while_the_server_writes_is_consistent_and_restores_into_a_new_data_folder() {
    let env = testutil::env().await;
    let nas = add_nas(&env, "nas").await;
    let config = configure(&env, "nas").await;
    let amy = env.user("amy", true).await;
    let folder = env.folder(&amy, amy.root(), "Kept in the copy").await;
    // Writes go on while the copy is made
    let writing = {
        let db = env.st.db.clone();
        tokio::spawn(async move {
            for i in 0..200 {
                sqlx::query("INSERT INTO settings (key, value) VALUES (?, 'x')").bind(format!("busy-{i}")).execute(&db).await.unwrap();
            }
        })
    };
    let name = run(&env.st, &config).await.unwrap();
    writing.await.unwrap();
    let stored = nas.join(DIR).join(&name);
    assert!(stored.is_file());
    let s = status(&mut env.st.db.acquire().await.unwrap()).await.unwrap();
    assert_eq!(s.last_file.as_deref(), Some(name.as_str()));
    assert_eq!(s.last_size, Some(std::fs::metadata(&stored).unwrap().len()));

    // Into a new data folder: the database as it was, accounts and folders included
    let data = env.dir.join("new-data");
    restore(&stored, &data, PASSPHRASE.into()).await.unwrap();
    // Opened as the server opens it (with its SQL functions)
    let restored = db::connect(&data.join("drive.db"), 16).await.unwrap();
    let (integrity,): (String,) = sqlx::query_as("PRAGMA integrity_check").fetch_one(&restored).await.unwrap();
    assert_eq!(integrity, "ok");
    let (name_there,): (String,) = sqlx::query_as("SELECT name FROM nodes WHERE id = ?").bind(&folder).fetch_one(&restored).await.unwrap();
    assert_eq!(name_there, "Kept in the copy");
    let (users,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE username = 'amy'").fetch_one(&restored).await.unwrap();
    assert_eq!(users, 1);
    restored.close().await;
    // Not over a database that is there
    assert!(restore(&stored, &data, PASSPHRASE.into()).await.is_err());
    // Nor with the wrong passphrase
    let other = env.dir.join("other-data");
    assert_eq!(restore(&stored, &other, "not the passphrase".into()).await.unwrap_err().to_string(), UNREADABLE);
    assert!(!other.join("drive.db").exists());
}

#[tokio::test]
async fn a_copy_from_a_newer_version_is_refused() {
    let env = testutil::env().await;
    let plain = env.dir.join("newer.db");
    db::backup_to(&env.st.db, &plain).await.unwrap();
    {
        let newer = sqlx::SqlitePool::connect(&format!("sqlite://{}", plain.display())).await.unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time) VALUES (9999, 'from the future', CURRENT_TIMESTAMP, 1, x'00', 0)")
            .execute(&newer)
            .await
            .unwrap();
        newer.close().await;
    }
    let sealed = env.dir.join("newer.tfdb");
    encrypt_file(&plain, &sealed, PASSPHRASE).unwrap();
    let data = env.dir.join("data-for-newer");
    let error = restore(&sealed, &data, PASSPHRASE.into()).await.unwrap_err().to_string();
    assert!(error.contains("newer version"), "{error}");
    assert!(!data.join("drive.db").exists());
    assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0, "the decrypted copy is removed");
}

#[tokio::test]
async fn copies_no_longer_kept_are_deleted_after_a_new_one_is_stored() {
    let env = testutil::env().await;
    let nas = add_nas(&env, "nas").await;
    let mut config = configure(&env, "nas").await;
    config.keep_last = 2;
    config.keep_daily = 0;
    config.keep_weekly = 0;
    config.keep_monthly = 0;
    let dir = nas.join(DIR);
    std::fs::create_dir_all(&dir).unwrap();
    // Older copies, and a file of something else that is never touched
    for t in [1_700_000_000, 1_700_086_400, 1_700_172_800] {
        std::fs::write(dir.join(copy_name(t)), b"old").unwrap();
    }
    std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
    let name = run(&env.st, &config).await.unwrap();
    let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(
        left,
        vec!["notes.txt".to_string(), copy_name(1_700_172_800), name].into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_failing_copy_is_told_once_and_again_when_it_works() {
    let env = testutil::env().await;
    let nas = add_nas(&env, "nas").await;
    let config = configure(&env, "nas").await;
    let admin = env.admin().await;
    // The location's folder is a file: storing fails
    std::fs::create_dir_all(nas.join(".thirtyfile-backups")).unwrap();
    std::fs::write(nas.join(DIR), b"not a folder").unwrap();
    let error = run(&env.st, &config).await.unwrap_err();
    after(&env.st, &config, Some(error.message)).await.unwrap();
    after(&env.st, &config, Some("still".into())).await.unwrap();
    let notices = || async {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT data FROM notifications WHERE user_id = ? AND kind = 'backup' ORDER BY id").bind(admin.id).fetch_all(&env.st.db).await.unwrap();
        rows.into_iter().map(|(d,)| serde_json::from_str::<Value>(&d).unwrap()).collect::<Vec<_>>()
    };
    let told = notices().await;
    assert_eq!(told.len(), 1, "told once: {told:?}");
    assert_eq!((told[0]["state"].as_str(), told[0]["database"].as_bool()), (Some("failing"), Some(true)));
    std::fs::remove_file(nas.join(DIR)).unwrap();
    run(&env.st, &config).await.unwrap();
    after(&env.st, &config, None).await.unwrap();
    let told = notices().await;
    assert_eq!(told.len(), 2);
    assert_eq!(told[1]["state"].as_str(), Some("recovered"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_is_made_when_one_is_due_and_the_next_time_is_set() {
    let env = testutil::env().await;
    let nas = add_nas(&env, "nas").await;
    configure(&env, "nas").await;
    change_status(&env.st, |s| s.next_run_at = Some(now() - 1)).await.unwrap();
    tick(&env.st, now()).await.unwrap();
    // Made in the background
    for _ in 0..200 {
        if !env.st.part::<Memory>().running.load(Ordering::Acquire) && status(&mut env.st.db.acquire().await.unwrap()).await.unwrap().last_success_at.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let s = status(&mut env.st.db.acquire().await.unwrap()).await.unwrap();
    assert!(s.last_success_at.is_some() && s.error.is_none(), "{s:?}");
    assert!(nas.join(DIR).join(s.last_file.unwrap()).is_file());
    assert!(s.next_run_at.is_some_and(|n| n > now()), "the next time is on the schedule: {:?}", s.next_run_at);
}
