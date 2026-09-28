//! Encryption of the secrets kept in the database: the session and share signing secret, single sign-on client
//! secrets, and the passwords and keys of storage locations. A copy of `drive.db` on its own (a backup made with
//! `thirtyfile backup`, a copied file) then gives none of them away.
//!
//! The key comes from `THIRTYFILE_SECRET_KEY` (64 hex characters or base64 of 32 bytes), from the file in
//! `THIRTYFILE_SECRET_KEY_FILE`, or from `secret.key` in the data folder, created on first start. Kept in the data
//! folder, it is backed up with the database when the whole folder is: the backup guide says to back it up
//! separately, or to keep it elsewhere with one of the two settings.
//!
//! Values are stored as `enc:v2:<base64 of nonce and ciphertext>` (AES-256-GCM), with where they are stored as
//! associated data. An empty value stays empty ("not set").

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, Generate, KeyInit, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD as B64};

/// Values encrypted together with where they are stored (the `context`, such as `user:12:totp`), as associated data:
/// a value copied into another row or setting can't be read there
const PREFIX: &str = "enc:v2:";
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

/// The key tests run with: generated once per run, so the code holds no key
#[cfg(test)]
pub fn test_key() -> &'static [u8; 32] {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    KEY.get_or_init(rand::random)
}

fn cipher() -> &'static Aes256Gcm {
    #[cfg(test)]
    init(test_key());
    CIPHER.get().expect("secrets::init runs at startup")
}

fn seal_with(cipher: &Aes256Gcm, context: &str, plain: &str) -> String {
    let nonce = Nonce::generate();
    let ct = cipher
        .encrypt(&nonce, Payload { msg: plain.as_bytes(), aad: context.as_bytes() })
        .expect("AES-GCM encryption doesn't fail for short values");
    let mut out = nonce.to_vec();
    out.extend(ct);
    format!("{PREFIX}{}", B64.encode(out))
}

fn open_with(cipher: &Aes256Gcm, context: &str, stored: &str) -> Result<String, String> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    let b64 = stored.strip_prefix(PREFIX).ok_or_else(|| "damaged encrypted value".to_string())?;
    let raw = B64.decode(b64).map_err(|_| "damaged encrypted value".to_string())?;
    if raw.len() < NONCE_LEN {
        return Err("damaged encrypted value".into());
    }
    let (nonce, ct) = raw.split_at(NONCE_LEN);
    let nonce = Nonce::try_from(nonce).map_err(|_| "damaged encrypted value".to_string())?;
    let plain = cipher.decrypt(&nonce, Payload { msg: ct, aad: context.as_bytes() }).map_err(|_| {
        "can't decrypt a saved secret: the key (THIRTYFILE_SECRET_KEY, or secret.key in the data folder) isn't the one it was saved with, or it was saved for something else".to_string()
    })?;
    String::from_utf8(plain).map_err(|_| "damaged encrypted value".into())
}

/// Encrypts a secret for storing in the database at `context` (what it belongs to, such as `user:12:totp`); an empty
/// value stays empty ("not set")
pub fn seal(context: &str, plain: &str) -> String {
    if plain.is_empty() { String::new() } else { seal_with(cipher(), context, plain) }
}

/// Decrypts a secret stored at `context`
pub fn open(context: &str, stored: &str) -> Result<String, String> {
    open_with(cipher(), context, stored)
}

/// Re-encrypts a secret stored at `context` with `new_key`, for rotating the key (`thirtyfile rotate-secret-key`)
pub fn reseal(context: &str, old: &str, new_key: &[u8; 32]) -> Result<String, String> {
    let plain = open(context, old)?;
    if plain.is_empty() {
        return Ok(String::new());
    }
    Ok(seal_with(&Aes256Gcm::new(new_key.into()), context, &plain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_values_round_trip_and_need_the_same_key() {
        let sealed = seal("user:1:totp", "hunter-2-but-generated");
        assert!(sealed.starts_with(PREFIX) && !sealed.contains("hunter"));
        assert_eq!(open("user:1:totp", &sealed).unwrap(), "hunter-2-but-generated");
        // Each value gets its own nonce
        assert_ne!(seal("c", "x"), seal("c", "x"));
        // Empty stays empty; anything else must be encrypted
        assert_eq!(seal("c", ""), "");
        assert_eq!(open("c", "").unwrap(), "");
        assert!(open("c", "plain").is_err());
        // Another key can't read it
        let other_key: [u8; 32] = rand::random();
        let other = Aes256Gcm::new(&other_key.into());
        assert!(open_with(&other, "user:1:totp", &sealed).is_err());
        let moved = reseal("user:1:totp", &sealed, &other_key).unwrap();
        assert_eq!(open_with(&other, "user:1:totp", &moved).unwrap(), "hunter-2-but-generated");
    }

    #[test]
    fn a_value_copied_elsewhere_cant_be_read_there() {
        let sealed = seal("user:1:totp", "secret");
        assert!(open("user:2:totp", &sealed).is_err(), "bound to where it is stored");
        assert_eq!(open("user:1:totp", &sealed).unwrap(), "secret");
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
