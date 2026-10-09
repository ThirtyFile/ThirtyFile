use super::*;
use crate::testutil::{self, write_old};
use axum::extract::{Path as UrlPath, Query, State};

#[tokio::test]
async fn fallback_scans_use_the_normal_interval_and_respect_disabled_scans() {
    let env = testutil::env().await;
    let at = now();
    assert!(!scan_due(&env.st, "unwatched", at - 899, at));
    assert!(scan_due(&env.st, "unwatched", at - 900, at));
    #[cfg(target_os = "linux")]
    {
        env.st.part::<crate::watch::Memory>().watched.lock().insert("watched".into());
        assert!(!scan_due(&env.st, "watched", at - 900, at));
        assert!(scan_due(&env.st, "watched", at - 3600, at));
    }
    env.st.system.write().scan_minutes = 0;
    assert!(!scan_due(&env.st, "unwatched", 0, at));
    assert!(!scan_due(&env.st, "watched", 0, at));
}

#[tokio::test]
async fn opening_a_folder_doesnt_wait_for_a_change_in_its_space_nor_read_it_again_right_away() {
    let env = testutil::env().await;
    let space = env.folder_space("Shared").await;
    let admin = env.admin().await;
    write_old(&space.dir.join("a.txt"), b"a");
    let list = || {
        let (st, admin, root) = (env.st.clone(), admin.clone(), space.root.clone());
        async move {
            let axum::Json(l) = crate::nodes::children(State(st), admin, UrlPath(root), Query(Default::default())).await.unwrap();
            l.into_items().into_iter().map(|n| n.name).collect::<Vec<_>>()
        }
    };
    // A long change holds the space (copying a large folder into it, say): the listing answers meanwhile, from the
    // index as it is
    let held = crate::fsops::lock_space(&env.st, &space.drive).await;
    let started = std::time::Instant::now();
    let names = tokio::time::timeout(std::time::Duration::from_secs(3), list()).await.expect("the listing answers");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(names.is_empty());
    drop(held);
    // Opened again: what was added on the server shows
    assert_eq!(list().await, ["a.txt"]);
    // Opened again right away: not read again
    write_old(&space.dir.join("b.txt"), b"b");
    assert_eq!(list().await, ["a.txt"]);
    // A while later it is (and a scan shows it anyway)
    forget_reads(&env.st);
    assert_eq!(list().await, ["a.txt", "b.txt"]);
}

/// The process's own memory (not files it maps, such as the database: Linux's RssAnon), in MB
fn anon_mb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("RssAnon:"))?;
    line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb / 1024)
}

