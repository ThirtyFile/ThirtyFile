use super::{Login, Permit};
use crate::{
    access,
    auth::User,
    error::AppError,
    paths::{self, Target},
    state::AppState,
    tree::Node,
    util::new_id,
};
use bytes::Bytes;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use thirtysftp::{
    server::{Filesystem, FsError, format_long_name},
    wire::{Attributes, NameEntry, OpenFlags, StatusCode, Times},
};

/// How long the checks of a session (the account, its app password or key, SFTP's settings) and of a file being
/// written (its size against the limits, the quota and the disk's room) are taken as still true: a transfer isn't
/// checked again for every 32 KiB packet. Closing a file checks everything again before it is stored.
const RECHECK: Duration = Duration::from_secs(3);

/// How much a file being written may grow before its size is checked again within `RECHECK` (what is stored is
/// checked against the quota in full anyway)
const SIZE_SLACK: u64 = 8 << 20;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// The most files and folders one session keeps open
pub(super) const MAX_HANDLES: usize = 32;

pub(super) struct Files {
    st: AppState,
    peer: SocketAddr,
    login: Login,
    _permit: Arc<Permit>,
    /// The account as checked last, when, and whether for writing (`RECHECK`)
    checked: crate::sync::Mutex<Option<(tokio::time::Instant, bool, User)>>,
}
/// An open file: one that is read from the stored content, or one being written to a staged copy
pub(super) enum OpenFile {
    Read { id: String },
    Write(tokio::sync::Mutex<Box<Staged>>),
}
/// An open folder, and how far its listing has come
pub(super) struct OpenDir {
    path: Vec<String>,
    state: tokio::sync::Mutex<ListState>,
}
#[derive(Default)]
struct ListState {
    after: Option<String>,
    done: bool,
}
pub(super) struct Staged {
    path: PathBuf,
    file: tokio::fs::File,
    up: access::Upload,
    append: bool,
    readable: bool,
    failed: bool,
    create_only: bool,
    /// When its size was checked last, and up to what size that holds (`RECHECK`, `SIZE_SLACK`)
    checked: Option<(tokio::time::Instant, u64)>,
    /// A modification time the client set while writing it (`fsetstat`), kept once it is stored
    mtime: Option<u32>,
}
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn code(e: AppError) -> StatusCode {
    match e.status.as_u16() {
        401 | 403 | 429 => StatusCode::PermissionDenied,
        404 => StatusCode::NoSuchFile,
        400 => StatusCode::BadMessage,
        _ => StatusCode::Failure,
    }
}
/// A status code as the reply to a request (the messages say nothing about the server)
fn fs(code: StatusCode) -> FsError {
    let message = match code {
        StatusCode::PermissionDenied => "permission denied",
        StatusCode::NoSuchFile => "no such file",
        StatusCode::BadMessage => "bad request",
        StatusCode::OpUnsupported => "operation not supported",
        _ => "failure",
    };
    FsError::new(code, message)
}
/// A path as sent by the client: it must be text
fn text(path: &[u8]) -> Result<&str, StatusCode> {
    std::str::from_utf8(path).map_err(|_| StatusCode::BadMessage)
}
fn has_any(flags: OpenFlags, wanted: &[OpenFlags]) -> bool {
    wanted.iter().any(|w| flags.0 & w.0 != 0)
}
fn same_time(seconds: u32) -> Times {
    Times { accessed: seconds, modified: seconds }
}
fn attrs(node: Option<&Node>) -> Attributes {
    let directory = node.is_none_or(Node::is_folder);
    Attributes {
        size: node.map(|n| n.size.max(0) as u64),
        permissions: Some(if directory { 0o040755 } else { 0o100644 }),
        times: node.map(|n| same_time(n.updated_at.clamp(0, i64::from(u32::MAX)) as u32)),
        ..Default::default()
    }
}
fn entry(name: &str, attrs: Attributes) -> NameEntry {
    NameEntry { name: Bytes::copy_from_slice(name.as_bytes()), long_name: format_long_name(name.as_bytes(), &attrs), attrs }
}

