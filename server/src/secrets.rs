//! Encryption of the secrets kept in the database: the session and share signing secret, single sign-on client
//! secrets, and the passwords and keys of storage locations. A copy of `drive.db` on its own (a backup made with
//! `thirtyfile backup`, a copied file) then gives none of them away.
//!
//! The key comes from `THIRTYFILE_SECRET_KEY` (64 hex characters or base64 of 32 bytes), from the file in
//! `THIRTYFILE_SECRET_KEY_FILE`, or from `secret.key` in the data folder, created on first start. Kept in the data
//! folder, it is backed up with the database when the whole folder is: the backup guide says to back it up
//! separately, or to keep it elsewhere with one of the two settings.
//!
//! Values are stored as `enc:v1:<base64 of nonce and ciphertext>` (AES-256-GCM). Values without that prefix were
//! written before encryption existed: they are read as they are, and encrypted on the next start.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, Generate, KeyInit},
};
use base64::{Engine, engine::general_purpose::STANDARD as B64};

const PREFIX: &str = "enc:v1:";
const NONCE_LEN: usize = 12;

static CIPHER: OnceLock<Aes256Gcm> = OnceLock::new();

/// Where the key comes from
pub enum KeySource {
    Env(String),
    File(PathBuf),
}

impl KeySource {
    /// From the settings; `data_dir/secret.key` when neither is set
    pub fn from_settings(key: Option<String>, file: Option<PathBuf>, data_dir: &Path) -> Self {
        match (key.filter(|k| !k.trim().is_empty()), file) {
            (Some(k), _) => KeySource::Env(k),
            (None, Some(f)) => KeySource::File(f),
            (None, None) => KeySource::File(data_dir.join("secret.key")),
        }
    }

    /// Reads the key; a key file that doesn't exist yet is created with a new random key (readable by its owner only)
    pub fn load(&self) -> Result<[u8; 32], String> {
        match self {
            KeySource::Env(k) => parse_key(k).ok_or_else(|| "THIRTYFILE_SECRET_KEY must be 64 hex characters or base64 of 32 bytes".into()),
            KeySource::File(path) => match std::fs::read_to_string(path) {
                Ok(text) => parse_key(&text).ok_or_else(|| format!("{} doesn't hold a valid key (64 hex characters)", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let key: [u8; 32] = rand::random();
                    write_key_file(path, &key)?;
                    tracing::info!("Created the key that encrypts saved passwords in {}. Back it up separately from the database.", path.display());
                    Ok(key)
                }
                Err(e) => Err(format!("Can't read {}: {e}", path.display())),
            },
        }
    }
}

pub fn write_key_file(path: &Path, key: &[u8; 32]) -> Result<(), String> {
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        std::io::Write::write_all(&mut opts.open(&tmp)?, format!("{}\n", hex::encode(key)).as_bytes())?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| format!("Can't write {}: {e}", path.display()))
}

fn parse_key(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    let bytes = hex::decode(text).ok().or_else(|| B64.decode(text).ok())?;
    bytes.try_into().ok()
}

/// Sets the key for this process (at startup, before any secret is read)
pub fn init(key: &[u8; 32]) {
    let _ = CIPHER.set(Aes256Gcm::new(key.into()));
}

fn cipher() -> &'static Aes256Gcm {
    #[cfg(test)]
    init(&[42; 32]);
    CIPHER.get().expect("secrets::init runs at startup")
}

fn seal_with(cipher: &Aes256Gcm, plain: &str) -> String {
    let nonce = Nonce::generate();
    let ct = cipher.encrypt(&nonce, plain.as_bytes()).expect("AES-GCM encryption doesn't fail for short values");
    let mut out = nonce.to_vec();
    out.extend(ct);
    format!("{PREFIX}{}", B64.encode(out))
}

fn open_with(cipher: &Aes256Gcm, stored: &str) -> Result<String, String> {
    let Some(b64) = stored.strip_prefix(PREFIX) else { return Ok(stored.to_string()) };
    let raw = B64.decode(b64).map_err(|_| "damaged encrypted value".to_string())?;
    if raw.len() < NONCE_LEN {
        return Err("damaged encrypted value".into());
    }
    let (nonce, ct) = raw.split_at(NONCE_LEN);
    let nonce = Nonce::try_from(nonce).map_err(|_| "damaged encrypted value".to_string())?;
    let plain = cipher
        .decrypt(&nonce, ct)
        .map_err(|_| "can't decrypt a saved secret: the key (THIRTYFILE_SECRET_KEY, or secret.key in the data folder) isn't the one it was saved with".to_string())?;
    String::from_utf8(plain).map_err(|_| "damaged encrypted value".into())
}

/// Encrypts a secret for storing in the database; an empty value stays empty ("not set")
pub fn seal(plain: &str) -> String {
    if plain.is_empty() { String::new() } else { seal_with(cipher(), plain) }
}

/// Decrypts a stored secret (a value from before encryption is returned as it is)
pub fn open(stored: &str) -> Result<String, String> {
    open_with(cipher(), stored)
}

pub fn is_sealed(stored: &str) -> bool {
    stored.starts_with(PREFIX)
}

/// Re-encrypts every secret in the database with `new_key`: for rotating the key (`thirtyfile rotate-secret-key`)
/// and for encrypting values saved before encryption existed
pub fn reseal(old: &str, new_key: &[u8; 32]) -> Result<String, String> {
    let plain = open(old)?;
    if plain.is_empty() {
        return Ok(String::new());
    }
    Ok(seal_with(&Aes256Gcm::new(new_key.into()), &plain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_values_round_trip_and_need_the_same_key() {
        let sealed = seal("hunter-2-but-generated");
        assert!(is_sealed(&sealed) && !sealed.contains("hunter"));
        assert_eq!(open(&sealed).unwrap(), "hunter-2-but-generated");
        // Each value gets its own nonce
        assert_ne!(seal("x"), seal("x"));
        // Old plain values still read, empty stays empty
        assert_eq!(open("plain").unwrap(), "plain");
        assert_eq!(seal(""), "");
        // Another key can't read it
        let other = Aes256Gcm::new(&[7u8; 32].into());
        assert!(open_with(&other, &sealed).is_err());
        let moved = reseal(&sealed, &[7; 32]).unwrap();
        assert_eq!(open_with(&other, &moved).unwrap(), "hunter-2-but-generated");
    }

    #[test]
    fn keys_are_read_as_hex_or_base64_and_created_when_missing() {
        assert_eq!(parse_key(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(parse_key(&B64.encode([1u8; 32])), Some([1; 32]));
        assert!(parse_key("short").is_none());
        let dir = std::env::temp_dir().join(format!("thirtyfile-test-{}", crate::util::new_id()));
        let src = KeySource::from_settings(None, None, &dir);
        let first = src.load().unwrap();
        assert_eq!(src.load().unwrap(), first, "the created key is kept");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join("secret.key")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