/// The issue's estimate (a first scan of a million files holds about 1 GB), measured on Linux: the most memory the
/// process held while scanning, sampled every 10 ms.
/// `ITEMS=200000 cargo test --release -- --ignored --nocapture measure_scanning`
#[tokio::test]
#[ignore]
async fn measure_scanning_a_large_space() {
    let env = testutil::env().await;
    let space = env.folder_space("Big").await;
    let n: usize = std::env::var("ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(200_000);
    for i in 0..n {
        write_old(&space.dir.join(format!("d{:04}/sub/file-with-a-longer-name-{i:07}.txt", i / 100)), b"x");
    }
    for what in ["first scan (everything new)", "scan with nothing changed"] {
        let before = anon_mb().unwrap_or(0);
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(before));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (p, s) = (peak.clone(), stop.clone());
        let sampler = std::thread::spawn(move || {
            while !s.load(std::sync::atomic::Ordering::SeqCst) {
                p.fetch_max(anon_mb().unwrap_or(0), std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        let started = std::time::Instant::now();
        let r = scan(&env.st, &space.drive).await.unwrap();
        let took = started.elapsed();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        sampler.join().unwrap();
        println!(
            "{n} files, {what}: {:.1} s, memory {before} MB before, at most {} MB during, {} added",
            took.as_secs_f64(),
            peak.load(std::sync::atomic::Ordering::SeqCst),
            r.added
        );
    }
}

/// Opening a large folder of a folder space again and again (the web asks for its first page each time it is opened
/// or refreshed): `ITEMS=20000 cargo test --release -- --ignored --nocapture measure_opening`
#[tokio::test]
#[ignore]
async fn measure_opening_a_large_folder() {
    let env = testutil::env().await;
    let space = env.folder_space("Shared").await;
    let admin = env.admin().await;
    let n: usize = std::env::var("ITEMS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000);
    for i in 0..n {
        write_old(&space.dir.join(format!("Big/f{i:06}.txt")), b"x");
    }
    scan(&env.st, &space.drive).await.unwrap();
    let (big, _) = env.node_at(&space.drive, "Big").await.unwrap();
    let open = || {
        let (st, admin, big) = (env.st.clone(), admin.clone(), big.clone());
        async move {
            let started = std::time::Instant::now();
            let q = Query(serde_json::from_value(serde_json::json!({ "limit": 100 })).unwrap());
            let _ = crate::nodes::children(State(st), admin, UrlPath(big), q).await.unwrap();
            started.elapsed()
        }
    };
    forget_reads(&env.st);
    let first = open().await;
    let again = open().await;
    println!("{n} items: opened {} ms (reads the folder), opened again right away {} ms", first.as_millis(), again.as_millis());
}

#[tokio::test]
async fn items_moved_between_folders_keep_their_ids_whichever_folder_is_read_first() {
    let env = testutil::env().await;
    let space = env.folder_space("Shared").await;
    let (dir, drive) = (space.dir.clone(), space.drive.clone());
    write_old(&dir.join("A/x.txt"), b"x");
    write_old(&dir.join("B/keep.txt"), b"k");
    write_old(&dir.join("Z/deep/y.txt"), b"y");
    write_old(&dir.join("C/Inner/z.txt"), b"z");
    write_old(&dir.join("D/Old/w.txt"), b"w");
    scan(&env.st, &drive).await.unwrap();
    let id = |rel: &'static str| {
        let env = &env;
        let drive = drive.clone();
        async move { env.node_at(&drive, rel).await.map(|(id, _)| id) }
    };
    let (x, deep, y, inner, z) = (id("A/x.txt").await, id("Z/deep").await, id("Z/deep/y.txt").await, id("C/Inner").await, id("C/Inner/z.txt").await);
    // Into a folder read later, into one read earlier, a folder with what is in it into a folder further down, and
    // a folder removed with what is in it
    std::fs::rename(dir.join("A/x.txt"), dir.join("Z/x.txt")).unwrap();
    std::fs::rename(dir.join("Z/deep"), dir.join("B/deep")).unwrap();
    std::fs::rename(dir.join("C/Inner"), dir.join("A/Inner")).unwrap();
    std::fs::remove_dir_all(dir.join("D/Old")).unwrap();
    let r = scan(&env.st, &drive).await.unwrap();
    assert!(id("A/x.txt").await.is_none() && id("Z/deep").await.is_none() && id("C/Inner/z.txt").await.is_none() && id("D/Old/w.txt").await.is_none(), "{r:?}");
    let now = (id("Z/x.txt").await, id("B/deep").await, id("B/deep/y.txt").await, id("A/Inner").await, id("A/Inner/z.txt").await);
    assert!(now.0.is_some() && now.1.is_some() && now.2.is_some() && now.3.is_some() && now.4.is_some(), "{r:?}");
    if cfg!(unix) {
        // Recognised by their inodes: the same nodes, with their shares and permissions
        assert_eq!(now, (x, deep, y, inner, z));
        assert_eq!((r.moved, r.added, r.removed), (3, 0, 2), "{r:?}");
    }
    // Nothing left to do
    let again = scan(&env.st, &drive).await.unwrap();
    assert_eq!((again.added, again.moved, again.removed, again.changed), (0, 0, 0, 0), "{again:?}");
}

#[tokio::test]
async fn a_folder_space_follows_changes_made_on_the_server() {
    let env = testutil::env().await;
    let space = env.folder_space("Shared").await;
    let admin = env.admin().await;
    let (dir, drive, root) = (space.dir.clone(), space.drive.clone(), space.root.clone());
    write_old(&dir.join("a.txt"), b"one");
    write_old(&dir.join("Sub").join("b.txt"), b"two");
    write_old(&dir.join("~$a.docx"), b"office lock file");
    std::fs::write(dir.join("fresh.txt"), b"still being written").unwrap();

    let r = scan(&env.st, &drive).await.unwrap();
    assert_eq!((r.added, r.removed), (3, 0), "{r:?}");
    assert!(env.node_at(&drive, "a.txt").await.is_some() && env.node_at(&drive, "Sub/b.txt").await.is_some());
    assert!(env.node_at(&drive, "~$a.docx").await.is_none(), "temporary files are ignored");
    assert!(env.node_at(&drive, "fresh.txt").await.is_none(), "a file still being written waits");
    let (used,): (i64,) = sqlx::query_as("SELECT used_bytes FROM drives WHERE id = ?").bind(&drive).fetch_one(&env.st.db).await.unwrap();
    assert_eq!(used, 6);

    // Changed, moved and removed on the server
    let (b_id, _) = env.node_at(&drive, "Sub/b.txt").await.unwrap();
    write_old(&dir.join("a.txt"), b"one, longer");
    std::fs::rename(dir.join("Sub"), dir.join("Moved")).unwrap();
    write_old(&dir.join("gone.txt"), b"x");
    scan(&env.st, &drive).await.unwrap();
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    let r = scan(&env.st, &drive).await.unwrap();
    assert_eq!(env.node_at(&drive, "a.txt").await.unwrap().1, 11);
    let (moved_id, _) = env.node_at(&drive, "Moved/b.txt").await.unwrap();
    if cfg!(unix) {
        // Recognised by its inode: the same node, so shares and permissions stay
        assert_eq!(moved_id, b_id);
    }
    assert!(env.node_at(&drive, "Sub/b.txt").await.is_none() && env.node_at(&drive, "gone.txt").await.is_none(), "{r:?}");

    // Browsing works; a read-only space can't be changed from the web
    let a = env.node_at(&drive, "a.txt").await.unwrap().0;
    let res = crate::files::content(
        State(env.st.clone()),
        admin.clone(),
        UrlPath(a.clone()),
        Query(serde_json::from_value(serde_json::json!({})).unwrap()),
        axum::http::HeaderMap::new(),
    )
    .await
    .unwrap();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"one, longer");
    assert!(tree::node_for(&mut env.st.db.acquire().await.unwrap(), &admin, &a, tree::Need::Write).await.is_ok());
    let req = serde_json::from_value(serde_json::json!({ "read_only": true })).unwrap();
    let _ = crate::drives::update(State(env.st.clone()), admin.clone(), UrlPath(drive.clone()), axum::Json(req)).await.unwrap();
    let err = tree::node_for(&mut env.st.db.acquire().await.unwrap(), &admin, &a, tree::Need::Write).await.unwrap_err();
    assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);

    // Opening a folder shows what was added there since
    write_old(&dir.join("new.txt"), b"new");
    let root_node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &root).await.unwrap().unwrap();
    sync_folder(&env.st, &root_node).await;
    assert!(env.node_at(&drive, "new.txt").await.is_some());
}

#[tokio::test]
async fn items_in_the_trash_stay_there_when_their_folder_is_removed_on_the_server() {
    let env = testutil::env().await;
    let space = env.folder_space("Shared").await;
    let admin = env.admin().await;
    let (dir, drive, root) = (space.dir.clone(), space.drive.clone(), space.root.clone());
    testutil::write_old(&dir.join("Plans").join("report.txt"), b"report");
    testutil::write_old(&dir.join("Plans").join("Old").join("draft.txt"), b"draft");
    testutil::write_old(&dir.join("Plans").join("keep.txt"), b"keep");
    scan(&env.st, &drive).await.unwrap();
    let (report, _) = env.node_at(&drive, "Plans/report.txt").await.unwrap();
    let (draft, _) = env.node_at(&drive, "Plans/Old/draft.txt").await.unwrap();
    for id in [&report, &draft] {
        let req = serde_json::from_value(serde_json::json!({ "ids": [id] })).unwrap();
        let _ = crate::nodes::trash(State(env.st.clone()), admin.clone(), axum::Json(req)).await.unwrap();
    }

    // The folder is deleted on the server (or renamed, where a rename looks like a new folder)
    std::fs::remove_dir_all(dir.join("Plans")).unwrap();
    scan(&env.st, &drive).await.unwrap();
    assert!(env.node_at(&drive, "Plans/keep.txt").await.is_none(), "what was in the folder is gone");
    for id in [&report, &draft] {
        let (parent, trashed): (String, Option<i64>) =
            sqlx::query_as("SELECT parent_id, trashed_at FROM nodes WHERE id = ?").bind(id).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(parent, root, "kept in the trash, as an item of the space's top folder");
        assert!(trashed.is_some());
    }
    crate::fsops::clean_trash(&env.st, &drive, &dir).await.unwrap();
    assert_eq!(std::fs::read_dir(dir.join(crate::fsops::TRASH_DIR)).unwrap().count(), 2, "their copies in the trash folder stay");

    // Restored, they come back to the space's top folder
    let req = serde_json::from_value(serde_json::json!({ "ids": [&report, &draft] })).unwrap();
    let _ = crate::nodes::restore(State(env.st.clone()), admin.clone(), axum::Json(req)).await.unwrap();
    assert_eq!(std::fs::read(dir.join("report.txt")).unwrap(), b"report");
    assert_eq!(std::fs::read(dir.join("draft.txt")).unwrap(), b"draft");
    assert_eq!(env.node_at(&drive, "report.txt").await.unwrap().0, report);
}

#[test]
fn file_times_far_from_now_dont_wrap() {
    use std::time::{Duration, UNIX_EPOCH};
    assert_eq!(nanos_since_1970(UNIX_EPOCH + Duration::from_secs(1)), 1_000_000_000);
    assert_eq!(nanos_since_1970(UNIX_EPOCH - Duration::from_secs(10)), 0);
    // Around 2270: more nanoseconds than an i64 holds
    assert_eq!(nanos_since_1970(UNIX_EPOCH + Duration::from_secs(300 * 365 * 86_400)), i64::MAX);
}

#[tokio::test]
async fn items_found_on_the_disk_have_no_uploader_and_arent_anyones_recent_files() {
    let env = testutil::env().await;
    let space = env.folder_space("Shared").await;
    let admin = env.admin().await;
    let uploaded = env.file(&admin, &space.root, "uploaded.txt").await;
    write_old(&space.dir.join("found.txt"), b"found");
    write_old(&space.dir.join("Found folder").join("inside.txt"), b"inside");
    scan(&env.st, &space.drive).await.unwrap();
    let (found, _) = env.node_at(&space.drive, "found.txt").await.unwrap();

    let axum::Json(recent) = crate::nodes::recent(State(env.st.clone()), admin.clone()).await.unwrap();
    let recent = serde_json::to_value(recent).unwrap();
    let names: Vec<&str> = recent.as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["uploaded.txt"]);

    let owner = |id: String| {
        let st = env.st.clone();
        async move { tree::get_node(&mut st.db.acquire().await.unwrap(), &id).await.unwrap().unwrap().owner_name }
    };
    assert_eq!(owner(uploaded).await, "admin");
    assert_eq!(owner(found.clone()).await, "");
    assert_eq!(owner(env.node_at(&space.drive, "Found folder").await.unwrap().0).await, "");
    // Its earlier content, once someone saves over it, has no author either
    let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &found).await.unwrap().unwrap();
    assert_eq!(crate::versions::content_author(&mut env.st.db.acquire().await.unwrap(), &node).await.unwrap().1, "");
    // Recent goes through the index of uploads, not every file of the space
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT id FROM nodes WHERE owner_id = 1 AND kind = 'file' AND found = 0 AND trashed_at IS NULL ORDER BY updated_at DESC LIMIT 5",
    )
    .fetch_all(&env.st.db)
    .await
    .unwrap();
    assert!(plan.iter().any(|p| p.3.contains("nodes_uploads_recent")), "{plan:?}");
}