async fn file_attrs(st: &AppState, node: &Node) -> Result<Attributes, StatusCode> {
    let mut attributes = attrs(Some(node));
    if !node.is_folder() {
        let source = crate::files::Source::resolve(st, node).await.map_err(code)?;
        let (size, version) = source.describe(node.size as u64).await.map_err(code)?;
        attributes.size = Some(size);
        // The time a client set for this content (`keep_mtime`), else when it changed in ThirtyFile
        if let Some(seconds) = crate::s3::metadata::read(st, &node.id, &version).await.ok().flatten().and_then(|t| t.split('.').next()?.parse::<i64>().ok()) {
            let seconds = seconds.clamp(0, i64::from(u32::MAX)) as u32;
            attributes.times = Some(same_time(seconds));
        }
    }
    Ok(attributes)
}

/// Keeps a file's modification time as a client set it, with the content's version: rclone and WinSCP sync by size and
/// time, as SFTP gives them no hash here. It is the column S3 keeps `x-amz-meta-mtime` in, so both tell the same time;
/// a change of the content from elsewhere drops it.
async fn keep_mtime(st: &AppState, node: &Node, mtime: u32) -> Result<(), StatusCode> {
    if node.is_folder() {
        return Ok(());
    }
    let source = crate::files::Source::resolve(st, node).await.map_err(code)?;
    // A disk that doesn't answer: the time isn't kept, as when nothing set it
    let Some(version) = source.version_within(node.drive()).await else { return Ok(()) };
    let _w = st.write_lock.lock().await;
    sqlx::query("UPDATE nodes SET s3_mtime = ?, s3_mtime_version = ? WHERE id = ?")
        .bind(mtime.to_string())
        .bind(version)
        .bind(&node.id)
        .execute(&st.db)
        .await
        .map_err(|_| StatusCode::Failure)?;
    Ok(())
}
fn segments(path: &str) -> Result<Vec<String>, StatusCode> {
    if path.len() > 4096 || path.contains(['\0', '\\']) {
        return Err(StatusCode::BadMessage);
    }
    let mut out = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => (),
            ".." => {
                if out.pop().is_none() {
                    return Err(StatusCode::PermissionDenied);
                }
            }
            name => out.push(name.to_string()),
        }
    }
    Ok(out)
}

