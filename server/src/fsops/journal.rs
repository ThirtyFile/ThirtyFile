//! Renames a change makes on disk before its transaction commits, written down first.
//!
//! Renaming an item, moving it to another folder of its space, moving it to the trash or out of it, and keeping a file
//! as an earlier version before new content takes its name rename it on disk within the change's transaction. Should
//! ThirtyFile stop after such a rename and before the commit (or the transaction fail), the index still has the item
//! where it was: the next scan would take it for removed, dropping it with its links, access and versions, and the
//! trash or versions folder would later delete what is now its only copy.
//! So each such rename is first written into the space's journal (`.thirtyfile-journal/<id>`, on disk before the
//! rename), and the entry goes once the change is committed, or undone. An entry still there was left by a change that
//! didn't finish: the next scan, holding the space's lock (so no change is under way), looks in the index to tell
//! whether it was committed, and puts back what it left (`recover`) before it reads the folders that differ. Without
//! that, an item renamed or moved could only be told from one removed and another added by its identity on disk: on
//! Windows and exFAT, which don't give one, it would lose its id, and with it its links, access and versions.

use serde::{Deserialize, Serialize};

use super::*;

/// A folder space's journal, in its folder (never indexed)
pub const JOURNAL_DIR: &str = ".thirtyfile-journal";

/// A rename written into the journal before it is made (paths below the space's folder)
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Intent {
    /// The file `node` at `from` kept as its earlier version `version` at `to` (linked, or moved, there) before new
    /// content takes its name
    Version { node: String, version: String, from: String, to: String },
    /// The item `node` at `from` moved into the trash, to `to`
    Trash { node: String, from: String, to: String },
    /// The item `node` at `from` renamed, or moved to another folder of the space (out of the trash too), to `to`
    Move { node: String, from: String, to: String },
}

/// An entry in a space's journal
#[derive(Debug)]
pub struct Entry {
    file: Pinned,
    /// Its path below the space's folder
    pub rel: String,
}

impl Entry {
    /// Removes the entry: the change it was written for was committed or undone (a blocking disk step)
    pub fn remove(self) {
        if let Err(e) = std::fs::remove_file(self.file.as_path())
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!("Couldn't remove {} from the journal of a folder space: {e}", self.rel);
        }
    }
}

/// Removes entries in the background
pub fn remove_entries_later(entries: Vec<Entry>) {
    if !entries.is_empty() {
        tokio::task::spawn_blocking(move || entries.into_iter().for_each(Entry::remove));
    }
}

/// Writes `intent` into the journal of the space whose folder is `root`, on disk before it returns. A blocking disk
/// step.
pub fn write(root: &Pinned, intent: &Intent) -> io::Result<Entry> {
    let dir = root.join(JOURNAL_DIR)?;
    ensure_dir(&dir)?;
    let id = new_id();
    let file = dir.join(&id)?;
    crate::beneath::write_new(&file, &serde_json::to_vec(intent).map_err(io::Error::other)?)?;
    // Its name on disk too, before the rename it is written for
    #[cfg(unix)]
    std::fs::File::open(dir.dir()?.as_path())?.sync_all()?;
    Ok(Entry { file, rel: format!("{JOURNAL_DIR}/{id}") })
}

/// Whether the journal of the space whose folder is `root` has entries (a blocking disk step)
pub fn journal_pending(root: &Pinned) -> bool {
    root.join(JOURNAL_DIR).and_then(|d| d.dir()).and_then(|d| std::fs::read_dir(d.as_path())).is_ok_and(|mut r| r.next().is_some())
}

/// Puts the item at `from` into the folder `dir` as `name`, or as "name (1)"… when something else took that name
/// meanwhile; returns the name it got (None when every name tried is taken)
pub(super) fn put_back_into(from: &Pinned, dir: &Pinned, name: &str, is_dir: bool) -> Option<io::Result<String>> {
    (0..10_000u32).map(|n| if n == 0 { name.to_string() } else { numbered_name(name, n, is_dir) }).find_map(|candidate| {
        match dir.join(&candidate).and_then(|to| rename_new(from.as_path(), to.as_path())) {
            Ok(()) => Some(Ok(candidate)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => None,
            Err(e) => Some(Err(e)),
        }
    })
}

/// What was found of a file kept as a version by a change that didn't finish
enum Kept {
    /// Nothing left to do: the version's file is gone, or was put back where it was
    Done,
    /// New content took the file's name: the version's file is what it had, of this size
    Replaced(i64),
}

/// Whether two items on disk are the same file (hard links to it): by identity where the system tells it, else by
/// size and time of change
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    let (x, y) = (crate::folders::identity(a), crate::folders::identity(b));
    if x.1 != 0 {
        return x == y;
    }
    a.len() == b.len() && crate::folders::mtime_ns(a) == crate::folders::mtime_ns(b)
}

