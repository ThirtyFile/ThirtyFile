use std::time::{SystemTime, UNIX_EPOCH};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rand::{RngExt, distr::Alphanumeric};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Whether `s` is an id as `new_id` makes them: 32 lowercase hexadecimal digits
pub fn is_new_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn random_token(len: usize) -> String {
    rand::rng().sample_iter(Alphanumeric).take(len).map(char::from).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Validates and normalizes a file name. Rejects path separators and characters Windows doesn't allow, so ZIPs extract on every platform.
pub fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("Name can't be blank"));
    }
    if name.len() > 255 {
        return Err(AppError::bad_request("Name is too long"));
    }
    if name == "." || name == ".." {
        return Err(AppError::bad_request("Invalid name"));
    }
    if let Some(c) = name.chars().find(|c| c.is_control() || r#"\/:*?"<>|"#.contains(*c)) {
        let shown = if c.is_control() { "control characters".to_string() } else { c.to_string() };
        return Err(AppError::bad_request(format!("Name can't contain {shown}")));
    }
    // Windows can't create these, so a downloaded ZIP wouldn't extract there
    if name.ends_with('.') {
        return Err(AppError::bad_request("Name can't end with a period"));
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit() && stem.as_bytes()[3] != b'0');
    if reserved {
        return Err(AppError::bad_request("This name is reserved by Windows"));
    }
    Ok(name.to_string())
}

pub fn guess_mime(name: &str) -> String {
    mime_guess::from_path(name).first_or_octet_stream().essence_str().to_string()
}

const RFC5987: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// `attachment` / `inline` Content-Disposition with both an ASCII fallback file name and a UTF-8 file name.
pub fn content_disposition(kind: &str, name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' })
        .collect();
    format!("{kind}; filename=\"{fallback}\"; filename*=UTF-8''{}", utf8_percent_encode(name, RFC5987))
}

/// Escapes `\`, `%` and `_` for a `LIKE` pattern (the query says `ESCAPE '\'`)
pub fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Days since 1970-01-01 → (year, month, day) in the Gregorian calendar (Howard Hinnant's civil_from_days)
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// A name's stem and extension (with the dot); folders have no extension
pub fn split_name(name: &str, is_folder: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if !is_folder && i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Compares names the way File Explorer sorts them: letter case is ignored in every language, and runs of digits
/// compare by their value, so "File 2" comes before "File 10". Names that only differ in case or leading zeros still
/// get a fixed order, so sorting is stable.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (la, lb) = (a.to_lowercase(), b.to_lowercase());
    let (mut x, mut y) = (la.chars().peekable(), lb.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = it.next_if(char::is_ascii_digit) {
                        digits.push(c);
                    }
                    digits
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (m, n) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let ord = m.len().cmp(&n.len()).then_with(|| m.cmp(n));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(c), Some(d)) => {
                if c != d {
                    return c.cmp(&d);
                }
                x.next();
                y.next();
            }
        }
    }
    la.cmp(&lb).then_with(|| a.cmp(b))
}

/// Generates a non-conflicting name: "report.pdf" → "report (1).pdf"
pub fn numbered_name(name: &str, n: u32, is_folder: bool) -> String {
    let (stem, ext) = split_name(name, is_folder);
    format!("{stem} ({n}){ext}")
}

/// Work that may not come back (a call to a disk or network share that stopped answering), by what it is waiting for
static WAITING: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Marks `key` as waited for until dropped; None when it is already
struct Waiting(String);

impl Waiting {
    fn claim(key: String) -> Option<Waiting> {
        let claimed = WAITING.lock().unwrap().insert(key.clone());
        claimed.then(|| Waiting(key))
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITING.lock().unwrap().remove(&self.0);
    }
}

/// Runs a blocking step on a blocking thread and waits up to `wait` for it; None when it didn't answer by then, or
/// when a step for the same `key` still hasn't answered: a disk that hangs ties up one thread, not one more per call
pub async fn blocking_within<T: Send + 'static>(key: String, wait: std::time::Duration, step: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let claimed = Waiting::claim(key)?;
    let task = tokio::task::spawn_blocking(move || {
        let r = step();
        drop(claimed);
        r
    });
    tokio::time::timeout(wait, task).await.ok()?.ok()
}