impl Files {
    pub(super) fn new(st: AppState, peer: SocketAddr, login: Login, permit: Arc<Permit>) -> Self {
        Self { st, peer, login, _permit: permit, checked: Default::default() }
    }
    /// The account, as checked within `RECHECK` (for writing when `write`), else checked again
    async fn user(&self, write: bool) -> Result<User, StatusCode> {
        if let Some((at, for_writing, user)) = self.checked.lock().as_ref()
            && at.elapsed() < RECHECK
            && (*for_writing || !write)
        {
            return Ok(user.clone());
        }
        self.user_now(write).await
    }
    /// The account, checked now
    async fn user_now(&self, write: bool) -> Result<User, StatusCode> {
        let user = self.login.user(&self.st, self.peer, write).await.map_err(code)?;
        *self.checked.lock() = Some((tokio::time::Instant::now(), write, user.clone()));
        Ok(user)
    }
    async fn node(&self, user: &User, path: &[String]) -> Result<Node, StatusCode> {
        access::item(&self.st, user, path).await.map_err(code)
    }
    async fn description(&self, user: &User, path: &[String]) -> Result<Attributes, StatusCode> {
        let found =
            paths::resolve(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, user, path).await.map_err(code)?.ok_or(StatusCode::NoSuchFile)?;
        Ok(match found.target {
            Target::Node(n) => file_attrs(&self.st, &n).await?,
            _ => attrs(None),
        })
    }
    async fn delete(&self, path: &str, directory: bool) -> Result<(), StatusCode> {
        let user = self.user(true).await?;
        let node = self.node(&user, &segments(path)?).await?;
        if node.parent_id.is_none() || node.is_folder() != directory {
            return Err(StatusCode::PermissionDenied);
        }
        if directory {
            return crate::nodes::trash_empty_folder(&self.st, &user, &node.id).await.map_err(code);
        }
        access::trash(&self.st, &user, &node.id).await.map_err(code)
    }
    async fn open_write(&self, user: &User, path: &[String], flags: OpenFlags) -> Result<Staged, StatusCode> {
        let (parent, name) = access::parent_and_name(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, user, path).await.map_err(code)?;
        let up = access::upload_to(&self.st, user, &parent, name).await.map_err(code)?;
        if up.existing.is_none() && !flags.contains(OpenFlags::CREATE) {
            return Err(StatusCode::NoSuchFile);
        }
        if up.existing.is_some() && flags.contains(OpenFlags::EXCLUSIVE) {
            return Err(StatusCode::Failure);
        }
        let path = self.st.tmp_dir().join(format!("sftp-{}", new_id()));
        let file = tokio::fs::OpenOptions::new().create_new(true).read(true).write(true).open(&path).await.map_err(|_| StatusCode::Failure)?;
        let mut stage = Staged {
            path,
            file,
            up,
            append: flags.contains(OpenFlags::APPEND),
            readable: flags.contains(OpenFlags::READ),
            failed: false,
            create_only: flags.contains(OpenFlags::EXCLUSIVE),
            checked: None,
            mtime: None,
        };
        if !flags.contains(OpenFlags::TRUNCATE)
            && let Some(node) = &stage.up.existing
        {
            access::check_size(&self.st, node.size as u64).map_err(code)?;
            let source = crate::files::Source::resolve(&self.st, node).await.map_err(code)?;
            let (size, _) = source.describe(node.size as u64).await.map_err(code)?;
            access::check_size(&self.st, size).map_err(code)?;
            let mut disk = crate::util::DiskRoom::check(&self.st.tmp_dir(), size).await.map_err(code)?;
            let mut reader = source.open(&self.st, 0, size).await.map_err(|_| StatusCode::Failure)?;
            let mut buffer = vec![0; 128 * 1024];
            loop {
                let n = reader.read(&mut buffer).await.map_err(|_| StatusCode::Failure)?;
                if n == 0 {
                    break;
                }
                disk.before(n as u64).await.map_err(code)?;
                stage.file.write_all(&buffer[..n]).await.map_err(|_| StatusCode::Failure)?;
            }
        }
        Ok(stage)
    }
}

async fn stage_size(st: &AppState, user: &User, stage: &Staged, size: u64) -> Result<(), StatusCode> {
    access::check_size(st, size).map_err(code)?;
    let up = access::upload_to(st, user, &stage.up.parent, stage.up.name.clone()).await.map_err(code)?;
    up.fits(st, size).await.map_err(code)?;
    let before = stage.file.metadata().await.map_err(|_| StatusCode::Failure)?.len();
    crate::util::DiskRoom::check(&st.tmp_dir(), size.saturating_sub(before)).await.map_err(code)?;
    Ok(())
}

/// Refuses what can't be kept: owners. Permissions are accepted and left as they are (`scp -p` and WinSCP set them
/// after an upload, and showed an error when refused); times are kept (`keep_mtime`).
fn metadata_supported(a: &Attributes) -> Result<(), StatusCode> {
    if a.owner.is_some() {
        return Err(StatusCode::OpUnsupported);
    }
    Ok(())
}

impl Files {
    async fn open_file(&self, path: &[u8], flags: OpenFlags) -> Result<OpenFile, StatusCode> {
        let write = has_any(flags, &[OpenFlags::WRITE, OpenFlags::APPEND, OpenFlags::TRUNCATE, OpenFlags::CREATE]);
        let user = self.user(write).await?;
        if write && !flags.contains(OpenFlags::WRITE) {
            return Err(StatusCode::BadMessage);
        }
        let path = segments(text(path)?)?;
        Ok(if write {
            OpenFile::Write(tokio::sync::Mutex::new(Box::new(self.open_write(&user, &path, flags).await?)))
        } else {
            if !flags.contains(OpenFlags::READ) {
                return Err(StatusCode::BadMessage);
            }
            let node = self.node(&user, &path).await?;
            if node.is_folder() {
                return Err(StatusCode::Failure);
            }
            OpenFile::Read { id: node.id }
        })
    }

