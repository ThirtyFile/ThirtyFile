//! Taking turns with scans: a change to a folder space holds the space's lock, and undoes its renames if it fails

use super::*;

/// A rename made on disk by a change that isn't committed yet
pub(super) struct Renamed {
    /// Where the item is now, and where it was
    pub(super) now: Pinned,
    pub(super) was: Pinned,
    /// A folder made for the rename (a trash folder), removed again when undoing
    pub(super) made: Option<Pinned>,
    /// Its entry in the space's journal, removed once the change is committed or the rename undone
    pub(super) journal: Option<Entry>,
}

/// Scan locks of the folder spaces a change touches, held until it is done. They also keep the renames the change
/// made on disk: unless `committed` is called, dropping them puts those back (still holding the locks), newest first.
/// And they keep what the change left to finish after its transaction (tree/changes.rs): once it is committed, that is
/// finished in the background, the locks held until it is done.
pub struct SpaceLocks {
    pub(super) st: AppState,
    pub(super) held: Vec<(String, OwnedMutexGuard<()>)>,
    pub(super) renamed: std::sync::Mutex<Vec<Renamed>>,
    pub(super) unfinished: std::sync::Mutex<Vec<changes::Unfinished>>,
    pub(super) committed: std::sync::atomic::AtomicBool,
}

impl SpaceLocks {
    /// An item of a folder space must be in a locked space (it could have moved to another one meanwhile)
    pub fn check(&self, n: &Node) -> AppResult<()> {
        if n.in_folder_space() && !self.held.iter().any(|(d, _)| d == n.drive()) {
            return Err(AppError::conflict("Something changed at the same time. Try again."));
        }
        Ok(())
    }

    pub(super) fn note(&self, now: Pinned, was: Pinned, made: Option<Pinned>) {
        self.note_journaled(now, was, made, None);
    }

    /// `note`, for a rename written into the space's journal (`journal`)
    pub(super) fn note_journaled(&self, now: Pinned, was: Pinned, made: Option<Pinned>, journal: Option<Entry>) {
        self.renamed.lock().unwrap_or_else(|e| e.into_inner()).push(Renamed { now, was, made, journal });
    }

    /// What the change leaves to finish after its transaction, if anything
    pub fn later(&self, unfinished: Option<changes::Unfinished>) {
        self.unfinished.lock().unwrap_or_else(|e| e.into_inner()).extend(unfinished);
    }

    /// The change is in the index: its renames stay, and what it left to finish is finished once the locks go
    pub fn committed(&self) {
        let renamed = std::mem::take(&mut *self.renamed.lock().unwrap_or_else(|e| e.into_inner()));
        remove_entries_later(renamed.into_iter().filter_map(|r| r.journal).collect());
        self.committed.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for SpaceLocks {
    fn drop(&mut self) {
        let unfinished = std::mem::take(self.unfinished.get_mut().unwrap_or_else(|e| e.into_inner()));
        if *self.committed.get_mut() && !unfinished.is_empty() {
            let held = std::mem::take(&mut self.held).into_iter().map(|(_, g)| g).collect();
            changes::finish_later(&self.st, unfinished, held);
        }
        let renamed = std::mem::take(self.renamed.get_mut().unwrap_or_else(|e| e.into_inner()));
        if renamed.is_empty() {
            return;
        }
        // A disk that doesn't answer is waited for on a blocking thread, the locks held until it is done
        if self.held.iter().any(|(d, _)| stuck(d)) {
            let held = std::mem::take(&mut self.held);
            tokio::task::spawn_blocking(move || {
                put_back(renamed);
                drop(held);
            });
            return;
        }
        // Else put back before the change reports its failure, the async worker handing its other tasks to another
        // thread meanwhile (the server's runtime; tests run on one thread)
        match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
            Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(|| put_back(renamed)),
            _ => put_back(renamed),
        }
    }
}

pub(super) fn put_back(renamed: Vec<Renamed>) {
    for r in renamed.into_iter().rev() {
        match rename_new(r.now.as_path(), r.was.as_path()) {
            Ok(()) => {
                if let Some(made) = r.made {
                    let _ = std::fs::remove_dir(made.as_path());
                }
                if let Some(entry) = r.journal {
                    entry.remove();
                }
            }
            Err(e) => tracing::error!("Couldn't put {:?} back after a failed change: {e}", r.was.name().unwrap_or_default()),
        }
    }
}

/// Locks the folder spaces holding these items (ids, or aliases such as `root`), always in the same order
pub async fn lock(st: &AppState, user: &User, ids: &[&str]) -> AppResult<SpaceLocks> {
    let ids: Vec<&str> = ids.iter().filter_map(|id| tree::resolve_alias(user, id).ok()).collect();
    let drives: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT n.drive_id FROM nodes n JOIN drives d ON d.id = n.drive_id
         WHERE d.mode = 'folder' AND n.id IN (SELECT value FROM json_each(?)) ORDER BY n.drive_id",
    )
    .bind(serde_json::to_string(&ids).unwrap())
    .fetch_all(&st.db)
    .await?;
    let mut held = Vec::with_capacity(drives.len());
    for (d,) in drives {
        let guard = lock_space(st, &d).await;
        held.push((d, guard));
    }
    // Their folders answer, before the change takes the write lock
    for (d, _) in &held {
        ready(st, d).await?;
    }
    Ok(SpaceLocks { st: st.clone(), held, renamed: Default::default(), unfinished: Default::default(), committed: Default::default() })
}

