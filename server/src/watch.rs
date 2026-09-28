//! Watching folder spaces on local disks (Linux inotify), so changes made outside ThirtyFile show up within seconds
//! instead of at the next regular scan.
//!
//! - Each folder of the space is watched; a changed folder is checked (`folders::sync_folder`) once nothing has
//!   changed in it for a little longer than files need to settle, so files still being written are indexed too.
//! - Network file systems (NFS, SMB/CIFS) don't report changes made by other computers, so they aren't watched: the
//!   regular scan keeps them up to date.
//! - When the event queue overflows, or the number of watches allowed (`fs.inotify.max_user_watches`) is reached, the
//!   spaces fall back to full scans, and the log says so.
//! - All spaces share one thread and one inotify instance.

use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::state::AppState;

/// A folder is checked this long after its last change (a little longer than files are left to settle)
const QUIET: Duration = Duration::from_secs(12);
/// More changed folders than this at once: one full scan instead
const MAX_FOLDERS: usize = 100;

const WATCH_MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_CLOSE_WRITE
    | libc::IN_ATTRIB
    | libc::IN_DELETE_SELF
    | libc::IN_ONLYDIR;

static CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// A folder space was added, changed or removed: the watchers are updated right away
pub fn spaces_changed() {
    CHANGED.notify_one();
}

/// What the watching thread is told: the folder spaces to watch now (id → folder)
type Spaces = HashMap<String, PathBuf>;

/// Watches every folder space from one thread with one inotify instance, since every user has a folder space and
/// Linux allows few instances per user (`fs.inotify.max_user_instances`, often 128). The list of spaces is read every
/// minute, and on `spaces_changed`.
pub fn spawn_watchers(st: AppState) {
    let handle = tokio::runtime::Handle::current();
    let (tx, rx) = std::sync::mpsc::channel::<Spaces>();
    {
        let st = st.clone();
        if let Err(e) = std::thread::Builder::new().name("thirtyfile-watch".into()).spawn(move || watch(st, handle, rx)) {
            tracing::warn!("Can't watch folder spaces for changes: {e}");
            return;
        }
    }
    tokio::spawn(async move {
        loop {
            let spaces: Vec<(String, String)> =
                sqlx::query_as("SELECT id, source_path FROM drives WHERE mode = 'folder' AND disabled = 0 AND source_path IS NOT NULL")
                    .fetch_all(&st.db)
                    .await
                    .unwrap_or_default();
            if tx.send(spaces.into_iter().map(|(id, p)| (id, PathBuf::from(p))).collect()).is_err() {
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                _ = CHANGED.notified() => {}
            }
        }
    });
}

/// File systems whose changes by other computers inotify doesn't see
fn network_fs(path: &Path) -> Option<&'static str> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid path and a writable statfs
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    match st.f_type as i64 {
        0x6969 => Some("NFS"),
        0x517B => Some("SMB"),
        0xFF534D42 => Some("CIFS"),
        0xFE534D42 => Some("SMB2"),
        _ => None,
    }
}

struct Watcher {
    fd: libc::c_int,
    /// Watch descriptor → (space, folder path below the space's folder)
    dirs: HashMap<libc::c_int, (String, String)>,
    /// The watched spaces and their folders
    roots: HashMap<String, PathBuf>,
    /// Watches could not all be added: rely on full scans
    limited: bool,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by this watcher
        unsafe { libc::close(self.fd) };
    }
}

impl Watcher {
    /// Watches `rel` of a space and every folder below it
    fn add_tree(&mut self, drive: &str, rel: &str) {
        let Some(root) = self.roots.get(drive).cloned() else { return };
        let Ok(pinned) = crate::beneath::Pinned::root(&root) else { return };
        let mut queue = vec![rel.to_string()];
        while let Some(rel) = queue.pop() {
            // Reached without following a symbolic link on the way, so a folder swapped for a link isn't watched
            let Ok(dir) = (if rel.is_empty() { Ok(pinned.clone()) } else { pinned.join(&rel).and_then(|d| d.dir()) }) else { continue };
            let path = dir.as_path();
            let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { continue };
            // SAFETY: an open inotify descriptor and a valid path; IN_DONT_FOLLOW leaves symbolic links alone
            let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), WATCH_MASK | libc::IN_DONT_FOLLOW) };
            if wd < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSPC) && !self.limited {
                    self.limited = true;
                    tracing::warn!(
                        "Can't watch every folder of the folder spaces (the limit fs.inotify.max_user_watches is reached, at {}): changes there are found by the regular scan",
                        root.join(&rel).display()
                    );
                }
                continue;
            }
            // A folder reached twice (a bind mount inside the space, say) keeps its first owner
            self.dirs.entry(wd).or_insert_with(|| (drive.to_string(), rel.clone()));
            let Ok(read) = std::fs::read_dir(path) else { continue };
            for item in read.flatten() {
                let Ok(name) = item.file_name().into_string() else { continue };
                if crate::folders::ignored(&name) || !item.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                queue.push(if rel.is_empty() { name } else { format!("{rel}/{name}") });
            }
        }
    }

    /// Starts and stops watching spaces so the watched ones are `wanted`
    fn update(&mut self, wanted: Spaces) {
        let gone: Vec<String> = self.roots.iter().filter(|(id, path)| wanted.get(*id) != Some(*path)).map(|(id, _)| id.clone()).collect();
        for id in &gone {
            self.roots.remove(id);
            let wds: Vec<libc::c_int> = self.dirs.iter().filter(|(_, (d, _))| d == id).map(|(wd, _)| *wd).collect();
            for wd in wds {
                self.dirs.remove(&wd);
                // SAFETY: a watch descriptor of this instance
                unsafe { libc::inotify_rm_watch(self.fd, wd) };
            }
        }
        for (id, path) in wanted {
            if self.roots.contains_key(&id) {
                continue;
            }
            if let Some(kind) = network_fs(&path) {
                tracing::info!("{} is on {kind}: changes made there by other computers are found by the regular scan", path.display());
                continue;
            }
            self.roots.insert(id.clone(), path);
            self.add_tree(&id, "");
        }
    }
}