    async fn close_open(&self, opened: OpenFile) -> Result<(), StatusCode> {
        if let OpenFile::Write(stage) = opened {
            let stage = stage.into_inner();
            // In full before it is stored, whatever was checked while it was written
            let user = self.user_now(true).await?;
            let mut stage = stage;
            if stage.failed {
                return Err(StatusCode::Failure);
            }
            stage.file.flush().await.map_err(|_| StatusCode::Failure)?;
            stage.file.sync_data().await.map_err(|_| StatusCode::Failure)?;
            let size = stage.file.metadata().await.map_err(|_| StatusCode::Failure)?.len();
            access::check_size(&self.st, size).map_err(code)?;
            if stage.create_only {
                access::store_new(self.st.clone(), user, stage.up.parent.clone(), stage.up.name.clone(), stage.path.clone(), size).await.map_err(code)?;
            } else {
                access::store(self.st.clone(), user.clone(), stage.up.parent.clone(), stage.up.name.clone(), stage.path.clone(), size, None).await.map_err(code)?;
            }
            if let Some(mtime) = stage.mtime {
                let mut c = self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?;
                if let Some(node) = crate::paths::child_named(&mut c, &stage.up.parent.id, &stage.up.name).await.map_err(code)? {
                    drop(c);
                    keep_mtime(&self.st, &node, mtime).await?;
                }
            }
        } else {
            self.user(false).await?;
        }
        Ok(())
    }

