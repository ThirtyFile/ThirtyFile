//! Physical file storage abstraction. Files are content-addressed by sha256; nodes only reference the hash.
//! Supports local folders, S3-compatible object storage (AWS S3, Cloudflare R2, MinIO…), SFTP and FTP / FTPS,
//! added by administrators under "System settings › Storage locations"; each space can choose where its new files are stored.

use std::{
    io::{self, SeekFrom},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use futures_util::{StreamExt, TryStreamExt, future::BoxFuture};
use object_store::{
    BackoffConfig, ClientOptions, GetOptions, ObjectStore, ObjectStoreExt, PutPayload, RetryConfig, WriteMultipart, aws::AmazonS3Builder,
    path::Path as ObjectPath,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt};
use tokio_util::io::StreamReader;

pub type BoxReader = Pin<Box<dyn AsyncRead + Send>>;

pub trait Storage: Send + Sync {
    /// Moves a temp file into storage (the temp file is removed on success). If the same hash already exists, the temp file is simply deleted.
    fn put_file<'a>(&'a self, hash: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>>;
    /// Reads the content in [start, start + len).
    fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>>;
    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<()>>;
    /// Connection test: write, read back and delete a small file
    fn check(&self) -> BoxFuture<'_, io::Result<()>>;
    /// Lightweight connection check (for periodic health monitoring; writes no data)
    fn ping(&self) -> BoxFuture<'_, io::Result<()>> {
        self.check()
    }
    /// SFTP: the server's host key fingerprint from the most recent connection (recorded when a location is added and compared afterwards)
    fn host_key(&self) -> Option<String> {
        None
    }
    /// Size of stored content, None when it isn't there (`thirtyfile check`)
    fn size<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<Option<u64>>> {
        let _ = hash;
        Box::pin(async { Err(io::Error::new(io::ErrorKind::Unsupported, "this storage can't be checked")) })
    }
    /// Hashes of all stored content (`thirtyfile check`); other files in the storage are left out
    fn list(&self) -> BoxFuture<'_, io::Result<Vec<String>>> {
        Box::pin(async { Err(io::Error::new(io::ErrorKind::Unsupported, "this storage can't be listed")) })
    }

    // Items by their path in the location (its folder, or its prefix in a bucket): the step-by-step test, browsing
    // and finding unused content (location_tools.rs). Paths are checked with `key_parts`.

    /// Where content is kept, relative to the location's folder or prefix: `ab/cd/<hash>` below it
    fn content_dir(&self) -> &'static str {
        "blobs"
    }
    /// One level of the folder `dir` ("" for the top), in no particular order. A symbolic link is listed as a link and
    /// never followed.
    fn list_dir<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        let _ = dir;
        unsupported()
    }
    /// The item at `key`, None when there is none
    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<Entry>>> {
        let _ = key;
        unsupported()
    }
    /// Stores a temp file at `key` the way content is stored (on S3, larger files as a multipart upload); the temp
    /// file is removed on success
    fn put_at<'a>(&'a self, key: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        let _ = (key, src);
        unsupported()
    }
    /// Reads [start, start + len) of the file at `key`, refusing a symbolic link
    fn open_at<'a>(&'a self, key: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        let _ = (key, start, len);
        unsupported()
    }
    /// Deletes the file at `key`; a file that isn't there counts as deleted
    fn delete_at<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<()>> {
        let _ = key;
        unsupported()
    }
    /// All stored content, with sizes and times (named by the hash); `seen` hears how many were found so far
    fn list_content<'a>(&'a self, seen: &'a (dyn Fn(u64) + Send + Sync)) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        Box::pin(async move {
            // Walked folder by folder: ab/cd/<hash>, the folders named by the hash's first characters
            let base = self.content_dir();
            let level = |e: &Entry| e.kind == EntryKind::Folder && e.name.len() == 2 && e.name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            let mut out = Vec::new();
            for a in missing_is_empty(self.list_dir(base).await)?.into_iter().filter(level) {
                for b in missing_is_empty(self.list_dir(&join_key(&[base, &a.name])).await)?.into_iter().filter(level) {
                    let dir = join_key(&[base, &a.name, &b.name]);
                    for f in missing_is_empty(self.list_dir(&dir).await)? {
                        if f.kind == EntryKind::File && content_hash(&join_key(&[&dir, &f.name]), base).is_some() {
                            out.push(f);
                        }
                    }
                    seen(out.len() as u64);
                }
            }
            Ok(out)
        })
    }
}

fn unsupported<T>() -> BoxFuture<'static, io::Result<T>> {
    Box::pin(async { Err(io::Error::new(io::ErrorKind::Unsupported, "this storage can't do this")) })
}

/// A folder that isn't there has nothing in it
fn missing_is_empty(r: io::Result<Vec<Entry>>) -> io::Result<Vec<Entry>> {
    match r {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        r => r,
    }
}

/// What an item in a location is
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Folder,
    File,
    /// A symbolic link: listed, never followed
    Link,
}

/// An item in a location's folder
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// Bytes (files only)
    pub size: u64,
    /// Unix seconds, when the storage tells
    pub modified: Option<i64>,
}

impl Entry {
    fn file(name: impl Into<String>, size: u64, modified: Option<i64>) -> Entry {
        Entry { name: name.into(), kind: EntryKind::File, size, modified }
    }
}

/// The parts of a path in a location ("" is the top): '/' between them, nothing that could step out of it
pub fn key_parts(key: &str) -> io::Result<Vec<&str>> {
    let parts = crate::beneath::parts(key)?;
    if parts.iter().any(|p| p.contains('\\')) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a path inside the location: {key}")));
    }
    Ok(parts)
}

/// Parts joined into a path, leaving out empty ones
pub fn join_key(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("/")
}

