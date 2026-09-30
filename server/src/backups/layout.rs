//! What a set looks like on its destination, and the manifest of a snapshot.
//!
//! ```text
//! .thirtyfile-backups/<set>/set.json                          what the set is (format, kind, name, source, installation)
//! .thirtyfile-backups/<set>/objects/ab/cd/<sha256>            each content once
//! .thirtyfile-backups/<set>/snapshots/<id>/manifest.jsonl     the spaces, folders, files and versions, a JSON value per line
//! .thirtyfile-backups/<set>/snapshots/<id>/complete.json      written last: the manifest's SHA-256 and size, and counts
//! ```
//!
//! A snapshot without `complete.json`, or whose manifest doesn't match it, is never a restore point. The manifest and
//! the objects are enough to get the files back without ThirtyFile's database (site/docs/backup.html says how).

use std::{io::BufRead, path::PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
    storage::Storage,
};

/// The folder ThirtyFile keeps copies and backups in, at the top of a location
pub const ROOT: &str = ".thirtyfile-backups";
/// The format of set.json, manifests and completion markers
pub const FORMAT: u32 = 1;

pub fn set_file(set: &str) -> String {
    format!("{ROOT}/{set}/set.json")
}

pub fn set_dir(set: &str) -> String {
    format!("{ROOT}/{set}")
}

pub fn object_key(set: &str, hash: &str) -> String {
    format!("{ROOT}/{set}/objects/{}/{}/{hash}", &hash[0..2], &hash[2..4])
}

pub fn manifest_key(set: &str, snapshot: &str) -> String {
    format!("{ROOT}/{set}/snapshots/{snapshot}/manifest.jsonl")
}

pub fn complete_key(set: &str, snapshot: &str) -> String {
    format!("{ROOT}/{set}/snapshots/{snapshot}/complete.json")
}

/// What a set is, as set.json says
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetFile {
    pub format: u32,
    pub kind: String,
    pub id: String,
    pub name: String,
    /// The location copied, by name
    pub source: String,
    /// The installation of ThirtyFile that made it (`locations::install_id`)
    pub installation: String,
    pub created_at: i64,
}

/// A line of a manifest. Folders come before what is in them (by path), files after the folders of their space,
/// versions after the files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Line {
    Header {
        format: u32,
        app: String,
        set: String,
        snapshot: String,
        created_at: i64,
        /// When the spaces were read
        cutoff: i64,
        /// What the snapshot can promise about files changed while it was made
        consistency: String,
    },
    Space {
        id: String,
        name: String,
        kind: String,
        /// Personal spaces: the owner's user name and id
        owner: String,
        owner_id: Option<i64>,
        mode: String,
        quota: i64,
    },
    Folder {
        space: String,
        id: String,
        /// None for the space's top folder
        parent: Option<String>,
        /// Below the space's top folder ('' for it)
        path: String,
        modified: i64,
        /// When it went to the trash (itself or a folder it is in)
        trashed: Option<i64>,
    },
    File {
        space: String,
        id: String,
        parent: String,
        path: String,
        hash: String,
        size: i64,
        mime: String,
        modified: i64,
        trashed: Option<i64>,
    },
    Version {
        space: String,
        file: String,
        id: String,
        hash: String,
        size: i64,
        /// When the file got this content, and when it was replaced
        modified: i64,
        replaced: i64,
        author: String,
    },
    /// Who had access to a folder or space when the snapshot was made (restores don't give it again)
    Grant {
        space: String,
        node: String,
        principal: String,
        name: String,
        role: String,
    },
}

/// Written last: the snapshot is complete, with this manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Complete {
    pub format: u32,
    pub set: String,
    pub snapshot: String,
    pub manifest_sha256: String,
    pub manifest_size: u64,
    pub cutoff: i64,
    pub completed_at: i64,
    pub folders: i64,
    pub files: i64,
    pub versions: i64,
    pub logical_bytes: i64,
}