pub async fn lock_space(st: &AppState, drive_id: &str) -> OwnedMutexGuard<()> {
    crate::folders::drive_lock(st, drive_id).lock_owned().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    /// Renames `from` to `to` in the space's folder `root` (making the folder `made` for it first), as a change does,
    /// and notes it in `locks`
    fn rename(locks: &SpaceLocks, root: &Path, from: &str, to: &str, made: Option<&str>) {
        let root = Pinned::root(root).unwrap();
        let made = made.map(|m| {
            std::fs::create_dir(root.as_path().join(m)).unwrap();
            root.join(m).unwrap()
        });
        let (was, now) = (root.join(from).unwrap(), root.join(to).unwrap());
        rename_new(was.as_path(), now.as_path()).unwrap();
        locks.note(now, was, made);
    }

    #[tokio::test]
    async fn renames_of_a_change_that_fails_are_put_back_even_when_the_disk_hangs() {
        let env = testutil::env().await;
        let admin = env.admin().await;
        let space = env.folder_space("Shared").await;
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(space.dir.join(name), name).unwrap();
        }

        // A change that fails: its renames are put back, newest first, and the folders it made removed
        let locks = lock(&env.st, &admin, &[&space.root]).await.unwrap();
        rename(&locks, &space.dir, "a.txt", "trash/a.txt", Some("trash"));
        rename(&locks, &space.dir, "b.txt", "a.txt", None);
        drop(locks);
        assert_eq!(std::fs::read(space.dir.join("a.txt")).unwrap(), b"a.txt");
        assert_eq!(std::fs::read(space.dir.join("b.txt")).unwrap(), b"b.txt");
        assert!(!space.dir.join("trash").exists());

        // A change that was committed keeps them
        let locks = lock(&env.st, &admin, &[&space.root]).await.unwrap();
        rename(&locks, &space.dir, "c.txt", "d.txt", None);
        locks.committed();
        drop(locks);
        assert!(space.dir.join("d.txt").exists() && !space.dir.join("c.txt").exists());

        // The disk stops answering while a change holds the lock: the change fails, and the space counts as stuck
        let locks = lock(&env.st, &admin, &[&space.root]).await.unwrap();
        rename(&locks, &space.dir, "d.txt", "e.txt", None);
        let _short = testing::short_waits();
        let hung = testing::hang(&space.drive, Duration::from_secs(2));
        assert_eq!(ready(&env.st, &space.drive).await.unwrap_err().status, StatusCode::SERVICE_UNAVAILABLE);
        drop(hung);
        assert!(stuck(&space.drive));
        // Its renames are put back on a blocking thread, which the async worker doesn't wait for…
        let started = std::time::Instant::now();
        drop(locks);
        assert!(started.elapsed() < Duration::from_millis(500));
        // …and the space stays locked until they are
        let _turn = tokio::time::timeout(Duration::from_secs(10), lock_space(&env.st, &space.drive)).await.unwrap();
        assert!(space.dir.join("d.txt").exists() && !space.dir.join("e.txt").exists());
    }
}