/// The hash when `key` is where content is stored (`<content_dir>/ab/cd/<hash>`, as the stores write it)
pub fn content_hash<'a>(key: &'a str, content_dir: &str) -> Option<&'a str> {
    let rest = if content_dir.is_empty() { key } else { key.strip_prefix(content_dir)?.strip_prefix('/')? };
    let mut parts = rest.split('/');
    let (a, b, hash) = (parts.next()?, parts.next()?, parts.next()?);
    let lower_hex = hash.len() == 64 && hash.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    (parts.next().is_none() && lower_hex && a == &hash[0..2] && b == &hash[2..4]).then_some(hash)
}

fn unix_seconds(t: std::time::SystemTime) -> Option<i64> {
    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

/// Shown when a file could be written but not deleted: such keys or accounts pass a write test and fail later
pub const CANT_DELETE_S3: &str = "The keys can't delete files. Allow them to delete objects in this bucket.";
pub const CANT_DELETE_ACCOUNT: &str = "The account can't delete files in this folder.";
pub const CANT_DELETE_LOCAL: &str = "ThirtyFile can't delete files in this folder.";

/// The error of a delete that failed, as a storage error with `message` (kept as it is when the storage service
/// can't be reached at all)
pub fn delete_failed(e: io::Error, message: &'static str) -> io::Error {
    let unreachable = e.get_ref().and_then(|i| i.downcast_ref::<StorageError>()).is_some_and(|se| se.message == UNAVAILABLE);
    if unreachable {
        return e;
    }
    io::Error::other(StorageError { message, detail: e.to_string() })
}

/// Whether a file name found in storage is stored content (a sha256 in hex)
pub fn is_hash(name: &str) -> bool {
    valid_hash(name).is_ok()
}

pub fn valid_hash(hash: &str) -> io::Result<()> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid blob hash"));
    }
    Ok(())
}

/// The folders to create, top down, for a remote folder path: "/a/b" → "/a", "/a/b"; "a/./b" → "a", "a/./b"
pub fn dir_levels(dir: &str) -> Vec<String> {
    let mut levels = Vec::new();
    let mut path = String::new();
    for part in dir.split('/') {
        if part.is_empty() {
            path.push('/');
            continue;
        }
        if !path.is_empty() && !path.ends_with('/') {
            path.push('/');
        }
        path.push_str(part);
        if part != "." {
            levels.push(path.clone());
        }
    }
    levels
}

// ───────────── Local folder ─────────────

/// The file in a Local folder location's folder that says which location the folder is: it holds the location's id
/// (on its first line; S3, SFTP and FTP locations have one too, with the installation's id on a second line,
/// `locations::claim_place`). It is written only when an administrator adds the location or changes its folder (the
/// built-in location's at the first start of a new server, `prepare_builtin`). A location whose folder doesn't hold its marker is unavailable, and nothing is
/// created or written there: a disk or network share that isn't mounted leaves an empty folder (or none) at its
/// mount point, and files written there would land on the server's own disk and be hidden once it is mounted again.
pub const LOCATION_MARKER: &str = ".thirtyfile-location";
/// Shown when a Local folder location's folder isn't there, or doesn't hold the location's marker
pub const NOT_MOUNTED: &str = "The folder isn't there, or a different disk is mounted there";
/// Shown when a Local folder location's folder is there and has items, but no marker: it may be the right folder whose
/// marker was lost (a copy that left out hidden files, say). Followed by the location's id, which the file must hold.
pub const MARKER_MISSING: &str = "The folder has items but no .thirtyfile-location file. If it is this location's folder, create that file in it holding this line:";
/// Shown when an administrator chooses a folder whose marker names another location
pub const FOLDER_TAKEN: &str = "Another storage location uses this folder (it holds that location's .thirtyfile-location file)";
/// Shown when an S3, SFTP or FTP location's marker names another location, or another installation of ThirtyFile
pub const PLACE_TAKEN: &str =
    "Another storage location uses this place: its .thirtyfile-location file names another location or another ThirtyFile installation";
/// Items a disk or a NAS puts in a folder by itself: they don't make a storage folder "used"
pub const SYSTEM_ENTRIES: [&str; 6] = ["lost+found", "#recycle", "@eaDir", ".DS_Store", "System Volume Information", "$RECYCLE.BIN"];

