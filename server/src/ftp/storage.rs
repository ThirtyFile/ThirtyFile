//! A receive folder is a small virtual filesystem. No device receives the creator's deletion or sharing powers.

use super::login::{Device, Login};
use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::Source,
    state::AppState,
    tree::{self, Need, Node},
};
use std::{
    fmt::Debug,
    path::PathBuf,
    time::{Duration, SystemTime},
};
use thirtyftp::{Backend, Entry, FsError, FtpPath, LoginContext, LoginError};
use tokio::io::{AsyncRead, AsyncReadExt};

/// What one connection hands to the FTP library: its sign-in, and the files
pub struct Receive {
    pub login: Login,
    pub files: Files,
}

pub struct Files(pub AppState);
impl Debug for Files {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FtpReceiveFiles")
    }
}

#[derive(Debug, Clone)]
pub struct Attributes {
    size: u64,
    folder: bool,
    modified: SystemTime,
}
impl Attributes {
    fn entry(&self, name: &str) -> Entry {
        if self.folder { Entry::directory(name, Some(self.modified)) } else { Entry::file(name, self.size, Some(self.modified)) }
    }
}
fn map(e: AppError) -> FsError {
    match e.status.as_u16() {
        401 | 403 => FsError::PermissionDenied,
        404 => FsError::NotFound,
        413 | 507 => FsError::QuotaExceeded,
        _ => FsError::Local(e.message),
    }
}
/// What a receive folder doesn't do (delete, rename, read in the middle of a file)
fn refused() -> FsError {
    FsError::PermissionDenied
}
/// The names of a path, each of them checked
fn parts(path: &FtpPath) -> AppResult<Vec<String>> {
    if path.is_root() {
        return Ok(vec![]);
    }
    crate::access::relative_parts(&path.components().join("/"))
}
impl Files {
    async fn resolve(&self, user: &User, path: &FtpPath, need: Need) -> AppResult<Node> {
        self.resolve_parts(user, parts(path)?, need).await
    }
    async fn resolve_parts(&self, user: &User, parts: Vec<String>, need: Need) -> AppResult<Node> {
        let root = user.protocol_root.as_deref().ok_or_else(AppError::unauthorized)?;
        let mut node = tree::folder_for(&mut *self.0.db.acquire().await?, user, root, Need::Read).await?;
        let last = parts.len().saturating_sub(1);
        for (i, name) in parts.into_iter().enumerate() {
            if !node.is_folder() {
                return Err(AppError::not_found("Folder not found"));
            }
            if node.in_folder_space() {
                crate::folders::sync_opened(&self.0, &node).await;
            }
            let mut conn = self.0.db.acquire().await?;
            let next = crate::paths::child_named(&mut conn, &node.id, &name).await?.ok_or_else(|| AppError::not_found("File not found"))?;
            if next.name != name {
                return Err(AppError::conflict("The filename differs only in case"));
            }
            node = tree::node_for(&mut conn, user, &next.id, if i == last { need } else { Need::Read }).await?;
        }
        tree::node_for(&mut *self.0.db.acquire().await?, user, &node.id, need).await
    }
    async fn attributes(&self, node: &Node) -> AppResult<Attributes> {
        let size = if node.is_folder() { 0 } else { Source::resolve(&self.0, node).await?.describe(node.size as u64).await?.0 };
        Ok(Attributes { size, folder: node.is_folder(), modified: SystemTime::UNIX_EPOCH + Duration::from_secs(node.updated_at.max(0) as u64) })
    }
    async fn parent(&self, user: &User, path: &FtpPath) -> AppResult<(Node, String)> {
        let mut segments = parts(path)?;
        let name = segments.pop().ok_or_else(|| AppError::bad_request("A filename is required"))?;
        Ok((self.resolve_parts(user, segments, Need::Add).await?, name))
    }
}

#[async_trait::async_trait]
impl Backend for Receive {
    type User = Device;

    async fn login(&self, context: &LoginContext, user: &str, password: &str) -> Result<Device, LoginError> {
        self.login.sign_in(context, user, password).await
    }

    async fn metadata(&self, user: &Device, path: &FtpPath) -> Result<Entry, FsError> {
        let files = &self.files;
        let actor = super::credentials::access(&files.0, &user.actor, false, true).await.map_err(map)?;
        let node = files.resolve(&actor, path, Need::Read).await.map_err(map)?;
        Ok(files.attributes(&node).await.map_err(map)?.entry(path.file_name().unwrap_or("")))
    }

    async fn list(&self, user: &Device, path: &FtpPath) -> Result<Vec<Entry>, FsError> {
        self.files.list(user, path).await
    }

