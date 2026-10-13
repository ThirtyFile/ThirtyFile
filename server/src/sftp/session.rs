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
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode};
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub(super) struct Files {
    st: AppState,
    peer: SocketAddr,
    login: Login,
    handles: HashMap<String, Opened>,
    _permit: Arc<Permit>,
}
enum Opened {
    Read { id: String },
    Write(Staged),
    Directory { path: Vec<String>, after: Option<String>, done: bool },
}
struct Staged {
    path: PathBuf,
    file: tokio::fs::File,
    up: access::Upload,
    append: bool,
    readable: bool,
    failed: bool,
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
fn okay(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: String::new(), language_tag: "en".into() }
}
fn attrs(node: Option<&Node>) -> FileAttributes {
    let directory = node.is_none_or(Node::is_folder);
    FileAttributes {
        size: node.map(|n| n.size.max(0) as u64),
        permissions: Some(if directory { 0o040755 } else { 0o100644 }),
        mtime: node.map(|n| n.updated_at.clamp(0, i64::from(u32::MAX)) as u32),
        atime: node.map(|n| n.updated_at.clamp(0, i64::from(u32::MAX)) as u32),
        ..Default::default()
    }
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
        Self { st, peer, login, handles: HashMap::new(), _permit: permit }
    }
    async fn user(&self, write: bool) -> Result<User, StatusCode> {
        self.login.user(&self.st, self.peer, write).await.map_err(code)
    }
    fn insert(&mut self, id: u32, opened: Opened) -> Result<Handle, StatusCode> {
        if self.handles.len() >= 32 {
            return Err(StatusCode::Failure);
        }
        let handle = new_id();
        self.handles.insert(handle.clone(), opened);
        Ok(Handle { id, handle })
    }
    async fn node(&self, user: &User, path: &[String]) -> Result<Node, StatusCode> {
        access::item(&self.st, user, path).await.map_err(code)
    }
    async fn description(&self, user: &User, path: &[String]) -> Result<FileAttributes, StatusCode> {
        let found =
            paths::resolve(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, user, path).await.map_err(code)?.ok_or(StatusCode::NoSuchFile)?;
        Ok(match found.target {
            Target::Node(n) => attrs(Some(&n)),
            _ => attrs(None),
        })
    }
    async fn delete(&self, path: &str, directory: bool) -> Result<(), StatusCode> {
        let user = self.user(true).await?;
        let node = self.node(&user, &segments(path)?).await?;
        if node.parent_id.is_none() || node.is_folder() != directory {
            return Err(StatusCode::PermissionDenied);
        }
        if directory && !access::list_page(&self.st, &user, &node.id, None).await.map_err(code)?.0.is_empty() {
            return Err(StatusCode::Failure);
        }
        access::trash(&self.st, &user, &node.id).await.map_err(code)
    }
    async fn open_write(&self, user: &User, path: &[String], flags: OpenFlags) -> Result<Staged, StatusCode> {
        let (parent, name) = access::parent_and_name(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, user, path).await.map_err(code)?;
        let up = access::upload_to(&self.st, user, &parent, name).await.map_err(code)?;
        if up.existing.is_none() && !flags.contains(OpenFlags::CREATE) {
            return Err(StatusCode::NoSuchFile);
        }
        if up.existing.is_some() && flags.contains(OpenFlags::EXCLUDE) {
            return Err(StatusCode::Failure);
        }
        let path = self.st.tmp_dir().join(format!("sftp-{}", new_id()));
        let file = tokio::fs::OpenOptions::new().create_new(true).read(true).write(true).open(&path).await.map_err(|_| StatusCode::Failure)?;
        let mut stage = Staged { path, file, up, append: flags.contains(OpenFlags::APPEND), readable: flags.contains(OpenFlags::READ), failed: false };
        if !flags.contains(OpenFlags::TRUNCATE)
            && let Some(node) = &stage.up.existing
        {
            access::check_size(&self.st, node.size as u64).map_err(code)?;
            let source = crate::files::Source::resolve(&self.st, node).await.map_err(code)?;
            let (size, _) = source.describe(node.size as u64).await.map_err(code)?;
            access::check_size(&self.st, size).map_err(code)?;
            crate::util::DiskRoom::check(&self.st.tmp_dir(), size).await.map_err(code)?;
            let mut reader = source.open(&self.st, 0, size).await.map_err(|_| StatusCode::Failure)?;
            tokio::io::copy(&mut reader, &mut stage.file).await.map_err(|_| StatusCode::Failure)?;
        }
        Ok(stage)
    }
}

impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn open(&mut self, id: u32, filename: String, flags: OpenFlags, _: FileAttributes) -> Result<Handle, StatusCode> {
        if self.handles.len() >= 32 {
            return Err(StatusCode::Failure);
        }
        let write = flags.intersects(OpenFlags::WRITE | OpenFlags::APPEND | OpenFlags::TRUNCATE | OpenFlags::CREATE);
        let user = self.user(write).await?;
        let path = segments(&filename)?;
        let opened = if write {
            Opened::Write(self.open_write(&user, &path, flags).await?)
        } else {
            if !flags.contains(OpenFlags::READ) {
                return Err(StatusCode::BadMessage);
            }
            let node = self.node(&user, &path).await?;
            if node.is_folder() {
                return Err(StatusCode::Failure);
            }
            Opened::Read { id: node.id }
        };
        self.insert(id, opened)
    }
    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        let opened = self.handles.remove(&handle).ok_or(StatusCode::NoSuchFile)?;
        if let Opened::Write(mut stage) = opened {
            let user = self.user(true).await?;
            if stage.failed {
                return Err(StatusCode::Failure);
            }
            stage.file.flush().await.map_err(|_| StatusCode::Failure)?;
            stage.file.sync_data().await.map_err(|_| StatusCode::Failure)?;
            let size = stage.file.metadata().await.map_err(|_| StatusCode::Failure)?.len();
            access::check_size(&self.st, size).map_err(code)?;
            access::store(self.st.clone(), user, stage.up.parent.clone(), stage.up.name.clone(), stage.path.clone(), size, None).await.map_err(code)?;
        } else {
            self.user(false).await?;
        }
        Ok(okay(id))
    }
    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, StatusCode> {
        let user = self.user(false).await?;
        let data = match self.handles.get_mut(&handle).ok_or(StatusCode::NoSuchFile)? {
            Opened::Read { id } => access::read_at(&self.st, &user, id, offset, len).await.map_err(code)?,
            Opened::Write(s) if s.readable => {
                crate::tree::folder_for(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, &s.up.parent.id, crate::tree::Need::Read)
                    .await
                    .map_err(code)?;
                s.file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|_| StatusCode::Failure)?;
                let mut data = vec![0; len.min(256 * 1024) as usize];
                let n = s.file.read(&mut data).await.map_err(|_| StatusCode::Failure)?;
                data.truncate(n);
                data
            }
            _ => return Err(StatusCode::PermissionDenied),
        };
        if data.is_empty() {
            return Err(StatusCode::Eof);
        }
        Ok(Data { id, data })
    }
    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, StatusCode> {
        let user = self.user(true).await?;
        let Some(Opened::Write(s)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::PermissionDenied);
        };
        if s.failed || data.len() > 256 * 1024 {
            return Err(StatusCode::Failure);
        }
        let result = async {
            let before = s.file.metadata().await.map_err(|_| StatusCode::Failure)?.len();
            let offset = if s.append { before } else { offset };
            let size = before.max(offset.checked_add(data.len() as u64).ok_or(StatusCode::Failure)?);
            access::check_size(&self.st, size).map_err(code)?;
            let up = access::upload_to(&self.st, &user, &s.up.parent, s.up.name.clone()).await.map_err(code)?;
            crate::tree::check_quota(
                &mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?,
                up.parent.drive(),
                size as i64 - up.existing.as_ref().map_or(0, |n| n.size),
            )
            .await
            .map_err(code)?;
            crate::util::DiskRoom::check(&self.st.tmp_dir(), size.saturating_sub(before)).await.map_err(code)?;
            s.file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|_| StatusCode::Failure)?;
            s.file.write_all(&data).await.map_err(|_| StatusCode::Failure)
        }
        .await;
        if result.is_err() {
            s.failed = true;
        }
        result?;
        Ok(okay(id))
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let user = self.user(false).await?;
        Ok(Attrs { id, attrs: self.description(&user, &segments(&path)?).await? })
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat(id, path).await
    }
    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        let user = self.user(false).await?;
        let a = match self.handles.get(&handle).ok_or(StatusCode::NoSuchFile)? {
            Opened::Read { id } => attrs(Some(
                &crate::tree::node_for(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, id, crate::tree::Need::Read)
                    .await
                    .map_err(code)?,
            )),
            Opened::Write(s) => {
                let mut a = attrs(s.up.existing.as_ref());
                a.size = Some(s.file.metadata().await.map_err(|_| StatusCode::Failure)?.len());
                a.permissions = Some(0o100644);
                a
            }
            Opened::Directory { path, .. } => self.description(&user, path).await?,
        };
        Ok(Attrs { id, attrs: a })
    }
    async fn setstat(&mut self, id: u32, path: String, a: FileAttributes) -> Result<Status, StatusCode> {
        let user = self.user(true).await?;
        let node = self.node(&user, &segments(&path)?).await?;
        crate::tree::node_for(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, &node.id, crate::tree::Need::Write).await.map_err(code)?;
        if a.size.is_some_and(|n| n != node.size as u64) {
            return Err(StatusCode::OpUnsupported);
        }
        Ok(okay(id))
    }
    async fn fsetstat(&mut self, id: u32, handle: String, a: FileAttributes) -> Result<Status, StatusCode> {
        self.user(true).await?;
        let Some(Opened::Write(s)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::PermissionDenied);
        };
        if let Some(size) = a.size {
            access::check_size(&self.st, size).map_err(code)?;
            s.file.set_len(size).await.map_err(|_| StatusCode::Failure)?;
        }
        Ok(okay(id))
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        let user = self.user(false).await?;
        let path = segments(&path)?;
        if !self.description(&user, &path).await?.is_dir() {
            return Err(StatusCode::Failure);
        }
        self.insert(id, Opened::Directory { path, after: None, done: false })
    }
    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        let user = self.user(false).await?;
        let Some(Opened::Directory { path, after, done }) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        if *done {
            return Err(StatusCode::Eof);
        }
        let mut c = self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?;
        let found = paths::resolve(&mut c, &user, path).await.map_err(code)?.ok_or(StatusCode::NoSuchFile)?;
        let files = match found.target {
            Target::Root => {
                *done = true;
                let mut files =
                    paths::tops(&mut c, &user).await.map_err(code)?.spaces.into_iter().map(|(name, _)| File::new(&name, attrs(None))).collect::<Vec<_>>();
                files.push(File::new(paths::SHARED, attrs(None)));
                files
            }
            Target::Shared => {
                *done = true;
                paths::tops(&mut c, &user).await.map_err(code)?.shared.into_iter().map(|(name, n)| File::new(&name, attrs(Some(&n)))).collect()
            }
            Target::Node(n) => {
                drop(c);
                let (nodes, next) = access::list_page(&self.st, &user, &n.id, after.take()).await.map_err(code)?;
                *done = next.is_none();
                *after = next;
                nodes.iter().map(|n| File::new(&n.name, attrs(Some(n)))).collect()
            }
        };
        if files.is_empty() {
            return Err(StatusCode::Eof);
        }
        Ok(Name { id, files })
    }
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        let user = self.user(false).await?;
        let path = segments(&path)?;
        self.description(&user, &path).await?;
        Ok(Name { id, files: vec![File::dummy(&format!("/{}", path.join("/")))] })
    }
    async fn mkdir(&mut self, id: u32, path: String, _: FileAttributes) -> Result<Status, StatusCode> {
        let user = self.user(true).await?;
        let (parent, name) =
            access::parent_and_name(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, &segments(&path)?).await.map_err(code)?;
        access::make_folder(&self.st, &user, &parent, &name).await.map_err(code)?;
        Ok(okay(id))
    }
    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.delete(&path, false).await?;
        Ok(okay(id))
    }
    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.delete(&path, true).await?;
        Ok(okay(id))
    }
    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, StatusCode> {
        let user = self.user(true).await?;
        let source = self.node(&user, &segments(&old)?).await?;
        if source.parent_id.is_none() {
            return Err(StatusCode::PermissionDenied);
        }
        let (parent, name) =
            access::parent_and_name(&mut *self.st.db.acquire().await.map_err(|_| StatusCode::Failure)?, &user, &segments(&new)?).await.map_err(code)?;
        access::move_to(&self.st, &user, &source, &parent, &name).await.map_err(code)?;
        Ok(okay(id))
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
