//! Physical file storage abstraction. Files are content-addressed by sha256; nodes only reference the hash.
//! Supports local folders, S3-compatible object storage (AWS S3, Cloudflare R2, MinIO…), SFTP and FTP / FTPS,
//! added by administrators under "System settings › Storage locations"; each space can choose where its new files are stored.

mod config;
pub mod ftp;
mod local;
mod markers;
mod s3;
pub mod sftp;

pub use config::*;
pub use local::*;
pub use markers::*;
pub use s3::*;

use std::{
    io::{self, SeekFrom},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use futures_util::{StreamExt, TryStreamExt, future::BoxFuture};
use object_store::{
    BackoffConfig, ClientOptions, GetOptions, ObjectStore, ObjectStoreExt, PutPayload, RetryConfig, WriteMultipart, aws::AmazonS3Builder, path::Path as ObjectPath,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt};
use tokio_util::io::StreamReader;

/// The id of the built-in storage location (its folder is THIRTYFILE_STORAGE)
pub const BUILTIN: &str = "local";

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

/// A storage service error is shown to people as what it is (503 with its message)
impl From<std::io::Error> for crate::error::AppError {
    fn from(e: std::io::Error) -> Self {
        if let Some(se) = e.get_ref().and_then(|inner| inner.downcast_ref::<StorageError>()) {
            tracing::warn!("{se}");
            return Self::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, se.text());
        }
        Self::internal(e)
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
}
