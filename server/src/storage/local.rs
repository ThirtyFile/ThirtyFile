//! Local folder locations: a folder on the server, or a disk or network share mounted there

use super::*;

pub struct LocalStorage {
    root: PathBuf,
    /// The location's id, which its folder's marker must hold
    id: String,
    /// How long a step on the folder's disk may take (`on_disk`)
    wait: Duration,
    /// Tests: every step on the disk takes this long first (a disk that doesn't answer)
    #[cfg(test)]
    hang: Option<Duration>,
}

/// How long a step on the disk of a Local folder location may take before it is given up: long enough for a disk to
/// spin up
const DISK_WAIT: Duration = Duration::from_secs(60);

/// The folders of Local folder locations with a step on their disk that was given up and hasn't returned yet
static STUCK: crate::sync::Mutex<Vec<PathBuf>> = crate::sync::Mutex::new(Vec::new());

fn not_answering(root: &Path) -> io::Error {
    io::Error::other(StorageError { message: UNAVAILABLE, detail: format!("{} doesn't answer", root.display()) })
}

impl LocalStorage {
    /// The location `id` in the folder `root`. Nothing on the disk is touched: the folder is checked before every
    /// write (`verify`).
    pub fn new(root: PathBuf, id: &str) -> Self {
        Self {
            root,
            id: id.to_string(),
            wait: DISK_WAIT,
            #[cfg(test)]
            hang: None,
        }
    }

    /// Claims `root` for the location `id` (`claim_folder`) and uses it (tests: a location added in the database)
    #[cfg(test)]
    pub fn create(root: PathBuf, id: &str) -> io::Result<Self> {
        claim_folder(&root, id)?;
        Ok(Self::new(root, id))
    }

    /// Runs a step on the folder's disk on a blocking thread, giving it `wait` to answer: a disk or network share that
    /// stops answering (its server went away) never holds up the server's async worker, of which a server with one
    /// processor has one. Until a step that was given up returns, the next ones fail at once instead of piling up
    /// threads that wait for it too.
    async fn on_disk<T: Send + 'static>(&self, step: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<T> {
        if STUCK.lock().contains(&self.root) {
            return Err(not_answering(&self.root));
        }
        #[cfg(test)]
        let hang = self.hang;
        let mut task = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(d) = hang {
                std::thread::sleep(d);
            }
            step()
        });
        match tokio::time::timeout(self.wait, &mut task).await {
            Ok(done) => done.map_err(io::Error::other)?,
            Err(_) => {
                tracing::warn!("The folder {} of a storage location didn't answer within {} s", self.root.display(), self.wait.as_secs());
                STUCK.lock().push(self.root.clone());
                // Let go once the step returns at last
                let root = self.root.clone();
                tokio::spawn(async move {
                    let _ = task.await;
                    STUCK.lock().retain(|r| *r != root);
                    tracing::info!("The folder {} of a storage location answers again", root.display());
                });
                Err(not_answering(&self.root))
            }
        }
    }

    /// The folder is there and holds this location's marker: one small read, so health checks can run it often
    async fn verify(&self) -> io::Result<()> {
        let (root, id) = (self.root.clone(), self.id.clone());
        self.on_disk(move || {
            let marker = match std::fs::read(root.join(LOCATION_MARKER)) {
                Ok(b) => Some(marker_location(&b)),
                Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => None,
                Err(e) => return Err(e),
            };
            match marker {
                Some(m) if m == id => Ok(()),
                Some(_) => {
                    Err(io::Error::other(StorageError { message: NOT_MOUNTED, detail: format!("{} holds another location's {LOCATION_MARKER}", root.display()) }))
                }
                // A folder with items but no marker: say which file is missing and what it must hold. A missing or
                // empty folder is a disk that isn't mounted.
                None => Err(io::Error::other(match nothing_but_system_entries(&root) {
                    Ok(false) => StorageError { message: MARKER_MISSING, detail: id },
                    _ => StorageError { message: NOT_MOUNTED, detail: format!("{} or its {LOCATION_MARKER} isn't there", root.display()) },
                })),
            }
        })
        .await
    }

    fn path(&self, hash: &str) -> io::Result<PathBuf> {
        valid_hash(hash)?;
        Ok(self.root.join(&hash[0..2]).join(&hash[2..4]).join(hash))
    }

    /// `key` in the folder, reached without following a symbolic link on the way (a blocking step on the disk)
    fn pinned(root: &Path, key: &str) -> io::Result<crate::beneath::Pinned> {
        key_parts(key)?;
        crate::beneath::Pinned::root(root)?.join(key)
    }
}