/// `blocking_within` for work that is a future (a check of a storage service): it runs as a task of its own, so it
/// finishes (and lets `key` go) whether or not anyone still waits for it
pub async fn within<T: Send + 'static>(key: String, wait: std::time::Duration, work: impl Future<Output = T> + Send + 'static) -> Option<T> {
    let claimed = Waiting::claim(key)?;
    let task = tokio::spawn(async move {
        let r = work.await;
        drop(claimed);
        r
    });
    tokio::time::timeout(wait, task).await.ok()?.ok()
}

/// `disk_space`, on a blocking thread and within a few seconds (None when the disk doesn't answer)
pub async fn disk_space_soon(path: &std::path::Path) -> Option<(u64, u64)> {
    let p = path.to_path_buf();
    blocking_within(format!("disk space of {}", path.display()), std::time::Duration::from_secs(3), move || disk_space(&p)).await.flatten()
}

/// Free and total bytes of the file system holding `path` (what an unprivileged user may still write)
#[cfg(unix)]
pub fn disk_space(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a writable statvfs
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let block = st.f_frsize as u64;
    Some((st.f_bavail as u64 * block, st.f_blocks as u64 * block))
}

/// Free and total bytes of the disk holding `path` (what this user may still write), for development builds on Windows
#[cfg(windows)]
pub fn disk_space(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut free, mut total, mut all_free) = (0u64, 0u64, 0u64);
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 path and the three outputs are writable u64s
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut all_free) } == 0 {
        return None;
    }
    Some((free, total))
}

#[cfg(not(any(unix, windows)))]
pub fn disk_space(_path: &std::path::Path) -> Option<(u64, u64)> {
    None
}

/// Memory the server may use: the container's limit (cgroup v2 or v1) when there is one, else the computer's memory
pub fn memory_limit() -> Option<u64> {
    let read = |p: &str| std::fs::read_to_string(p).ok();
    let cgroup = read("/sys/fs/cgroup/memory.max")
        .or_else(|| read("/sys/fs/cgroup/memory/memory.limit_in_bytes"))
        .and_then(|v| v.trim().parse::<u64>().ok())
        // "max", or v1's "no limit" (a number near u64::MAX)
        .filter(|&v| v < 1 << 60);
    let total = read("/proc/meminfo").and_then(|m| {
        let line = m.lines().find(|l| l.starts_with("MemTotal:"))?;
        line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb * 1024)
    });
    match (cgroup, total) {
        (Some(c), Some(t)) => Some(c.min(t)),
        (c, t) => c.or(t),
    }
}

pub fn format_bytes_u64(bytes: u64) -> String {
    format_bytes(i64::try_from(bytes).unwrap_or(i64::MAX))
}

/// Converts bytes to a human-readable size, e.g. 10 GB, 512 MB (for the activity log)
pub fn format_bytes(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if (v - v.round()).abs() < 0.05 { format!("{} {}", v.round() as i64, UNITS[i]) } else { format!("{v:.1} {}", UNITS[i]) }
}