/// Whether the folder `dir` has nothing in it but `SYSTEM_ENTRIES`
pub fn nothing_but_system_entries(dir: &Path) -> io::Result<bool> {
    for e in std::fs::read_dir(dir)? {
        let name = e?.file_name();
        let name = name.to_string_lossy();
        if !SYSTEM_ENTRIES.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The location a marker names: its first line. A second line, when there is one, names the ThirtyFile installation
/// (`locations::install_id`), which the markers of S3, SFTP and FTP locations hold.
pub fn marker_location(body: &[u8]) -> String {
    String::from_utf8_lossy(body).lines().next().unwrap_or_default().trim().to_string()
}

/// The location id in the marker of the folder `root`; None when the folder or the marker isn't there
pub async fn marker_of(root: &Path) -> io::Result<Option<String>> {
    match tokio::fs::read(root.join(LOCATION_MARKER)).await {
        Ok(b) => Ok(Some(marker_location(&b))),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(None),
        Err(e) => Err(e),
    }
}

/// An administrator added the location `id` with the folder `root`, or changed its folder to it: the folder is
/// created when it isn't there, and gets the location's marker. A folder whose marker names another location is
/// refused (`FOLDER_TAKEN`); one that already holds this location's (added again) is taken as it is. Returns
/// whether the marker was written now.
pub fn claim_folder(root: &Path, id: &str) -> io::Result<bool> {
    std::fs::create_dir_all(root)?;
    let marker = root.join(LOCATION_MARKER);
    match std::fs::read(&marker) {
        Ok(b) if marker_location(&b) == id => Ok(false),
        Ok(_) => Err(io::Error::other(StorageError { message: FOLDER_TAKEN, detail: root.display().to_string() })),
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::write(&marker, id).map(|()| true),
        Err(e) => Err(e),
    }
}

/// Removes the marker of the location `id` from its folder, when it holds that one (the location is deleted, or
/// adding it failed): the folder can be used for a location again
pub async fn release_folder(root: &Path, id: &str) {
    if marker_of(root).await.ok().flatten().is_some_and(|m| m == id) {
        let _ = tokio::fs::remove_file(root.join(LOCATION_MARKER)).await;
    }
}

/// The built-in location's folder (the storage folder) at startup, once the database is open. `recorded`: the database
/// records content or spaces on the built-in location.
///
/// A new install creates the folder, or finds it empty (items a disk or NAS makes by itself, `SYSTEM_ENTRIES`, don't
/// count): it gets the marker. When the database records something there, a missing or empty folder is never taken:
/// a disk or Docker volume that isn't mounted leaves exactly that, and uploads would land on the wrong disk. A folder
/// with items in it must already hold the built-in location's marker. Otherwise ThirtyFile doesn't start.
pub fn prepare_builtin(root: &Path, recorded: bool) -> Result<(), String> {
    let builtin = crate::locations::BUILTIN;
    let failed = |e: io::Error| format!("Can't use the storage folder {}: {e}", root.display());
    let unmounted = || {
        format!(
            "The storage folder {} is missing or empty, but the database records files and spaces stored there. The disk or volume that holds them may not be mounted. Check THIRTYFILE_STORAGE and the disks and volumes mounted there, then start ThirtyFile again.",
            root.display()
        )
    };
    let marker = root.join(LOCATION_MARKER);
    match std::fs::read(&marker) {
        Ok(b) if marker_location(&b) == builtin => Ok(()),
        Ok(_) => Err(format!(
            "The storage folder {} belongs to another storage location (its {LOCATION_MARKER} file names another one). Check THIRTYFILE_STORAGE and the disks mounted there.",
            root.display()
        )),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {
            let empty = match nothing_but_system_entries(root) {
                Ok(empty) => empty,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if recorded {
                        return Err(unmounted());
                    }
                    std::fs::create_dir_all(root).map_err(failed)?;
                    true
                }
                Err(e) => return Err(failed(e)),
            };
            if !empty {
                return Err(format!(
                    "The storage folder {} has items in it but no {LOCATION_MARKER} file: it may be a different disk than the one ThirtyFile used. Check THIRTYFILE_STORAGE and the disks mounted there. If it is the right folder, create the file {} holding the word {builtin}.",
                    root.display(),
                    marker.display()
                ));
            }
            if recorded {
                return Err(unmounted());
            }
            std::fs::write(&marker, builtin).map_err(failed)
        }
        Err(e) => Err(failed(e)),
    }
}

pub struct LocalStorage {
    root: PathBuf,
    /// The location's id, which its folder's marker must hold
    id: String,
}

impl LocalStorage {
    /// The location `id` in the folder `root`. Nothing on the disk is touched: the folder is checked before every
    /// write (`verify`).
    pub fn new(root: PathBuf, id: &str) -> Self {
        Self { root, id: id.to_string() }
    }

    /// Claims `root` for the location `id` (`claim_folder`) and uses it (tests: a location added in the database)
    #[cfg(test)]
    pub fn create(root: PathBuf, id: &str) -> io::Result<Self> {
        claim_folder(&root, id)?;
        Ok(Self::new(root, id))
    }

    /// The folder is there and holds this location's marker: one small read, so health checks can run it often
    async fn verify(&self) -> io::Result<()> {
        match marker_of(&self.root).await? {
            Some(id) if id == self.id => Ok(()),
            Some(_) => Err(io::Error::other(StorageError {
                message: NOT_MOUNTED,
                detail: format!("{} holds another location's {LOCATION_MARKER}", self.root.display()),
            })),
            None => {
                // A folder with items but no marker: say which file is missing and what it must hold. A missing or
                // empty folder is a disk that isn't mounted.
                let root = self.root.clone();
                let has_items = tokio::task::spawn_blocking(move || nothing_but_system_entries(&root).map(|empty| !empty)).await.map_err(io::Error::other)?;
                Err(io::Error::other(match has_items {
                    Ok(true) => StorageError { message: MARKER_MISSING, detail: self.id.clone() },
                    _ => StorageError { message: NOT_MOUNTED, detail: format!("{} or its {LOCATION_MARKER} isn't there", self.root.display()) },
                }))
            }
        }
    }

    fn path(&self, hash: &str) -> io::Result<PathBuf> {
        valid_hash(hash)?;
        Ok(self.root.join(&hash[0..2]).join(&hash[2..4]).join(hash))
    }

    /// `key` in the folder, reached without following a symbolic link on the way
    fn pinned(&self, key: &str) -> io::Result<crate::beneath::Pinned> {
        key_parts(key)?;
        crate::beneath::Pinned::root(&self.root)?.join(key)
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

/// Moves a temp file to `dest` (the temp file is gone on success)
async fn move_into(src: &Path, dest: &Path) -> io::Result<()> {
    if tokio::fs::rename(src, dest).await.is_err() {
        // Copy instead when the temp directory and storage are on different volumes. The copy gets a name of
        // its own, so two uploads of the same content can't write into one file, and is removed if it fails
        let tmp = dest.with_extension(format!("partial-{}", uuid::Uuid::new_v4().simple()));
        let copied = async {
            tokio::fs::copy(src, &tmp).await?;
            // On the disk before it takes the content's name: after a power loss, a name never leads to part of it
            tokio::fs::File::open(&tmp).await?.sync_all().await?;
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
            // is replaced
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
            let at = self.pinned(dir)?.dir()?;
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
            let found = self.pinned(key).and_then(|p| {
                let meta = std::fs::symlink_metadata(p.as_path())?;
                Ok(local_entry(p.name().unwrap_or_default().to_string(), &meta))
            });
            match found {
                Ok(e) => Ok(Some(e)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    fn put_at<'a>(&'a self, key: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let parts = key_parts(key)?;
            let (last, dirs) = parts.split_last().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
            self.verify().await?;
            // Folders on the way are made one by one, never through a link
            let mut at = crate::beneath::Pinned::root(&self.root)?;
            for d in dirs {
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
            let dest = at.join(last)?;
            move_into(src, dest.as_path()).await
        })
    }

    fn open_at<'a>(&'a self, key: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move {
            let mut f = tokio::fs::File::from_std(self.pinned(key)?.open_file()?);
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
            let removed = match self.pinned(key) {
                // A link is removed itself, never what it points to
                Ok(p) => tokio::fs::remove_file(p.as_path()).await,
                Err(e) => Err(e),
            };
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

// ───────────── S3-compatible object storage ─────────────

pub struct S3Storage {
    store: Arc<dyn ObjectStore>,
    prefix: String,
}

/// Connect timeout: report quickly when the service can't be reached at all
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Read timeout: a connection counts as stuck only after receiving no data for this long (the total transfer time isn't limited, so large files and slow downloads aren't interrupted)
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Retry count: when the service doesn't respond, users see an error after about 45 seconds instead of waiting several minutes
const MAX_RETRIES: usize = 2;
/// Deadline for resuming from the break point when the network drops during a download (counted from the start of the download)
const RETRY_WINDOW: Duration = Duration::from_secs(30 * 60);

/// By default object_store limits a whole request (including the downloaded content) to 30 seconds with a 3-minute retry deadline:
/// downloads longer than 30 seconds keep getting interrupted and resumed, and fail after 3 minutes, so large downloads on slow networks always break.
/// Instead, only detect "stuck" connections (read timeout) without limiting the total time.
fn client_options() -> (ClientOptions, RetryConfig) {
    let options = ClientOptions::new().with_connect_timeout(CONNECT_TIMEOUT).with_read_timeout(READ_TIMEOUT).with_timeout_disabled();
    let retry = RetryConfig { backoff: BackoffConfig::default(), max_retries: MAX_RETRIES, retry_timeout: RETRY_WINDOW };
    (options, retry)
}

const MULTIPART_THRESHOLD: u64 = 16 * 1024 * 1024;
const MULTIPART_CHUNK: usize = 8 * 1024 * 1024;

/// S3 / R2 allow at most 10,000 parts: starting at 8 MiB, larger files use larger parts (in whole MiB, all parts the same size)
fn part_size(size: u64) -> usize {
    const MIB: u64 = 1024 * 1024;
    let needed = size.div_ceil(9_000).div_ceil(MIB) * MIB;
    needed.max(MULTIPART_CHUNK as u64) as usize
}

/// Explanation shown to users when the storage service doesn't respond or can't be reached
pub const UNAVAILABLE: &str = "The storage service is temporarily unavailable (no response or the connection failed). Try again later.";
/// Shown when S3 rejects the credentials
pub const DENIED_KEYS: &str = "The storage service denied access. Ask an administrator to check the keys and permissions.";

/// Error from a remote storage service (S3, SFTP, FTP): tells the user it's a storage service problem rather than a generic "A server error occurred"
#[derive(Debug)]
pub struct StorageError {
    pub message: &'static str,
    pub detail: String,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.detail)
    }
}

impl std::error::Error for StorageError {}

impl StorageError {
    /// What people are shown: the message, with the location's id when the message asks for it (`MARKER_MISSING`)
    pub fn text(&self) -> std::borrow::Cow<'static, str> {
        if self.message == MARKER_MISSING { format!("{} {}", self.message, self.detail).into() } else { self.message.into() }
    }
}

fn s3_err(e: object_store::Error) -> io::Error {
    let message = match &e {
        object_store::Error::NotFound { .. } => return io::Error::new(io::ErrorKind::NotFound, e.to_string()),
        object_store::Error::PermissionDenied { .. } | object_store::Error::Unauthenticated { .. } => DENIED_KEYS,
        _ => UNAVAILABLE,
    };
    io::Error::other(StorageError { message, detail: e.to_string() })
}

impl S3Storage {
    pub fn new(cfg: &S3Config) -> io::Result<Self> {
        let bucket = cfg.bucket.trim();
        let endpoint = cfg.endpoint.trim().trim_end_matches('/');
        // AWS's wildcard certificate (*.s3.amazonaws.com) only covers one domain level, so buckets whose names contain "." must use path-style
        let path_style = cfg.path_style || bucket.contains('.');
        let (options, retry) = client_options();
        let mut b = AmazonS3Builder::new()
            .with_client_options(options)
            .with_retry(retry)
            .with_bucket_name(bucket)
            .with_region(if cfg.region.trim().is_empty() { "us-east-1" } else { cfg.region.trim() })
            .with_access_key_id(cfg.access_key_id.trim())
            .with_secret_access_key(cfg.secret_access_key.trim())
            .with_allow_http(cfg.allow_http)
            .with_virtual_hosted_style_request(!path_style)
            // We delete one object at a time; single DELETE is a basic API every S3-compatible service supports (some lack DeleteObjects)
            .with_disable_bulk_delete(true);
        if !endpoint.is_empty() {
            // With virtual-hosted style object_store uses the endpoint as is, so we must prepend the bucket to the host name ourselves
            b = b.with_endpoint(if path_style { endpoint.to_string() } else { virtual_hosted_endpoint(endpoint, bucket)? });
        }
        let store = b.build().map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        Ok(Self { store: Arc::new(store), prefix: cfg.prefix.trim().trim_matches('/').to_string() })
    }

    fn key(&self, name: &str) -> ObjectPath {
        if self.prefix.is_empty() { ObjectPath::from(name) } else { ObjectPath::from(format!("{}/{name}", self.prefix)) }
    }

    fn blob_key(&self, hash: &str) -> io::Result<ObjectPath> {
        valid_hash(hash)?;
        Ok(self.key(&format!("blobs/{}/{}/{hash}", &hash[0..2], &hash[2..4])))
    }

    /// A path in the location as an object key (a checked path; "" is the prefix itself)
    fn key_at(&self, key: &str) -> io::Result<ObjectPath> {
        key_parts(key)?;
        Ok(ObjectPath::from(join_key(&[&self.prefix, key])))
    }

    /// Stores a temp file at `key`: one request up to 16 MiB, a multipart upload above (the temp file is kept)
    async fn put_object(&self, key: &ObjectPath, src: &Path) -> io::Result<()> {
        let size = tokio::fs::metadata(src).await?.len();
        if size <= MULTIPART_THRESHOLD {
            let data = tokio::fs::read(src).await?;
            self.store.put(key, PutPayload::from(data)).await.map_err(s3_err)?;
            return Ok(());
        }
        let upload = self.store.put_multipart(key).await.map_err(s3_err)?;
        let chunk = part_size(size);
        let mut writer = WriteMultipart::new_with_chunk_size(upload, chunk);
        // Every part (except the last) has the same size: R2 requires it and AWS accepts it
        let sent: io::Result<()> = async {
            let mut f = tokio::fs::File::open(src).await?;
            let mut buf = vec![0u8; MULTIPART_CHUNK];
            loop {
                let n = f.read(&mut buf).await?;
                if n == 0 {
                    return Ok(());
                }
                writer.wait_for_capacity(if chunk > 64 << 20 { 2 } else { 4 }).await.map_err(s3_err)?;
                writer.write(&buf[..n]);
            }
        }
        .await;
        if let Err(e) = sent {
            // Abort the multipart upload so unfinished parts don't stay in the bucket and keep incurring charges
            let _ = writer.abort().await;
            return Err(e);
        }
        writer.finish().await.map_err(s3_err)?;
        Ok(())
    }

    async fn open_object(&self, key: &ObjectPath, start: u64, len: u64) -> io::Result<BoxReader> {
        if len == 0 {
            return Ok(Box::pin(tokio::io::empty()) as BoxReader);
        }
        let opts = GetOptions::new().with_range(Some(start..start + len));
        let result = self.store.get_opts(key, opts).await.map_err(s3_err)?;
        let stream = result.into_stream().map_err(s3_err).boxed();
        Ok(Box::pin(StreamReader::new(stream)) as BoxReader)
    }

    async fn delete_object(&self, key: &ObjectPath) -> io::Result<()> {
        match self.store.delete(key).await {
            Err(object_store::Error::NotFound { .. }) | Ok(()) => Ok(()),
            Err(e) => Err(s3_err(e)),
        }
    }
}

fn object_entry(meta: &object_store::ObjectMeta) -> Entry {
    Entry::file(meta.location.filename().unwrap_or_default(), meta.size, Some(meta.last_modified.timestamp()))
}

impl Storage for S3Storage {
    fn put_file<'a>(&'a self, hash: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.put_object(&self.blob_key(hash)?, src).await?;
            let _ = tokio::fs::remove_file(src).await;
            Ok(())
        })
    }

    fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move { self.open_object(&self.blob_key(hash)?, start, len).await })
    }

    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move { self.delete_object(&self.blob_key(hash)?).await })
    }

    fn list_dir<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        Box::pin(async move {
            let prefix = self.key_at(dir)?;
            let listed = self.store.list_with_delimiter(Some(&prefix)).await.map_err(s3_err)?;
            let folders = listed.common_prefixes.iter().filter_map(|p| p.filename()).map(|name| Entry {
                name: name.to_string(),
                kind: EntryKind::Folder,
                size: 0,
                modified: None,
            });
            Ok(folders.chain(listed.objects.iter().map(object_entry)).collect())
        })
    }

    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<Entry>>> {
        Box::pin(async move {
            match self.store.head(&self.key_at(key)?).await {
                Ok(meta) => Ok(Some(object_entry(&meta))),
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(s3_err(e)),
            }
        })
    }

    fn put_at<'a>(&'a self, key: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.put_object(&self.key_at(key)?, src).await?;
            let _ = tokio::fs::remove_file(src).await;
            Ok(())
        })
    }

    fn open_at<'a>(&'a self, key: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move { self.open_object(&self.key_at(key)?, start, len).await })
    }

    fn delete_at<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move { self.delete_object(&self.key_at(key)?).await })
    }

    fn list_content<'a>(&'a self, seen: &'a (dyn Fn(u64) + Send + Sync)) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        Box::pin(async move {
            // One listing of everything below blobs/ (a request per 1000 objects), not one per folder level
            let base = self.key("blobs");
            let mut objects = self.store.list(Some(&base));
            let mut out = Vec::new();
            while let Some(meta) = objects.next().await {
                let meta = meta.map_err(s3_err)?;
                let rel: Vec<_> = meta.location.prefix_match(&base).map(|p| p.map(|p| p.as_ref().to_string()).collect()).unwrap_or_default();
                let key = format!("blobs/{}", rel.join("/"));
                if content_hash(&key, "blobs").is_some() {
                    out.push(object_entry(&meta));
                    if out.len() % 1000 == 0 {
                        seen(out.len() as u64);
                    }
                }
            }
            seen(out.len() as u64);
            Ok(out)
        })
    }

    fn size<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<Option<u64>>> {
        Box::pin(async move {
            match self.store.head(&self.blob_key(hash)?).await {
                Ok(meta) => Ok(Some(meta.size)),
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(s3_err(e)),
            }
        })
    }

    fn list(&self) -> BoxFuture<'_, io::Result<Vec<String>>> {
        Box::pin(async move {
            let prefix = self.key("blobs");
            let mut objects = self.store.list(Some(&prefix));
            let mut out = Vec::new();
            while let Some(meta) = objects.next().await {
                let meta = meta.map_err(s3_err)?;
                if let Some(name) = meta.location.filename().filter(|n| is_hash(n)) {
                    out.push(name.to_string());
                }
            }
            Ok(out)
        })
    }

    fn ping(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            // Look up a nonexistent object: a "not found" response means the service works and the keys are valid (costs just one read request)
            match self.store.head(&self.key(".thirtyfile-check/ping")).await {
                Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
                Err(e) => Err(s3_err(e)),
            }
        })
    }

    fn check(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let key = self.key(&format!(".thirtyfile-check/{}", uuid::Uuid::new_v4().simple()));
            self.store.put(&key, PutPayload::from_static(b"ok")).await.map_err(s3_err)?;
            let back = self.store.get(&key).await.map_err(s3_err)?.bytes().await.map_err(s3_err);
            // Keys that can write but not delete would pass and fail later, when files are deleted
            let deleted = self.delete_object(&key).await;
            if back?.as_ref() != b"ok" {
                return Err(io::Error::other("The content read back didn't match"));
            }
            deleted.map_err(|e| delete_failed(e, CANT_DELETE_S3))
        })
    }
}