/// Where manifests are kept on this server once written or read: restores and browsing read them from here
pub fn cache_dir(st: &AppState) -> PathBuf {
    st.data_dir.join("backup-manifests")
}

pub fn cached_manifest(st: &AppState, snapshot: &str) -> PathBuf {
    cache_dir(st).join(format!("{snapshot}.jsonl"))
}

/// SHA-256 and size of a file on this server
pub async fn file_sha256(path: PathBuf) -> std::io::Result<(String, u64)> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut f = std::fs::File::open(&path)?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut len = 0u64;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            len += n as u64;
        }
        Ok((hex::encode(h.finalize()), len))
    })
    .await
    .map_err(std::io::Error::other)?
}

/// The manifest of a complete snapshot on this server, checked against its SHA-256: read from the cache, or fetched
/// from the destination (and cached) when the cache doesn't have it or it doesn't match
pub async fn manifest(st: &AppState, dst: &dyn Storage, set: &str, snapshot: &str, sha256: &str, size: u64) -> AppResult<PathBuf> {
    let path = cached_manifest(st, snapshot);
    if tokio::fs::metadata(&path).await.is_ok_and(|m| m.len() == size)
        && file_sha256(path.clone()).await.is_ok_and(|(h, _)| h == sha256)
    {
        return Ok(path);
    }
    tokio::fs::create_dir_all(cache_dir(st)).await?;
    let tmp = st.tmp_dir().join(format!("backup-manifest-{}", crate::util::new_id()));
    let fetched = async {
        let mut reader = dst.open_at(&manifest_key(set, snapshot), 0, size).await?;
        let mut out = tokio::fs::File::create(&tmp).await?;
        tokio::io::copy(&mut reader, &mut out).await?;
        tokio::io::AsyncWriteExt::flush(&mut out).await?;
        std::io::Result::Ok(())
    }
    .await;
    if let Err(e) = fetched {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(AppError::new(axum::http::StatusCode::BAD_GATEWAY, format!("The snapshot's list of files couldn't be read: {}", crate::locations::describe(&e))));
    }
    let (hash, len) = file_sha256(tmp.clone()).await?;
    if hash != sha256 || len != size {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(AppError::new(axum::http::StatusCode::BAD_GATEWAY, "The snapshot's list of files on the destination isn't the one that was written: it was changed or damaged"));
    }
    tokio::fs::rename(&tmp, &path).await?;
    Ok(path)
}

/// Reads a small file of a set (set.json, complete.json)
pub async fn read_small(dst: &dyn Storage, key: &str) -> std::io::Result<Option<Vec<u8>>> {
    let Some(entry) = dst.stat(key).await? else { return Ok(None) };
    if entry.size > 1024 * 1024 {
        return Err(std::io::Error::other("too large"));
    }
    let mut body = Vec::new();
    dst.open_at(key, 0, entry.size).await?.read_to_end(&mut body).await?;
    Ok(Some(body))
}

/// The lines of a manifest on this server, one at a time (a blocking reader: call from `spawn_blocking`)
pub fn lines(path: &std::path::Path) -> std::io::Result<impl Iterator<Item = std::io::Result<Line>>> {
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    Ok(reader.lines().map(|l| l.and_then(|l| serde_json::from_str(&l).map_err(std::io::Error::other))))
}

/// Writes a small file of a set from memory
pub async fn write_small(st: &AppState, dst: &dyn Storage, key: &str, body: &[u8]) -> std::io::Result<()> {
    let tmp = st.tmp_dir().join(format!("backup-{}", crate::util::new_id()));
    tokio::fs::write(&tmp, body).await?;
    let put = dst.put_at(key, &tmp).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    put?;
    match dst.stat(key).await? {
        Some(e) if e.size == body.len() as u64 => Ok(()),
        _ => Err(std::io::Error::other("The file written on the destination isn't complete")),
    }
}