/// Finishes what changes to the space `drive` left in its journal (`root`: its folder, its marker checked). Called by
/// a scan with the space's lock held, so no change is under way: an entry is from a change that was committed (only
/// the entry is left), or from one that never will be. For those, an item moved to the trash is put back where it was,
/// and a file kept as a version goes back to its place, unless new content took that, when it is recorded as the
/// earlier version it was meant to be.
pub async fn recover(st: &AppState, drive: &str, root: &Pinned) -> AppResult<()> {
    let r = root.clone();
    let entries = tokio::task::spawn_blocking(move || -> Vec<(Entry, Option<Intent>)> {
        let Ok(dir) = r.join(JOURNAL_DIR).and_then(|d| d.dir()) else { return Vec::new() };
        let Ok(read) = std::fs::read_dir(dir.as_path()) else { return Vec::new() };
        read.flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| {
                let file = dir.join(&name).ok()?;
                // Written in full before its rename: one cut short was never acted on
                let intent = std::fs::read(file.as_path()).ok().and_then(|b| serde_json::from_slice(&b).ok());
                Some((Entry { file, rel: format!("{JOURNAL_DIR}/{name}") }, intent))
            })
            .collect()
    })
    .await
    .map_err(AppError::internal)?;
    for (entry, intent) in entries {
        let done = match intent {
            None => Ok(()),
            Some(Intent::Trash { node, from, to }) => recover_trash(st, drive, root, &node, &from, &to).await?,
            Some(Intent::Move { node, from, to }) => recover_move(st, drive, root, &node, &from, &to).await?,
            Some(Intent::Version { node, version, from, to }) => recover_version(st, drive, root, &node, &version, &from, &to).await?,
        };
        match done {
            Ok(()) => tokio::task::spawn_blocking(move || entry.remove()).await.map_err(AppError::internal)?,
            Err(e) => tracing::warn!("Couldn't finish what a change left in a folder space ({}): {e}", entry.rel),
        }
    }
    Ok(())
}

/// The item `node` (in the index at `from`, unless the change was committed) moved into the trash, to `to`
async fn recover_trash(st: &AppState, drive: &str, root: &Pinned, node: &str, from: &str, to: &str) -> AppResult<io::Result<()>> {
    let at: Option<(Option<String>,)> =
        sqlx::query_as("SELECT fs_path FROM nodes WHERE id = ? AND drive_id = ? AND trashed_at IS NULL").bind(node).bind(drive).fetch_optional(&st.db).await?;
    // In the trash (the change was committed), or gone: nothing to put back
    if at.and_then(|(p,)| p).as_deref() != Some(from) {
        return Ok(Ok(()));
    }
    let (root, from, to) = (root.clone(), from.to_string(), to.to_string());
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let item = root.join(&to)?;
        let Ok(meta) = std::fs::symlink_metadata(item.as_path()) else { return Ok(()) };
        let (dir, name) = match from.rsplit_once('/') {
            Some((up, name)) => (root.join(up)?.dir()?, name),
            None => (root.clone(), from.as_str()),
        };
        match put_back_into(&item, &dir, name, meta.is_dir()) {
            Some(Ok(n)) => tracing::warn!("Put back {n:?}: it was being moved to the trash when ThirtyFile stopped"),
            Some(Err(e)) => return Err(e),
            None => return Err(io::Error::new(io::ErrorKind::AlreadyExists, "too many items with the same name")),
        }
        if let Some(folder) = item.parent() {
            let _ = std::fs::remove_dir(folder.as_path());
        }
        Ok(())
    })
    .await
    .map_err(AppError::internal)
}

/// What was found of an item a change that didn't finish renamed or moved
enum Moved {
    /// Nothing left to do: it is where the index has it, or gone
    Done,
    /// It couldn't go back: something else has its name there now, or its folder is gone
    Stays,
}