/// `https://host[:port]` → `https://{bucket}.host[:port]`
fn virtual_hosted_endpoint(endpoint: &str, bucket: &str) -> io::Result<String> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid endpoint URL. It should be https://hostname");
    let (scheme, rest) = endpoint.split_once("://").ok_or_else(invalid)?;
    let host = rest.split('/').next().filter(|h| !h.is_empty()).ok_or_else(invalid)?;
    if host.starts_with(&format!("{bucket}.")) {
        return Ok(format!("{scheme}://{host}"));
    }
    Ok(format!("{scheme}://{bucket}.{host}"))
}

// ───────────── Settings ─────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct S3Config {
    /// Blank means AWS; enter the endpoint URL for R2, MinIO, etc.
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub bucket: String,
    /// Object key prefix, so several systems can share one bucket
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub access_key_id: String,
    #[serde(default)]
    pub secret_access_key: String,
    /// Path-style URLs (usually needed for MinIO)
    #[serde(default)]
    pub path_style: bool,
    /// Allow http (private networks only)
    #[serde(default)]
    pub allow_http: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalConfig {
    /// Absolute path of the folder (may be a NAS mount point)
    #[serde(default)]
    pub path: String,
}

/// Tidies up S3 settings before saving:
/// - If the endpoint was pasted as `https://host/bucket`, split out the bucket and keep only the host
/// - The Cloudflare R2 region is always `auto`
/// - For AWS (blank endpoint), always look up the bucket's actual region and correct it, avoiding signature region errors (301 / AuthorizationHeaderMalformed)
pub async fn normalize(kind: &str, config: serde_json::Value) -> serde_json::Value {
    if kind == "sftp" || kind == "ftp" {
        return normalize_host(config);
    }
    if kind != "s3" {
        return config;
    }
    let Ok(mut cfg) = serde_json::from_value::<S3Config>(config.clone()) else { return config };
    let endpoint = cfg.endpoint.trim().trim_end_matches('/').to_string();
    if let Some((scheme, rest)) = endpoint.split_once("://") {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let path_bucket = path.split('/').next().unwrap_or_default();
        if cfg.bucket.trim().is_empty() && !path_bucket.is_empty() {
            cfg.bucket = path_bucket.to_string();
        }
        cfg.endpoint = format!("{scheme}://{host}");
        if host.to_ascii_lowercase().ends_with(".r2.cloudflarestorage.com") {
            cfg.region = "auto".into();
        }
    }
    if cfg.endpoint.is_empty() && !cfg.bucket.trim().is_empty() {
        let options = object_store::ClientOptions::new();
        let lookup = object_store::aws::resolve_bucket_region(cfg.bucket.trim(), &options);
        if let Ok(Ok(region)) = tokio::time::timeout(std::time::Duration::from_secs(10), lookup).await {
            cfg.region = region;
        }
    }
    serde_json::to_value(cfg).unwrap_or(config)
}

