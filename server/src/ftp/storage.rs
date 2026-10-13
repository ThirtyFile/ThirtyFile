//! A receive folder is a small virtual filesystem. No device receives the creator's deletion or sharing powers.

use super::login::Device;
use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::Source,
    state::AppState,
    tree::{self, Need, Node},
};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tokio::io::{AsyncRead, AsyncReadExt};
use unftp_core::storage::{Error, ErrorKind, Fileinfo, Metadata, Permissions, StorageBackend};

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
impl Metadata for Attributes {
    fn len(&self) -> u64 {
        self.size
    }
    fn is_dir(&self) -> bool {
        self.folder
    }
    fn is_file(&self) -> bool {
        !self.folder
    }
    fn is_symlink(&self) -> bool {
        false
    }
    fn modified(&self) -> Result<SystemTime, Error> {
        Ok(self.modified)
    }
    fn gid(&self) -> u32 {
        0
    }
    fn uid(&self) -> u32 {
        0
    }
    fn permissions(&self) -> Permissions {
        Permissions(if self.folder { 0o700 } else { 0o600 })
    }
}
fn map(e: AppError) -> Error {
    let kind = match e.status.as_u16() {
        401 | 403 => ErrorKind::PermissionDenied,
        404 => ErrorKind::PermanentFileNotAvailable,
        413 | 507 => ErrorKind::InsufficientStorageSpaceError,
        _ => ErrorKind::LocalError,
    };
    Error::new(kind, std::io::Error::other(e.message))
}
fn refused() -> Error {
    Error::new(ErrorKind::PermissionDenied, std::io::Error::other("This FTP receive folder only accepts uploads"))
}
fn parts(path: &Path) -> AppResult<Vec<String>> {
    let p = path.to_str().ok_or_else(|| AppError::bad_request("Invalid FTP path"))?;
    // libunftp builds its virtual paths with PathBuf, which uses backslashes for nested paths on Windows.
    // Every component is still validated and resolved by node identity; this never becomes a disk path.
    #[cfg(windows)]
    let normalized = p.replace('\\', "/");
    #[cfg(windows)]
    let p = normalized.as_str();
    if matches!(p, "" | "/" | ".") {
        return Ok(vec![]);
    }
    crate::access::relative_parts(p.strip_prefix('/').unwrap_or(p))
}
impl Files {
    async fn resolve(&self, user: &User, path: &Path, need: Need) -> AppResult<Node> {
        let root = user.protocol_root.as_deref().ok_or_else(AppError::unauthorized)?;
        let mut node = tree::folder_for(&mut *self.0.db.acquire().await?, user, root, Need::Read).await?;
        let parts = parts(path)?;
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
    async fn parent(&self, user: &User, path: &Path) -> AppResult<(Node, String)> {
        let mut segments = parts(path)?;
        let name = segments.pop().ok_or_else(|| AppError::bad_request("A filename is required"))?;
        let parent = PathBuf::from(format!("/{}", segments.join("/")));
        Ok((self.resolve(user, &parent, Need::Add).await?, name))
    }
}

#[async_trait::async_trait]
impl StorageBackend<Device> for Files {
    type Metadata = Attributes;
    async fn metadata<P: AsRef<Path> + Send + Debug>(&self, user: &Device, path: P) -> Result<Attributes, Error> {
        let actor = super::credentials::access(&self.0, &user.actor, false, true).await.map_err(map)?;
        let node = self.resolve(&actor, path.as_ref(), Need::Read).await.map_err(map)?;
        self.attributes(&node).await.map_err(map)
    }
    async fn list<P: AsRef<Path> + Send + Debug>(&self, user: &Device, path: P) -> Result<Vec<Fileinfo<PathBuf, Attributes>>, Error> {
        let actor = super::credentials::access(&self.0, &user.actor, true, false).await.map_err(map)?;
        let node = self.resolve(&actor, path.as_ref(), Need::Read).await.map_err(map)?;
        if !node.is_folder() {
            return Err(refused());
        }
        let mut out = vec![];
        let mut after = None;
        let mut examined = 0usize;
        loop {
            let (nodes, next) = crate::access::list_page(&self.0, &actor, &node.id, after).await.map_err(map)?;
            for child in nodes {
                examined += 1;
                if examined > 10000 {
                    return Err(Error::new(ErrorKind::LocalError, std::io::Error::other("This directory is too large to list through FTP")));
                }
                if tree::node_for(&mut *self.0.db.acquire().await.map_err(|e| map(e.into()))?, &actor, &child.id, Need::Read).await.is_err() {
                    continue;
                }
                out.push(Fileinfo { path: PathBuf::from(child.name.clone()), metadata: self.attributes(&child).await.map_err(map)? });
                if out.len() > 10000 {
                    return Err(Error::new(ErrorKind::LocalError, std::io::Error::other("This directory is too large to list through FTP")));
                }
            }
            if next.is_none() {
                break;
            }
            after = next;
        }
        Ok(out)
    }
    async fn get<P: AsRef<Path> + Send + Debug>(&self, user: &Device, path: P, start_pos: u64) -> Result<Box<dyn AsyncRead + Send + Sync + Unpin>, Error> {
        if start_pos != 0 {
            return Err(refused());
        }
        let actor = super::credentials::access(&self.0, &user.actor, false, true).await.map_err(map)?;
        let node = self.resolve(&actor, path.as_ref(), Need::Read).await.map_err(map)?;
        if node.is_folder() {
            return Err(refused());
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
                loop {
                    super::credentials::access(&st, &actor, false, true).await?;
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
                            _ = tokio::time::sleep(Duration::from_millis(200)) => { super::credentials::access(&st,&actor,false,true).await?; }
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
    async fn put<P: AsRef<Path> + Send + Debug, R: AsyncRead + Send + Sync + Unpin + 'static>(
        &self,
        user: &Device,
        input: R,
        path: P,
        start_pos: u64,
    ) -> Result<u64, Error> {
        // REST/APPE are not advertised. Their offsets must never truncate or append directly to a visible file.
        if start_pos != 0 {
            return Err(refused());
        }
        let mut actor = super::credentials::access(&self.0, &user.actor, false, false).await.map_err(map)?;
        let transfer_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let _transfer = Transfer(transfer_alive.clone());
        actor.ftp_credential.as_mut().ok_or_else(refused)?.transfer = Some(transfer_alive);
        let (parent, name) = self.parent(&actor, path.as_ref()).await.map_err(map)?;
        let up = crate::access::upload_to(&self.0, &actor, &parent, name).await.map_err(map)?;
        let path = self.0.tmp_dir().join(format!("ftp-{}", crate::util::new_id()));
        let temporary = Temporary(path.clone());
        let stream = futures_util::stream::unfold((input, actor.clone(), self.0.clone()), |(mut input, actor, st)| async move {
            let mut bytes = vec![0; 64 * 1024];
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                let read = input.read(&mut bytes);
                tokio::pin!(read);
                loop {
                    tokio::select! {
                        result = &mut read => break result,
                        _ = tokio::time::sleep(Duration::from_millis(200)) => {
                            if super::credentials::access(&st,&actor,false,false).await.is_err() {
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
                    let alive = super::credentials::access(&st, &actor, false, false).await;
                    let item = alive.map(|_| {
                        bytes.truncate(n);
                        bytes::Bytes::from(bytes)
                    });
                    Some((item, (input, actor, st)))
                }
                _ => Some((Err(AppError::bad_request("FTP upload interrupted")), (input, actor, st))),
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
                crate::content::Received { path, size, hash },
                crate::access::ScopedWrite { root, mtime: None },
            )
            .await;
            drop(temporary);
            result
        })
        .await
        .map_err(|e| Error::new(ErrorKind::LocalError, e))?
        .map_err(map)?;
        Ok(size)
    }
    async fn del<P: AsRef<Path> + Send + Debug>(&self, _: &Device, _: P) -> Result<(), Error> {
        Err(refused())
    }
    async fn rename<P: AsRef<Path> + Send + Debug>(&self, _: &Device, _: P, _: P) -> Result<(), Error> {
        Err(refused())
    }
    async fn rmd<P: AsRef<Path> + Send + Debug>(&self, _: &Device, _: P) -> Result<(), Error> {
        Err(refused())
    }
    async fn mkd<P: AsRef<Path> + Send + Debug>(&self, user: &Device, path: P) -> Result<(), Error> {
        let actor = super::credentials::access(&self.0, &user.actor, false, false).await.map_err(map)?;
        let (parent, name) = self.parent(&actor, path.as_ref()).await.map_err(map)?;
        crate::access::make_folder(&self.0, &actor, &parent, &name).await.map(|_| ()).map_err(map)
    }
    async fn cwd<P: AsRef<Path> + Send + Debug>(&self, user: &Device, path: P) -> Result<(), Error> {
        let actor = super::credentials::access(&self.0, &user.actor, false, false).await.map_err(map)?;
        let node = self.resolve(&actor, path.as_ref(), Need::Read).await.map_err(map)?;
        if !node.is_folder() {
            return Err(refused());
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