/// The item `node` (in the index at `from`, unless the change was committed) renamed or moved to `to`. It goes back
/// where the index has it; when it can't (its old name was taken meanwhile, say), the index follows it instead, so it
/// keeps its id either way.
async fn recover_move(st: &AppState, drive: &str, root: &Pinned, node: &str, from: &str, to: &str) -> AppResult<io::Result<()>> {
    let at: Option<(Option<String>, Option<i64>)> =
        sqlx::query_as("SELECT fs_path, trashed_at FROM nodes WHERE id = ? AND drive_id = ?").bind(node).bind(drive).fetch_optional(&st.db).await?;
    // Committed (the index has it at `to`, or anywhere else since), or gone
    let Some((Some(indexed), trashed)) = at else { return Ok(Ok(())) };
    if indexed != from {
        return Ok(Ok(()));
    }
    let (r, from_rel, to_rel) = (root.clone(), from.to_string(), to.to_string());
    let found = tokio::task::spawn_blocking(move || -> io::Result<Moved> {
        let item = r.join(&to_rel)?;
        if std::fs::symlink_metadata(item.as_path()).is_err() {
            return Ok(Moved::Done);
        }
        let (up, name) = from_rel.rsplit_once('/').unwrap_or(("", from_rel.as_str()));
        let dir = if up.is_empty() { Ok(r.clone()) } else { r.join(up).and_then(|d| d.dir()) };
        let dir = match dir {
            Ok(d) => d,
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(Moved::Stays),
            Err(e) => return Err(e),
        };
        match dir.join(name).and_then(|back| rename_new(item.as_path(), back.as_path())) {
            Ok(()) => {
                tracing::warn!("Put back {name:?}: it was being renamed or moved when ThirtyFile stopped");
                Ok(Moved::Done)
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound) => Ok(Moved::Stays),
            Err(e) => Err(e),
        }
    })
    .await
    .map_err(AppError::internal)?;
    match found {
        Ok(Moved::Done) => return Ok(Ok(())),
        Ok(Moved::Stays) => {}
        Err(e) => return Ok(Err(e)),
    }
    // Out of the trash, the item's trash would have to be undone too: that is left to the scan
    if trashed.is_some() {
        tracing::warn!("An item was being taken out of the trash when ThirtyFile stopped, and couldn't go back: the check of the folder finds it");
        return Ok(Ok(()));
    }
    let (up, name) = to.rsplit_once('/').unwrap_or(("", to));
    let parent: Option<(String,)> = if up.is_empty() {
        sqlx::query_as("SELECT root_id FROM drives WHERE id = ?").bind(drive).fetch_optional(&st.db).await?
    } else {
        sqlx::query_as("SELECT id FROM nodes WHERE drive_id = ? AND fs_path = ? AND kind = 'folder' AND trashed_at IS NULL")
            .bind(drive)
            .bind(up)
            .fetch_optional(&st.db)
            .await?
    };
    let Some((parent,)) = parent else {
        tracing::warn!("An item was being moved when ThirtyFile stopped, into a folder that is gone: the check of the folder finds it");
        return Ok(Ok(()));
    };
    let recorded = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = async {
            sqlx::query("UPDATE nodes SET parent_id = ?, name = ?, mime = CASE WHEN kind = 'file' THEN ? ELSE mime END WHERE id = ?")
                .bind(&parent)
                .bind(name)
                .bind(guess_mime(name))
                .bind(node)
                .execute(&mut *tx)
                .await?;
            AppResult::Ok(())
        }
        .await;
        crate::db::settle(tx, res).await
    };
    if let Err(e) = recorded {
        // Another item has that name in the index: the check of the folder sorts it out
        tracing::warn!("Couldn't record an item moved when ThirtyFile stopped: {}", e.message);
        return Ok(Ok(()));
    }
    changes::repath_now(st, drive, from, to).await?;
    tracing::warn!("Kept {name:?} where it was renamed or moved to when ThirtyFile stopped");
    Ok(Ok(()))
}