/// Unencrypted connections (S3 over http, FTP without TLS) or ones that skip certificate verification (FTPS) are only allowed to private addresses:
/// the host name is resolved first and rejected if any result is a public IP
pub async fn check_insecure_target(kind: &str, config: &serde_json::Value) -> Result<(), String> {
    let (host, port, reason) = match kind {
        "s3" => {
            let cfg: S3Config = serde_json::from_value(config.clone()).unwrap_or_default();
            let endpoint = cfg.endpoint.trim();
            // A blank endpoint means AWS (https)
            if endpoint.is_empty() {
                return Ok(());
            }
            let url = reqwest::Url::parse(endpoint).map_err(|_| "Invalid endpoint URL".to_string())?;
            if url.scheme() != "http" {
                return Ok(());
            }
            let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']).to_string();
            (host, url.port_or_known_default().unwrap_or(80), "HTTP (unencrypted)")
        }
        "ftp" => {
            let cfg: crate::ftp::FtpConfig = serde_json::from_value(config.clone()).unwrap_or_default();
            let reason = if !cfg.tls {
                "Unencrypted FTP"
            } else if cfg.tls_insecure {
                "Skipping certificate verification"
            } else {
                return Ok(());
            };
            (cfg.host.trim().to_string(), cfg.port(), reason)
        }
        _ => return Ok(()),
    };
    if host.is_empty() {
        return Err(format!("{reason} requires a host address"));
    }
    // Only checked when settings are saved (guards against administrator mistakes; later DNS changes aren't handled)
    let addrs: Vec<std::net::IpAddr> = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => match tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::lookup_host((host.as_str(), port))).await {
            Ok(Ok(list)) => list.map(|a| a.ip()).collect(),
            // Not allowed when it can't be confirmed to be a private address
            _ => return Err(format!("{reason} is only allowed to private network addresses: can't resolve {host}")),
        },
    };
    match addrs.into_iter().find(|ip| !is_private_ip(ip)) {
        Some(ip) => Err(format!("{reason} is only allowed to private network addresses: {host} is a public address ({ip})")),
        None => Ok(()),
    }
}