/// Whether a folder (with links resolved) is one of the operating system's own, is inside one, or holds one (the root
/// of the disk): the system's settings, secrets such as `/run/secrets`, and programs are no folder to show as a space
pub fn system_folder(real: &std::path::Path) -> bool {
    #[cfg(unix)]
    let system: Vec<std::path::PathBuf> =
        ["/etc", "/run", "/var/run", "/var/lib", "/proc", "/sys", "/dev", "/boot", "/root", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/usr"]
            .iter()
            .map(std::path::PathBuf::from)
            .collect();
    #[cfg(not(unix))]
    let system: Vec<std::path::PathBuf> = ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData"]
        .iter()
        .filter_map(std::env::var_os)
        .map(std::path::PathBuf::from)
        .collect();
    let real = real.to_string_lossy();
    // Windows: compared without the `\\?\` form and letter case
    let real = std::path::PathBuf::from(real.strip_prefix(r"\\?\").unwrap_or(&real).to_lowercase());
    system.iter().map(|s| std::path::PathBuf::from(s.to_string_lossy().to_lowercase())).any(|s| real.starts_with(&s) || s.starts_with(&real))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn a_disk_that_doesnt_answer_ties_up_one_thread_and_is_asked_again_once_it_answers() {
        let key = || format!("test disk {:?}", std::thread::current().id());
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let started = Instant::now();
        // It doesn't answer: given up after the wait
        assert_eq!(super::blocking_within(key(), Duration::from_millis(100), move || wait.recv().is_ok()).await, None);
        assert!(started.elapsed() < Duration::from_secs(1));
        // Asked again meanwhile: not asked at all (no second thread waits for it)
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let a = asked.clone();
        let again = Instant::now();
        assert_eq!(super::blocking_within(key(), Duration::from_secs(5), move || a.store(true, std::sync::atomic::Ordering::SeqCst)).await, None);
        assert!(again.elapsed() < Duration::from_millis(100) && !asked.load(std::sync::atomic::Ordering::SeqCst));
        // Once it has answered, it is asked again
        go.send(()).unwrap();
        let mut answered = None;
        for _ in 0..100 {
            answered = super::blocking_within(key(), Duration::from_secs(1), || 7).await;
            if answered.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(answered, Some(7));
        // The same for a check that is a future (a storage service that doesn't answer)
        let key = format!("test service {:?}", std::thread::current().id());
        assert_eq!(super::within(key.clone(), Duration::from_millis(50), std::future::pending::<()>()).await, None);
        let again = Instant::now();
        assert_eq!(super::within(key, Duration::from_secs(5), async { 1 }).await, None);
        assert!(again.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn system_folders_are_told_apart() {
        use std::path::Path;
        #[cfg(unix)]
        {
            for system in ["/etc", "/run/secrets", "/", "/var", "/usr/share"] {
                assert!(super::system_folder(Path::new(system)), "{system}");
            }
            for other in ["/mnt/nas", "/srv/files", "/home/amy/shared", "/tmp/x", "/var/www"] {
                assert!(!super::system_folder(Path::new(other)), "{other}");
            }
        }
        #[cfg(windows)]
        {
            let root = std::env::var("SystemRoot").unwrap();
            assert!(super::system_folder(Path::new(&root)));
            assert!(super::system_folder(Path::new(&format!(r"\\?\{root}\System32"))));
            assert!(super::system_folder(Path::new(&root[..3])), "the drive holding the system");
            assert!(!super::system_folder(Path::new(r"D:\Shared")));
        }
    }

    #[test]
    fn dates_and_like_patterns() {
        assert_eq!(super::civil_from_days(0), (1970, 1, 1));
        assert_eq!(super::civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(super::civil_from_days(-1), (1969, 12, 31));
        assert_eq!(super::like_escape(r"50%_off\x"), r"50\%\_off\\x");
    }

    #[test]
    fn natural_order_compares_numbers_by_value_and_ignores_case() {
        let mut names = vec!["File 10.txt", "file 2.txt", "File 1.txt", "Été", "abc", "ÉTÉ 3", "File 02.txt", "B"];
        names.sort_by(|a, b| super::natural_cmp(a, b));
        assert_eq!(names, ["abc", "B", "File 1.txt", "File 02.txt", "file 2.txt", "File 10.txt", "Été", "ÉTÉ 3"]);
        assert_eq!(super::natural_cmp("a", "A"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn names_windows_cannot_create_are_rejected() {
        use super::validate_name;
        for bad in ["CON", "con.txt", "Nul", "COM1", "lpt9.log", "file.", "trailing. "] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        for ok in ["COM0", "COM10", "CONsole", "console.txt", "aux-files", "file.txt", "LPT"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn format_bytes() {
        assert_eq!(super::format_bytes(10 * 1024 * 1024 * 1024), "10 GB");
        assert_eq!(super::format_bytes(1536 * 1024 * 1024), "1.5 GB");
        assert_eq!(super::format_bytes(500), "500 B");
    }
}
