//! Watching folder spaces on local disks (Linux inotify), so changes made outside ThirtyFile show up within seconds
//! instead of at the next regular scan.
//!
//! - Each folder of the space is watched; a changed folder is checked (`folders::sync_folder`) once nothing has
//!   changed in it for a little longer than files need to settle, so files still being written are indexed too.
//! - Network file systems (NFS, SMB/CIFS) don't report changes made by other computers, so they aren't watched: the
//!   regular scan keeps them up to date.
//! - When the event queue overflows, or the number of watches allowed (`fs.inotify.max_user_watches`) is reached, the
//!   space falls back to full scans, and the log says so.

use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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

/// Starts and stops watchers as folder spaces come and go (checked every minute, and on `spaces_changed`)
pub fn spawn_watchers(st: AppState) {
    let handle = tokio::runtime::Handle::current();
    tokio::spawn(async move {
        let mut running: HashMap<String, (PathBuf, Arc<AtomicBool>)> = HashMap::new();
        loop {
            let spaces: Vec<(String, String)> =
                sqlx::query_as("SELECT id, source_path FROM drives WHERE mode = 'folder' AND disabled = 0 AND source_path IS NOT NULL")
                    .fetch_all(&st.db)
                    .await
                    .unwrap_or_default();
            let wanted: HashMap<String, PathBuf> = spaces.into_iter().map(|(id, p)| (id, PathBuf::from(p))).collect();
            running.retain(|id, (path, stop)| {
                let keep = wanted.get(id) == Some(path);
                if !keep {
                    stop.store(true, Ordering::Relaxed);
                }
                keep
            });
            for (id, path) in wanted {
                if running.contains_key(&id) {
                    continue;
                }
                let stop = Arc::new(AtomicBool::new(false));
                running.insert(id.clone(), (path.clone(), stop.clone()));
                let (st, handle) = (st.clone(), handle.clone());
                std::thread::Builder::new()
                    .name("thirtyfile-watch".into())
                    .spawn(move || watch(st, handle, id, path, stop))
                    .ok();
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
    /// Watch descriptor → folder path below the space's folder
    dirs: HashMap<libc::c_int, String>,
    root: PathBuf,
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
    /// Watches `rel` and every folder below it
    fn add_tree(&mut self, rel: &str) {
        let mut queue = vec![rel.to_string()];
        while let Some(rel) = queue.pop() {
            let path = if rel.is_empty() { self.root.clone() } else { self.root.join(&rel) };
            let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { continue };
            // SAFETY: an open inotify descriptor and a valid path; IN_DONT_FOLLOW leaves symbolic links alone
            let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), WATCH_MASK | libc::IN_DONT_FOLLOW) };
            if wd < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSPC) && !self.limited {
                    self.limited = true;
                    tracing::warn!(
                        "Can't watch every folder of {} (the limit fs.inotify.max_user_watches is reached): changes there are found by the regular scan",
                        self.root.display()
                    );
                }
                continue;
            }
            self.dirs.insert(wd, rel.clone());
            let Ok(read) = std::fs::read_dir(&path) else { continue };
            for item in read.flatten() {
                let Ok(name) = item.file_name().into_string() else { continue };
                if crate::folders::ignored(&name) || !item.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                queue.push(if rel.is_empty() { name } else { format!("{rel}/{name}") });
            }
        }
    }
}

fn watch(st: AppState, handle: tokio::runtime::Handle, drive_id: String, root: PathBuf, stop: Arc<AtomicBool>) {
    if let Some(kind) = network_fs(&root) {
        tracing::info!("{} is on {kind}: changes made there by other computers are found by the regular scan", root.display());
        return;
    }
    // SAFETY: plain system call
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if fd < 0 {
        tracing::warn!("Can't watch {} for changes: {}", root.display(), std::io::Error::last_os_error());
        return;
    }
    let mut w = Watcher { fd, dirs: HashMap::new(), root: root.clone(), limited: false };
    w.add_tree("");
    let mut changed: HashMap<String, Instant> = HashMap::new();
    let mut rescan = false;
    let mut buf = vec![0u8; 64 * 1024];
    while !stop.load(Ordering::Relaxed) {
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
                    rescan = true;
                    continue;
                }
                if ev.mask & libc::IN_IGNORED != 0 {
                    w.dirs.remove(&ev.wd);
                    continue;
                }
                let Some(dir) = w.dirs.get(&ev.wd).cloned() else { continue };
                if !name.is_empty() && crate::folders::ignored(&name) {
                    continue;
                }
                if ev.mask & libc::IN_ISDIR != 0 && ev.mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                    w.add_tree(&if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") });
                }
                changed.insert(dir, Instant::now());
            }
        } else if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            break;
        }
        // Folders quiet long enough are checked
        let due: Vec<String> = changed.iter().filter(|(_, t)| t.elapsed() >= QUIET).map(|(d, _)| d.clone()).collect();
        if rescan || due.len() > MAX_FOLDERS {
            if rescan {
                tracing::info!("Too many changes at once in {}: checking the whole folder", root.display());
            }
            changed.clear();
            rescan = false;
            let (st, id) = (st.clone(), drive_id.clone());
            handle.spawn(async move { crate::folders::scan_later(&st, &id) });
            continue;
        }
        if due.is_empty() {
            continue;
        }
        for d in &due {
            changed.remove(d);
        }
        let (st, id) = (st.clone(), drive_id.clone());
        handle.spawn(async move { check_folders(&st, &id, due).await });
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