fn watch(st: AppState, handle: tokio::runtime::Handle, rx: std::sync::mpsc::Receiver<Spaces>) {
    // SAFETY: plain system call
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if fd < 0 {
        tracing::warn!("Can't watch folder spaces for changes: {}", std::io::Error::last_os_error());
        return;
    }
    let mut w = Watcher { fd, dirs: HashMap::new(), roots: HashMap::new(), limited: false };
    // (space, folder) → last change
    let mut changed: HashMap<(String, String), Instant> = HashMap::new();
    let mut overflow = false;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match rx.try_recv() {
            Ok(wanted) => w.update(wanted),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd
        let ready = unsafe { libc::poll(&mut pfd, 1, 1000) };
        if ready > 0 {
            // SAFETY: reading into our buffer from the inotify descriptor
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            let mut off = 0usize;
            while n > 0 && off + std::mem::size_of::<libc::inotify_event>() <= n as usize {
                // SAFETY: the kernel writes whole events; the header is read unaligned from the buffer
                let ev: libc::inotify_event = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off).cast()) };
                let name_start = off + std::mem::size_of::<libc::inotify_event>();
                let name_bytes = &buf[name_start..name_start + ev.len as usize];
                let name = String::from_utf8_lossy(name_bytes.split(|b| *b == 0).next().unwrap_or_default()).into_owned();
                off = name_start + ev.len as usize;
                if ev.mask & libc::IN_Q_OVERFLOW != 0 {
                    overflow = true;
                    continue;
                }
                if ev.mask & libc::IN_IGNORED != 0 {
                    w.dirs.remove(&ev.wd);
                    continue;
                }
                let Some((drive, dir)) = w.dirs.get(&ev.wd).cloned() else { continue };
                if !name.is_empty() && crate::folders::ignored(&name) {
                    continue;
                }
                if ev.mask & libc::IN_ISDIR != 0 && ev.mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                    w.add_tree(&drive, &if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") });
                }
                changed.insert((drive, dir), Instant::now());
            }
        } else if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            break;
        }
        // Events were lost: every watched space is checked in full
        if overflow {
            overflow = false;
            changed.clear();
            tracing::info!("Too many changes at once in the folder spaces: checking them in full");
            for id in w.roots.keys() {
                let (st, id) = (st.clone(), id.clone());
                handle.spawn(async move { crate::folders::scan_later(&st, &id) });
            }
            continue;
        }
        // Folders quiet long enough are checked, per space
        let mut due: HashMap<String, Vec<String>> = HashMap::new();
        changed.retain(|(drive, dir), t| {
            if t.elapsed() < QUIET {
                return true;
            }
            due.entry(drive.clone()).or_default().push(dir.clone());
            false
        });
        for (drive, dirs) in due {
            let st = st.clone();
            if dirs.len() > MAX_FOLDERS {
                handle.spawn(async move { crate::folders::scan_later(&st, &drive) });
            } else {
                handle.spawn(async move { check_folders(&st, &drive, dirs).await });
            }
        }
    }
}

async fn check_folders(st: &AppState, drive_id: &str, dirs: Vec<String>) {
    let dirs: HashSet<String> = dirs.into_iter().collect();
    for rel in dirs {
        let node: Option<crate::tree::Node> = {
            let sql = format!("SELECT {} FROM nodes n WHERE n.drive_id = ? AND n.fs_path = ? AND n.kind = 'folder' AND n.trashed_at IS NULL", crate::tree::NODE_COLS);
            sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str())).bind(drive_id).bind(&rel).fetch_optional(&st.db).await.ok().flatten()
        };
        match node {
            Some(folder) => crate::folders::sync_folder(st, &folder).await,
            // Not indexed yet (a new folder inside a new folder): the whole space is checked
            None => crate::folders::scan_later(st, drive_id),
        }
    }
}