/// The file `node` at `from` kept as its version `version` at `to`, before new content was to take its name
async fn recover_version(st: &AppState, drive: &str, root: &Pinned, node: &str, version: &str, from: &str, to: &str) -> AppResult<io::Result<()>> {
    let (recorded,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM node_versions WHERE id = ?)").bind(version).fetch_one(&st.db).await?;
    if recorded {
        return Ok(Ok(()));
    }
    let file = tree::get_node(&mut *st.db.acquire().await?, node)
        .await?
        .filter(|n| n.drive() == drive && n.trashed_at.is_none() && !n.is_folder() && n.fs_path.as_deref() == Some(from));
    // Gone from the index (or changed since): a file no version has, left to `versions::clean_folder`
    let Some(file) = file else { return Ok(Ok(())) };
    let (r, from_rel, to_rel) = (root.clone(), from.to_string(), to.to_string());
    let found = tokio::task::spawn_blocking(move || -> io::Result<Kept> {
        let (kept, at) = (r.join(&to_rel)?, r.join(&from_rel)?);
        let Ok(k) = std::fs::symlink_metadata(kept.as_path()) else { return Ok(Kept::Done) };
        match std::fs::symlink_metadata(at.as_path()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                rename_new(kept.as_path(), at.as_path())?;
                tracing::warn!("Put back {from_rel:?}: it was being saved when ThirtyFile stopped");
                Ok(Kept::Done)
            }
            Err(e) => Err(e),
            // Still the file itself (the version is a hard link to it): the version goes
            Ok(now) if same_file(&now, &k) => std::fs::remove_file(kept.as_path()).map(|()| Kept::Done),
            Ok(_) => Ok(Kept::Replaced(k.len() as i64)),
        }
    })
    .await
    .map_err(AppError::internal)?;
    let size = match found {
        Ok(Kept::Replaced(size)) => size,
        Ok(Kept::Done) => return Ok(Ok(())),
        Err(e) => return Ok(Err(e)),
    };
    let policy = versions::Policy::of(st);
    let removed = {
        let _w = st.write_lock.lock().await;
        let mut tx = crate::db::begin_write(&st.db).await?;
        let res = versions::record_found(&mut tx, policy, &file, version, to, size).await;
        crate::db::settle(tx, res).await?
    };
    removed.finish(st);
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::super::testing::{self, Stop};
    use super::super::*;
    use crate::testutil::{self, FolderSpace, TestEnv, write_old};
    use axum::{
        Json,
        body::Bytes,
        extract::{Path as UrlPath, State},
        http::HeaderMap,
    };

    async fn save(env: &TestEnv, user: &User, id: &str, body: &'static [u8]) -> AppResult<()> {
        crate::files::save_content(State(env.st.clone()), user.clone(), UrlPath(id.to_string()), HeaderMap::new(), Bytes::from_static(body)).await.map(|_| ())
    }

    /// What the versions of a file hold, oldest first
    async fn kept(env: &TestEnv, space: &FolderSpace, id: &str) -> Vec<Vec<u8>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT fs_path FROM node_versions WHERE node_id = ? ORDER BY created_at, rowid").bind(id).fetch_all(&env.st.db).await.unwrap();
        rows.into_iter().map(|(p,)| std::fs::read(space.dir.join(p)).unwrap()).collect()
    }

    /// Files in a folder (none when it isn't there)
    fn files_in(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |d| d.flatten().count())
    }

    #[tokio::test]
    async fn a_save_that_stops_before_the_new_content_is_in_place_leaves_the_file_where_it_was() {
        for links in [false, true] {
            let env = testutil::env().await;
            let space = env.folder_space("Shared").await;
            let admin = env.admin().await;
            write_old(&space.dir.join("notes.txt"), b"one");
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
            let _no_links = (!links).then(|| testing::no_hard_links(&space.drive));
            let _stop = testing::stop_at(&space.drive, Stop::VersionKept);
            assert!(save(&env, &admin, &id, b"second").await.is_err());

            // ThirtyFile starts again: the next scan finds the file where it was, still the same item
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"one", "links: {links}");
            assert_eq!(env.node_at(&space.drive, "notes.txt").await, Some((id.clone(), 3)), "links: {links}");
            assert!(kept(&env, &space, &id).await.is_empty());
            assert_eq!(files_in(&space.dir.join(versions::VERSIONS_DIR).join(&id)), 0, "links: {links}");
            assert_eq!(files_in(&space.dir.join(JOURNAL_DIR)), 0);
        }
    }

    #[tokio::test]
    async fn a_save_that_stops_before_it_is_committed_keeps_the_earlier_content_as_a_version() {
        for links in [false, true] {
            let env = testutil::env().await;
            let space = env.folder_space("Shared").await;
            let admin = env.admin().await;
            write_old(&space.dir.join("notes.txt"), b"one");
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
            let _no_links = (!links).then(|| testing::no_hard_links(&space.drive));
            let _stop = testing::stop_at(&space.drive, Stop::Replaced);
            assert!(save(&env, &admin, &id, b"second").await.is_err());
            // A while later (a scan leaves alone what was written just now)
            let a_minute_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
            std::fs::File::options().write(true).open(space.dir.join("notes.txt")).unwrap().set_modified(a_minute_ago).unwrap();

            // The new content reached the disk, so the next scan shows it; what the file had is an earlier version
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            assert_eq!(std::fs::read(space.dir.join("notes.txt")).unwrap(), b"second");
            assert_eq!(env.node_at(&space.drive, "notes.txt").await, Some((id.clone(), 6)), "links: {links}");
            assert_eq!(kept(&env, &space, &id).await, [b"one".to_vec()], "links: {links}");
            assert_eq!(files_in(&space.dir.join(JOURNAL_DIR)), 0);
        }
    }

    #[tokio::test]
    async fn changes_that_are_committed_leave_nothing_in_the_journal() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("notes.txt"), b"one");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (id, _) = env.node_at(&space.drive, "notes.txt").await.unwrap();
        save(&env, &admin, &id, b"two").await.unwrap();
        let req = Json(serde_json::from_value(serde_json::json!({ "ids": [id] })).unwrap());
        let _ = crate::nodes::trash(State(env.st.clone()), admin.clone(), req).await.unwrap();
        for _ in 0..100 {
            if files_in(&space.dir.join(JOURNAL_DIR)) == 0 {
                assert_eq!(kept(&env, &space, &id).await, [b"one".to_vec()]);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("entries stayed in the journal");
    }

    #[tokio::test]
    async fn moving_to_the_trash_that_stops_before_it_is_committed_leaves_the_item_where_it_was() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        let ben = env.user("ben", true).await;
        write_old(&space.dir.join("Docs/a.txt"), b"alpha");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (docs, _) = env.node_at(&space.drive, "Docs").await.unwrap();
        env.grant(&docs, &ben, "viewer").await;
        let _stop = testing::stop_at(&space.drive, Stop::Trashed);
        let req = Json(serde_json::from_value(serde_json::json!({ "ids": [docs] })).unwrap());
        assert!(crate::nodes::trash(State(env.st.clone()), admin.clone(), req).await.is_err());

        // The next scan puts it back before it would take it for removed: it keeps its id, and the access given on it
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        assert_eq!(std::fs::read(space.dir.join("Docs/a.txt")).unwrap(), b"alpha");
        assert_eq!(env.node_at(&space.drive, "Docs").await.map(|n| n.0), Some(docs.clone()));
        let (grants,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM grants WHERE node_id = ?").bind(&docs).fetch_one(&env.st.db).await.unwrap();
        assert_eq!(grants, 1);
        assert_eq!(files_in(&space.dir.join(JOURNAL_DIR)), 0);
    }

    async fn rename(env: &TestEnv, user: &User, id: &str, name: &str) -> AppResult<()> {
        let req = Json(serde_json::from_value(serde_json::json!({ "name": name })).unwrap());
        crate::nodes::rename(State(env.st.clone()), user.clone(), UrlPath(id.to_string()), req).await.map(|_| ())
    }

    async fn move_into(env: &TestEnv, user: &User, id: &str, dest: &str) -> AppResult<()> {
        let req = Json(serde_json::from_value(serde_json::json!({ "ids": [id], "dest_id": dest })).unwrap());
        crate::nodes::move_nodes(State(env.st.clone()), user.clone(), req).await.map(|_| ())
    }

    #[tokio::test]
    async fn a_rename_or_move_that_stops_before_it_is_committed_leaves_the_item_where_it_was() {
        // Where the disk tells items apart by identity (Linux), and where it doesn't (Windows, exFAT): the item keeps its
        // id, and what is attached to it, either way
        for identities in [true, false] {
            let env = testutil::env().await;
            let space = env.folder_space("Shared").await;
            let admin = env.admin().await;
            let ben = env.user("ben", true).await;
            let _hidden = (!identities).then(|| testing::no_identities(&space.dir));
            write_old(&space.dir.join("Docs/a.txt"), b"alpha");
            std::fs::create_dir_all(space.dir.join("Other")).unwrap();
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            let (docs, _) = env.node_at(&space.drive, "Docs").await.unwrap();
            let (a, _) = env.node_at(&space.drive, "Docs/a.txt").await.unwrap();
            let (other, _) = env.node_at(&space.drive, "Other").await.unwrap();
            env.grant(&docs, &ben, "viewer").await;
            let unchanged = async || {
                assert_eq!(std::fs::read(space.dir.join("Docs/a.txt")).unwrap(), b"alpha", "identities: {identities}");
                assert_eq!(env.node_at(&space.drive, "Docs").await.map(|n| n.0), Some(docs.clone()), "identities: {identities}");
                assert_eq!(env.node_at(&space.drive, "Docs/a.txt").await.map(|n| n.0), Some(a.clone()), "identities: {identities}");
                let (grants,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM grants WHERE node_id = ?").bind(&docs).fetch_one(&env.st.db).await.unwrap();
                assert_eq!(grants, 1);
                assert_eq!(files_in(&space.dir.join(JOURNAL_DIR)), 0);
            };

            // A folder renamed on disk, and ThirtyFile stops before the change is committed: the next scan puts it back
            let stop = testing::stop_at(&space.drive, Stop::Moved);
            assert!(rename(&env, &admin, &docs, "Papers").await.is_err());
            drop(stop);
            assert!(space.dir.join("Papers/a.txt").is_file());
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            assert!(!space.dir.join("Papers").exists());
            unchanged().await;

            // A file moved to another folder
            let stop = testing::stop_at(&space.drive, Stop::Moved);
            assert!(move_into(&env, &admin, &a, &other).await.is_err());
            drop(stop);
            assert!(space.dir.join("Other/a.txt").is_file());
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            assert!(!space.dir.join("Other/a.txt").exists());
            unchanged().await;

            // Taken out of the trash: it goes back into the trash
            let req = Json(serde_json::from_value(serde_json::json!({ "ids": [a] })).unwrap());
            let _ = crate::nodes::trash(State(env.st.clone()), admin.clone(), req).await.unwrap();
            let stop = testing::stop_at(&space.drive, Stop::Moved);
            let req = Json(serde_json::from_value(serde_json::json!({ "ids": [a] })).unwrap());
            assert!(crate::nodes::restore(State(env.st.clone()), admin.clone(), req).await.is_err());
            drop(stop);
            assert!(space.dir.join("Docs/a.txt").is_file());
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            assert!(!space.dir.join("Docs/a.txt").exists());
            let in_trash = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &a).await.unwrap().unwrap();
            assert!(in_trash.trashed_at.is_some() && in_trash.fs_file().unwrap().is_file(), "identities: {identities}");
            assert_eq!(files_in(&space.dir.join(JOURNAL_DIR)), 0);
        }
    }

    #[tokio::test]
    async fn an_item_whose_old_name_was_taken_meanwhile_is_kept_where_it_was_moved() {
        for identities in [true, false] {
            let env = testutil::env().await;
            let space = env.folder_space("Shared").await;
            let admin = env.admin().await;
            let _hidden = (!identities).then(|| testing::no_identities(&space.dir));
            write_old(&space.dir.join("Docs/a.txt"), b"alpha");
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            let (a, _) = env.node_at(&space.drive, "Docs/a.txt").await.unwrap();
            let stop = testing::stop_at(&space.drive, Stop::Moved);
            assert!(rename(&env, &admin, &a, "b.txt").await.is_err());
            drop(stop);
            // Before the next scan, another program puts a file under the old name
            write_old(&space.dir.join("Docs/a.txt"), b"another");

            // The index follows the item instead: it keeps its id under its new name, and the other file is new
            crate::folders::scan(&env.st, &space.drive).await.unwrap();
            assert_eq!(std::fs::read(space.dir.join("Docs/b.txt")).unwrap(), b"alpha");
            assert_eq!(env.node_at(&space.drive, "Docs/b.txt").await.map(|n| n.0), Some(a.clone()), "identities: {identities}");
            let other = env.node_at(&space.drive, "Docs/a.txt").await.unwrap().0;
            assert_ne!(other, a);
            let node = tree::get_node(&mut env.st.db.acquire().await.unwrap(), &a).await.unwrap().unwrap();
            assert_eq!(node.name, "b.txt");
            assert_eq!(files_in(&space.dir.join(JOURNAL_DIR)), 0);
        }
    }

    #[tokio::test]
    async fn renames_and_moves_that_are_committed_leave_nothing_in_the_journal() {
        let env = testutil::env().await;
        let space = env.folder_space("Shared").await;
        let admin = env.admin().await;
        write_old(&space.dir.join("Docs/a.txt"), b"alpha");
        crate::folders::scan(&env.st, &space.drive).await.unwrap();
        let (a, _) = env.node_at(&space.drive, "Docs/a.txt").await.unwrap();
        rename(&env, &admin, &a, "b.txt").await.unwrap();
        move_into(&env, &admin, &a, &space.root).await.unwrap();
        assert!(space.dir.join("b.txt").is_file());
        for _ in 0..100 {
            if files_in(&space.dir.join(JOURNAL_DIR)) == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("entries stayed in the journal");
    }
}
