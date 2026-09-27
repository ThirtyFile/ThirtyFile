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

#[cfg(test)]
mod tests {
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