/// Writes, reads back and deletes a small file in `dir`
pub async fn write_probe(dir: &Path) -> io::Result<()> {
    let probe = dir.join(format!(".thirtyfile-check-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::write(&probe, b"ok").await?;
    let back = tokio::fs::read(&probe).await;
    let deleted = tokio::fs::remove_file(&probe).await;
    if back? != b"ok" {
        return Err(io::Error::other("The content read back didn't match"));
    }
    deleted.map_err(|e| delete_failed(e, CANT_DELETE_LOCAL))
}

/// Puts a temp file's content on disk before it takes the content's name: after a power loss, a name never leads to
/// part of it
async fn sync_temp(path: &Path) -> io::Result<()> {
    // Opened for writing: Windows syncs only a file opened so
    tokio::fs::OpenOptions::new().write(true).open(path).await?.sync_all().await?;
    #[cfg(test)]
    crate::fsops::testing::synced(path);
    Ok(())
}

/// Moves a temp file to `dest` (the temp file is gone on success)
async fn move_into(src: &Path, dest: &Path) -> io::Result<()> {
    sync_temp(src).await?;
    if tokio::fs::rename(src, dest).await.is_err() {
        // Copy instead when the temp directory and storage are on different volumes. The copy gets a name of
        // its own, so two uploads of the same content can't write into one file, and is removed if it fails
        let tmp = dest.with_extension(format!("partial-{}", uuid::Uuid::new_v4().simple()));
        let copied = async {
            tokio::fs::copy(src, &tmp).await?;
            sync_temp(&tmp).await?;
            tokio::fs::rename(&tmp, dest).await
        }
        .await;
        if let Err(e) = copied {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
        let _ = tokio::fs::remove_file(src).await;
    }
    Ok(())
}

fn local_entry(name: String, meta: &std::fs::Metadata) -> Entry {
    let t = meta.file_type();
    let kind = if t.is_symlink() {
        EntryKind::Link
    } else if t.is_dir() {
        EntryKind::Folder
    } else {
        EntryKind::File
    };
    let size = if kind == EntryKind::File { meta.len() } else { 0 };
    Entry { name, kind, size, modified: meta.modified().ok().and_then(unix_seconds) }
}

impl Storage for LocalStorage {
    fn put_file<'a>(&'a self, hash: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let dest = self.path(hash)?;
            // Below the verified folder, the folders named by the hash are made as needed
            self.verify().await?;
            // Already stored, unless what is there is shorter or longer (a copy cut short by a power loss, say): then it
            // is replaced. A copy of the right length isn't read again here (that would read every file uploaded twice):
            // one damaged in place is found by `thirtyfile check`, and replaced from a verified copy by restores and
            // replicas (`repair_file`)
            if let Ok(there) = tokio::fs::symlink_metadata(&dest).await
                && there.is_file()
                && there.len() == tokio::fs::metadata(src).await?.len()
            {
                let _ = tokio::fs::remove_file(src).await;
                return Ok(());
            }
            tokio::fs::create_dir_all(dest.parent().unwrap()).await?;
            move_into(src, &dest).await
        })
    }

    fn content_dir(&self) -> &'static str {
        ""
    }

    fn list_dir<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        Box::pin(async move {
            // An empty mount point would look like a location that holds nothing
            self.verify().await?;
            let (root, dir) = (self.root.clone(), dir.to_string());
            let at = self.on_disk(move || Self::pinned(&root, &dir)?.dir()).await?;
            tokio::task::spawn_blocking(move || {
                let mut out = Vec::new();
                for e in std::fs::read_dir(at.as_path())? {
                    let e = e?;
                    // The item itself, not what a link points to
                    let Ok(meta) = e.metadata() else { continue };
                    out.push(local_entry(e.file_name().to_string_lossy().into_owned(), &meta));
                }
                Ok(out)
            })
            .await
            .map_err(io::Error::other)?
        })
    }

    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<Entry>>> {
        Box::pin(async move {
            let (root, key) = (self.root.clone(), key.to_string());
            let found = self
                .on_disk(move || {
                    let p = Self::pinned(&root, &key)?;
                    let meta = std::fs::symlink_metadata(p.as_path())?;
                    Ok(local_entry(p.name().unwrap_or_default().to_string(), &meta))
                })
                .await;
            match found {
                Ok(e) => Ok(Some(e)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    fn put_at<'a>(&'a self, key: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let parts: Vec<String> = key_parts(key)?.into_iter().map(String::from).collect();
            let (last, dirs) = parts.split_last().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
            self.verify().await?;
            let (root, last, dirs) = (self.root.clone(), last.clone(), dirs.to_vec());
            let dest = self
                .on_disk(move || {
                    // Folders on the way are made one by one, never through a link
                    let mut at = crate::beneath::Pinned::root(&root)?;
                    for d in &dirs {
                        let next = at.join(d)?;
                        match std::fs::symlink_metadata(next.as_path()) {
                            Ok(m) if m.is_dir() => {}
                            Ok(_) => return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{d} isn't a folder"))),
                            Err(e) if e.kind() == io::ErrorKind::NotFound => match std::fs::create_dir(next.as_path()) {
                                Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                                _ => {}
                            },
                            Err(e) => return Err(e),
                        }
                        at = next.dir()?;
                    }
                    at.join(&last)
                })
                .await?;
            move_into(src, dest.as_path()).await
        })
    }

    fn open_at<'a>(&'a self, key: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move {
            let (root, key) = (self.root.clone(), key.to_string());
            let mut f = tokio::fs::File::from_std(self.on_disk(move || Self::pinned(&root, &key)?.open_file()).await?);
            if start > 0 {
                f.seek(SeekFrom::Start(start)).await?;
            }
            Ok(Box::pin(f.take(len)) as BoxReader)
        })
    }

    fn delete_at<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            // A file that isn't there counts as deleted: only true when the folder is really there
            self.verify().await?;
            let (root, key) = (self.root.clone(), key.to_string());
            // A link is removed itself, never what it points to
            let removed = self.on_disk(move || std::fs::remove_file(Self::pinned(&root, &key)?.as_path())).await;
            match removed {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        })
    }

    fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move {
            let mut f = tokio::fs::File::open(self.path(hash)?).await?;
            if start > 0 {
                f.seek(SeekFrom::Start(start)).await?;
            }
            Ok(Box::pin(f.take(len)) as BoxReader)
        })
    }

    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let path = self.path(hash)?;
            // Content that isn't there counts as deleted: only true when the folder is really there
            self.verify().await?;
            match tokio::fs::remove_file(path).await {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        })
    }

    fn size<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<Option<u64>>> {
        Box::pin(async move {
            match tokio::fs::metadata(self.path(hash)?).await {
                Ok(m) => Ok(Some(m.len())),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    fn list(&self) -> BoxFuture<'_, io::Result<Vec<String>>> {
        let root = self.root.clone();
        Box::pin(async move {
            self.verify().await?;
            // ab/cd/<hash>; anything else in the folder isn't content
            tokio::task::spawn_blocking(move || {
                let mut out = Vec::new();
                for a in std::fs::read_dir(&root)?.flatten().filter(|e| e.file_name().len() == 2) {
                    for b in std::fs::read_dir(a.path())?.flatten().filter(|e| e.file_name().len() == 2) {
                        for f in std::fs::read_dir(b.path())?.flatten() {
                            let name = f.file_name().to_string_lossy().into_owned();
                            if is_hash(&name) && f.file_type().is_ok_and(|t| t.is_file()) {
                                out.push(name);
                            }
                        }
                    }
                }
                Ok(out)
            })
            .await
            .map_err(io::Error::other)?
        })
    }

    fn ping(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(self.verify())
    }

    fn check(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            self.verify().await?;
            write_probe(&self.root).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_local_folder_is_checked_before_anything_is_written() {
        let base = std::env::temp_dir().join(format!("thirtyfile-local-{}", crate::util::new_id()));
        let root = base.join("nas");
        let s = LocalStorage::new(root.clone(), "nas");
        let tmp = base.join("tmp");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&tmp, b"x").unwrap();
        let hash = crate::util::sha256_hex(b"x");
        let not_mounted = |e: io::Error| e.get_ref().and_then(|i| i.downcast_ref::<StorageError>()).map(|se| se.message);
        // Missing, empty or someone else's: every use that writes, deletes or lists is refused, and nothing is made
        for what in ["missing", "empty", "another"] {
            match what {
                "empty" => std::fs::create_dir(&root).unwrap(),
                "another" => std::fs::write(root.join(LOCATION_MARKER), "another").unwrap(),
                _ => {}
            }
            assert_eq!(not_mounted(s.ping().await.unwrap_err()), Some(NOT_MOUNTED), "{what}");
            assert_eq!(not_mounted(s.check().await.unwrap_err()), Some(NOT_MOUNTED), "{what}");
            assert_eq!(not_mounted(s.put_file(&hash, &tmp).await.unwrap_err()), Some(NOT_MOUNTED), "{what}");
            assert_eq!(not_mounted(s.put_at(".thirtyfile-check/a", &tmp).await.unwrap_err()), Some(NOT_MOUNTED), "{what}");
            assert_eq!(not_mounted(s.delete(&hash).await.unwrap_err()), Some(NOT_MOUNTED), "{what}");
            assert_eq!(not_mounted(s.delete_at("a").await.unwrap_err()), Some(NOT_MOUNTED), "{what}");
            assert_eq!(not_mounted(s.list_dir("").await.unwrap_err()), Some(NOT_MOUNTED), "{what}");
            assert_eq!(not_mounted(s.list().await.unwrap_err()), Some(NOT_MOUNTED), "{what}");
            let made = std::fs::read_dir(&root).map(|d| d.count()).unwrap_or(0);
            assert_eq!(made, usize::from(what == "another"), "{what}");
            assert!(tmp.is_file(), "the temp file is kept");
        }
        std::fs::remove_dir_all(&root).unwrap();
        // Its own folder: the content's folders are made below it
        claim_folder(&root, "nas").unwrap();
        s.ping().await.unwrap();
        s.check().await.unwrap();
        s.put_file(&hash, &tmp).await.unwrap();
        assert!(root.join(&hash[0..2]).join(&hash[2..4]).join(&hash).is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn a_folder_whose_disk_stops_answering_is_given_up_without_holding_up_the_server() {
        let base = std::env::temp_dir().join(format!("thirtyfile-hung-{}", crate::util::new_id()));
        let root = base.join("nas");
        claim_folder(&root, "nas").unwrap();
        let mut s = LocalStorage::new(root.clone(), "nas");
        s.wait = Duration::from_millis(300);
        s.hang = Some(Duration::from_secs(2));
        let unavailable = |e: io::Error| e.get_ref().and_then(|i| i.downcast_ref::<StorageError>()).map(|se| se.message);
        // The async worker (the only one here) goes on meanwhile, and the step is given up after `wait`
        let started = std::time::Instant::now();
        let (checked, ticked) = tokio::join!(s.ping(), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            started.elapsed()
        });
        assert_eq!(unavailable(checked.unwrap_err()), Some(UNAVAILABLE));
        assert!(ticked < Duration::from_millis(250), "{ticked:?}");
        assert!(started.elapsed() < Duration::from_millis(1500));
        // Until the step returns, the next ones fail at once
        let started = std::time::Instant::now();
        assert_eq!(unavailable(s.stat("a.txt").await.unwrap_err()), Some(UNAVAILABLE));
        assert_eq!(unavailable(s.open_at("a.txt", 0, 1).await.err().unwrap()), Some(UNAVAILABLE));
        assert!(started.elapsed() < Duration::from_millis(200));
        // Then the folder is used again
        s.hang = None;
        let mut answered = false;
        for _ in 0..50 {
            if s.ping().await.is_ok() {
                answered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(answered);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn a_copy_cut_short_in_the_store_is_replaced_by_the_whole_content() {
        let base = std::env::temp_dir().join(format!("thirtyfile-short-{}", crate::util::new_id()));
        let root = base.join("nas");
        claim_folder(&root, "nas").unwrap();
        let s = LocalStorage::new(root.clone(), "nas");
        let content = b"the whole content of the file";
        let hash = crate::util::sha256_hex(content);
        // Left by a copy that a power loss cut short
        let stored = root.join(&hash[0..2]).join(&hash[2..4]).join(&hash);
        std::fs::create_dir_all(stored.parent().unwrap()).unwrap();
        std::fs::write(&stored, &content[..7]).unwrap();
        let tmp = base.join("tmp");
        std::fs::write(&tmp, content).unwrap();
        s.put_file(&hash, &tmp).await.unwrap();
        assert_eq!(std::fs::read(&stored).unwrap(), content);
        assert!(!tmp.exists());
        // The whole content already there: kept, and the temp file goes
        std::fs::write(&tmp, content).unwrap();
        s.put_file(&hash, &tmp).await.unwrap();
        assert_eq!(std::fs::read(&stored).unwrap(), content);
        assert!(!tmp.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn a_folder_with_items_but_no_marker_says_what_the_marker_must_hold() {
        let base = std::env::temp_dir().join(format!("thirtyfile-nomarker-{}", crate::util::new_id()));
        let root = base.join("nas");
        std::fs::create_dir_all(root.join("teams/Sales")).unwrap();
        let s = LocalStorage::new(root.clone(), "loc-42");
        let err = s.ping().await.unwrap_err();
        let se = err.get_ref().and_then(|i| i.downcast_ref::<StorageError>()).unwrap();
        assert_eq!(se.message, MARKER_MISSING);
        assert_eq!(se.text(), format!("{MARKER_MISSING} loc-42"));
        assert_eq!(crate::error::AppError::from(err).message, format!("{MARKER_MISSING} loc-42"));
        // Written as it says, the folder is used again
        std::fs::write(
            root.join(LOCATION_MARKER),
            "loc-42
",
        )
        .unwrap();
        s.ping().await.unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn stored_content_is_on_disk_before_it_takes_its_name() {
        let base = std::env::temp_dir().join(format!("thirtyfile-local-{}", crate::util::new_id()));
        let root = base.join("nas");
        claim_folder(&root, "nas").unwrap();
        let s = LocalStorage::new(root.clone(), "nas");
        let tmp = base.join("tmp");
        std::fs::write(&tmp, b"x").unwrap();
        let hash = crate::util::sha256_hex(b"x");
        s.put_file(&hash, &tmp).await.unwrap();
        assert!(root.join(&hash[0..2]).join(&hash[2..4]).join(&hash).is_file());
        assert!(crate::fsops::testing::was_synced(&tmp));
        let _ = std::fs::remove_dir_all(&base);
    }
}