    async fn read(&self, user: &Device, path: &FtpPath, offset: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>, FsError> {
        self.files.get(user, path, offset).await
    }

    async fn write(&self, user: &Device, path: &FtpPath, data: &mut (dyn AsyncRead + Send + Unpin)) -> Result<u64, FsError> {
        self.files.put(user, data, path).await
    }

    async fn make_dir(&self, user: &Device, path: &FtpPath) -> Result<(), FsError> {
        let files = &self.files;
        let actor = super::credentials::access(&files.0, &user.actor, false, false).await.map_err(map)?;
        let (parent, name) = files.parent(&actor, path).await.map_err(map)?;
        crate::access::make_folder(&files.0, &actor, &parent, &name).await.map(|_| ()).map_err(map)
    }

    async fn change_dir(&self, user: &Device, path: &FtpPath) -> Result<(), FsError> {
        let files = &self.files;
        let actor = super::credentials::access(&files.0, &user.actor, false, false).await.map_err(map)?;
        let node = files.resolve(&actor, path, Need::Read).await.map_err(map)?;
        if !node.is_folder() {
            return Err(FsError::WrongType);
        }
        Ok(())
    }
}

impl Files {
    async fn list(&self, user: &Device, path: &FtpPath) -> Result<Vec<Entry>, FsError> {
        let actor = super::credentials::access(&self.0, &user.actor, true, false).await.map_err(map)?;
        let node = self.resolve(&actor, path, Need::Read).await.map_err(map)?;
        if !node.is_folder() {
            return Err(FsError::WrongType);
        }
        let mut out = vec![];
        let mut after = None;
        let mut examined = 0usize;
        loop {
            let (nodes, next) = crate::access::list_page(&self.0, &actor, &node.id, after).await.map_err(map)?;
            for child in nodes {
                examined += 1;
                if examined > 10000 {
                    return Err(FsError::Local("This directory is too large to list through FTP".into()));
                }
                if tree::node_for(&mut *self.0.db.acquire().await.map_err(|e| map(e.into()))?, &actor, &child.id, Need::Read).await.is_err() {
                    continue;
                }
                out.push(self.attributes(&child).await.map_err(map)?.entry(&child.name));
                if out.len() > 10000 {
                    return Err(FsError::Local("This directory is too large to list through FTP".into()));
                }
            }
            if next.is_none() {
                break;
            }
            after = next;
        }
        Ok(out)
    }
    async fn get(&self, user: &Device, path: &FtpPath, start_pos: u64) -> Result<Box<dyn AsyncRead + Send + Unpin>, FsError> {
        if start_pos != 0 {
            return Err(refused());
        }
        let actor = super::credentials::access(&self.0, &user.actor, false, true).await.map_err(map)?;
        let node = self.resolve(&actor, path, Need::Read).await.map_err(map)?;
        if node.is_folder() {
            return Err(FsError::WrongType);
        }
        // Each bounded read checks credential generation, current permissions and the publication boundary.
        let (mut writer, reader) = tokio::io::duplex(256 * 1024);
        let error = std::sync::Arc::new(crate::sync::Mutex::new(None));
        let send_error = error.clone();
        let st = self.0.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let result = async {
                let mut offset = 0;
                let mut recheck = Recheck::new();
                loop {
                    recheck.due(&st, &actor, false, true).await?;
                    let bytes = crate::access::read_at(&st, &actor, &node.id, offset, 256 * 1024).await?;
                    if bytes.is_empty() {
                        return Ok::<(), AppError>(());
                    }
                    offset += bytes.len() as u64;
                    let write = writer.write_all(&bytes);
                    tokio::pin!(write);
                    let deadline = tokio::time::sleep(Duration::from_secs(30));
                    tokio::pin!(deadline);
                    loop {
                        tokio::select! {
                            result = &mut write => { result?; break; }
                            _ = &mut deadline => return Err(AppError::bad_request("FTP download interrupted")),
                            _ = tokio::time::sleep(Duration::from_millis(500)) => { recheck.due(&st, &actor, false, true).await?; }
                        }
                    }
                }
            }
            .await;
            if let Err(e) = result {
                *send_error.lock() = Some(std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.message));
            }
        });
        Ok(Box::new(CheckedRead { reader, error }))
    }
    async fn put(&self, user: &Device, input: &mut (dyn AsyncRead + Send + Unpin), path: &FtpPath) -> Result<u64, FsError> {
        let mut actor = super::credentials::access(&self.0, &user.actor, false, false).await.map_err(map)?;
        let transfer_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let _transfer = Transfer(transfer_alive.clone());
        actor.ftp_credential.as_mut().ok_or_else(refused)?.transfer = Some(transfer_alive);
        let (parent, name) = self.parent(&actor, path).await.map_err(map)?;
        let mut up = crate::access::upload_to(&self.0, &actor, &parent, name).await.map_err(map)?;
        // A receive folder that keeps both: a taken name gives the new file a number, and nothing there is replaced
        let id = actor.ftp_credential.as_ref().map(|c| c.id.clone()).ok_or_else(refused)?;
        let (replaces,): (bool,) =
            sqlx::query_as("SELECT replace_existing FROM ftp_receive_folders WHERE id = ?").bind(&id).fetch_one(&self.0.db).await.map_err(|e| map(e.into()))?;
        let expect = if up.existing.is_some() && !replaces {
            let mut c = self.0.db.acquire().await.map_err(|e| map(e.into()))?;
            up.name = if up.parent.in_folder_space() {
                crate::fsops::free_name(&mut c, &up.parent, &up.name, false).await
            } else {
                crate::tree::unique_name(&mut c, &up.parent.id, &up.name, false).await
            }
            .map_err(map)?;
            up.existing = None;
            crate::access::Expect::Absent
        } else {
            crate::access::Expect::Any
        };
        let path = self.0.tmp_dir().join(format!("ftp-{}", crate::util::new_id()));
        let temporary = Temporary(path.clone());
        let state = (input, actor.clone(), self.0.clone(), Recheck::new());
        let stream = futures_util::stream::unfold(state, |(input, actor, st, mut recheck)| async move {
            let mut bytes = vec![0; 64 * 1024];
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                let read = input.read(&mut bytes);
                tokio::pin!(read);
                loop {
                    tokio::select! {
                        result = &mut read => break result,
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {
                            if recheck.due(&st, &actor, false, false).await.is_err() {
                                break Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,"FTP receive folder is no longer active"));
                            }
                        }
                    }
                }
            })
            .await;
            match result {
                Ok(Ok(0)) => None,
                Ok(Ok(n)) => {
                    let alive = recheck.due(&st, &actor, false, false).await;
                    let item = alive.map(|_| {
                        bytes.truncate(n);
                        bytes::Bytes::from(bytes)
                    });
                    Some((item, (input, actor, st, recheck)))
                }
                _ => Some((Err(AppError::bad_request("FTP upload interrupted")), (input, actor, st, recheck))),
            }
        });
        let replaced = up.existing.as_ref().map_or(0, |n| n.size);
        let room = crate::tree::room_left(&mut *self.0.db.acquire().await.map_err(|e| map(e.into()))?, up.parent.drive(), None)
            .await
            .map_err(map)?
            .map(|(left, drive)| ((left + replaced).max(0) as u64, drive));
        let (size, hash) = crate::access::receive(&self.0, Box::pin(stream), &path, !up.parent.in_folder_space(), room).await.map_err(map)?;
        let st = self.0.clone();
        let root = actor.protocol_root.clone().ok_or_else(refused)?;
        tokio::spawn(async move {
            let result = crate::access::store_in_scope(
                st,
                actor,
                up.parent,
                up.name,
                crate::content::Received { path, size, hash, md5: None },
                crate::access::ScopedWrite { root, mtime: None, expect },
            )
            .await;
            drop(temporary);
            result
        })
        .await
        .map_err(|e| FsError::Local(e.to_string()))?
        .map_err(map)?;
        Ok(size)
    }
}