    async fn read_open(&self, file: &OpenFile, offset: u64, len: u32) -> Result<Option<Bytes>, StatusCode> {
        let user = self.user(false).await?;
        let data = match file {
            OpenFile::Read { id } => access::read_at(&self.st, &user, id, offset, len).await.map_err(code)?,
            OpenFile::Write(s) => {
                let mut s = s.lock().await;
                if !s.readable {
                    return Err(StatusCode::PermissionDenied);
                }
                crate::tree::folder_for(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, &s.up.parent.id, crate::tree::Need::Read)
                    .await
                    .map_err(code)?;
                s.file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|_| StatusCode::Failure)?;
                let mut data = vec![0; len.min(256 * 1024) as usize];
                let n = s.file.read(&mut data).await.map_err(|_| StatusCode::Failure)?;
                data.truncate(n);
                data
            }
        };
        Ok(if data.is_empty() { None } else { Some(Bytes::from(data)) })
    }

    async fn write_open(&self, file: &OpenFile, offset: u64, data: Bytes) -> Result<(), StatusCode> {
        let user = self.user(true).await?;
        let OpenFile::Write(s) = file else {
            return Err(StatusCode::PermissionDenied);
        };
        let mut s = s.lock().await;
        let s = &mut **s;
        if s.failed || data.len() > 256 * 1024 {
            return Err(StatusCode::Failure);
        }
        let result = async {
            let before = s.file.metadata().await.map_err(|_| StatusCode::Failure)?.len();
            let offset = if s.append { before } else { offset };
            let size = before.max(offset.checked_add(data.len() as u64).ok_or(StatusCode::Failure)?);
            access::check_size(&self.st, size).map_err(code)?;
            if !s.checked.is_some_and(|(at, upto)| at.elapsed() < RECHECK && size <= upto) {
                stage_size(&self.st, &user, s, size).await?;
                s.checked = Some((tokio::time::Instant::now(), size.saturating_add(SIZE_SLACK)));
            }
            s.file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|_| StatusCode::Failure)?;
            s.file.write_all(&data).await.map_err(|_| StatusCode::Failure)
        }
        .await;
        if result.is_err() {
            s.failed = true;
        }
        result
    }

    async fn stat_path(&self, path: &[u8]) -> Result<Attributes, StatusCode> {
        let user = self.user(false).await?;
        self.description(&user, &segments(text(path)?)?).await
    }

    async fn stat_open(&self, file: &OpenFile) -> Result<Attributes, StatusCode> {
        let user = self.user(false).await?;
        Ok(match file {
            OpenFile::Read { id } => {
                file_attrs(
                    &self.st,
                    &crate::tree::node_for(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, id, crate::tree::Need::Read)
                        .await
                        .map_err(code)?,
                )
                .await?
            }
            OpenFile::Write(s) => {
                let s = s.lock().await;
                crate::tree::folder_for(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, &s.up.parent.id, crate::tree::Need::Read)
                    .await
                    .map_err(code)?;
                let mut a = attrs(s.up.existing.as_ref());
                a.size = Some(s.file.metadata().await.map_err(|_| StatusCode::Failure)?.len());
                a.permissions = Some(0o100644);
                a
            }
        })
    }

    async fn set_path(&self, path: &[u8], a: &Attributes) -> Result<(), StatusCode> {
        let user = self.user(true).await?;
        let node = self.node(&user, &segments(text(path)?)?).await?;
        metadata_supported(a)?;
        crate::tree::node_for(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, &node.id, crate::tree::Need::Write).await.map_err(code)?;
        if a.size.is_some_and(|n| n != node.size as u64) {
            return Err(StatusCode::OpUnsupported);
        }
        if let Some(times) = a.times {
            keep_mtime(&self.st, &node, times.modified).await?;
        }
        Ok(())
    }

    async fn set_open(&self, file: &OpenFile, a: &Attributes) -> Result<(), StatusCode> {
        let user = self.user(true).await?;
        let OpenFile::Write(s) = file else {
            return Err(StatusCode::PermissionDenied);
        };
        let mut s = s.lock().await;
        let s = &mut **s;
        metadata_supported(a)?;
        if let Some(times) = a.times {
            s.mtime = Some(times.modified);
        }
        if let Some(size) = a.size {
            let change = async {
                stage_size(&self.st, &user, s, size).await?;
                s.file.set_len(size).await.map_err(|_| StatusCode::Failure)
            }
            .await;
            if change.is_err() {
                s.failed = true;
            }
            change?;
        }
        Ok(())
    }

    async fn open_folder(&self, path: &[u8]) -> Result<OpenDir, StatusCode> {
        let user = self.user(false).await?;
        let path = segments(text(path)?)?;
        if !self.description(&user, &path).await?.is_dir() {
            return Err(StatusCode::Failure);
        }
        Ok(OpenDir { path, state: Default::default() })
    }

    async fn list_folder(&self, dir: &OpenDir) -> Result<Option<Vec<NameEntry>>, StatusCode> {
        let user = self.user(false).await?;
        let mut state = dir.state.lock().await;
        if state.done {
            return Ok(None);
        }
        let mut c = self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?;
        let found = paths::resolve(&mut c, &user, &dir.path).await.map_err(code)?.ok_or(StatusCode::NoSuchFile)?;
        let files = match found.target {
            Target::Root => {
                state.done = true;
                let mut files = paths::tops(&mut c, &user).await.map_err(code)?.spaces.into_iter().map(|(name, _)| entry(&name, attrs(None))).collect::<Vec<_>>();
                files.push(entry(paths::SHARED, attrs(None)));
                files
            }
            Target::Shared => {
                state.done = true;
                let shared = paths::tops(&mut c, &user).await.map_err(code)?.shared;
                drop(c);
                let mut files = Vec::with_capacity(shared.len());
                for (name, node) in shared {
                    files.push(entry(&name, file_attrs(&self.st, &node).await?));
                }
                files
            }
            Target::Node(n) => {
                drop(c);
                let (nodes, next) = access::list_page(&self.st, &user, &n.id, state.after.take()).await.map_err(code)?;
                state.done = next.is_none();
                state.after = next;
                let mut files = Vec::with_capacity(nodes.len());
                for node in nodes {
                    files.push(entry(&node.name, file_attrs(&self.st, &node).await?));
                }
                files
            }
        };
        Ok(if files.is_empty() { None } else { Some(files) })
    }

    async fn real_path(&self, path: &[u8]) -> Result<Bytes, StatusCode> {
        let user = self.user(false).await?;
        let path = segments(text(path)?)?;
        self.description(&user, &path).await?;
        Ok(Bytes::from(format!("/{}", path.join("/"))))
    }

    async fn make_folder(&self, path: &[u8]) -> Result<(), StatusCode> {
        let user = self.user(true).await?;
        let (parent, name) =
            access::parent_and_name(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, &segments(text(path)?)?).await.map_err(code)?;
        access::make_folder(&self.st, &user, &parent, &name).await.map_err(code)
    }

    async fn move_item(&self, old: &[u8], new: &[u8]) -> Result<(), StatusCode> {
        let user = self.user(true).await?;
        let source = self.node(&user, &segments(text(old)?)?).await?;
        if source.parent_id.is_none() {
            return Err(StatusCode::PermissionDenied);
        }
        let (parent, name) =
            access::parent_and_name(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, &segments(text(new)?)?).await.map_err(code)?;
        access::move_to(&self.st, &user, &source, &parent, &name).await.map_err(code)
    }
}

