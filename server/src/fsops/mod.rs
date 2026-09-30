//! Changing folder spaces from the web.
//!
//! The folder on disk is the source of truth, so every change is made there first and the index follows only once
//! that worked: a failure leaves both as they were, and anything the index misses is picked up by the next scan.
//! A web change to a folder space and a scan of it take turns (the space's scan lock, always taken before the write
//! lock), so a scan never sees half of a change. New content is written under a name scans ignore (`.thirtyfile-…`)
//! and renamed into place once it is complete, and renames never replace an item that is already there.
//!
//! A change of several items can fail after some of them were renamed on disk: the transaction then rolls back, and
//! the renames are put back too (`SpaceLocks` keeps them until the change is committed), so the folder and the index
//! stay as they were.

mod across;
mod build;
mod disk;
mod index;
mod locks;
mod organize;
mod save;

pub use across::*;
pub use build::*;
pub use disk::*;
pub use index::*;
pub use locks::*;
pub use organize::*;
pub use save::*;

use std::{
    collections::{HashMap, HashSet},
    io,
    path::Path,
    time::Duration,
};

use axum::http::StatusCode;
use sqlx::SqliteConnection;
use tokio::sync::OwnedMutexGuard;

use crate::{
    auth::User,
    beneath::Pinned,
    error::{AppError, AppResult},
    files::Source,
    jobs::Tracker,
    logs,
    state::AppState,
    tree::{self, BlobRef, Need, Node, StagedBlob, changes},
    util::{guess_mime, new_id, now, numbered_name, split_name},
    versions,
};

/// A folder space's trash, in its folder: `.thirtyfile-trash/<trash id>/<name>` (never indexed)
pub const TRASH_DIR: &str = ".thirtyfile-trash";

/// What the index keeps of an item on disk
#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub is_dir: bool,
    pub dev: i64,
    pub ino: i64,
    pub size: i64,
    pub mtime_ns: i64,
    /// When it was created, where the file system tells (`folders::birth_ns`)
    pub birth_ns: Option<i64>,
}

pub fn stat(path: &Path) -> io::Result<Stat> {
    let meta = std::fs::symlink_metadata(path)?;
    let (dev, ino) = crate::folders::identity(&meta);
    Ok(Stat {
        is_dir: meta.is_dir(),
        dev,
        ino,
        size: if meta.is_dir() { 0 } else { meta.len() as i64 },
        mtime_ns: crate::folders::mtime_ns(&meta),
        birth_ns: crate::folders::birth_ns(&meta),
    })
}

pub fn child_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") }
}

fn rel_of(n: &Node) -> &str {
    n.fs_path.as_deref().unwrap_or_default()
}

/// The item on disk, reached without following a symbolic link on the way (`beneath`)
fn abs(n: &Node) -> AppResult<Pinned> {
    let rel = n.fs_path.as_deref().ok_or_else(|| gone_or_disk_error(io::ErrorKind::NotFound.into()))?;
    space_root(n)?.join(rel).map_err(gone_or_disk_error)
}

/// A path that no longer leads to the item: a folder on the way is missing, or was replaced by a link
fn gone_or_disk_error(e: io::Error) -> AppError {
    match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory | io::ErrorKind::InvalidInput => {
            AppError::not_found("The item is no longer in the folder on the server")
        }
        #[cfg(unix)]
        _ if e.raw_os_error() == Some(libc::ELOOP) => AppError::not_found("The item is no longer in the folder on the server"),
        _ => disk_error(e),
    }
}

/// A space's folder, to change something in it. When it isn't there (its disk or share isn't mounted, say), nothing is
/// changed: it is never made again, which would put the space's files on the disk below the mount point. It must also
/// hold this space's marker (`folders::MARKER`): another disk mounted at the same place, with a folder of the same
/// name, isn't written to.
fn space_root(n: &Node) -> AppResult<Pinned> {
    space_root_at(n.drive(), n.fs_root.as_deref())
}

/// `space_root` of the space `drive` whose folder is `root`
fn space_root_at(drive: &str, root: Option<&str>) -> AppResult<Pinned> {
    #[cfg(test)]
    testing::disk_call(drive);
    let root = root.ok_or_else(|| gone_or_disk_error(io::ErrorKind::NotFound.into()))?;
    let not_mounted = || AppError::new(StatusCode::SERVICE_UNAVAILABLE, crate::storage::NOT_MOUNTED);
    let pinned = Pinned::root(Path::new(root)).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => not_mounted(),
        _ => gone_or_disk_error(e),
    })?;
    match crate::folders::space_marker(&pinned).map_err(disk_error)? {
        Some(id) if id == drive => Ok(pinned),
        _ => Err(not_mounted()),
    }
}

/// Refuses, for an item of a folder space, a name scans skip (`folders::ignored`): ThirtyFile's own files there
/// (`.thirtyfile-…`) and temporary files of other programs. An item with such a name would never show, wouldn't count
/// against the space's size limit, and could be taken for something ThirtyFile left behind and removed.
pub fn check_name(name: &str) -> AppResult<()> {
    if crate::folders::ignored(name) {
        return Err(AppError::bad_request(format!(
            "\"{name}\" can't be used in a folder on the server: names like this are kept for ThirtyFile's own files and for temporary files of other programs"
        )));
    }
    Ok(())
}

/// A disk error as people see it
pub fn disk_error(e: io::Error) -> AppError {
    match e.kind() {
        io::ErrorKind::AlreadyExists => AppError::conflict("An item with the same name already exists"),
        io::ErrorKind::NotFound => AppError::not_found("The item is no longer in the folder on the server"),
        io::ErrorKind::PermissionDenied => AppError::forbidden("ThirtyFile isn't allowed to change this in the folder on the server"),
        io::ErrorKind::StorageFull => AppError::new(StatusCode::INSUFFICIENT_STORAGE, "The disk of the folder on the server is full"),
        _ => AppError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Couldn't change the folder on the server: {e}")),
    }
}

fn incomplete() -> AppError {
    AppError::new(StatusCode::INTERNAL_SERVER_ERROR, "A file wasn't copied completely")
}

// ───────────── Disks that don't answer ─────────────

/// How long a disk step of a change may take while the change holds the write lock: a disk that doesn't answer by
/// then (a network share whose server went away) fails the change, rather than holding up every other change
const DISK_WAIT: Duration = Duration::from_secs(20);
/// How long the first look at a space's folder may take, before the write lock is taken: long enough for a disk to
/// spin up
const DISK_WAKE: Duration = Duration::from_secs(60);