#[tokio::test]
async fn a_folder_space_needs_a_folder_outside_thirtyfiles_own() {
    let env = testutil::env().await;
    assert!(check_source(&env.st, "relative/path").is_err());
    assert!(check_source(&env.st, &env.dir.join("missing").to_string_lossy()).is_err());
    assert!(check_source(&env.st, &env.dir.to_string_lossy()).is_err(), "the data folder itself");
    assert!(check_source(&env.st, &env.dir.join("blobs").to_string_lossy()).is_err(), "the storage folder");
}

#[tokio::test]
async fn an_item_is_taken_for_a_moved_one_only_when_it_is_provably_the_same() {
    let env = testutil::env().await;
    let space = env.folder_space("Shared").await;
    let drive = folder_drive(&env.st, &space.drive).await.unwrap();
    // Items with the same device and inode number: what the index had, and what the folder has now
    let old = |kind: &str, rel: &str, size: i64, mtime: i64, birth: Option<i64>| Indexed {
        id: format!("id-{rel}"),
        parent_id: Some(drive.root_id.clone()),
        kind: kind.into(),
        fs_path: Some(rel.into()),
        fs_dev: Some(1),
        fs_ino: Some(7),
        fs_size: Some(size),
        fs_mtime_ns: Some(mtime),
        fs_birth_ns: birth,
    };
    let new = |kind: &str, rel: &str, size: i64, mtime: i64, birth: Option<i64>| Entry {
        rel: rel.into(),
        parent_rel: String::new(),
        name: rel.into(),
        is_dir: kind == "folder",
        dev: 1,
        ino: 7,
        size,
        mtime_ns: mtime,
        birth_ns: birth,
        settling: false,
    };
    // (moved, added, removed)
    let seen = |was: Indexed, now: Entry| {
        let mut report = ScanReport::default();
        let ops = plan(&drive, &[was], &[now], true, &mut report);
        assert_eq!(ops.iter().filter(|op| matches!(op, Op::Move { .. })).count(), report.moved);
        (report.moved, report.added, report.removed)
    };
    // Deleted, and a new file got its inode number (ext4 and XFS give them out again): a new file
    assert_eq!(seen(old("file", "a.xlsx", 10, 100, None), new("file", "notes.txt", 3, 200, None)), (0, 1, 1));
    // Renamed: the same size and date
    assert_eq!(seen(old("file", "a.xlsx", 10, 100, None), new("file", "b.xlsx", 10, 100, None)), (1, 0, 0));
    // Creation times, where known, tell: the same file renamed and edited, or another one
    assert_eq!(seen(old("file", "a.xlsx", 10, 100, Some(5)), new("file", "b.xlsx", 12, 300, Some(5))), (1, 0, 0));
    assert_eq!(seen(old("file", "a.xlsx", 10, 100, Some(5)), new("file", "b.xlsx", 10, 100, Some(6))), (0, 1, 1));
    // Folders only by their creation time
    assert_eq!(seen(old("folder", "Old", 0, 100, Some(5)), new("folder", "New", 0, 900, Some(5))), (1, 0, 0));
    assert_eq!(seen(old("folder", "Old", 0, 100, Some(5)), new("folder", "New", 0, 100, Some(6))), (0, 1, 1));
    assert_eq!(seen(old("folder", "Old", 0, 100, None), new("folder", "New", 0, 100, None)), (0, 1, 1));

    // A folder renamed on the server keeps its id where the file system tells when it was created
    std::fs::create_dir(space.dir.join("Sub")).unwrap();
    write_old(&space.dir.join("Sub/a.txt"), b"a");
    scan(&env.st, &space.drive).await.unwrap();
    let (sub, _) = env.node_at(&space.drive, "Sub").await.unwrap();
    let (a, _) = env.node_at(&space.drive, "Sub/a.txt").await.unwrap();
    std::fs::rename(space.dir.join("Sub"), space.dir.join("Moved")).unwrap();
    scan(&env.st, &space.drive).await.unwrap();
    if cfg!(unix) && std::fs::metadata(space.dir.join("Moved")).and_then(|m| m.created()).is_ok() {
        assert_eq!(env.node_at(&space.drive, "Moved").await.unwrap().0, sub);
    }
    if cfg!(unix) {
        assert_eq!(env.node_at(&space.drive, "Moved/a.txt").await.unwrap().0, a);
    }
}