impl Filesystem for Files {
    type File = OpenFile;
    type Dir = OpenDir;

    async fn open(&self, path: &[u8], flags: OpenFlags, _: &Attributes) -> Result<OpenFile, FsError> {
        self.open_file(path, flags).await.map_err(fs)
    }
    async fn close_file(&self, file: OpenFile) -> Result<(), FsError> {
        self.close_open(file).await.map_err(fs)
    }
    async fn read(&self, file: &OpenFile, offset: u64, len: u32) -> Result<Option<Bytes>, FsError> {
        self.read_open(file, offset, len).await.map_err(fs)
    }
    async fn write(&self, file: &OpenFile, offset: u64, data: Bytes) -> Result<(), FsError> {
        self.write_open(file, offset, data).await.map_err(fs)
    }
    async fn fstat(&self, file: &OpenFile) -> Result<Attributes, FsError> {
        self.stat_open(file).await.map_err(fs)
    }
    async fn fsetstat(&self, file: &OpenFile, attrs: &Attributes) -> Result<(), FsError> {
        self.set_open(file, attrs).await.map_err(fs)
    }
    async fn stat(&self, path: &[u8]) -> Result<Attributes, FsError> {
        self.stat_path(path).await.map_err(fs)
    }
    async fn lstat(&self, path: &[u8]) -> Result<Attributes, FsError> {
        self.stat_path(path).await.map_err(fs)
    }
    async fn setstat(&self, path: &[u8], attrs: &Attributes) -> Result<(), FsError> {
        self.set_path(path, attrs).await.map_err(fs)
    }
    async fn open_dir(&self, path: &[u8]) -> Result<OpenDir, FsError> {
        self.open_folder(path).await.map_err(fs)
    }
    async fn read_dir(&self, dir: &OpenDir) -> Result<Option<Vec<NameEntry>>, FsError> {
        self.list_folder(dir).await.map_err(fs)
    }
    async fn remove(&self, path: &[u8]) -> Result<(), FsError> {
        self.delete(text(path).map_err(fs)?, false).await.map_err(fs)
    }
    async fn mkdir(&self, path: &[u8], _: &Attributes) -> Result<(), FsError> {
        self.make_folder(path).await.map_err(fs)
    }
    async fn rmdir(&self, path: &[u8]) -> Result<(), FsError> {
        self.delete(text(path).map_err(fs)?, true).await.map_err(fs)
    }
    async fn realpath(&self, path: &[u8]) -> Result<Bytes, FsError> {
        self.real_path(path).await.map_err(fs)
    }
    async fn rename(&self, from: &[u8], to: &[u8]) -> Result<(), FsError> {
        self.move_item(from, to).await.map_err(fs)
    }
    async fn read_link(&self, _: &[u8]) -> Result<Bytes, FsError> {
        Err(FsError::unsupported())
    }
    async fn symlink(&self, _: &[u8], _: &[u8]) -> Result<(), FsError> {
        Err(FsError::unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_stay_inside_the_virtual_tree() {
        assert_eq!(segments("./My files/a/../b").unwrap(), ["My files", "b"]);
        assert!(segments("../../etc/passwd").is_err());
        assert!(segments("/a/../../b").is_err());
        assert!(segments("a\\b").is_err());
        assert!(segments("a\0b").is_err());
    }
}
