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

/// Generates a non-conflicting name: "report.pdf" → "report (1).pdf"
/// A name's stem and extension (with the dot); folders have no extension
pub fn split_name(name: &str, is_folder: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if !is_folder && i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

pub fn numbered_name(name: &str, n: u32, is_folder: bool) -> String {
    let (stem, ext) = split_name(name, is_folder);
    format!("{stem} ({n}){ext}")
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