/// Private ranges, loopback, link-local, and 100.64.0.0/10 (carrier-grade NAT, also commonly used by VPNs such as Tailscale)
pub fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(&std::net::IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// SFTP / FTP: when the host field is pasted as `sftp://host:2222/data`, split out the port and folder
pub(crate) fn normalize_host(mut config: serde_json::Value) -> serde_json::Value {
    let Some(obj) = config.as_object_mut() else { return config };
    let raw = obj.get("host").and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
    let rest = raw.split_once("://").map(|(_, r)| r).unwrap_or(&raw);
    let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, String::new()));
    // Also split out the username when it precedes the host (user@host)
    let hostport = match hostport.rsplit_once('@') {
        Some((user, h)) => {
            if obj.get("username").and_then(|v| v.as_str()).is_none_or(str::is_empty) {
                obj.insert("username".into(), user.into());
            }
            h
        }
        None => hostport,
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') || h.starts_with('[') => match p.parse::<u16>() {
            Ok(port) => (h.trim_matches(['[', ']']).to_string(), Some(port)),
            Err(_) => (hostport.to_string(), None),
        },
        _ => (hostport.to_string(), None),
    };
    obj.insert("host".into(), host.into());
    if let Some(port) = port {
        obj.insert("port".into(), port.into());
    }
    if !path.is_empty() && obj.get("path").and_then(|v| v.as_str()).is_none_or(|p| p.trim().is_empty()) {
        obj.insert("path".into(), path.trim_end_matches('/').to_string().into());
    }
    config
}