/// How often a transfer checks its receive folder again (`credentials::access`): a folder stopped, deleted or changed
/// meanwhile ends the transfer within that, without ten queries for every chunk
const RECHECK: Duration = Duration::from_secs(2);

/// When a transfer last checked its receive folder
struct Recheck(tokio::time::Instant);

impl Recheck {
    fn new() -> Self {
        Self(tokio::time::Instant::now())
    }

    /// Checks again when `RECHECK` has passed since the last time
    async fn due(&mut self, st: &crate::state::AppState, actor: &crate::auth::User, listing: bool, reading: bool) -> crate::error::AppResult<()> {
        if self.0.elapsed() >= RECHECK {
            super::credentials::access(st, actor, listing, reading).await?;
            self.0 = tokio::time::Instant::now();
        }
        Ok(())
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A revoked read is an I/O failure, never a successful short EOF/FTP 226 response.
struct CheckedRead {
    reader: tokio::io::DuplexStream,
    error: std::sync::Arc<crate::sync::Mutex<Option<std::io::Error>>>,
}
impl AsyncRead for CheckedRead {
    fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let remaining = buf.remaining();
        let result = std::pin::Pin::new(&mut self.reader).poll_read(cx, buf);
        if matches!(result, std::task::Poll::Ready(Ok(())))
            && remaining > 0
            && buf.filled().len() == before
            && let Some(error) = self.error.lock().take()
        {
            return std::task::Poll::Ready(Err(error));
        }
        result
    }
}

struct Transfer(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for Transfer {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}