#[cfg(test)]
thread_local! {
    /// Tests: `DISK_WAIT` and `DISK_WAKE` (see `testing::short_waits`)
    static TEST_WAIT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

fn disk_wait() -> Duration {
    #[cfg(test)]
    if let Some(w) = TEST_WAIT.with(|w| w.get()) {
        return w;
    }
    DISK_WAIT
}

fn disk_wake() -> Duration {
    #[cfg(test)]
    if let Some(w) = TEST_WAIT.with(|w| w.get()) {
        return w;
    }
    DISK_WAKE
}

/// Folder spaces whose disk left a step unanswered: until it answers, their changes fail at once, so a disk that hangs
/// ties up one thread, not one more per request
static STUCK: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn stuck(drive: &str) -> bool {
    STUCK.lock().unwrap().iter().any(|d| d == drive)
}

fn not_answering() -> AppError {
    AppError::new(StatusCode::SERVICE_UNAVAILABLE, "The disk of the folder on the server doesn't answer. Try again later.")
}

/// Runs a disk step of the folder space `drive` on a blocking thread, giving up after `wait`. The async worker (one,
/// on a server with one processor) never waits for a disk.
pub async fn on_disk<T, F>(drive: &str, wait: Duration, step: F) -> AppResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> AppResult<T> + Send + 'static,
{
    if stuck(drive) {
        return Err(not_answering());
    }
    let mut task = tokio::task::spawn_blocking(step);
    match tokio::time::timeout(wait, &mut task).await {
        Ok(done) => done.map_err(AppError::internal)?,
        Err(_) => {
            tracing::warn!("The disk of the folder space {drive} didn't answer within {} s", wait.as_secs());
            STUCK.lock().unwrap().push(drive.to_string());
            // It is let go once the step is done at last (whatever it did then shows at the next scan)
            let drive = drive.to_string();
            tokio::spawn(async move {
                let _ = task.await;
                STUCK.lock().unwrap().retain(|d| *d != drive);
                tracing::info!("The disk of the folder space {drive} answers again");
            });
            Err(not_answering())
        }
    }
}

/// Before a change to the folder space `drive` takes the write lock: its folder is there and answers (a disk spinning
/// up does so now, while nothing else waits for it)
pub async fn ready(st: &AppState, drive: &str) -> AppResult<()> {
    let root: Option<(Option<String>,)> =
        sqlx::query_as("SELECT source_path FROM drives WHERE id = ? AND mode = 'folder'").bind(drive).fetch_optional(&st.db).await?;
    let Some((Some(root),)) = root else { return Ok(()) };
    let id = drive.to_string();
    on_disk(drive, disk_wake(), move || space_root_at(&id, Some(&root)).map(|_| ())).await
}

#[cfg(test)]
thread_local! {
    /// Tests: treat every folder as being on another disk, so moves copy
    static OTHER_DISK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn other_disk() -> bool {
    OTHER_DISK.with(|d| d.get())
}

#[cfg(test)]
type Hook = Box<dyn Fn() -> futures_util::future::BoxFuture<'static, ()>>;

#[cfg(test)]
thread_local! {
    /// Tests: runs once the content of items moved to another space is in place, before the index follows
    static AFTER_PLACE: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
async fn after_place() {
    let run = AFTER_PLACE.with(|h| h.borrow().as_ref().map(|f| f()));
    if let Some(run) = run {
        run.await;
    }
}

/// Tests: a disk that doesn't answer
#[cfg(test)]
pub(crate) mod testing {
    use std::{collections::HashMap, sync::Mutex, time::Duration};

    static HUNG: Mutex<Option<HashMap<String, Duration>>> = Mutex::new(None);

    /// Every call to the disk of the folder space `drive` takes `delay` until the guard is dropped
    pub fn hang(drive: &str, delay: Duration) -> impl Drop {
        struct Answer(String);
        impl Drop for Answer {
            fn drop(&mut self) {
                HUNG.lock().unwrap().get_or_insert_default().remove(&self.0);
            }
        }
        HUNG.lock().unwrap().get_or_insert_default().insert(drive.to_string(), delay);
        Answer(drive.to_string())
    }

    static NO_LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// The disk of the folder space `drive` has no hard links until the guard is dropped
    pub fn no_hard_links(drive: &str) -> impl Drop {
        struct Links(String);
        impl Drop for Links {
            fn drop(&mut self) {
                NO_LINKS.lock().unwrap().retain(|d| *d != self.0);
            }
        }
        NO_LINKS.lock().unwrap().push(drive.to_string());
        Links(drive.to_string())
    }

    pub fn hard_links(drive: &str) -> bool {
        !NO_LINKS.lock().unwrap().iter().any(|d| d == drive)
    }

    /// Disk steps are given up after a second (instead of 20 or 60) until the guard is dropped
    pub fn short_waits() -> impl Drop {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                super::TEST_WAIT.with(|w| w.set(None));
            }
        }
        super::TEST_WAIT.with(|w| w.set(Some(Duration::from_secs(1))));
        Reset
    }

    /// A call to the disk of `drive`
    pub fn disk_call(drive: &str) {
        let delay = HUNG.lock().unwrap().as_ref().and_then(|h| h.get(drive).copied());
        if let Some(d) = delay {
            std::thread::sleep(d);
        }
    }
}

/// Tests: clears the hook of `hook_after_place` when dropped
#[cfg(test)]
pub(crate) struct HookGuard;

#[cfg(test)]
impl Drop for HookGuard {
    fn drop(&mut self) {
        AFTER_PLACE.with(|h| *h.borrow_mut() = None);
    }
}

/// Tests: runs `hook` each time the content of a move or copy to another space is in place, before the index follows
#[cfg(test)]
pub(crate) fn hook_after_place(hook: impl Fn() -> futures_util::future::BoxFuture<'static, ()> + 'static) -> HookGuard {
    AFTER_PLACE.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    HookGuard
}

/// Tests: a hook for `hook_after_place` that waits until `go` is notified
#[cfg(test)]
pub(crate) fn wait_for(go: &std::sync::Arc<tokio::sync::Notify>) -> impl Fn() -> futures_util::future::BoxFuture<'static, ()> + 'static {
    let go = go.clone();
    move || {
        let go = go.clone();
        Box::pin(async move { go.notified().await })
    }
}

#[cfg(not(test))]
fn other_disk() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, write_old};
    use axum::{
        Json,
        body::Bytes,
        extract::{Path as UrlPath, Query, State},
        http::HeaderMap,
    };
    use serde_json::json;

    fn req<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Json<T> {
        Json(serde_json::from_value(v).unwrap())
    }

    async fn node(env: &testutil::TestEnv, id: &str) -> Node {
        tree::get_node(&mut env.st.db.acquire().await.unwrap(), id).await.unwrap().unwrap()
    }

    /// Removing from disk happens in the background
    async fn eventually_gone(path: &Path) -> bool {
        for _ in 0..100 {
            if !path.exists() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        false
    }

    async fn content(env: &testutil::TestEnv, user: &User, id: &str) -> Vec<u8> {
        let q = Query(serde_json::from_value(json!({})).unwrap());
        let res = crate::files::content(State(env.st.clone()), user.clone(), UrlPath(id.to_string()), q, HeaderMap::new()).await.unwrap();
        axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()
    }

    #[tokio::test]
    async fn nothing_is_written_into_a_folder_that_isnt_the_spaces_own() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Scans").await;
        let (dir, drive) = (&space.dir, &space.drive);
        let marker = dir.join(crate::folders::MARKER);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), *drive);
        let away = dir.with_extension("away");
        std::fs::rename(dir, &away).unwrap();
        let refused = |what: &'static str| {
            let (env, admin, root) = (&env, admin.clone(), space.root.clone());
            async move {
                let err = env.try_upload(&admin, &root, "a.txt", b"a").await.unwrap_err();
                assert!(err.message.starts_with(crate::storage::NOT_MOUNTED), "{what}: {}", err.message);
                let req = req(json!({ "parent_id": root, "name": "Docs" }));
                let err = crate::nodes::create_folder(State(env.st.clone()), admin, req).await.unwrap_err();
                assert_eq!((err.status, err.message.as_str()), (StatusCode::SERVICE_UNAVAILABLE, crate::storage::NOT_MOUNTED), "{what}");
            }
        };
        // Missing: not made again
        refused("missing").await;
        assert!(!dir.exists());
        // Another disk mounted there, with a folder of the same name: without the marker, or with another space's
        for other in [None, Some("another space")] {
            std::fs::create_dir(dir).unwrap();
            if let Some(id) = other {
                std::fs::write(&marker, id).unwrap();
            }
            refused("another disk").await;
            let report = crate::folders::scan(&env.st, drive).await.unwrap();
            let names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
            if other.is_some() {
                assert!(report.error.is_some(), "a scan leaves another space's folder alone");
                assert_eq!(names, [crate::folders::MARKER]);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
        // Its own folder back: changes are made again
        std::fs::rename(&away, dir).unwrap();
        env.upload(&admin, &space.root, "a.txt", b"a").await;
        assert!(dir.join("a.txt").is_file());
    }

    #[tokio::test]
    async fn changes_from_the_web_are_made_in_the_folder() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let (dir, drive) = (&space.dir, &space.drive);
        let st = || State(env.st.clone());

        let Json(docs) = crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": space.root, "name": "Docs" }))).await.unwrap();
        assert!(dir.join("Docs").is_dir());
        write_old(&dir.join("a.txt"), b"one");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (a, _) = env.node_at(drive, "a.txt").await.unwrap();

        // Renamed on disk, and the index follows
        let _ = crate::nodes::rename(st(), admin.clone(), UrlPath(a.clone()), req(json!({ "name": "b.txt" }))).await.unwrap();
        assert!(dir.join("b.txt").is_file() && !dir.join("a.txt").exists());
        assert_eq!(env.node_at(drive, "b.txt").await.unwrap().0, a);

        // A rename onto an item the index doesn't have yet fails, and changes nothing
        write_old(&dir.join("taken.txt"), b"not indexed yet");
        let err = crate::nodes::rename(st(), admin.clone(), UrlPath(a.clone()), req(json!({ "name": "taken.txt" }))).await.unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(std::fs::read(dir.join("taken.txt")).unwrap(), b"not indexed yet");
        assert_eq!(node(&env, &a).await.name, "b.txt");
        assert!(dir.join("b.txt").is_file());

        // Moved into a folder
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [a], "dest_id": docs.id }))).await.unwrap();
        assert!(dir.join("Docs/b.txt").is_file());
        assert_eq!(env.node_at(drive, "Docs/b.txt").await.unwrap().0, a);
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert_eq!((r.changed, r.moved, r.removed), (0, 0, 0), "the index already knows: {r:?}");

        // The trash is a folder in the space; restoring puts the item back
        let _ = crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [docs.id] }))).await.unwrap();
        assert!(!dir.join("Docs").exists());
        let in_trash = node(&env, &a).await.fs_file().unwrap();
        assert!(in_trash.starts_with(dir.join(TRASH_DIR)) && in_trash.is_file(), "{}", in_trash.display());
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert_eq!((r.added, r.removed), (0, 0), "the trash isn't indexed: {r:?}");
        let _ = crate::nodes::restore(st(), admin.clone(), req(json!({ "ids": [docs.id] }))).await.unwrap();
        assert!(dir.join("Docs/b.txt").is_file());
        assert_eq!(env.node_at(drive, "Docs/b.txt").await.unwrap().0, a);

        // Deleted for good: gone from disk too
        let _ = crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        let folder = trash_folder(&node(&env, &a).await).unwrap();
        let folder = folder.root.join(&folder.rel);
        assert!(folder.is_dir());
        let _ = crate::nodes::delete_forever(st(), admin.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        assert!(eventually_gone(&folder).await);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_folder_swapped_for_a_link_leads_nowhere() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let (dir, drive) = (&space.dir, &space.drive);
        let st = || State(env.st.clone());
        write_old(&dir.join("Sub/drive.db"), b"indexed");
        write_old(&dir.join("a.txt"), b"a");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (file, _) = env.node_at(drive, "Sub/drive.db").await.unwrap();
        let (sub, _) = env.node_at(drive, "Sub").await.unwrap();
        let (a, _) = env.node_at(drive, "a.txt").await.unwrap();

        // Someone with access to the folder (over SMB, say) replaces the indexed folder with a link elsewhere
        let outside = dir.with_extension("outside");
        write_old(&outside.join("drive.db"), b"SECRET");
        std::fs::rename(dir.join("Sub"), dir.with_extension("was-sub")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("Sub")).unwrap();

        let q = Query(serde_json::from_value(json!({})).unwrap());
        let read = crate::files::content(st(), admin.clone(), UrlPath(file.clone()), q, HeaderMap::new()).await;
        assert!(read.is_err(), "read through the link");
        assert!(crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": sub, "name": "New" }))).await.is_err());
        assert!(crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [a], "dest_id": sub }))).await.is_err());
        assert!(crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [file] }))).await.is_err());
        let mut left: Vec<String> = std::fs::read_dir(&outside).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["drive.db"], "nothing was written there");
        assert_eq!(std::fs::read(outside.join("drive.db")).unwrap(), b"SECRET");
        assert!(dir.join("a.txt").is_file());
        // A scan doesn't index what the link leads to
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert!(r.skipped.iter().any(|s| s.contains("symbolic link")), "{r:?}");
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(dir.with_extension("was-sub"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_space_folder_swapped_for_a_link_leads_nowhere() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let (dir, drive) = (&space.dir, &space.drive);
        write_old(&dir.join("drive.db"), b"indexed");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (file, _) = env.node_at(drive, "drive.db").await.unwrap();

        // Someone who can write where the space's folder is (over SMB, say) replaces it with a link to another folder
        let (outside, was) = (dir.with_extension("outside"), dir.with_extension("was"));
        write_old(&outside.join("drive.db"), b"SECRET");
        write_old(&outside.join("secret.key"), b"KEY");
        std::fs::rename(dir, &was).unwrap();
        std::os::unix::fs::symlink(&outside, dir).unwrap();
        let q = Query(serde_json::from_value(json!({})).unwrap());
        let read = crate::files::content(State(env.st.clone()), admin.clone(), UrlPath(file.clone()), q, HeaderMap::new()).await;
        assert!(read.is_err(), "read through the link");
        // Neither a scan nor opening the folder indexes what the link leads to
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert!(r.error.is_some(), "{r:?}");
        crate::folders::sync_folder(&env.st, &node(&env, &space.root).await).await;
        assert!(env.node_at(drive, "secret.key").await.is_none());
        assert_eq!(env.node_at(drive, "drive.db").await.unwrap().0, file);
        assert!(!outside.join(crate::folders::MARKER).exists(), "nothing is written there");

        // The folder back, but its marker gone (deleted over SMB, say): the scan recognises the folder and marks it again
        std::fs::remove_file(dir).unwrap();
        std::fs::rename(&was, dir).unwrap();
        std::fs::remove_file(dir.join(crate::folders::MARKER)).unwrap();
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert!(r.error.is_none(), "{r:?}");
        assert_eq!(std::fs::read_to_string(dir.join(crate::folders::MARKER)).unwrap(), *drive);
        assert_eq!(content(&env, &admin, &file).await, b"indexed");
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn saving_and_restoring_versions_keep_to_the_space_size_limit() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("notes.txt"), b"one");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
        let limit = |bytes: i64| sqlx::query("UPDATE drives SET quota_bytes = ? WHERE id = ?").bind(bytes).bind(&space.drive).execute(&env.st.db);
        let save = |body: &'static [u8]| crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(id.clone()), HeaderMap::new(), Bytes::from_static(body));
        limit(10).await.unwrap();
        assert!(save(b"far more than ten bytes").await.is_err(), "over the limit");
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"one");
        let _ = save(b"0123456789").await.unwrap();
        let _ = save(b"x").await.unwrap();
        // The earlier ten bytes don't fit once the limit is lower
        limit(5).await.unwrap();
        let axum::Json(list) = versions::list(State(env.st.clone()), admin.clone(), UrlPath(id.clone())).await.unwrap();
        let list = serde_json::to_value(&list).unwrap();
        let ten = list.as_array().unwrap().iter().find(|v| v["size"] == 10).unwrap()["id"].as_str().unwrap().to_string();
        assert!(versions::restore(State(env.st.clone()), admin.clone(), UrlPath((id.clone(), ten))).await.is_err());
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"x");
    }

    #[tokio::test]
    async fn a_file_changed_on_the_server_while_editing_is_kept_and_the_edit_saved_as_a_copy() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("notes.txt"), b"one");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
        let save = |body: &'static [u8], base: i64| {
            let mut h = HeaderMap::new();
            h.insert("x-base-version", base.to_string().parse().unwrap());
            crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(id.clone()), h, Bytes::from_static(body))
        };

        let Json(saved) = save(b"two", node(&env, &id).await.updated_at).await.unwrap();
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"two");
        assert_eq!(saved.size, 3);

        // Changed on the server (over SMB, say) while someone edits it here
        write_old(&space.dir.join("notes.txt"), b"changed on the server");
        let err = save(b"three", saved.updated_at).await.unwrap_err();
        assert_eq!((err.status, err.code), (StatusCode::CONFLICT, Some("conflict_copy")), "{}", err.message);
        assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"changed on the server");
        assert_eq!(std::fs::read(space.dir.join("notes (conflict copy).txt")).unwrap(), b"three");
        assert!(env.node_at(&space.drive, "notes (conflict copy).txt").await.is_some());
    }

    #[tokio::test]
    async fn an_earlier_version_stays_as_it_was_when_the_file_is_written_in_place() {
        use std::io::Write;
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("notes.txt"), b"one");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
        let _ = crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(id.clone()), HeaderMap::new(), Bytes::from_static(b"two"))
            .await
            .unwrap();
        // The version keeps the file that was replaced (a hard link to it): the file's name now has new content, so
        // a program writing to the file in place (over SMB, say) changes only that
        std::fs::OpenOptions::new().append(true).open(space.dir.join("notes.txt")).unwrap().write_all(b" and more").unwrap();
        let kept: Vec<Vec<u8>> = std::fs::read_dir(space.dir.join(versions::VERSIONS_DIR).join(&id))
            .unwrap()
            .flatten()
            .map(|e| std::fs::read(e.path()).unwrap())
            .collect();
        assert_eq!(kept, [b"one".to_vec()]);
    }

    #[tokio::test]
    async fn a_file_replaced_on_a_disk_without_hard_links_is_kept_without_copying_it() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("big.bin"), &vec![7u8; 1 << 20]);
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (id, _) = env.node_at(&space.drive, "big.bin").await.unwrap();
        let before = stat(&space.dir.join("big.bin")).unwrap();
        let _no_links = testing::no_hard_links(&space.drive);
        let _ = crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(id.clone()), HeaderMap::new(), Bytes::from_static(b"small"))
            .await
            .unwrap();
        assert_eq!(std::fs::read(space.dir.join("big.bin")).unwrap(), b"small");
        // The file itself became the version (moved, not copied: copying a large file held up every change)
        let kept: Vec<std::path::PathBuf> = std::fs::read_dir(space.dir.join(versions::VERSIONS_DIR).join(&id)).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read(&kept[0]).unwrap().len(), 1 << 20);
        let version = stat(&kept[0]).unwrap();
        assert_eq!((version.dev, version.ino), (before.dev, before.ino));
        #[cfg(unix)]
        assert!(before.ino != 0);
        // And it restores as any version does
        let (vid,): (String,) = sqlx::query_as("SELECT id FROM node_versions WHERE node_id = ?").bind(&id).fetch_one(&env.st.db).await.unwrap();
        let _ = crate::versions::restore(State(env.st.clone()), admin.clone(), UrlPath((id.clone(), vid))).await.unwrap();
        assert_eq!(std::fs::read(space.dir.join("big.bin")).unwrap().len(), 1 << 20);
    }

    #[tokio::test]
    async fn items_move_and_copy_between_folder_spaces_and_the_content_store() {
        let env = testutil::env().await;
        let one = env.folder_space("One").await;
        let two = env.folder_space("Two").await;
        let admin = env.admin().await;
        let st = || State(env.st.clone());
        write_old(&one.dir.join("Sub/x.txt"), b"x");
        write_old(&one.dir.join("Sub/Inner/y.txt"), b"yy");
        crate::folders::scan(&env.st, &one.drive).await.unwrap();
        let (sub, _) = env.node_at(&one.drive, "Sub").await.unwrap();
        let (x, _) = env.node_at(&one.drive, "Sub/x.txt").await.unwrap();

        // Copied to another folder space
        let _ = crate::nodes::copy_nodes(st(), admin.clone(), req(json!({ "ids": [sub], "dest_id": two.root }))).await.unwrap();
        assert_eq!(std::fs::read(two.dir.join("Sub/Inner/y.txt")).unwrap(), b"yy");
        assert!(env.node_at(&two.drive, "Sub/Inner/y.txt").await.is_some());

        // Moved to a folder space on another disk: copied, checked, then the original goes; the items keep their ids
        let Json(dest) = crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": two.root, "name": "Dest" }))).await.unwrap();
        OTHER_DISK.with(|d| d.set(true));
        let moved = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [sub], "dest_id": dest.id }))).await;
        OTHER_DISK.with(|d| d.set(false));
        let _ = moved.unwrap();
        assert_eq!(std::fs::read(two.dir.join("Dest/Sub/x.txt")).unwrap(), b"x");
        assert!(eventually_gone(&one.dir.join("Sub")).await);
        assert_eq!(env.node_at(&two.drive, "Dest/Sub/x.txt").await.unwrap().0, x);
        assert_eq!(env.drive_of(&x).await, two.drive);

        // Into the content store: stored, then removed from the folder
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [sub], "dest_id": admin.root() }))).await.unwrap();
        let n = node(&env, &x).await;
        assert!(n.blob_hash.is_some() && n.fs_path.is_none());
        assert_eq!(content(&env, &admin, &x).await, b"x");
        assert!(eventually_gone(&two.dir.join("Dest/Sub/x.txt")).await);

        // And back out of it into a folder space: written to the folder, the stored content released
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [sub], "dest_id": one.root }))).await.unwrap();
        assert_eq!(std::fs::read(one.dir.join("Sub/x.txt")).unwrap(), b"x");
        assert_eq!(std::fs::read(one.dir.join("Sub/Inner/y.txt")).unwrap(), b"yy");
        let n = node(&env, &x).await;
        assert!(n.blob_hash.is_none() && n.fs_path.as_deref() == Some("Sub/x.txt"));
        let (blobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM blobs").fetch_one(&env.st.db).await.unwrap();
        assert_eq!(blobs, 0);

        // A copy from a folder space into the content store
        let _ = crate::nodes::copy_nodes(st(), admin.clone(), req(json!({ "ids": [x], "dest_id": admin.root() }))).await.unwrap();
        let (copy,): (String,) =
            sqlx::query_as("SELECT id FROM nodes WHERE parent_id = ? AND name = 'x.txt'").bind(admin.root()).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(content(&env, &admin, &copy).await, b"x");
        assert!(one.dir.join("Sub/x.txt").is_file(), "copying leaves the original");
    }

    #[tokio::test]
    async fn a_change_of_several_items_that_fails_halfway_puts_the_first_ones_back() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let (dir, drive) = (&space.dir, &space.drive);
        let st = || State(env.st.clone());
        write_old(&dir.join("a.txt"), b"a");
        write_old(&dir.join("b.txt"), b"b");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (a, _) = env.node_at(drive, "a.txt").await.unwrap();
        let (b, _) = env.node_at(drive, "b.txt").await.unwrap();
        let Json(docs) = crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": space.root, "name": "Docs" }))).await.unwrap();
        let unchanged = || async {
            assert!(dir.join("a.txt").is_file() && dir.join("b.txt").is_file());
            assert_eq!(env.node_at(drive, "a.txt").await.unwrap().0, a);
            assert!(node(&env, &a).await.trashed_at.is_none());
            let r = crate::folders::scan(&env.st, drive).await.unwrap();
            assert_eq!((r.added, r.moved, r.removed), (0, 0, 0), "{r:?}");
        };

        // The space's root folder can't be deleted or moved: the first item was already changed on disk by then
        assert!(crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [a, space.root] }))).await.is_err());
        unchanged().await;
        assert_eq!(std::fs::read_dir(dir.join(TRASH_DIR)).map(|r| r.count()).unwrap_or(0), 0, "no trash folder is left");
        assert!(crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [a, space.root], "dest_id": docs.id }))).await.is_err());
        unchanged().await;

        // Restoring: the first item goes back to the trash when the second one fails
        let _ = crate::nodes::trash(st(), admin.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        let in_trash = node(&env, &a).await.fs_file().unwrap();
        assert!(crate::nodes::restore(st(), admin.clone(), req(json!({ "ids": [a, "no-such-item"] }))).await.is_err());
        assert!(in_trash.is_file() && !dir.join("a.txt").exists());
        assert_eq!(node(&env, &a).await.fs_file().unwrap(), in_trash);
        let _ = crate::nodes::restore(st(), admin.clone(), req(json!({ "ids": [a] }))).await.unwrap();
        unchanged().await;

        // Nothing went wrong: the changes stay
        let _ = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [a, b], "dest_id": docs.id }))).await.unwrap();
        assert!(dir.join("Docs/a.txt").is_file() && dir.join("Docs/b.txt").is_file());
    }

    #[tokio::test]
    async fn a_folder_that_looks_empty_or_cant_be_read_keeps_its_items() {
        let env = testutil::env().await;
        let space = env.folder_space("NAS").await;
        let (dir, drive) = (&space.dir, &space.drive);
        write_old(&dir.join("a.txt"), b"a");
        write_old(&dir.join("Sub/b.txt"), b"b");
        crate::folders::scan(&env.st, drive).await.unwrap();
        let (a, _) = env.node_at(drive, "a.txt").await.unwrap();

        // A share that isn't mounted: its mount point is an empty folder, without the marker
        let mounted = dir.with_extension("mounted");
        std::fs::rename(dir, &mounted).unwrap();
        std::fs::create_dir(dir).unwrap();
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert!(r.error.is_some() && r.removed == 0, "{r:?}");
        assert_eq!(env.node_at(drive, "a.txt").await.unwrap().0, a);
        std::fs::remove_dir(dir).unwrap();
        std::fs::rename(&mounted, dir).unwrap();
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert_eq!((r.added, r.removed, r.error.is_none()), (0, 0, true), "{r:?}");

        // A folder that can't be read keeps what the index has in it
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join("Sub"), std::fs::Permissions::from_mode(0o000)).unwrap();
            let r = crate::folders::scan(&env.st, drive).await;
            std::fs::set_permissions(dir.join("Sub"), std::fs::Permissions::from_mode(0o755)).unwrap();
            let r = r.unwrap();
            assert_eq!(r.removed, 0, "{r:?}");
            assert!(env.node_at(drive, "Sub/b.txt").await.is_some());
        }

        // Emptied for real (the marker is there): the items go
        std::fs::remove_file(dir.join("a.txt")).unwrap();
        std::fs::remove_dir_all(dir.join("Sub")).unwrap();
        let r = crate::folders::scan(&env.st, drive).await.unwrap();
        assert_eq!(r.removed, 3, "{r:?}");
        assert!(env.node_at(drive, "a.txt").await.is_none());
    }

    #[tokio::test]
    async fn a_file_replaced_by_a_folder_of_the_same_name_shows_as_the_folder_when_opened() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        write_old(&space.dir.join("Report"), b"a file");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        std::fs::remove_file(space.dir.join("Report")).unwrap();
        write_old(&space.dir.join("Report/inside.txt"), b"x");
        let later = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let _ = std::fs::File::open(space.dir.join("Report")).and_then(|f| f.set_modified(later));
        crate::folders::sync_folder(&env.st, &node(&env, &space.root).await).await;
        let (id, _) = env.node_at(&space.drive, "Report").await.unwrap();
        assert!(node(&env, &id).await.is_folder());
    }

    #[test]
    fn a_move_to_another_disk_keeps_what_changed_while_it_was_copied() {
        let top = std::env::temp_dir().join(format!("thirtyfile-copied-{}", new_id()));
        let (from, to) = (top.join("from"), top.join("to"));
        write_old(&from.join("same.txt"), b"same");
        write_old(&from.join("Sub/edited.txt"), b"before");
        let pin = |p: &Path| Pinned::root(p.parent().unwrap()).unwrap().join(p.file_name().unwrap().to_str().unwrap()).unwrap();
        let mut items = Vec::new();
        copy_tree(&pin(&from), &pin(&to), "", &mut items, &Tracker::default()).unwrap();
        assert_eq!(std::fs::read(to.join("Sub/edited.txt")).unwrap(), b"before");
        // Saved over SMB while the copy ran, and a file added
        std::fs::write(from.join("Sub/edited.txt"), b"after, and longer").unwrap();
        write_old(&from.join("new.txt"), b"new");
        remove_copied(CopiedTree { top: pin(&from), items });
        assert!(!from.join("same.txt").exists());
        assert_eq!(std::fs::read(from.join("Sub/edited.txt")).unwrap(), b"after, and longer");
        assert!(from.join("new.txt").is_file());
        let _ = std::fs::remove_dir_all(&top);
    }

    #[test]
    fn what_a_stopped_change_left_behind_is_put_back_or_removed() {
        let dir = std::env::temp_dir().join(format!("thirtyfile-leftovers-{}", new_id()));
        let (moving, upload) = (format!("{MOVE_PREFIX}{}", new_id()), format!("{UPLOAD_PREFIX}{}", new_id()));
        write_old(&dir.join(&moving).join("Report/a.txt"), b"moving");
        write_old(&dir.join("Report"), b"already taken");
        write_old(&dir.join(&upload), b"half an upload");
        assert!(is_leftover(&moving) && is_leftover(&format!("{SAVE_PREFIX}{}", new_id())) && !is_leftover(TRASH_DIR));
        let found = [dir.join(&moving), dir.join(&upload)];
        let root = Pinned::root(&dir).unwrap();
        let pinned = || vec![root.join(&moving).unwrap(), root.join(&upload).unwrap()];
        clean_leftovers(pinned(), TRASH_GRACE);
        assert!(found.iter().all(|p| p.exists()), "recent ones may still be in use");
        clean_leftovers(pinned(), std::time::Duration::ZERO);
        assert_eq!(std::fs::read(dir.join("Report (1)/a.txt")).unwrap(), b"moving");
        assert!(found.iter().all(|p| !p.exists()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn names_scans_skip_are_refused_in_folder_spaces() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let st = || State(env.st.clone());
        write_old(&space.dir.join("app.py"), b"print()");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (app, _) = env.node_at(&space.drive, "app.py").await.unwrap();
        for name in [
            ".thirtyfile-save-x",
            ".thirtyfile-upload-x",
            ".thirtyfile-trash",
            ".THIRTYFILE-versions",
            ".thirtyfile-space",
            "~$Budget.xlsx",
            ".~lock.Budget.xlsx#",
            "movie.mp4.part",
            "setup.crdownload",
            "Thumbs.db",
            "desktop.ini",
            ".DS_Store",
            ".smbdelete0001",
        ] {
            let err = crate::nodes::create_folder(st(), admin.clone(), req(json!({ "parent_id": space.root, "name": name }))).await.unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST, "{name}: {}", err.message);
            let err = crate::nodes::rename(st(), admin.clone(), UrlPath(app.clone()), req(json!({ "name": name }))).await.unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST, "{name}: {}", err.message);
            let name: &'static str = name.to_string().leak();
            assert!(env.try_upload(&admin, &space.root, name, b"x").await.is_err(), "{name}");
        }
        let names: Vec<String> = std::fs::read_dir(&space.dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        let mut names = names;
        names.sort();
        assert_eq!(names, [crate::folders::MARKER, "app.py"]);
        assert_eq!(node(&env, &app).await.name, "app.py");

        // Nor can they come in from the content store, where they are ordinary names
        let docs = env.folder(&admin, admin.root(), "Docs").await;
        env.upload(&admin, &docs, "desktop.ini", b"[.ShellClassInfo]").await;
        let err = crate::nodes::copy_nodes(st(), admin.clone(), req(json!({ "ids": [docs], "dest_id": space.root }))).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST, "{}", err.message);
        let err = crate::nodes::move_nodes(st(), admin.clone(), req(json!({ "ids": [docs], "dest_id": space.root }))).await.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST, "{}", err.message);
        assert_eq!(env.drive_of(&docs).await, env.drive_of(admin.root()).await);
        assert!(!space.dir.join("Docs").exists());
        // Names that only look alike are fine
        env.upload(&admin, &space.root, "part.txt", b"x").await;
        env.upload(&admin, &space.root, "thirtyfile-notes.txt", b"x").await;
    }

    #[tokio::test]
    async fn only_what_thirtyfile_left_behind_is_cleaned_up() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let dir = &space.dir;
        let old = |path: &Path| {
            let two_days = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600);
            std::fs::File::options().write(true).open(path).unwrap().set_modified(two_days).unwrap();
        };
        // Named by people (over SMB, say) like ThirtyFile's own files, and old
        let people = [
            ".thirtyfile-save-x".to_string(),
            format!("{UPLOAD_PREFIX}notes.txt"),
            format!("{MOVE_PREFIX}{}", "A".repeat(32)),
            format!("{TRASH_DIR}/Notes/a.txt"),
            format!("{}/Diary/2026.txt", versions::VERSIONS_DIR),
        ];
        for p in &people {
            write_old(&dir.join(p), b"kept");
            old(&dir.join(p));
        }
        for p in [format!("{TRASH_DIR}/Notes"), format!("{}/Diary", versions::VERSIONS_DIR)] {
            let two_days = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600);
            let _ = std::fs::File::open(dir.join(p)).and_then(|f| f.set_modified(two_days));
        }
        // Left by ThirtyFile
        let upload = format!("{UPLOAD_PREFIX}{}", new_id());
        write_old(&dir.join(&upload), b"half an upload");
        old(&dir.join(&upload));
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        assert!(eventually_gone(&dir.join(&upload)).await);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        for p in &people {
            assert_eq!(std::fs::read(dir.join(p)).unwrap(), b"kept", "{p}");
        }
    }

    /// Waits until the removal of this content from the built-in location is scheduled
    async fn removal_scheduled(env: &testutil::TestEnv, content: &[u8]) -> bool {
        let hash = crate::util::sha256_hex(content);
        for _ in 0..100 {
            let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pending_blob_deletes WHERE hash = ? AND location_id = 'local'")
                .bind(&hash)
                .fetch_one(&env.st.db)
                .await
                .unwrap();
            if n > 0 {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_file_changed_while_it_was_being_stored_stays_in_the_folder() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("Sub/same.txt"), b"same");
        write_old(&space.dir.join("Sub/edited.txt"), b"before");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (sub, _) = env.node_at(&space.drive, "Sub").await.unwrap();
        let (edited, _) = env.node_at(&space.drive, "Sub/edited.txt").await.unwrap();
        // Saved over SMB after it was stored in the content store, before the originals are removed
        let path = space.dir.join("Sub/edited.txt");
        let _hook = hook_after_place(move || {
            let path = path.clone();
            Box::pin(async move { std::fs::write(&path, b"after, and longer").unwrap() })
        });
        let _ = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [sub], "dest_id": admin.root() }))).await.unwrap();
        assert!(eventually_gone(&space.dir.join("Sub/same.txt")).await);
        assert_eq!(std::fs::read(space.dir.join("Sub/edited.txt")).unwrap(), b"after, and longer");
        assert_eq!(content(&env, &admin, &edited).await, b"before");
    }

    #[tokio::test]
    async fn items_stay_in_a_space_that_started_moving_to_another_location_meanwhile() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("a.txt"), b"a");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (a, _) = env.node_at(&space.drive, "a.txt").await.unwrap();
        // The space starts moving to another storage location while the item's content is being copied
        let (st, drive) = (env.st.clone(), space.drive.clone());
        let _hook = hook_after_place(move || {
            let (st, drive) = (st.clone(), drive.clone());
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO space_moves (id, drive_id, space_name, space_kind, from_mode, to_location, to_mode, state, created_at)
                     VALUES ('m', ?, 'Shared', 'team', 'folder', 'local', 'store', 'running', 0)",
                )
                .bind(&drive)
                .execute(&st.db)
                .await
                .unwrap();
                sqlx::query("UPDATE drives SET moving = 1 WHERE id = ?").bind(drive).execute(&st.db).await.unwrap();
            })
        });
        let res = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [a], "dest_id": admin.root() }))).await;
        assert!(res.is_err());
        assert_eq!(env.drive_of(&a).await, space.drive);
        assert_eq!(node(&env, &a).await.fs_path.as_deref(), Some("a.txt"));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(space.dir.join("a.txt").is_file());
    }

    #[tokio::test]
    async fn a_folder_space_whose_disk_doesnt_answer_holds_up_nothing_else() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Nas").await;
        write_old(&space.dir.join("a.txt"), b"a");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (a, _) = env.node_at(&space.drive, "a.txt").await.unwrap();
        let rename = |name: &'static str| {
            let (st, admin, a) = (env.st.clone(), admin.clone(), a.clone());
            async move { crate::nodes::rename(State(st), admin, UrlPath(a), req(json!({ "name": name }))).await.map(|_| ()) }
        };
        // The disk of the space stops answering (a network share whose server went away, say)
        let _short = testing::short_waits();
        let hung = testing::hang(&space.drive, std::time::Duration::from_secs(3));
        let started = std::time::Instant::now();
        let renaming = tokio::spawn(rename("b.txt"));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Changes elsewhere go on meanwhile
        let _ = crate::nodes::create_folder(State(env.st.clone()), admin.clone(), req(json!({ "parent_id": admin.root(), "name": "Other" }))).await.unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(1), "waited {:?}", started.elapsed());
        // The change to the space gives up, saying why
        let err = renaming.await.unwrap().unwrap_err();
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE, "{}", err.message);
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "waited {:?}", started.elapsed());
        // Asked again while the disk still hasn't answered: refused at once, without another wait
        let again = std::time::Instant::now();
        assert_eq!(rename("c.txt").await.unwrap_err().status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(again.elapsed() < std::time::Duration::from_millis(500));
        // Once it answers again, changes go through. (The step given up on may have been done in the end: the next scan
        // shows it, as it would a change made on the server.)
        drop(hung);
        let mut done = false;
        for _ in 0..100 {
            match rename("d.txt").await {
                Ok(()) => {
                    done = true;
                    break;
                }
                Err(e) if e.status == StatusCode::NOT_FOUND => {
                    crate::folders::scan(&env.st, &space.drive).await.unwrap();
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
        assert!(done && space.dir.join("d.txt").is_file());
        assert_eq!(env.node_at(&space.drive, "d.txt").await.unwrap().0, a);
    }

    #[tokio::test]
    async fn a_move_to_another_space_goes_on_when_the_request_is_given_up() {
        let env = testutil::env().await;
        let one = env.folder_space("One").await;
        let two = env.folder_space("Two").await;
        let admin = env.admin().await;
        write_old(&one.dir.join("Sub/x.txt"), b"x");
        crate::folders::scan(&env.st, &one.drive).await.unwrap();
        let (sub, _) = env.node_at(&one.drive, "Sub").await.unwrap();
        // The content takes a while to copy: longer than the browser waits
        let go = std::sync::Arc::new(tokio::sync::Notify::new());
        let _hook = hook_after_place(wait_for(&go));
        OTHER_DISK.with(|d| d.set(true));
        let asked = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [sub], "dest_id": two.root })));
        // The browser (or a proxy) gives up on the request: the move goes on regardless
        let _ = tokio::time::timeout(std::time::Duration::from_millis(300), asked).await;
        go.notify_one();
        let mut moved = false;
        for _ in 0..200 {
            if env.drive_of(&sub).await == two.drive {
                moved = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        OTHER_DISK.with(|d| d.set(false));
        assert!(moved, "the move finished");
        assert_eq!(std::fs::read(two.dir.join("Sub/x.txt")).unwrap(), b"x");
        assert!(eventually_gone(&one.dir.join("Sub")).await);
        let hidden: Vec<String> =
            std::fs::read_dir(&two.dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(".thirtyfile-m") || n.starts_with(".thirtyfile-c")).collect();
        assert!(hidden.is_empty(), "nothing is left behind: {hidden:?}");

        // A copy inside a folder space, the same
        let asked = crate::nodes::copy_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [sub], "dest_id": two.root })));
        let _ = tokio::time::timeout(std::time::Duration::from_millis(300), asked).await;
        go.notify_one();
        let mut copied = false;
        for _ in 0..200 {
            if env.node_at(&two.drive, "Sub (1)/x.txt").await.is_some() {
                copied = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(copied, "the copy finished");
        assert_eq!(std::fs::read(two.dir.join("Sub (1)/x.txt")).unwrap(), b"x");
    }

    #[tokio::test]
    async fn a_move_to_another_disk_copies_under_a_name_that_is_removed_should_it_stop() {
        let env = testutil::env().await;
        let one = env.folder_space("One").await;
        let two = env.folder_space("Two").await;
        let admin = env.admin().await;
        write_old(&one.dir.join("Sub/x.txt"), b"x");
        crate::folders::scan(&env.st, &one.drive).await.unwrap();
        let (sub, _) = env.node_at(&one.drive, "Sub").await.unwrap();
        // While the copy waits for the index, the destination holds it under a copy's name: should ThirtyFile stop
        // now, the copy is removed later (`clean_leftovers`) rather than put in place next to the original, which is
        // still in "One"
        let (dir, seen) = (two.dir.clone(), std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
        let names = seen.clone();
        let _hook = hook_after_place(move || {
            let (dir, names) = (dir.clone(), names.clone());
            Box::pin(async move {
                let found = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned());
                names.lock().unwrap().extend(found.filter(|n| n.starts_with(".thirtyfile-") && n != crate::folders::MARKER));
            })
        });
        OTHER_DISK.with(|d| d.set(true));
        let moved = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [sub], "dest_id": two.root }))).await;
        OTHER_DISK.with(|d| d.set(false));
        assert_eq!(moved.unwrap().0.state, "done");
        let seen = seen.lock().unwrap().clone();
        assert!(seen.len() == 1 && seen[0].starts_with(COPY_PREFIX), "{seen:?}");
        assert!(two.dir.join("Sub/x.txt").is_file());
    }

    #[tokio::test]
    async fn earlier_versions_in_the_content_store_go_with_a_file_deleted_on_the_server() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        // A file of the content store, with an earlier version there, moved into the folder space
        let notes = env.upload(&admin, admin.root(), "notes.txt", b"version one").await;
        let _ = crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(notes.clone()), HeaderMap::new(), Bytes::from_static(b"two"))
            .await
            .unwrap();
        let _ = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [notes], "dest_id": space.root }))).await.unwrap();
        let (kept,): (Option<String>,) = sqlx::query_as("SELECT blob_hash FROM node_versions WHERE node_id = ?").bind(&notes).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(kept, Some(crate::util::sha256_hex(b"version one")));
        // Deleted on the server
        std::fs::remove_file(space.dir.join("notes.txt")).unwrap();
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        assert!(removal_scheduled(&env, b"version one").await);

        // The same when the file is gone from the server while its folder is moved to another folder space
        let two = env.folder_space("Two").await;
        let docs = env.folder(&admin, admin.root(), "Docs").await;
        let other = env.upload(&admin, &docs, "other.txt", b"other, one").await;
        let _ = crate::files::save_content(State(env.st.clone()), admin.clone(), UrlPath(other.clone()), HeaderMap::new(), Bytes::from_static(b"two"))
            .await
            .unwrap();
        let _ = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [docs], "dest_id": space.root }))).await.unwrap();
        let dir = two.dir.clone();
        let _hook = hook_after_place(move || {
            let dir = dir.clone();
            Box::pin(async move {
                for e in std::fs::read_dir(&dir).unwrap().flatten() {
                    if e.file_name().to_string_lossy().starts_with(MOVE_PREFIX) {
                        std::fs::remove_file(e.path().join("Docs/other.txt")).unwrap();
                    }
                }
            })
        });
        let _ = crate::nodes::move_nodes(State(env.st.clone()), admin.clone(), req(json!({ "ids": [docs], "dest_id": two.root }))).await.unwrap();
        assert!(two.dir.join("Docs").is_dir());
        assert!(tree::get_node(&mut env.st.db.acquire().await.unwrap(), &other).await.unwrap().is_none());
        assert!(removal_scheduled(&env, b"other, one").await);
    }
}