/// The folder of a Local folder location (`id`): the built-in location's is always `default_root`
pub fn local_root(id: &str, config: &serde_json::Value, default_root: &Path) -> io::Result<PathBuf> {
    if id == crate::locations::BUILTIN {
        return Ok(default_root.to_path_buf());
    }
    let cfg: LocalConfig = serde_json::from_value(config.clone()).unwrap_or_default();
    let p = PathBuf::from(cfg.path.trim());
    if !p.is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Enter an absolute path"));
    }
    Ok(p)
}

/// Builds the backend of the storage location `id` from its settings. Nothing is created or written: a Local folder
/// location's folder is checked on use (`LocalStorage::verify`).
pub fn build(id: &str, kind: &str, config: &serde_json::Value, default_root: &Path) -> io::Result<Arc<dyn Storage>> {
    match kind {
        "local" => Ok(Arc::new(LocalStorage::new(local_root(id, config, default_root)?, id))),
        "s3" => {
            let cfg: S3Config = serde_json::from_value(config.clone()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            if cfg.bucket.trim().is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "Enter a bucket name"));
            }
            Ok(Arc::new(S3Storage::new(&cfg)?))
        }
        "sftp" => {
            let cfg: crate::sftp::SftpConfig =
                serde_json::from_value(config.clone()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            Ok(Arc::new(crate::sftp::SftpStorage::new(&cfg)?))
        }
        "ftp" => {
            let cfg: crate::ftp::FtpConfig =
                serde_json::from_value(config.clone()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            Ok(Arc::new(crate::ftp::FtpStorage::new(&cfg)?))
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "Unsupported storage type")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn remote_folders_are_created_level_by_level() {
        assert_eq!(super::dir_levels("/srv/blobs/ab"), ["/srv", "/srv/blobs", "/srv/blobs/ab"]);
        assert_eq!(super::dir_levels("./blobs/ab"), ["./blobs", "./blobs/ab"]);
        assert!(super::dir_levels("").is_empty());
    }

    use super::*;

    #[tokio::test]
    async fn insecure_connections_only_to_private_networks() {
        use serde_json::json;
        let ok = |kind: &'static str, cfg: serde_json::Value| async move { check_insecure_target(kind, &cfg).await };
        assert!(ok("s3", json!({ "endpoint": "http://127.0.0.1:9100", "allow_http": true })).await.is_ok());
        assert!(ok("s3", json!({ "endpoint": "http://192.168.1.20:9000/bucket", "allow_http": true })).await.is_ok());
        assert!(ok("s3", json!({ "endpoint": "http://[fd00::5]:9000", "allow_http": true })).await.is_ok());
        assert!(ok("s3", json!({ "endpoint": "http://8.8.8.8:9000", "allow_http": true })).await.is_err());
        assert!(ok("s3", json!({ "endpoint": "HTTP://8.8.8.8:9000", "allow_http": true })).await.is_err(), "case-insensitive");
        assert!(ok("s3", json!({ "endpoint": "http://user@8.8.8.8:9000", "allow_http": true })).await.is_err(), "credentials in the URL");
        assert!(ok("s3", json!({ "endpoint": "http://no-such-host.invalid:9000", "allow_http": true })).await.is_err(), "rejected when unresolvable");
        assert!(ok("s3", json!({ "endpoint": "https://8.8.8.8", "allow_http": true })).await.is_ok(), "https isn't restricted");
        assert!(ok("ftp", json!({ "host": "10.0.0.8", "tls": false })).await.is_ok());
        assert!(ok("ftp", json!({ "host": "100.101.102.103", "tls": true, "tls_insecure": true })).await.is_ok());
        let e = ok("ftp", json!({ "host": "1.1.1.1", "tls": false })).await.unwrap_err();
        assert!(e.contains("Unencrypted FTP") && e.contains("1.1.1.1"), "{e}");
        assert!(ok("ftp", json!({ "host": "1.1.1.1", "tls": true, "tls_insecure": true })).await.is_err());
        assert!(ok("ftp", json!({ "host": "1.1.1.1", "tls": true })).await.is_ok());
        assert!(ok("sftp", json!({ "host": "1.1.1.1" })).await.is_ok());
    }

    #[test]
    fn paths_in_a_location_stay_inside_it() {
        let h = format!("abcd{}", "0".repeat(60));
        assert_eq!(content_hash(&format!("blobs/ab/cd/{h}"), "blobs"), Some(h.as_str()));
        assert_eq!(content_hash(&format!("ab/cd/{h}"), ""), Some(h.as_str()));
        assert_eq!(content_hash(&format!("blobs/ab/ce/{h}"), "blobs"), None, "not where the store puts it");
        assert_eq!(content_hash(&format!("blobs/AB/CD/{}", h.to_uppercase()), "blobs"), None);
        assert_eq!(content_hash(&format!("other/ab/cd/{h}"), "blobs"), None);
        for bad in ["..", "a/../b", "/etc", "a//b", "a\\b"] {
            assert!(key_parts(bad).is_err(), "{bad}");
        }
        let cfg = S3Config {
            endpoint: "http://127.0.0.1:9000".into(),
            bucket: "files".into(),
            prefix: "/thirtyfile/".into(),
            access_key_id: "key".into(),
            secret_access_key: crate::testutil::password().into(),
            path_style: true,
            allow_http: true,
            ..Default::default()
        };
        let s3 = S3Storage::new(&cfg).unwrap();
        assert_eq!(s3.key_at(".thirtyfile-check/x").unwrap().as_ref(), "thirtyfile/.thirtyfile-check/x");
        assert_eq!(s3.key_at("").unwrap().as_ref(), "thirtyfile");
        assert!(s3.key_at("../other").is_err());
        assert_eq!(s3.content_dir(), "blobs");
    }

    #[test]
    fn the_built_in_folder_gets_its_marker_only_when_new_or_empty() {
        let base = std::env::temp_dir().join(format!("thirtyfile-builtin-{}", crate::util::new_id()));
        let marker = |dir: &Path| std::fs::read_to_string(dir.join(LOCATION_MARKER)).ok();
        // A fresh install: the folder is created with the marker, and used from then on
        let fresh = base.join("storage");
        prepare_builtin(&fresh, false).unwrap();
        assert_eq!(marker(&fresh).as_deref(), Some("local"));
        std::fs::create_dir_all(fresh.join("users/admin")).unwrap();
        prepare_builtin(&fresh, true).unwrap();
        // An empty folder, or one with only what a disk or NAS makes by itself: nothing was there
        let empty = base.join("empty");
        for system in SYSTEM_ENTRIES {
            std::fs::create_dir_all(empty.join(system)).unwrap();
        }
        prepare_builtin(&empty, false).unwrap();
        assert_eq!(marker(&empty).as_deref(), Some("local"));
        // Items but no marker, or another location's: it may be another disk, so it isn't used or changed
        let full = base.join("full");
        std::fs::create_dir_all(full.join("users/amy")).unwrap();
        assert!(prepare_builtin(&full, false).unwrap_err().contains(LOCATION_MARKER));
        assert_eq!(marker(&full), None);
        let other = base.join("other");
        claim_folder(&other, "nas").unwrap();
        assert!(prepare_builtin(&other, false).is_err());
        assert_eq!(marker(&other).as_deref(), Some("nas"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_or_empty_built_in_folder_is_never_taken_when_the_database_records_files_there() {
        let base = std::env::temp_dir().join(format!("thirtyfile-unmounted-{}", crate::util::new_id()));
        // A volume that isn't mounted: the folder is missing, or an empty mount point (with a disk's own entries)
        let missing = base.join("missing");
        let e = prepare_builtin(&missing, true).unwrap_err();
        assert!(e.contains("may not be mounted"), "{e}");
        assert!(!missing.exists(), "nothing is made");
        let empty = base.join("empty");
        std::fs::create_dir_all(empty.join("lost+found")).unwrap();
        assert!(prepare_builtin(&empty, true).unwrap_err().contains("may not be mounted"));
        assert!(!empty.join(LOCATION_MARKER).exists());
        let _ = std::fs::remove_dir_all(&base);
    }

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
        std::fs::write(root.join(LOCATION_MARKER), "loc-42
").unwrap();
        s.ping().await.unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn virtual_hosted_endpoints() {
        assert_eq!(virtual_hosted_endpoint("https://acct.r2.cloudflarestorage.com", "files").unwrap(), "https://files.acct.r2.cloudflarestorage.com");
        assert_eq!(virtual_hosted_endpoint("http://nas.local:9000", "b").unwrap(), "http://b.nas.local:9000");
        assert_eq!(virtual_hosted_endpoint("https://b.s3.example.com", "b").unwrap(), "https://b.s3.example.com");
        assert!(virtual_hosted_endpoint("s3.example.com", "b").is_err());
    }

    #[test]
    fn part_sizes_stay_under_10000_parts() {
        assert_eq!(part_size(40 << 20), MULTIPART_CHUNK);
        for size in [100u64 << 30, 1 << 40, 4_900u64 << 30] {
            let p = part_size(size) as u64;
            assert!(size.div_ceil(p) <= 10_000 && p.is_multiple_of(1 << 20) && p <= 5 << 30);
        }
    }

    #[tokio::test]
    async fn sftp_and_ftp_hosts_are_split_into_parts() {
        let cfg = normalize("sftp", serde_json::json!({ "host": "sftp://backup@nas.local:2222/volume1/drive/", "username": "" })).await;
        assert_eq!((cfg["host"].as_str(), cfg["port"].as_u64(), cfg["path"].as_str(), cfg["username"].as_str()), (Some("nas.local"), Some(2222), Some("/volume1/drive"), Some("backup")));
        // Fields already filled in aren't overwritten; nothing changes without a port
        let cfg = normalize("ftp", serde_json::json!({ "host": " ftp.example.com ", "path": "/data", "port": 2121 })).await;
        assert_eq!((cfg["host"].as_str(), cfg["port"].as_u64(), cfg["path"].as_str()), (Some("ftp.example.com"), Some(2121), Some("/data")));
        let cfg = normalize("ftp", serde_json::json!({ "host": "[::1]:21" })).await;
        assert_eq!((cfg["host"].as_str(), cfg["port"].as_u64()), (Some("::1"), Some(21)));
    }

    #[tokio::test]
    async fn normalize_splits_bucket_from_endpoint() {
        let cfg = serde_json::json!({ "endpoint": "https://acct.r2.cloudflarestorage.com/files/", "region": "", "bucket": "" });
        let out: S3Config = serde_json::from_value(normalize("s3", cfg).await).unwrap();
        assert_eq!((out.endpoint.as_str(), out.bucket.as_str(), out.region.as_str()), ("https://acct.r2.cloudflarestorage.com", "files", "auto"));
    }
}