#[test]
fn only_a_small_file_is_read_as_the_spaces_marker() {
    let dir = std::env::temp_dir().join(format!("thirtyfile-marker-{}", new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let root = crate::beneath::Pinned::root(&dir).unwrap();
    std::fs::write(dir.join(MARKER), "space-1\n").unwrap();
    assert_eq!(space_marker(&root).unwrap().as_deref(), Some("space-1"));
    // A large file isn't read whole
    std::fs::write(dir.join(MARKER), vec![b'x'; 8 << 20]).unwrap();
    assert!(space_marker(&root).unwrap().is_some_and(|m| m.len() <= 4096));
    // Something else than a file is no marker, and isn't waited on
    std::fs::remove_file(dir.join(MARKER)).unwrap();
    #[cfg(target_os = "linux")]
    {
        let fifo = std::ffi::CString::new(dir.join(MARKER).to_string_lossy().as_bytes()).unwrap();
        // SAFETY: a valid path
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let r = root.clone();
        std::thread::spawn(move || tx.send(space_marker(&r).ok().flatten()).unwrap());
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(None));
        std::fs::remove_file(dir.join(MARKER)).unwrap();
        std::os::unix::fs::symlink(dir.with_extension("elsewhere"), dir.join(MARKER)).unwrap();
        std::fs::write(dir.with_extension("elsewhere"), "space-1").unwrap();
        assert_eq!(space_marker(&root).unwrap(), None);
        let _ = std::fs::remove_file(dir.with_extension("elsewhere"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn temporary_files_are_ignored() {
    for name in ["~$Report.docx", ".~lock.Budget.xlsx#", "Thumbs.db", ".DS_Store", "desktop.ini", "movie.mp4.part", ".thirtyfile-trash"] {
        assert!(ignored(name), "{name}");
    }
    for name in ["Report.docx", "part.txt", "thumbs.png"] {
        assert!(!ignored(name), "{name}");
    }
}
